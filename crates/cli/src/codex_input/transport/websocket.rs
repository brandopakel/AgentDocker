//! Strict loopback WebSocket framing. Capability bytes never enter diagnostics.
use super::MAX_FRAME;
use anyhow::{Context, Result, bail, ensure};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::{
    net::{Ipv4Addr, SocketAddrV4},
    time::Duration,
};
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{
    WebSocketStream, client_async_with_config,
    tungstenite::{
        Message,
        client::IntoClientRequest,
        http::{HeaderValue, header::AUTHORIZATION},
        protocol::WebSocketConfig,
    },
};

pub(super) struct Remote {
    socket: WebSocketStream<TcpStream>,
}

impl Remote {
    pub async fn connect(port: u16, token: &str) -> Result<Self> {
        ensure!(
            port != 0
                && (32..=256).contains(&token.len())
                && token
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "invalid native server capability"
        );
        let mut request = format!("ws://127.0.0.1:{port}/").into_client_request()?;
        let mut header = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| anyhow::anyhow!("invalid native server capability header"))?;
        header.set_sensitive(true);
        request.headers_mut().insert(AUTHORIZATION, header);
        let config = WebSocketConfig::default()
            .read_buffer_size(8192)
            .write_buffer_size(0)
            .max_write_buffer_size(MAX_FRAME + 8192)
            .max_message_size(Some(MAX_FRAME))
            .max_frame_size(Some(MAX_FRAME));
        timeout(Duration::from_secs(5), async {
            let stream = TcpStream::connect(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
                .await
                .context("native server connection failed")?;
            // No URI lookup, redirect, proxy, TLS downgrade or body/error echo.
            let (socket, _) = client_async_with_config(request, stream, Some(config))
                .await
                .map_err(|_| anyhow::anyhow!("native server authentication/handshake failed"))?;
            Ok(Self { socket })
        })
        .await
        .context("native server connection timed out")?
    }

    pub async fn send(&mut self, data: Vec<u8>) -> Result<()> {
        ensure!(
            data.len() <= MAX_FRAME,
            "native request exceeds frame limit"
        );
        timeout(
            Duration::from_secs(5),
            self.socket
                .send(Message::Text(String::from_utf8(data)?.into())),
        )
        .await
        .context("native server write timed out")?
        .map_err(|_| anyhow::anyhow!("native server write failed"))
    }

    pub async fn receive(&mut self) -> Result<Value> {
        for _ in 0..64 {
            let message = self
                .socket
                .next()
                .await
                .context("native server disconnected")?
                .map_err(|_| anyhow::anyhow!("native server frame failed"))?;
            match message {
                Message::Text(text) => {
                    return serde_json::from_slice(text.as_bytes())
                        .context("native server sent invalid JSON");
                }
                Message::Ping(_) | Message::Pong(_) => {
                    timeout(Duration::from_secs(5), self.socket.flush())
                        .await
                        .context("native server control frame timed out")?
                        .map_err(|_| anyhow::anyhow!("native server control frame failed"))?;
                }
                Message::Close(_) => bail!("native server closed the connection"),
                _ => bail!("native server sent a non-text protocol frame"),
            }
        }
        bail!("native server exceeded control-frame bound")
    }

    pub async fn close(mut self) -> Result<()> {
        timeout(Duration::from_secs(5), self.socket.close(None))
            .await
            .context("native server observer close timed out")?
            .map_err(|_| anyhow::anyhow!("native server observer close failed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codex_input::transport::Provider;
    use serde_json::json;
    use tokio::{io::AsyncWriteExt, net::TcpListener};

    const TOKEN: &str = "private-test-capability-0123456789abcdef";

    struct VerifyAuth;
    impl tokio_tungstenite::tungstenite::handshake::server::Callback for VerifyAuth {
        fn on_request(
            self,
            request: &tokio_tungstenite::tungstenite::handshake::server::Request,
            response: tokio_tungstenite::tungstenite::handshake::server::Response,
        ) -> std::result::Result<
            tokio_tungstenite::tungstenite::handshake::server::Response,
            tokio_tungstenite::tungstenite::handshake::server::ErrorResponse,
        > {
            assert_eq!(request.headers()[AUTHORIZATION], format!("Bearer {TOKEN}"));
            Ok(response)
        }
    }

    async fn read(socket: &mut WebSocketStream<TcpStream>) -> Value {
        let message = timeout(Duration::from_secs(3), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        serde_json::from_slice(message.into_text().unwrap().as_bytes()).unwrap()
    }

    #[tokio::test]
    async fn authenticated_observer_discards_hints_and_closes_only_its_connection() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_hdr_async(stream, VerifyAuth)
                .await
                .unwrap();
            let initialize = read(&mut socket).await;
            assert_eq!(initialize["method"], "initialize");
            assert!(
                initialize["params"]["capabilities"]["optOutNotificationMethods"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("item/agentMessage/delta"))
            );
            socket
                .send(Message::text(
                    json!({"id":initialize["id"],"result":{"codexHome":"private-profile"}})
                        .to_string(),
                ))
                .await
                .unwrap();
            assert_eq!(read(&mut socket).await["method"], "initialized");
            let request = read(&mut socket).await;
            assert_eq!(request["method"], "thread/read");
            for _ in 0..100 {
                socket
                    .send(Message::text(
                        json!({"method":"item/completed","params":{}}).to_string(),
                    ))
                    .await
                    .unwrap();
            }
            socket
                .send(Message::text(
                    json!({"id":request["id"],"result":{"thread":{"id":"exact"}}}).to_string(),
                ))
                .await
                .unwrap();
            assert!(matches!(
                socket.next().await.unwrap().unwrap(),
                Message::Close(_)
            ));
            // The listening server survives observer shutdown and can accept a
            // new observer; it never receives an approval response.
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            assert!(matches!(
                socket.next().await.unwrap().unwrap(),
                Message::Close(_)
            ));
        });
        let mut provider = Provider::connect_native(port, TOKEN).await.unwrap();
        assert_eq!(
            provider.initialize().await.unwrap()["codexHome"],
            "private-profile"
        );
        assert_eq!(
            provider
                .request("thread/read", json!({"threadId":"exact"}))
                .await
                .unwrap()["thread"]["id"],
            "exact"
        );
        assert!(provider.pending.is_empty());
        assert!(provider.next().await.is_err());
        assert!(
            provider
                .send(&json!({"id":9,"result":{"decision":"accept"}}))
                .await
                .is_err()
        );
        provider.shutdown().await.unwrap();
        Provider::connect_native(port, TOKEN)
            .await
            .unwrap()
            .shutdown()
            .await
            .unwrap();
        timeout(Duration::from_secs(3), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn native_approval_request_is_never_answered_by_the_observer() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let request = read(&mut socket).await;
            socket.send(Message::text(json!({"id":request["id"],"method":"item/commandExecution/requestApproval","params":{}}).to_string())).await.unwrap();
            assert!(matches!(
                socket.next().await.unwrap().unwrap(),
                Message::Close(_)
            ));
        });
        let mut provider = Provider::connect_native(port, TOKEN).await.unwrap();
        assert!(
            provider
                .request("thread/read", json!({}))
                .await
                .unwrap_err()
                .to_string()
                .contains("unexpected request")
        );
        provider.shutdown().await.unwrap();
        timeout(Duration::from_secs(3), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn invalid_frames_notification_floods_and_foreign_replies_fail_closed() {
        for case in 0..4 {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                read(&mut socket).await;
                let frames = match case {
                    0 => vec![Message::Binary(vec![1, 2, 3].into())],
                    1 => vec![Message::text("x".repeat(MAX_FRAME + 1))],
                    2 => vec![Message::text("{\"method\":\"notification\"}"); 513],
                    _ => vec![Message::text("{\"id\":999,\"result\":{}}")],
                };
                for frame in frames {
                    if socket.send(frame).await.is_err() {
                        break;
                    }
                }
            });
            let mut provider = Provider::connect_native(port, TOKEN).await.unwrap();
            let error = provider
                .request("thread/read", json!({}))
                .await
                .unwrap_err()
                .to_string();
            assert!(
                error.contains(match case {
                    0 => "non-text",
                    1 => "frame failed",
                    2 => "request bound",
                    _ => "unexpected request",
                }),
                "case {case}: {error}"
            );
            assert!(provider.pending.is_empty());
            drop(provider);
            timeout(Duration::from_secs(3), server)
                .await
                .unwrap()
                .unwrap();
        }
    }

    #[tokio::test]
    async fn authentication_failures_do_not_echo_capability_or_response_body() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let reply = format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\nX-Token: {TOKEN}\r\n\r\n{TOKEN}",
                TOKEN.len()
            );
            stream.write_all(reply.as_bytes()).await.unwrap();
        });
        let error = match Remote::connect(port, TOKEN).await {
            Ok(_) => panic!("accepted failed authentication"),
            Err(e) => format!("{e:#}"),
        };
        assert!(error.contains("authentication/handshake failed"));
        assert!(!error.contains(TOKEN));
        assert!(!error.contains("X-Token"));
        timeout(Duration::from_secs(3), server)
            .await
            .unwrap()
            .unwrap();
        assert!(Remote::connect(port, "short").await.is_err());
        assert!(
            Remote::connect(port, &format!("{TOKEN}\r\nX: injected"))
                .await
                .is_err()
        );
    }
}

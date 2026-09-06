//! Bounded, opaque restricted-protocol relay over the engine CLI's attached streams.
use agentdocker_core::AgentRecord;
use agentdocker_host::containers::ContainerError;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, io, path::PathBuf, process::Stdio, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    sync::{Semaphore, mpsc},
    task::{AbortHandle, JoinHandle, JoinSet},
};

const FRAME: usize = 1024 * 1024;
#[derive(Serialize, Deserialize)]
struct Envelope {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    frame: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    close: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    ready: bool,
}
async fn line(reader: &mut (impl AsyncBufRead + Unpin), max: usize) -> io::Result<String> {
    let mut line = String::new();
    (&mut *reader)
        .take((max + 1) as u64)
        .read_line(&mut line)
        .await?;
    if line.len() > max || !line.ends_with('\n') {
        return Err(io::Error::other("invalid relay frame length"));
    }
    Ok(line)
}
// Keep partial bytes outside the select future: other ready branches may cancel it.
async fn envelope_line(
    reader: &mut (impl AsyncBufRead + Unpin),
    buffer: &mut Vec<u8>,
) -> io::Result<String> {
    let max = 8 * FRAME;
    (&mut *reader)
        .take((max + 1 - buffer.len()) as u64)
        .read_until(b'\n', buffer)
        .await?;
    if buffer.len() > max || !buffer.ends_with(b"\n") {
        return Err(io::Error::other("invalid relay envelope length"));
    }
    String::from_utf8(std::mem::take(buffer)).map_err(io::Error::other)
}
pub(super) fn needs_cleanup(record: &AgentRecord) -> bool {
    record
        .container
        .as_ref()
        .and_then(|c| c.workspace.as_ref())
        .and_then(|w| w.access.as_ref())
        .and_then(|a| a.relay.as_ref())
        .is_some_and(|r| !r.retired)
}
pub(super) async fn start(
    record: AgentRecord,
    target: PathBuf,
) -> Result<JoinHandle<()>, ContainerError> {
    let prepared = record.clone();
    let args = tokio::task::spawn_blocking(move || agentdocker_host::relay::start_args(&prepared))
        .await
        .map_err(|e| ContainerError::unavailable(e.to_string()))??;
    let mut child = tokio::process::Command::new(&args[0])
        .args(&args[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| ContainerError::unavailable(e.to_string()))?;
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let ready = tokio::time::timeout(Duration::from_secs(15), line(&mut stdout, 1024))
        .await
        .map_err(|_| {
            ContainerError::unavailable("engine socket relay did not become ready".into())
        })?
        .map_err(|_| {
            ContainerError::unavailable("engine socket relay exited before readiness".into())
        })?;
    let ready: Envelope = serde_json::from_str(&ready)
        .map_err(|_| ContainerError::invalid("invalid engine relay readiness response".into()))?;
    if !ready.ready || !ready.id.is_empty() || ready.frame.is_some() {
        return Err(ContainerError::invalid(
            "invalid engine relay readiness response".into(),
        ));
    }
    tokio::task::spawn_blocking(move || agentdocker_host::relay::verify_helper(&record))
        .await
        .map_err(|e| ContainerError::unavailable(e.to_string()))??;
    let stdin = child.stdin.take().unwrap();
    Ok(tokio::spawn(async move {
        tokio::select! {
            () = forward(stdout, stdin, target) => {},
            status = child.wait() => {tracing::warn!(?status, "relay CLI exited");},
        }
        // Closing the attached stream makes the helper exit. Recovery additionally
        // verifies and replaces an orphan by its exact owned engine identity.
        let _ = child.kill().await;
    }))
}

struct Peer {
    input: mpsc::Sender<String>,
    generation: u64,
    abort: AbortHandle,
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

async fn forward(
    mut stdout: impl AsyncBufRead + Unpin,
    mut stdin: impl tokio::io::AsyncWrite + Unpin,
    target: PathBuf,
) {
    let (output, mut responses) = mpsc::channel::<(u64, Envelope)>(32);
    let mut peers: HashMap<String, Peer> = HashMap::new();
    let slots = Arc::new(Semaphore::new(32));
    let mut generation = 0u64;
    let mut tasks = JoinSet::new();
    let mut pending_line = Vec::new();
    loop {
        // Reap cancelled/completed workers even while input stays continuously ready.
        while tasks.try_join_next().is_some() {}
        tokio::select! {
            raw=envelope_line(&mut stdout,&mut pending_line)=> {
                let Ok(raw)=raw else {tracing::warn!("relay CLI stream ended");break;};
                let Ok(message)=serde_json::from_str::<Envelope>(&raw) else {tracing::warn!("invalid relay envelope");break;};
                if message.id.len()!=32 || !message.id.bytes().all(|b|b.is_ascii_hexdigit()) || message.ready {break;}
                if message.close {peers.remove(&message.id);continue;}
                let Some(frame)=message.frame.filter(|f|f.len()<=FRAME && f.ends_with('\n')) else {break;};
                if !peers.contains_key(&message.id) {
                    if peers.len()>=32 {break;}
                    let Some(next)=generation.checked_add(1) else {break;};
                    generation=next;
                    // Cancellation releases the permit only once the old upstream
                    // connection has actually been dropped.
                    let Ok(permit)=slots.clone().acquire_owned().await else {break;};
                    let (input,mut requests)=mpsc::channel::<String>(2);
                    let id=message.id.clone();let target=target.clone();let output=output.clone();
                    let abort=tasks.spawn(async move {
                        let _permit=permit;
                        let outcome=tokio::time::timeout(Duration::from_secs(30),async {
                            let mut upstream=BufReader::new(UnixStream::connect(target).await?);
                            for _ in 0..2 {
                                let Some(frame)=requests.recv().await else {return Ok::<_,io::Error>(());};
                                upstream.get_mut().write_all(frame.as_bytes()).await?;
                                let response=line(&mut upstream,FRAME).await?;
                                output.send((next,Envelope {id:id.clone(),frame:Some(response),close:false,ready:false})).await.map_err(io::Error::other)?;
                            }
                            Ok(())
                        }).await;
                        if !matches!(outcome,Ok(Ok(()))) {
                            let _=output.send((next,Envelope {id,frame:None,close:true,ready:false})).await;
                        }
                    });
                    peers.insert(message.id.clone(),Peer {input,generation:next,abort});
                }
                if peers.get(&message.id).unwrap().input.try_send(frame).is_err() {break;}
            }
            Some((generation,response))=responses.recv()=> {
                if !peers.get(&response.id).is_some_and(|p|p.generation==generation) {continue;}
                let Ok(mut frame)=serde_json::to_vec(&response) else {break;};frame.push(b'\n');
                if !matches!(tokio::time::timeout(Duration::from_secs(30),stdin.write_all(&frame)).await,Ok(Ok(()))) {break;}
            }
            Some(_)=tasks.join_next(),if !tasks.is_empty()=> {
                // Keep queued replies valid until the helper closes this generation.
                // A completion must never evict a replacement using the same ID.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn close_cancels_upstream_and_reused_identity_keeps_its_replies() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let root = tempfile::tempdir_in("/tmp").unwrap();
            let target = root.path().join("upstream");
            let listener = tokio::net::UnixListener::bind(&target).unwrap();
            let (helper, daemon) = tokio::io::duplex(16384);
            let (reader, writer) = tokio::io::split(daemon);
            let broker = tokio::spawn(forward(BufReader::new(reader), writer, target));
            let (reader, mut writer) = tokio::io::split(helper);
            let mut reader = BufReader::new(reader);
            let id = "a".repeat(32);
            let request = format!("{{\"id\":\"{id}\",\"frame\":\"first\\n\"}}\n");
            let close = format!("{{\"id\":\"{id}\",\"close\":true}}\n");
            // More than the admission limit, with every upstream blocked on a
            // response. Closing must release the actual connection promptly.
            for _ in 0..40 {
                writer.write_all(request.as_bytes()).await.unwrap();
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = BufReader::new(stream);
                assert_eq!(line(&mut stream, FRAME).await.unwrap(), "first\n");
                writer.write_all(close.as_bytes()).await.unwrap();
                let mut tail = String::new();
                assert_eq!(stream.read_line(&mut tail).await.unwrap(), 0);
            }
            writer.write_all(request.as_bytes()).await.unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            assert_eq!(line(&mut stream, FRAME).await.unwrap(), "first\n");
            stream
                .get_mut()
                .write_all(b"new first reply\n")
                .await
                .unwrap();
            let reply: Envelope =
                serde_json::from_str(&line(&mut reader, 8 * FRAME).await.unwrap()).unwrap();
            assert_eq!(reply.frame.as_deref(), Some("new first reply\n"));
            writer.write_all(request.as_bytes()).await.unwrap();
            assert_eq!(line(&mut stream, FRAME).await.unwrap(), "first\n");
            stream
                .get_mut()
                .write_all(b"new final reply\n")
                .await
                .unwrap();
            let reply: Envelope =
                serde_json::from_str(&line(&mut reader, 8 * FRAME).await.unwrap()).unwrap();
            assert_eq!(reply.frame.as_deref(), Some("new final reply\n"));
            writer.write_all(close.as_bytes()).await.unwrap();
            writer.shutdown().await.unwrap();
            broker.await.unwrap();
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn cancelled_envelope_read_keeps_partial_bytes_and_checks_the_complete_limit() {
        let (mut writer, reader) = tokio::io::duplex(16384);
        let mut reader = BufReader::new(reader);
        let mut pending = Vec::new();
        writer.write_all(b"{\"id\":\"partial").await.unwrap();
        assert!(
            tokio::time::timeout(
                Duration::from_millis(10),
                envelope_line(&mut reader, &mut pending)
            )
            .await
            .is_err()
        );
        assert!(!pending.is_empty());
        writer.write_all(b"\"}\n").await.unwrap();
        assert_eq!(
            envelope_line(&mut reader, &mut pending).await.unwrap(),
            "{\"id\":\"partial\"}\n"
        );
        assert!(pending.is_empty());
        pending = vec![b'x'; 8 * FRAME];
        writer.write_all(b"x\n").await.unwrap();
        assert!(envelope_line(&mut reader, &mut pending).await.is_err());
    }
}

//! Just enough HTTP/1.1 for the connector: one request per connection,
//! read with bounds and a deadline, answered with a `Content-Length` and
//! closed. The tunnel in front terminates TLS and keeps its own
//! connections alive; this side only has to be correct and small.
//! Hand-rolled like the JSON-RPC in `mcp.rs`, and for the same reason: a
//! dependency that parses the internet for us is a larger surface than a
//! parser we can read in one sitting.

use std::fmt;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The request line may not exceed this.
pub const MAX_LINE: usize = 8 * 1024;
/// All header lines together may not exceed this.
pub const MAX_HEADERS: usize = 32 * 1024;
/// A body may not exceed this: a JSON-RPC batch or a token form is far
/// smaller, and a page is never posted here.
pub const MAX_BODY: usize = 1024 * 1024;
/// How long one request may take to arrive in full.
pub const READ_DEADLINE: Duration = Duration::from_secs(15);

#[derive(Debug)]
pub enum HttpError {
    /// The peer closed before a request line.
    Closed,
    Timeout,
    /// The request is malformed, or outside the bounds; the status to answer.
    Bad(u16, &'static str),
    Io(std::io::Error),
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => write!(f, "connection closed"),
            Self::Timeout => write!(f, "request did not arrive in time"),
            Self::Bad(status, why) => write!(f, "{status}: {why}"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<std::io::Error> for HttpError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    /// Path and query as sent, without scheme or host.
    pub target: String,
    /// Names lowercased; values trimmed.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or("")
    }

    pub fn query(&self) -> &str {
        self.target.split_once('?').map(|(_, q)| q).unwrap_or("")
    }

    /// The first header of that name, case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.as_str())
    }

    /// `Authorization: Bearer <token>`, when present and well-formed.
    pub fn bearer(&self) -> Option<&str> {
        let value = self.header("authorization")?;
        let (scheme, token) = value.split_once(' ')?;
        (scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty()).then(|| token.trim())
    }

    pub fn content_type(&self) -> Option<&str> {
        self.header("content-type")
            .map(|v| v.split(';').next().unwrap_or("").trim())
    }

    /// The body as a form, when it says it is one.
    pub fn form(&self) -> Option<Vec<(String, String)>> {
        (self.content_type() == Some("application/x-www-form-urlencoded"))
            .then(|| parse_query(std::str::from_utf8(&self.body).unwrap_or("")))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn new(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_owned(), value.into()));
        self
    }

    pub fn body(mut self, content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self.header("Content-Type", content_type)
    }

    pub fn json(status: u16, value: &Value) -> Self {
        Self::new(status).body(
            "application/json",
            serde_json::to_vec(value).unwrap_or_default(),
        )
    }

    pub fn text(status: u16, text: impl Into<String>) -> Self {
        Self::new(status).body("text/plain; charset=utf-8", text.into())
    }

    pub fn html(status: u16, html: impl Into<String>) -> Self {
        Self::new(status).body("text/html; charset=utf-8", html.into())
    }

    /// A `302` to `location`, with no body to speak of.
    pub fn redirect(location: &str) -> Self {
        Self::new(302).header("Location", location)
    }

    pub fn reason(status: u16) -> &'static str {
        match status {
            200 => "OK",
            201 => "Created",
            202 => "Accepted",
            204 => "No Content",
            302 => "Found",
            400 => "Bad Request",
            401 => "Unauthorized",
            403 => "Forbidden",
            404 => "Not Found",
            405 => "Method Not Allowed",
            408 => "Request Timeout",
            411 => "Length Required",
            413 => "Payload Too Large",
            415 => "Unsupported Media Type",
            429 => "Too Many Requests",
            431 => "Request Header Fields Too Large",
            500 => "Internal Server Error",
            501 => "Not Implemented",
            _ => "",
        }
    }
}

/// Read one request. Bounds first, deadline over the whole read: a peer
/// that trickles headers cannot hold a task forever.
pub async fn read_request<R>(reader: &mut R) -> Result<Request, HttpError>
where
    R: AsyncBufReadExt + Unpin,
{
    tokio::time::timeout(READ_DEADLINE, read_request_inner(reader))
        .await
        .map_err(|_| HttpError::Timeout)?
}

async fn read_line<R>(reader: &mut R, limit: usize) -> Result<Option<String>, HttpError>
where
    R: AsyncBufReadExt + Unpin,
{
    let mut line = Vec::new();
    let mut taken = (&mut *reader).take(limit as u64 + 1);
    let read = taken.read_until(b'\n', &mut line).await?;
    if read == 0 {
        return Ok(None);
    }
    if line.len() > limit {
        return Err(HttpError::Bad(431, "line too long"));
    }
    if line.last() != Some(&b'\n') {
        return Err(HttpError::Bad(400, "unterminated line"));
    }
    line.pop();
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    String::from_utf8(line)
        .map(Some)
        .map_err(|_| HttpError::Bad(400, "line is not UTF-8"))
}

async fn read_request_inner<R>(reader: &mut R) -> Result<Request, HttpError>
where
    R: AsyncBufReadExt + Unpin,
{
    let Some(line) = read_line(reader, MAX_LINE).await? else {
        return Err(HttpError::Closed);
    };
    let mut parts = line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(HttpError::Bad(400, "malformed request line"));
    };
    if !matches!(version, "HTTP/1.1" | "HTTP/1.0") {
        return Err(HttpError::Bad(400, "unsupported HTTP version"));
    }
    if method.is_empty() || !method.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(HttpError::Bad(400, "malformed method"));
    }
    if !target.starts_with('/') {
        return Err(HttpError::Bad(400, "target must be a path"));
    }
    let mut headers = Vec::new();
    let mut total = 0usize;
    loop {
        let Some(line) = read_line(reader, MAX_LINE).await? else {
            return Err(HttpError::Bad(400, "headers cut short"));
        };
        if line.is_empty() {
            break;
        }
        total += line.len();
        if total > MAX_HEADERS || headers.len() >= 100 {
            return Err(HttpError::Bad(431, "too many headers"));
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(HttpError::Bad(400, "malformed header"));
        };
        if name.is_empty() || name.contains(' ') {
            return Err(HttpError::Bad(400, "malformed header name"));
        }
        headers.push((name.to_ascii_lowercase(), value.trim().to_owned()));
    }
    let request = Request {
        method: method.to_owned(),
        target: target.to_owned(),
        headers,
        body: Vec::new(),
    };
    if request
        .header("transfer-encoding")
        .is_some_and(|te| !te.eq_ignore_ascii_case("identity"))
    {
        return Err(HttpError::Bad(501, "chunked requests are not accepted"));
    }
    let length = match request.header("content-length") {
        None => 0,
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| HttpError::Bad(400, "malformed Content-Length"))?,
    };
    if length > MAX_BODY {
        return Err(HttpError::Bad(413, "body too large"));
    }
    let mut body = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut body).await?;
    }
    Ok(Request { body, ..request })
}

/// Write a response and finish: `Connection: close` is the whole
/// connection model here.
pub async fn write_response<W>(writer: &mut W, response: &Response) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let mut head = format!(
        "HTTP/1.1 {} {}\r\n",
        response.status,
        Response::reason(response.status)
    );
    for (name, value) in &response.headers {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str(&format!("Content-Length: {}\r\n", response.body.len()));
    head.push_str("Cache-Control: no-store\r\nConnection: close\r\n\r\n");
    writer.write_all(head.as_bytes()).await?;
    writer.write_all(&response.body).await?;
    writer.flush().await
}

/// `a=1&b=two%20words&c=x+y` as pairs, decoded; a malformed escape is
/// kept as written rather than dropped, so nothing silently disappears.
pub fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(key), percent_decode(value))
        })
        .collect()
}

/// The value of `name` in decoded pairs, when present and non-empty.
pub fn field<'a>(pairs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
        .filter(|v| !v.is_empty())
}

pub fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                    .ok()
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok());
                match hex {
                    Some(byte) => {
                        out.push(byte);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Percent-encode for a query component: everything but the unreserved set.
pub fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Escape text for an HTML page.
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    async fn parse(raw: &[u8]) -> Result<Request, HttpError> {
        let mut reader = tokio::io::BufReader::new(Cursor::new(raw.to_vec()));
        read_request(&mut reader).await
    }

    #[tokio::test]
    async fn a_request_with_a_body_parses_and_headers_are_case_insensitive() {
        let raw = b"POST /mcp?x=1 HTTP/1.1\r\nHost: h\r\nContent-Type: application/json\r\nAuthorization: Bearer  tok \r\nContent-Length: 2\r\n\r\n{}";
        let request = parse(raw).await.unwrap();
        assert_eq!(request.method, "POST");
        assert_eq!(request.path(), "/mcp");
        assert_eq!(request.query(), "x=1");
        assert_eq!(request.header("HOST"), Some("h"));
        assert_eq!(request.bearer(), Some("tok"));
        assert_eq!(request.content_type(), Some("application/json"));
        assert_eq!(request.body, b"{}");
    }

    #[tokio::test]
    async fn bounds_and_malformed_requests_answer_with_a_status_not_a_hang() {
        let closed = parse(b"").await.unwrap_err();
        assert!(matches!(closed, HttpError::Closed));
        let long = format!("GET /{} HTTP/1.1\r\n\r\n", "a".repeat(MAX_LINE));
        assert!(matches!(
            parse(long.as_bytes()).await.unwrap_err(),
            HttpError::Bad(431, _)
        ));
        assert!(matches!(
            parse(b"GET / HTTP/2\r\n\r\n").await.unwrap_err(),
            HttpError::Bad(400, _)
        ));
        assert!(matches!(
            parse(b"GET / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n")
                .await
                .unwrap_err(),
            HttpError::Bad(501, _)
        ));
        let big = format!(
            "POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY + 1
        );
        assert!(matches!(
            parse(big.as_bytes()).await.unwrap_err(),
            HttpError::Bad(413, _)
        ));
        assert!(matches!(
            parse(b"GET / HTTP/1.1\r\nNo colon here\r\n\r\n")
                .await
                .unwrap_err(),
            HttpError::Bad(400, _)
        ));
        assert!(matches!(
            parse(b"GET http://x/ HTTP/1.1\r\n\r\n").await.unwrap_err(),
            HttpError::Bad(400, _)
        ));
    }

    #[tokio::test]
    async fn a_response_carries_its_length_and_closes() {
        let mut out = Vec::new();
        let response = Response::json(200, &serde_json::json!({"ok": true}));
        write_response(&mut out, &response).await.unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains("Content-Type: application/json\r\n"));
        assert!(text.contains("Content-Length: 11\r\n"));
        assert!(text.contains("Connection: close\r\n\r\n{\"ok\":true}"));
        let mut out = Vec::new();
        write_response(&mut out, &Response::redirect("https://x/cb?code=1"))
            .await
            .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("HTTP/1.1 302 Found\r\nLocation: https://x/cb?code=1\r\n"));
    }

    #[test]
    fn forms_decode_and_encode_round_trip() {
        let pairs = parse_query("a=1&b=two%20words&c=x+y&d=%zz&e");
        assert_eq!(field(&pairs, "b"), Some("two words"));
        assert_eq!(field(&pairs, "c"), Some("x y"));
        assert_eq!(field(&pairs, "d"), Some("%zz"), "a bad escape is kept");
        assert_eq!(field(&pairs, "e"), None, "empty is absent");
        let odd = "state with spaces&=?/ü";
        assert_eq!(percent_decode(&percent_encode(odd)), odd);
        assert_eq!(
            escape_html("<a href=\"x\">&'"),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;"
        );
        let request = Request {
            method: "POST".into(),
            target: "/token".into(),
            headers: vec![(
                "content-type".into(),
                "application/x-www-form-urlencoded; charset=utf-8".into(),
            )],
            body: b"grant_type=refresh_token&refresh_token=r1".to_vec(),
        };
        assert_eq!(
            field(&request.form().unwrap(), "grant_type"),
            Some("refresh_token")
        );
    }
}

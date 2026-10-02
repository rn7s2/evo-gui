//! A blocking HTTP/1.1 client for serve's protocol: loopback only, a bearer
//! token on every request (CONTRACT.md §5).
//!
//! It is hand-rolled on `std::net::TcpStream` on purpose: serve's HTTP is small
//! and speaks nothing else, and an SSE stream is read line by line on a
//! dedicated thread anyway (§5.3), so a runtime is dead weight.

use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpStream};
use std::time::Duration;

use serde_json::Value;

use crate::error::{Error, Result, StatusError};

/// A bearer token. Its `Debug` never prints the secret (§2.4: never log it).
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    pub fn new(text: impl Into<String>) -> Token {
        Token(text.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Whether a host names this machine. Everything else is refused: there is no
/// TLS and serve's trust boundary is the loopback interface.
pub fn is_loopback(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    match bare.parse::<IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        Err(_) => false,
    }
}

/// How much patience a body of `bytes` bytes buys, on top of the base timeout — a
/// request is not the only thing on this machine's clock, and the server's own
/// work is not the client's to hurry.
///
/// A turn carrying a pasted picture costs the server far more than its size:
/// measured 2026-10-02 against `/usr/local/bin/evo-agent`, a turn with a 314 KB
/// PNG (419 KB of base64) was *answered* 8.7 s after the request and one with a
/// 640 KB PNG (854 KB of base64, the payload the journal held) after 36 s — while
/// a body of the same 854 KB made of words was answered in 41 ms. Twice the
/// picture, four times the wait: the cost is the server's handling of the base64
/// image, not the transport. The base 30 s patience is therefore shorter than the
/// request itself for one pasted screenshot, and a client that gives up there
/// reports a refusal for a turn the session has already taken — which is how a
/// reader ends up sending the same picture three times.
///
/// The model is deliberately crude — the body's size at a rate under any pace
/// measured here, capped so that a request cannot be held open all day. It is a
/// deadline, not a delay: a send that is answered in 36 s is shown in 36 s, and
/// the rest of the patience is only what a wedged server would have used.
const BODY_RATE_FLOOR: u64 = 8 * 1024;

/// The most extra patience one body may earn, whatever it carries. The largest
/// picture this server will take is about 1.5 MB of base64 (past ~2 MB it refuses
/// the body outright), which its own rate of growth puts under four minutes.
const BODY_PATIENCE_CAP: Duration = Duration::from_secs(240);

/// An HTTP client bound to one server.
#[derive(Clone, Debug)]
pub struct HttpClient {
    host: String,
    port: u16,
    token: Token,
    timeout: Duration,
    stream_timeout: Duration,
}

impl HttpClient {
    pub fn new(host: impl Into<String>, port: u16, token: Token) -> Result<HttpClient> {
        let host = host.into();
        if !is_loopback(&host) {
            return Err(Error::Config(format!(
                "refusing to talk to {host:?}: only loopback servers are supported"
            )));
        }
        Ok(HttpClient {
            host,
            port,
            token,
            timeout: Duration::from_secs(30),
            stream_timeout: Duration::from_secs(45),
        })
    }

    pub fn loopback(port: u16, token: Token) -> HttpClient {
        HttpClient {
            host: "127.0.0.1".to_owned(),
            port,
            token,
            timeout: Duration::from_secs(30),
            stream_timeout: Duration::from_secs(45),
        }
    }

    /// The same server, with a different patience — a `/shutdown` is asked for
    /// with a short one, so a wedged server reaches the next rung of the ladder.
    pub fn with_timeout(&self, timeout: Duration) -> HttpClient {
        HttpClient {
            timeout,
            ..self.clone()
        }
    }

    /// The patience for an idle long-lived stream: serve keeps one alive with a
    /// `: keepalive` comment every 15 s, so a silence past this is a dead socket.
    pub fn with_stream_timeout(&self, timeout: Duration) -> HttpClient {
        HttpClient {
            stream_timeout: timeout,
            ..self.clone()
        }
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn token(&self) -> &Token {
        &self.token
    }

    fn socket_addr(&self) -> Result<SocketAddr> {
        let ip: IpAddr = if self.host.eq_ignore_ascii_case("localhost") {
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        } else {
            self.host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse()
                .map_err(|_| Error::Config(format!("{:?} is not an address", self.host)))?
        };
        Ok(SocketAddr::new(ip, self.port))
    }

    /// How long a request may take, body and reply together: the base patience
    /// plus what a body of `body` bytes plausibly needs ([`BODY_RATE_FLOOR`],
    /// capped by [`BODY_PATIENCE_CAP`]).
    ///
    /// This is the patience for both ends of one request, not only for its reply:
    /// a body larger than the socket's buffers is written as the server drains it,
    /// so a slow server can stall the write as well as the answer.
    pub fn patience_for_body(&self, body: usize) -> Duration {
        let extra = Duration::from_secs(body as u64 / BODY_RATE_FLOOR);
        self.timeout + extra.min(BODY_PATIENCE_CAP)
    }

    fn connect(&self, read_timeout: Duration) -> Result<TcpStream> {
        let addr = self.socket_addr()?;
        let stream = TcpStream::connect_timeout(&addr, self.timeout.min(Duration::from_secs(5)))?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(read_timeout))?;
        stream.set_write_timeout(Some(read_timeout))?;
        Ok(stream)
    }

    fn write_request(
        &self,
        stream: &mut TcpStream,
        method: &str,
        path: &str,
        payload: Option<&[u8]>,
        accept_sse: bool,
    ) -> Result<()> {
        let host = if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        };
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {}\r\nConnection: close\r\n",
            self.token.as_str()
        );
        if accept_sse {
            head.push_str("Accept: text/event-stream\r\n");
        }
        if let Some(payload) = payload {
            head.push_str(&format!(
                "Content-Type: application/json\r\nContent-Length: {}\r\n",
                payload.len()
            ));
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes())?;
        if let Some(payload) = payload {
            if !payload.is_empty() {
                stream.write_all(payload)?;
            }
        }
        stream.flush()?;
        Ok(())
    }

    /// One request, its whole reply read.
    pub fn request(&self, method: &str, path: &str, body: Option<&Value>) -> Result<HttpResponse> {
        let payload = body.map(serde_json::to_vec).transpose()?;
        let timeout = self.patience_for_body(payload.as_ref().map_or(0, Vec::len));
        let mut stream = self.connect(timeout)?;
        self.write_request(&mut stream, method, path, payload.as_deref(), false)?;
        let mut reader = BufReader::new(stream);
        let (status, headers) = read_head(&mut reader, timeout)?;
        let body = read_body(&mut reader, &headers)?;
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }

    pub fn get(&self, path: &str) -> Result<HttpResponse> {
        self.request("GET", path, None)
    }

    pub fn post(&self, path: &str, body: &Value) -> Result<HttpResponse> {
        self.request("POST", path, Some(body))
    }

    /// Open an SSE stream. A refusal (no token, a shut-down server) is written
    /// before any stream header, so it arrives as an ordinary HTTP error here.
    /// Where the stream resumes from is the `since` query parameter, not a
    /// header: the cursor is `<epoch>.<seq>`, and the epoch is the server's.
    pub fn open_sse(&self, path: &str) -> Result<SseConnection> {
        let mut stream = self.connect(self.stream_timeout)?;
        self.write_request(&mut stream, "GET", path, None, true)?;
        let socket = stream.try_clone()?;
        let mut reader = BufReader::new(stream);
        let (status, headers) = read_head(&mut reader, self.stream_timeout)?;
        if status != 200 {
            let body = read_body(&mut reader, &headers)?;
            let text = String::from_utf8_lossy(&body).into_owned();
            let raw = serde_json::from_str(&text).unwrap_or(Value::String(text.clone()));
            return Err(Error::Status(StatusError::from_reply(status, raw, &text)));
        }
        Ok(SseConnection { reader, socket })
    }
}

/// One reply, whole.
#[derive(Clone, Debug)]
pub struct HttpResponse {
    pub status: u16,
    /// Header names lower-cased.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        let want = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(n, _)| *n == want)
            .map(|(_, v)| v.as_str())
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn json(&self) -> Result<Value> {
        serde_json::from_slice(&self.body).map_err(Error::from)
    }

    /// This reply as a typed error, for a status the client did not expect.
    pub fn error(&self) -> StatusError {
        let text = self.text();
        let raw = serde_json::from_str(&text).unwrap_or(Value::String(text.clone()));
        StatusError::from_reply(self.status, raw, &text)
    }
}

/// A live SSE connection, read a line at a time.
pub struct SseConnection {
    reader: BufReader<TcpStream>,
    socket: TcpStream,
}

impl SseConnection {
    /// The next line, without its terminator; `Ok(None)` at end of stream.
    pub fn read_line(&mut self) -> Result<Option<String>> {
        let mut buf = Vec::new();
        let n = self.reader.read_until(b'\n', &mut buf)?;
        if n == 0 {
            return Ok(None);
        }
        let mut line = String::from_utf8_lossy(&buf).into_owned();
        while line.ends_with('\n') || line.ends_with('\r') {
            line.pop();
        }
        Ok(Some(line))
    }

    /// Unblock a reader on another thread (a cancelled stream must not wait out
    /// its read timeout).
    pub fn close(&self) {
        let _ = self.socket.shutdown(Shutdown::Both);
    }

    /// A handle on the same socket, for cancelling from elsewhere.
    pub fn socket(&self) -> &TcpStream {
        &self.socket
    }
}

impl fmt::Debug for SseConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SseConnection").finish_non_exhaustive()
    }
}

fn read_head(reader: &mut impl BufRead, timeout: Duration) -> Result<(u16, Vec<(String, String)>)> {
    let mut line = Vec::new();
    if reader.read_until(b'\n', &mut line)? == 0 {
        return Err(Error::Protocol("the server closed before answering".into()));
    }
    let start = String::from_utf8_lossy(&line);
    let mut parts = start.trim_end().splitn(3, ' ');
    let _version = parts.next();
    let status: u16 = parts
        .next()
        .ok_or_else(|| Error::Protocol(format!("bad status line {start:?}")))?
        .parse()
        .map_err(|_| Error::Protocol(format!("bad status line {start:?}")))?;
    let mut headers = Vec::new();
    loop {
        let mut raw = Vec::new();
        if reader.read_until(b'\n', &mut raw)? == 0 {
            return Err(Error::Protocol(
                "the response headers were cut short".into(),
            ));
        }
        let text = String::from_utf8_lossy(&raw);
        let text = text.trim_end_matches(['\r', '\n']);
        if text.is_empty() {
            break;
        }
        if let Some((name, value)) = text.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    let _ = timeout;
    Ok((status, headers))
}

fn read_body(reader: &mut impl BufRead, headers: &[(String, String)]) -> Result<Vec<u8>> {
    let value = |name: &str| {
        headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    };
    if let Some(encoding) = value("transfer-encoding") {
        if encoding.to_ascii_lowercase().contains("chunked") {
            return read_chunked(reader);
        }
    }
    if let Some(length) = value("content-length") {
        let size: usize = length
            .trim()
            .parse()
            .map_err(|_| Error::Protocol(format!("bad Content-Length {length:?}")))?;
        let mut body = vec![0u8; size];
        reader.read_exact(&mut body)?;
        return Ok(body);
    }
    let mut body = Vec::new();
    reader.read_to_end(&mut body)?;
    Ok(body)
}

fn read_chunked(reader: &mut impl BufRead) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let mut line = Vec::new();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Err(Error::Protocol("chunked body cut short".into()));
        }
        let text = String::from_utf8_lossy(&line);
        let size = usize::from_str_radix(text.trim().split(';').next().unwrap_or(""), 16)
            .map_err(|_| Error::Protocol(format!("bad chunk size {text:?}")))?;
        if size == 0 {
            let mut trailer = Vec::new();
            loop {
                trailer.clear();
                if reader.read_until(b'\n', &mut trailer)? == 0 {
                    break;
                }
                if trailer == b"\r\n" || trailer == b"\n" {
                    break;
                }
            }
            return Ok(body);
        }
        let mut chunk = vec![0u8; size];
        reader.read_exact(&mut chunk)?;
        body.extend_from_slice(&chunk);
        let mut crlf = [0u8; 2];
        let _ = reader.read_exact(&mut crlf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §5.5: a POST body is not free. `serve` reads one a few kilobytes a second,
    /// so the reply to a turn that carries a screenshot's base64 is tens of
    /// seconds behind its request — measured: 419 KB of base64 answered in
    /// 8.7 s, 854 KB in 35.7 s — and the base patience alone calls that a
    /// failure. The patience therefore grows with the body, and stops growing
    /// where a request would otherwise be held open indefinitely.
    #[test]
    fn a_bigger_body_buys_more_patience_with_a_ceiling() {
        let http = HttpClient::loopback(8421, Token::new("t"));
        let base = http.patience_for_body(0);
        assert_eq!(base, Duration::from_secs(30), "nothing to read: the base");
        assert_eq!(base, http.patience_for_body(1024), "a body of nothing");

        // The payload the journal showed: 640 KB of PNG is 854 KB of base64, which
        // the server answered 35.7 s in.
        let pasted = http.patience_for_body(854 * 1024);
        assert!(
            pasted >= Duration::from_secs(30 + 90),
            "a pasted screenshot waits well past the 36 s the server takes: {pasted:?}"
        );

        // Monotone, and capped: a body past the cap earns no more patience.
        assert!(http.patience_for_body(100 * 1024) < http.patience_for_body(200 * 1024));
        let huge = http.patience_for_body(64 * 1024 * 1024);
        assert_eq!(
            huge,
            base + BODY_PATIENCE_CAP,
            "the cap is the cap: {huge:?}"
        );
    }
}

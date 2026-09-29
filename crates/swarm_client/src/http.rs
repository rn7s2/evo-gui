//! A blocking HTTP/1.1 client for serve's protocol: one request per connection
//! (`Connection: close`), loopback only, a bearer token on every request
//! (docs/PROMPT.md §4).
//!
//! It is hand-rolled on `std::net::TcpStream` on purpose: serve's HTTP is ~200
//! lines and speaks nothing else (docs/serve.md §"What serve is, underneath"),
//! and a stream is read line by line on a dedicated thread anyway (§5), so a
//! runtime is dead weight.

use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpStream};
use std::path::Path;
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

    /// Read the token a server wrote with `--token-file`. An empty file is not
    /// a token — the server creates it empty and fills it a moment later.
    pub fn from_file(path: &Path) -> Result<Token> {
        let text = std::fs::read_to_string(path)?;
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(Error::Config(format!(
                "token file {} is still empty",
                path.display()
            )));
        }
        Ok(Token(trimmed.to_owned()))
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

    fn connect(&self, read_timeout: Duration) -> Result<TcpStream> {
        let addr = self.socket_addr()?;
        let stream = TcpStream::connect_timeout(&addr, self.timeout.min(Duration::from_secs(5)))?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(read_timeout))?;
        stream.set_write_timeout(Some(self.timeout))?;
        Ok(stream)
    }

    fn write_request(
        &self,
        stream: &mut TcpStream,
        method: &str,
        path: &str,
        body: Option<&Value>,
        accept_sse: bool,
        last_event_id: Option<i64>,
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
        if let Some(id) = last_event_id {
            head.push_str(&format!("Last-Event-ID: {id}\r\n"));
        }
        let payload = match body {
            Some(value) => {
                let text = serde_json::to_vec(value)?;
                head.push_str(&format!(
                    "Content-Type: application/json\r\nContent-Length: {}\r\n",
                    text.len()
                ));
                text
            }
            None => Vec::new(),
        };
        head.push_str("\r\n");
        stream.write_all(head.as_bytes())?;
        if !payload.is_empty() {
            stream.write_all(&payload)?;
        }
        stream.flush()?;
        Ok(())
    }

    /// One request, its whole reply read.
    pub fn request(&self, method: &str, path: &str, body: Option<&Value>) -> Result<HttpResponse> {
        let timeout = self.timeout;
        let mut stream = self.connect(timeout)?;
        self.write_request(&mut stream, method, path, body, false, None)?;
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

    /// Open an SSE stream. A refusal is written before any stream header, so it
    /// arrives as an ordinary HTTP error here (§swarm/routes.lisp).
    pub fn open_sse(&self, path: &str, last_event_id: Option<i64>) -> Result<SseConnection> {
        let mut stream = self.connect(self.stream_timeout)?;
        self.write_request(&mut stream, "GET", path, None, true, last_event_id)?;
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

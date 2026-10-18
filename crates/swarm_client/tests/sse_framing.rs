//! SSE framing, driven over a real socket with hostile chunk boundaries.
//!
//! The op stream is the browser app's only live feed, and a reader that
//! mis-frames it fails silently: a dropped frame is a turn that never appears, a
//! spliced one is an op applied twice. So the framing is asserted here against a
//! server that pushes the stream in fragments that land at the worst places —
//! between the `\r` and the `\n` of a terminator, inside the literal `data:`
//! token, and between a frame's last field and the blank line that ends it.
//!
//! # What this covers, and what it does not
//!
//! The parser ([`SseParser`]) is shared by both transports — the native one and
//! the browser's `fetch` reader in `src/fetch.rs` — and it is exercised here
//! through the **native** connection ([`SseConnection::read_line`]), over a
//! `TcpStream`, with the bytes deliberately arriving in pieces.
//!
//! The browser's own byte buffer is *not* covered by this file, and cannot be
//! from a native test: `FetchSseConnection` holds `web_sys` types
//! (`ReadableStreamDefaultReader`, `AbortController`), its module is compiled
//! only for `target_family = "wasm"`, and the crate has no wasm test harness. See
//! the note at the bottom of this file for what that leaves untested.

#![cfg(not(target_family = "wasm"))]

use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, TcpListener};
use std::time::Duration;

use swarm_client::{HttpClient, SseEvent, SseParser, Token};

/// The stream under test, cut into the fragments a server will push.
///
/// The cuts are the point, and each one is a place a byte-at-a-time reader has to
/// get right:
///
/// * `": keepalive\r"` / `"\n"` — a `\r\n` split across two writes, so a reader
///   that looks for the pair as a unit sees neither half;
/// * `"...seq\":1}\r\n"` / `"\r\n"` — the blank line that terminates frame 1
///   arrives alone, which is the boundary a "frame" is actually made of;
/// * `"...event: op\nda"` / `"ta: {...}"` — the literal `data:` token split down
///   the middle, so field recognition cannot assume it arrives whole;
/// * frame 2's payload is **two `data:` lines**, which is how SSE carries a `\n`
///   inside a payload, and the parser must join them with `\n`;
/// * `"id: 7f3a.3\nevent: op\ndata: {}\n\n"` — a whole frame in one fragment, the
///   opposite case to the one above it.
const FRAGMENTS: &[&str] = &[
    ": keepalive\r",
    "\n",
    "\r\n",
    "id: 7f3a.1\r",
    "\nevent: op\r\ndata: {\"op\":\"hello\",\"seq\":1}\r\n",
    "\r\n",
    "id: 7f3a.2\nevent: op\nda",
    "ta: {\"op\":\"item.append\",\"text\":\"line one\"}\ndata: line two\n",
    "\n",
    ": ping\n",
    "\n",
    "id: 7f3a.3\nevent: op\ndata: {}\n\n",
];

/// Exactly the frames [`FRAGMENTS`] is the wire form of.
fn expected() -> Vec<SseEvent> {
    vec![
        SseEvent {
            id: Some("7f3a.1".to_owned()),
            kind: Some("op".to_owned()),
            data: "{\"op\":\"hello\",\"seq\":1}".to_owned(),
        },
        // Two `data:` lines, joined the way SSE says: the payload holds a real
        // newline, which is the case a single-line reader would truncate.
        SseEvent {
            id: Some("7f3a.2".to_owned()),
            kind: Some("op".to_owned()),
            data: "{\"op\":\"item.append\",\"text\":\"line one\"}\nline two".to_owned(),
        },
        SseEvent {
            id: Some("7f3a.3".to_owned()),
            kind: Some("op".to_owned()),
            data: "{}".to_owned(),
        },
    ]
}

/// The whole stream, in order — the fragments must be this and nothing else.
fn wire() -> String {
    FRAGMENTS.concat()
}

/// A server that answers one request with an SSE stream, pushed one fragment at a
/// time. Returns the port it is listening on.
///
/// `TCP_NODELAY` and a short pause after each fragment are what make the cuts
/// real: without them the kernel is free to coalesce the writes into one segment
/// and the reader would be handed the stream whole, which is precisely the case
/// that is *not* interesting. The assertion is on the frames, so a coalesced run
/// still passes — it just proves less, which is why the pause is here.
fn serve(fragments: Vec<String>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port to listen on");
    let port = listener.local_addr().expect("the bound address").port();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("the client's connection");
        stream.set_nodelay(true).expect("no Nagle");
        let mut writer = stream.try_clone().expect("a second handle");
        // Drain the request head. What the client asked for is not this test's
        // business; that it asked at all is.
        {
            let mut reader = BufReader::new(stream.try_clone().expect("a third handle"));
            let mut head = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                head.push_str(&line);
                if head.ends_with("\r\n\r\n") {
                    break;
                }
            }
            assert!(
                head.starts_with("GET "),
                "the client opened a stream: {head:?}"
            );
            assert!(
                head.contains("Authorization: Bearer sse-framing"),
                "the request carries the token: {head:?}"
            );
        }
        writer
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n")
            .expect("the response head");
        writer.flush().expect("flush");
        for fragment in fragments {
            writer
                .write_all(fragment.as_bytes())
                .expect("a stream fragment");
            writer.flush().expect("flush");
            std::thread::sleep(Duration::from_millis(15));
        }
        // End of stream: the reader's `read_line` sees this as `Ok(None)`, which
        // is what a server that closed the stream looks like.
        let _ = writer.shutdown(Shutdown::Write);
    });
    port
}

/// Read every line the connection has, the way a stream reader does.
fn read_lines(client: &HttpClient, path: &str) -> Vec<String> {
    let mut connection = client.open_sse(path).expect("an SSE connection");
    let mut lines = Vec::new();
    loop {
        match connection.read_line().expect("a line off the stream") {
            Some(line) => lines.push(line),
            None => return lines,
        }
    }
}

/// The frames that come out of the reader are exactly the frames that went in —
/// byte for byte, including a payload that holds a newline and one that arrived
/// split down the middle of `data:`.
#[test]
fn the_reader_reassembles_frames_across_hostile_chunk_boundaries() {
    let port = serve(FRAGMENTS.iter().map(|f| (*f).to_owned()).collect());
    let client = HttpClient::loopback(port, Token::new("sse-framing"));

    let lines = read_lines(&client, "/stream?since=7f3a.0");

    // The line split is the contract the parser is fed: one line per `\n`, no
    // terminator, `\r\n` and `\n` treated alike, and no empty line lost — the
    // blank ones are what end a frame.
    assert_eq!(
        lines,
        vec![
            ": keepalive",
            "",
            "id: 7f3a.1",
            "event: op",
            "data: {\"op\":\"hello\",\"seq\":1}",
            "",
            "id: 7f3a.2",
            "event: op",
            "data: {\"op\":\"item.append\",\"text\":\"line one\"}",
            "data: line two",
            "",
            ": ping",
            "",
            "id: 7f3a.3",
            "event: op",
            "data: {}",
            "",
        ],
        "the reader's line splitting"
    );

    // And the frames those lines make: the pings are dropped, the blank line
    // before anything has been seen makes no frame, and the two `data:` lines of
    // frame 2 become one payload with a newline in it.
    let mut parser = SseParser::new();
    let frames: Vec<SseEvent> = lines.iter().filter_map(|line| parser.feed(line)).collect();
    assert_eq!(frames, expected(), "the frames off the wire");
}

/// A frame that arrives one byte at a time is still one frame: the split is the
/// transport's business, not the framing's.
#[test]
fn a_frame_split_into_single_bytes_is_still_one_frame() {
    let whole = "id: 7f3a.9\nevent: op\ndata: {\"op\":\"hello\",\"seq\":9}\n\n";
    let fragments: Vec<String> = whole.chars().map(|c| c.to_string()).collect();
    let port = serve(fragments);
    let client = HttpClient::loopback(port, Token::new("sse-framing"));

    let lines = read_lines(&client, "/stream");
    let mut parser = SseParser::new();
    let frames: Vec<SseEvent> = lines.iter().filter_map(|line| parser.feed(line)).collect();

    assert_eq!(
        frames,
        vec![SseEvent {
            id: Some("7f3a.9".to_owned()),
            kind: Some("op".to_owned()),
            data: "{\"op\":\"hello\",\"seq\":9}".to_owned(),
        }],
        "one byte at a time, one frame"
    );
}

/// Several frames in a single write are several frames — the opposite failure to
/// the one above, and the one a reader that assumes "one chunk, one frame" has.
#[test]
fn several_frames_in_one_chunk_are_several_frames() {
    let whole = concat!(
        "id: 7f3a.1\nevent: op\ndata: {\"seq\":1}\n\n",
        "id: 7f3a.2\nevent: op\ndata: {\"seq\":2}\n\n",
        "id: 7f3a.3\nevent: op\ndata: {\"seq\":3}\n\n",
    );
    let port = serve(vec![whole.to_owned()]);
    let client = HttpClient::loopback(port, Token::new("sse-framing"));

    let lines = read_lines(&client, "/stream");
    let mut parser = SseParser::new();
    let frames: Vec<SseEvent> = lines.iter().filter_map(|line| parser.feed(line)).collect();

    assert_eq!(
        frames
            .iter()
            .map(|frame| (frame.id.as_deref(), frame.data.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (Some("7f3a.1"), "{\"seq\":1}"),
            (Some("7f3a.2"), "{\"seq\":2}"),
            (Some("7f3a.3"), "{\"seq\":3}"),
        ],
        "three frames, one write"
    );
}

/// The wire form this test is the assertion of: the fragments are the stream, so
/// a fragment dropped from the list above cannot quietly change what is asserted.
#[test]
fn the_fragments_are_the_whole_stream() {
    assert_eq!(
        wire(),
        concat!(
            ": keepalive\r\n\r\n",
            "id: 7f3a.1\r\nevent: op\r\ndata: {\"op\":\"hello\",\"seq\":1}\r\n\r\n",
            "id: 7f3a.2\nevent: op\n",
            "data: {\"op\":\"item.append\",\"text\":\"line one\"}\ndata: line two\n\n",
            ": ping\n\n",
            "id: 7f3a.3\nevent: op\ndata: {}\n\n",
        )
    );
}

// ---------------------------------------------------------------------------
// What this file does NOT prove
// ---------------------------------------------------------------------------
//
// The browser transport's own byte buffer is untested, and no native test can
// reach it. In `src/fetch.rs`, the module is `#[cfg(target_family = "wasm")]`
// (`src/lib.rs`), `FetchSseConnection` holds `ReadableStreamDefaultReader` and
// `AbortController`, and its splitter is a private method over that struct:
//
//     pub struct FetchSseConnection {
//         reader: ReadableStreamDefaultReader,   // web_sys
//         controller: AbortController,           // web_sys
//         buffer: Vec<u8>,
//         ended: bool,
//         stream_timeout: Duration,
//     }
//
//     fn take_line(&mut self) -> Option<String> {          // private
//         let at = self.buffer.iter().position(|byte| *byte == b'\n')?;
//         let mut line: Vec<u8> = self.buffer.drain(..=at).collect();
//         while line.last().is_some_and(|byte| *byte == b'\n' || *byte == b'\r') {
//             line.pop();
//         }
//         Some(lossy(&line))
//     }
//
// `take_line` is called only from `read_line`, whose other half (`read_chunk`) is
// `JsFuture`/`web_sys` all the way down; `buffer` is a private field with no
// accessor. So there is no target-independent seam to drive, and the crate has no
// wasm test harness (`rg wasm-bindgen-test` finds nothing). The fix is to lift
// `buffer` + `take_line` (and the `ended`/last-line case) into `sse.rs` as a pure
// `LineBuffer`, and have `FetchSseConnection` hold one — at which point this same
// test can drive it directly. That is a change to `fetch.rs`, which this task
// deliberately does not make.

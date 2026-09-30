//! The SSE wire format, parsed line by line (CONTRACT.md §5.3).
//!
//! serve writes `id: <epoch>.<seq>`, `event: op`, `data: <json>` and a blank
//! line after each frame, and a `: ping` comment every 15 s of silence. The
//! parser is pure, so the framing can be tested without a socket.

/// One event off the wire.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SseEvent {
    /// The `id:` line — `<epoch>.<seq>`, the cursor a reconnect resumes from.
    /// Kept as text because the epoch is the server's to mint.
    pub id: Option<String>,
    /// `event:` — always `op` in this protocol.
    pub kind: Option<String>,
    /// `data:` lines joined with newlines. serve sends one, a JSON object.
    pub data: String,
}

#[derive(Debug, Default)]
pub struct SseParser {
    id: Option<String>,
    kind: Option<String>,
    data: Vec<String>,
    seen: bool,
}

impl SseParser {
    pub fn new() -> SseParser {
        SseParser::default()
    }

    /// Feed one line, without its terminator. Returns the event when the blank
    /// line that ends it arrives. `:` comments (the 15 s pings) are dropped.
    pub fn feed(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            if !self.seen {
                return None;
            }
            let event = SseEvent {
                id: self.id.take(),
                kind: self.kind.take(),
                data: self.data.join("\n"),
            };
            self.data.clear();
            self.seen = false;
            return Some(event);
        }
        if line.starts_with(':') {
            // A ping; nothing to do with it.
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "id" => {
                self.id = Some(value.trim().to_owned());
                self.seen = true;
            }
            "event" => {
                self.kind = Some(value.to_owned());
                self.seen = true;
            }
            "data" => {
                self.data.push(value.to_owned());
                self.seen = true;
            }
            // "retry" and anything else serve does not send: ignored.
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(lines: &[&str]) -> Vec<SseEvent> {
        let mut parser = SseParser::new();
        lines.iter().filter_map(|line| parser.feed(line)).collect()
    }

    #[test]
    fn one_frame() {
        let events = parse(&[
            "id: 7f3a.1042",
            "event: op",
            "data: {\"op\":\"item.append\",\"id\":\"e_1\",\"text\":\"hi\"}",
            "",
        ]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id.as_deref(), Some("7f3a.1042"));
        assert_eq!(events[0].kind.as_deref(), Some("op"));
        assert_eq!(
            events[0].data,
            "{\"op\":\"item.append\",\"id\":\"e_1\",\"text\":\"hi\"}"
        );
    }

    #[test]
    fn pings_and_blanks_are_not_frames() {
        let events = parse(&[": ping", "", ": ping", ""]);
        assert!(events.is_empty());
    }

    #[test]
    fn two_frames_with_a_ping_between() {
        let events = parse(&[
            "id: 7f3a.1",
            "event: op",
            "data: {\"op\":\"hello\",\"epoch\":\"7f3a\",\"seq\":1}",
            "",
            ": ping",
            "",
            "id: 7f3a.2",
            "event: op",
            "data: {\"op\":\"stream.reset\",\"reason\":\"restarted\"}",
            "",
        ]);
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].id.as_deref(), Some("7f3a.2"));
        assert_eq!(events[1].kind.as_deref(), Some("op"));
    }

    #[test]
    fn a_data_line_without_a_space_and_a_missing_id() {
        let events = parse(&["event: op", "data:{}", ""]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, None);
        assert_eq!(events[0].data, "{}");
    }

    #[test]
    fn a_parser_can_end_mid_frame_and_resume() {
        let mut parser = SseParser::new();
        assert!(parser.feed("id: 7f3a.9").is_none());
        assert!(parser.feed("event: op").is_none());
        assert!(parser.feed("data: {\"op\":\"hello\"}").is_none());
        let frame = parser.feed("").unwrap();
        assert_eq!(frame.id.as_deref(), Some("7f3a.9"));
        assert_eq!(frame.data, "{\"op\":\"hello\"}");
        // And a fresh frame after it starts clean.
        assert!(parser.feed("id: 7f3a.10").is_none());
        let next = parser.feed("").unwrap();
        assert_eq!(next.id.as_deref(), Some("7f3a.10"));
        assert_eq!(next.data, "");
    }
}

//! The SSE wire format, parsed line by line (docs/serve.md §Events).
//!
//! serve writes `id:`, `event:`, `data:` and a blank line after each event, and
//! `: keepalive` every 15 s of silence. The parser is pure so the framing can be
//! tested without a socket.

/// One event off the wire.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SseEvent {
    /// serve numbers events consecutively from 1 within one process; the cursor
    /// a reconnect resumes from.
    pub id: Option<i64>,
    /// `event:` — the event's kind (`text-delta`, `settled`, …).
    pub kind: Option<String>,
    /// `data:` lines joined with newlines. serve sends one, a JSON object.
    pub data: String,
}

#[derive(Debug, Default)]
pub struct SseParser {
    id: Option<i64>,
    kind: Option<String>,
    data: Vec<String>,
    seen: bool,
}

impl SseParser {
    pub fn new() -> SseParser {
        SseParser::default()
    }

    /// Feed one line, without its terminator. Returns the event when the blank
    /// line that ends it arrives. `:` comments (keepalives) are dropped.
    pub fn feed(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            if !self.seen {
                return None;
            }
            let event = SseEvent {
                id: self.id,
                kind: self.kind.take(),
                data: self.data.join("\n"),
            };
            self.data.clear();
            self.seen = false;
            return Some(event);
        }
        if let Some(rest) = line.strip_prefix(':') {
            let _ = rest; // a keepalive; nothing to do with it
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "id" => {
                // An id that is not a number is not our cursor; ignore it.
                if let Ok(n) = value.trim().parse::<i64>() {
                    self.id = Some(n);
                }
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
    fn one_event() {
        let events = parse(&[
            "id: 57",
            "event: text-delta",
            "data: {\"type\":\"text-delta\",\"text\":\"hi\"}",
            "",
        ]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, Some(57));
        assert_eq!(events[0].kind.as_deref(), Some("text-delta"));
        assert_eq!(events[0].data, "{\"type\":\"text-delta\",\"text\":\"hi\"}");
    }

    #[test]
    fn comments_and_blanks_are_not_events() {
        let events = parse(&[": keepalive", "", ": evo-swarm events after 0", ""]);
        assert!(events.is_empty());
    }

    #[test]
    fn two_events_and_a_keepalive_between() {
        let events = parse(&[
            "id: 1",
            "event: hello",
            "data: {\"type\":\"hello\"}",
            "",
            ": keepalive",
            "",
            "id: 2",
            "event: ready",
            "data: {\"type\":\"ready\",\"resumed\":null}",
            "",
        ]);
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].id, Some(2));
        assert_eq!(events[1].kind.as_deref(), Some("ready"));
    }

    #[test]
    fn data_without_a_space_and_a_non_numeric_id() {
        let events = parse(&["id: abc", "event:output", "data:{}", ""]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, None);
        assert_eq!(events[0].kind.as_deref(), Some("output"));
        assert_eq!(events[0].data, "{}");
    }

    #[test]
    fn a_parser_can_end_mid_event_and_resume() {
        let mut parser = SseParser::new();
        assert!(parser.feed("id: 9").is_none());
        assert!(parser.feed("event: output").is_none());
        let event = parser.feed("data: {\"type\":\"output\"}").is_none();
        assert!(event);
        let done = parser.feed("").unwrap();
        assert_eq!(done.id, Some(9));
    }
}

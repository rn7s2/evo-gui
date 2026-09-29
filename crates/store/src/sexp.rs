//! A small, strict reader for the journal's "sexpr-JSON" vocabulary.
//!
//! Journals are data, not code (see `../evo-agent/docs/journal-format.md`):
//! plists with keyword keys, keywords, strings, integers, ratios/floats,
//! `t`/`nil`, and vectors. This reader understands exactly that, refuses to
//! evaluate anything, and is deliberately forgiving about the rest — a form it
//! cannot read is an error the caller skips, never a panic.
//!
//! It is also the only place that knows how evo writes sexprs, so the managed
//! `swarm.lisp` block ([`crate::swarm_config`]) and the history scan
//! ([`crate::history`]) share it.

use std::fmt;

/// How deep a form may nest before we call it hostile.
const MAX_DEPTH: usize = 128;

/// One value from a journal.
#[derive(Clone, Debug, PartialEq)]
pub enum Sexp {
    /// `nil` — the empty list, and the only false.
    Nil,
    Bool(bool),
    Int(i64),
    /// Floats and ratios; a ratio `1/2` is folded to `0.5`.
    Float(f64),
    Str(String),
    /// `:foo` — stored without the leading colon.
    Keyword(String),
    Symbol(String),
    List(Vec<Sexp>),
    /// `#(…)`
    Vector(Vec<Sexp>),
}

impl Sexp {
    /// Read the first form in `s`, skipping whitespace and `;` comments.
    pub fn parse(s: &str) -> Result<Sexp, Error> {
        let mut parser = Parser { bytes: s.as_bytes(), src: s, pos: 0 };
        parser.skip_trivia();
        parser.form(0)
    }

    /// Read every form in `s`.
    pub fn parse_all(s: &str) -> Result<Vec<Sexp>, Error> {
        let mut parser = Parser { bytes: s.as_bytes(), src: s, pos: 0 };
        let mut out = Vec::new();
        loop {
            parser.skip_trivia();
            if parser.at_end() {
                return Ok(out);
            }
            out.push(parser.form(0)?);
        }
    }

    pub fn is_nil(&self) -> bool {
        matches!(self, Sexp::Nil)
    }

    /// Common Lisp truthiness: only `nil` is false.
    pub fn is_truthy(&self) -> bool {
        !self.is_nil()
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Sexp::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_symbol(&self) -> Option<&str> {
        match self {
            Sexp::Symbol(s) | Sexp::Keyword(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Sexp::Int(i) => Some(*i),
            Sexp::Float(f) if f.fract() == 0.0 => Some(*f as i64),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Sexp::Int(i) => Some(*i as f64),
            Sexp::Float(f) => Some(*f),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Sexp::Nil => Some(false),
            Sexp::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// The elements of a list or a vector; `nil` is the empty list.
    pub fn items(&self) -> &[Sexp] {
        match self {
            Sexp::List(items) | Sexp::Vector(items) => items,
            Sexp::Nil => &[],
            _ => &[],
        }
    }

    /// A plist lookup: `(:key value :other …)`.
    pub fn get(&self, key: &str) -> Option<&Sexp> {
        let items = self.items();
        let mut i = 0;
        while i + 1 < items.len() {
            let matches = match &items[i] {
                Sexp::Keyword(k) | Sexp::Symbol(k) => k == key,
                _ => false,
            };
            if matches {
                return Some(&items[i + 1]);
            }
            i += 2;
        }
        None
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Sexp::as_str)
    }

    pub fn get_i64(&self, key: &str) -> Option<i64> {
        self.get(key).and_then(Sexp::as_i64)
    }

    /// The `:type` of a journal entry, as a string (`"custom"`, `"model-change"`…).
    pub fn entry_type(&self) -> Option<&str> {
        self.get_str("type")
    }
}

impl fmt::Display for Sexp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Sexp::Nil => f.write_str("nil"),
            Sexp::Bool(true) => f.write_str("t"),
            Sexp::Bool(false) => f.write_str("nil"),
            Sexp::Int(i) => write!(f, "{i}"),
            Sexp::Float(x) => write!(f, "{x}"),
            Sexp::Str(s) => write!(f, "{s:?}"),
            Sexp::Keyword(k) => write!(f, ":{k}"),
            Sexp::Symbol(s) => f.write_str(s),
            Sexp::List(items) => write_seq(f, items, "(", ")"),
            Sexp::Vector(items) => write_seq(f, items, "#(", ")"),
        }
    }
}

fn write_seq(f: &mut fmt::Formatter<'_>, items: &[Sexp], open: &str, close: &str) -> fmt::Result {
    f.write_str(open)?;
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            f.write_str(" ")?;
        }
        write!(f, "{item}")?;
    }
    f.write_str(close)
}

/// Where a read failed, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub pos: usize,
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sexp at byte {}: {}", self.pos, self.message)
    }
}

impl std::error::Error for Error {}

struct Parser<'a> {
    bytes: &'a [u8],
    src: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn at_end(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn err<T>(&self, message: impl Into<String>) -> Result<T, Error> {
        Err(Error { pos: self.pos, message: message.into() })
    }

    fn skip_trivia(&mut self) {
        loop {
            while let Some(&b) = self.bytes.get(self.pos) {
                if b.is_ascii_whitespace() {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            if self.bytes.get(self.pos) == Some(&b';') {
                while let Some(&b) = self.bytes.get(self.pos) {
                    self.pos += 1;
                    if b == b'\n' {
                        break;
                    }
                }
                continue;
            }
            return;
        }
    }

    fn form(&mut self, depth: usize) -> Result<Sexp, Error> {
        if depth > MAX_DEPTH {
            return self.err("form nested too deeply");
        }
        self.skip_trivia();
        let Some(&b) = self.bytes.get(self.pos) else {
            return self.err("unexpected end of input");
        };
        match b {
            b'(' => self.sequence(b')', depth, Sexp::List),
            b'#' => {
                if self.bytes.get(self.pos + 1) == Some(&b'(') {
                    self.pos += 1;
                    self.sequence(b')', depth, Sexp::Vector)
                } else {
                    self.err("unsupported # reader macro")
                }
            }
            b')' => self.err("unexpected )"),
            b'"' => self.string(),
            _ => self.atom(),
        }
    }

    fn sequence(
        &mut self,
        close: u8,
        depth: usize,
        build: fn(Vec<Sexp>) -> Sexp,
    ) -> Result<Sexp, Error> {
        self.pos += 1; // ( or #(
        let mut items = Vec::new();
        loop {
            self.skip_trivia();
            match self.bytes.get(self.pos) {
                None => return self.err(format!("unterminated form, expected {}", close as char)),
                Some(&b) if b == close => {
                    self.pos += 1;
                    return Ok(build(items));
                }
                _ => items.push(self.form(depth + 1)?),
            }
        }
    }

    fn string(&mut self) -> Result<Sexp, Error> {
        self.pos += 1; // opening quote
        let mut out = String::new();
        loop {
            let Some(&b) = self.bytes.get(self.pos) else {
                return self.err("unterminated string");
            };
            self.pos += 1;
            match b {
                b'"' => return Ok(Sexp::Str(out)),
                // Common Lisp: a backslash quotes the next character as it is;
                // a backslash immediately before a newline removes both.
                b'\\' => match self.bytes.get(self.pos) {
                    None => return self.err("unterminated escape"),
                    Some(b'\n') => self.pos += 1,
                    Some(_) => {
                        let ch = self.src[self.pos..].chars().next().unwrap();
                        out.push(ch);
                        self.pos += ch.len_utf8();
                    }
                },
                _ => {
                    let ch = self.src[self.pos - 1..].chars().next().unwrap();
                    out.push(ch);
                    self.pos += ch.len_utf8() - 1;
                }
            }
        }
    }

    fn atom(&mut self) -> Result<Sexp, Error> {
        let start = self.pos;
        while let Some(&b) = self.bytes.get(self.pos) {
            if b.is_ascii_whitespace() || matches!(b, b'(' | b')' | b'"' | b';') {
                break;
            }
            self.pos += 1;
        }
        let text = &self.src[start..self.pos];
        Ok(classify(text))
    }
}

/// Turn one atom's text into a value. A token we do not recognise is a symbol,
/// which is how the reader stays open-ended.
fn classify(text: &str) -> Sexp {
    match text {
        "nil" | "NIL" | "Nil" => return Sexp::Nil,
        "t" | "T" => return Sexp::Bool(true),
        _ => {}
    }
    if let Some(rest) = text.strip_prefix(':') {
        return Sexp::Keyword(rest.to_owned());
    }
    if let Some((num, den)) = text.split_once('/') {
        if let (Ok(n), Ok(d)) = (num.parse::<i64>(), den.parse::<i64>()) {
            if d != 0 {
                return Sexp::Float(n as f64 / d as f64);
            }
        }
        return Sexp::Symbol(text.to_owned());
    }
    let looks_numeric = text
        .strip_prefix(['+', '-'])
        .unwrap_or(text)
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_digit);
    if looks_numeric {
        if let Ok(i) = text.parse::<i64>() {
            return Sexp::Int(i);
        }
        if let Ok(f) = text.parse::<f64>() {
            return Sexp::Float(f);
        }
    }
    Sexp::Symbol(text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_journal_header() {
        let line = r#"(:type :session :version 1 :id "ed99c60d1dee3c3f" :cwd "/Users/bytedance/coding/evo-gui/" :timestamp "2026-09-29T09:09:56Z")"#;
        let form = Sexp::parse(line).unwrap();
        assert_eq!(form.entry_type(), Some("session"));
        assert_eq!(form.get_i64("version"), Some(1));
        assert_eq!(form.get_str("id"), Some("ed99c60d1dee3c3f"));
        assert_eq!(form.get_str("cwd"), Some("/Users/bytedance/coding/evo-gui/"));
        assert_eq!(form.get_str("timestamp"), Some("2026-09-29T09:09:56Z"));
    }

    #[test]
    fn reads_a_swarm_record() {
        let line = r#"(:type :custom :id "471a27ca" :parent-id "f29ef9ce" :timestamp "2026-09-29T05:16:22Z" :key "swarm" :data (:id "20260929T051622-d540" :dir "/Users/bytedance/.evo/swarm/20260929T051622-d540/" :workers 6 :lanes #((:n 1 :cwd "/Users/bytedance/" :worktree nil :branch nil :task nil :extra-forms #()) (:n 2 :cwd "/work/" :worktree nil :branch nil :task nil :extra-forms #()))))"#;
        let form = Sexp::parse(line).unwrap();
        assert_eq!(form.entry_type(), Some("custom"));
        assert_eq!(form.get_str("key"), Some("swarm"));
        let data = form.get("data").unwrap();
        assert_eq!(data.get_str("id"), Some("20260929T051622-d540"));
        assert_eq!(data.get_i64("workers"), Some(6));
        let lanes = data.get("lanes").unwrap().items();
        assert_eq!(lanes.len(), 2);
        assert_eq!(lanes[0].get_i64("n"), Some(1));
        assert_eq!(lanes[0].get_str("cwd"), Some("/Users/bytedance/"));
        assert_eq!(lanes[1].get_str("cwd"), Some("/work/"));
        assert!(lanes[0].get("worktree").unwrap().is_nil());
        // The vector's task is absent, and asking for it is not an error.
        assert_eq!(lanes[0].get("task"), None);
    }

    #[test]
    fn strings_keep_newlines_and_escapes() {
        let form = Sexp::parse(r#"(:text "one\ntwo" :q "a\"b" :bs "a\\b" :raw "x
y")"#)
        .unwrap();
        // Common Lisp escaping: `\n` is the single character `n`.
        assert_eq!(form.get_str("text"), Some("onetwo"));
        assert_eq!(form.get_str("q"), Some("a\"b"));
        assert_eq!(form.get_str("bs"), Some("a\\b"));
        assert_eq!(form.get_str("raw"), Some("x\ny"));
    }

    #[test]
    fn a_backslash_before_a_newline_continues_the_string() {
        let form = Sexp::parse("(:text \"one\\\n     two\")").unwrap();
        assert_eq!(form.get_str("text"), Some("onetwo"));
    }

    #[test]
    fn atoms_numbers_and_trivia() {
        let forms = Sexp::parse_all(
            "; a comment\n(:a 1 :b -2 :c 2.5 :d 3/2 :e nil :f t :g #(1 2) :h sym :i \"s\")",
        )
        .unwrap();
        assert_eq!(forms.len(), 1);
        let f = &forms[0];
        assert_eq!(f.get_i64("a"), Some(1));
        assert_eq!(f.get_i64("b"), Some(-2));
        assert_eq!(f.get("c").unwrap().as_f64(), Some(2.5));
        assert_eq!(f.get("d").unwrap().as_f64(), Some(1.5));
        assert!(!f.get("e").unwrap().is_truthy());
        assert!(f.get("f").unwrap().is_truthy());
        assert_eq!(f.get("g").unwrap().items().len(), 2);
        assert_eq!(f.get("h").unwrap().as_symbol(), Some("sym"));
        assert_eq!(f.get_str("i"), Some("s"));
        assert_eq!(Sexp::parse("nil").unwrap(), Sexp::Nil);
        assert_eq!(Sexp::parse("#(1)").unwrap(), Sexp::Vector(vec![Sexp::Int(1)]));
    }

    #[test]
    fn nesting_is_bounded_and_errors_are_reported() {
        let deep = "(".repeat(MAX_DEPTH + 5);
        assert!(Sexp::parse(&deep).is_err());
        let cases = ["(:a 1", "\"unterminated", "#<", ")", "(:a \\"];
        for case in cases {
            let err = Sexp::parse(case).unwrap_err();
            assert!(!err.message.is_empty(), "{case}");
            assert!(err.to_string().starts_with("sexp at byte"), "{case}");
        }
        // `parse` stops after one form; `parse_all` is what notices the rest.
        assert!(Sexp::parse("(:a 1) (:b 2)").is_ok());
        assert!(Sexp::parse_all("(:a 1))").is_err());
    }

    #[test]
    fn unknown_atoms_are_symbols_not_panics() {
        assert_eq!(Sexp::parse("(a b c)").unwrap().items().len(), 3);
        assert_eq!(Sexp::parse("#'x").unwrap_err().message, "unsupported # reader macro");
        assert_eq!(Sexp::parse("(:x 1/0)").unwrap().get("x").unwrap().as_symbol(), Some("1/0"));
        assert_eq!(Sexp::parse("(:x 1e3)").unwrap().get("x").unwrap().as_f64(), Some(1000.0));
    }

    #[test]
    fn unicode_strings_survive() {
        let form = Sexp::parse(r#"(:t "héllo — 世界 🚀" :k :完成)"#).unwrap();
        assert_eq!(form.get_str("t"), Some("héllo — 世界 🚀"));
        assert_eq!(form.get("k"), Some(&Sexp::Keyword("完成".into())));
    }
}

//! Small formats shared by every row and readout.
//!
//! These are the TUI's own numbers (`src/tui/tui.lisp`'s `fmt-ktokens`, evo's
//! `short-duration`), kept here so the GUI and the TUI read one session the same way.
//! A segment's *text* is no longer built here: the server publishes `segments` whole
//! (CONTRACT §4.2) and the UI renders them.

/// `(round n 1000)` in Common Lisp, which rounds half to **even**: 1500 → 2, 2500 → 2,
/// 3500 → 4. Integer arithmetic only, so the result is exact for any token count.
pub fn round_div_half_even(n: u64, d: u64) -> u64 {
    debug_assert!(d > 0);
    let quotient = n / d;
    let remainder = n % d;
    match (remainder * 2).cmp(&d) {
        std::cmp::Ordering::Greater => quotient + 1,
        std::cmp::Ordering::Less => quotient,
        // An exact half goes to the even quotient, as CL `round` does.
        std::cmp::Ordering::Equal => quotient + (quotient & 1),
    }
}

/// `(format nil "~dk" (round n 1000))` — the TUI's `fmt-ktokens`.
pub fn k_tokens(n: u64) -> String {
    format!("{}k", round_div_half_even(n, 1000))
}

/// A compact elapsed clock: `45s`, `3m`, `1h2m` — `evo.tui:short-duration`.
pub fn short_duration(seconds: u64) -> String {
    if seconds < 60 {
        format!("{}s", seconds)
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}h{}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

/// `~:p`: the plural `s` for anything but one — `0 reports`, `1 report`.
pub fn plural(n: u64, word: &str) -> String {
    if n == 1 {
        word.to_string()
    } else {
        format!("{}s", word)
    }
}

/// One line, at most `limit` characters, with an ellipsis when it was cut.
pub fn clip(text: &str, limit: usize) -> String {
    let one_line: String = text
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    if one_line.chars().count() <= limit {
        return one_line;
    }
    let mut label: String = one_line.chars().take(limit).collect();
    label.push('…');
    label
}

/// A lane's task on one line, at most 60 characters — what a lane row shows.
pub fn lane_task_label(task: &str) -> String {
    clip(task, 60)
}

/// RFC 7386 JSON merge patch, applied in place: objects merge key by key, `null`
/// removes, and anything that is not an object replaces what was there (arrays
/// included — CONTRACT §5.3).
pub fn merge_patch(target: &mut serde_json::Value, patch: &serde_json::Value) {
    match patch {
        serde_json::Value::Object(patch) => {
            let base = match target {
                serde_json::Value::Object(_) => target,
                other => {
                    *other = serde_json::Value::Object(serde_json::Map::new());
                    other
                }
            };
            let base = base.as_object_mut().expect("just made an object");
            for (key, value) in patch {
                if value.is_null() {
                    base.remove(key);
                } else {
                    merge_patch(
                        base.entry(key.clone()).or_insert(serde_json::Value::Null),
                        value,
                    );
                }
            }
        }
        other => *target = other.clone(),
    }
}

//! What the keyboard means to the shell.
//!
//! A terminal has no text field to type into: it has a byte stream. Every key is
//! translated here into the bytes a shell expects on the other end, in the shape
//! the grid's current mode asks for — the arrows and the function keys travel as
//! `ESC[` sequences, or as the older `ESC O` ones while a full-screen program has
//! asked for application cursor keys.
//!
//! Two kinds of keystroke are deliberately not the terminal's:
//!
//! * the platform's own chords (`⌘C`, `⌘V`), which belong to the window's
//!   keymap and are left to bubble;
//! * anything this does not know how to spell, which is dropped rather than
//!   guessed at.

use alacritty_terminal::term::TermMode;
use gpui_kit::Keystroke;

/// The bytes one keystroke sends to the pty, or `None` when the terminal does
/// not take it.
pub(crate) fn bytes(keystroke: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    // The window's shortcuts are the window's: a `⌘`-chord is a command, not
    // text, and the keymap above this pane gets to see it.
    if keystroke.modifiers.platform {
        return None;
    }

    // Ctrl+` is the terminal pane's own toggle: let it bubble to the window's
    // action handler rather than sending a byte to the shell.
    if keystroke.modifiers.control && keystroke.key.as_str() == "`" {
        return None;
    }

    // `control` turns a key into a control character, and a terminal sends the
    // character, not the key: `Ctrl+C` is 0x03, and the shell reads it as one.
    if keystroke.modifiers.control {
        if let Some(byte) = control(&keystroke.key) {
            return Some(vec![byte]);
        }
    }

    // A full-screen program (an editor, a pager, a shell's own line editor with
    // a menu open) can ask for the older cursor-key encoding.
    let application = mode.contains(TermMode::APP_CURSOR);
    let named: &[u8] = match keystroke.key.as_str() {
        "enter" => b"\r",
        "tab" if keystroke.modifiers.shift => b"\x1b[Z",
        "tab" => b"\t",
        // A terminal's backspace is `DEL`: the shells read it as one, and the
        // `BS` a control character would send means something else.
        "backspace" => b"\x7f",
        "escape" => b"\x1b",
        "space" => b" ",
        "up" => arrow(application, b'A'),
        "down" => arrow(application, b'B'),
        "right" => arrow(application, b'C'),
        "left" => arrow(application, b'D'),
        "home" => arrow(application, b'H'),
        "end" => arrow(application, b'F'),
        "insert" => b"\x1b[2~",
        "delete" => b"\x1b[3~",
        "pageup" => b"\x1b[5~",
        "pagedown" => b"\x1b[6~",
        "f1" => b"\x1bOP",
        "f2" => b"\x1bOQ",
        "f3" => b"\x1bOR",
        "f4" => b"\x1bOS",
        "f5" => b"\x1b[15~",
        "f6" => b"\x1b[17~",
        "f7" => b"\x1b[18~",
        "f8" => b"\x1b[19~",
        "f9" => b"\x1b[20~",
        "f10" => b"\x1b[21~",
        "f11" => b"\x1b[23~",
        "f12" => b"\x1b[24~",
        // Everything else is text, and the character the key produced is the
        // text — it is where a non-ASCII layout's own characters come from.
        _ => {
            return keystroke
                .key_char
                .as_deref()
                .filter(|text| !text.is_empty())
                .map(|text| text.as_bytes().to_vec())
        }
    };
    Some(named.to_vec())
}

/// A key that moves the view through the scrollback rather than reaching the
/// shell: Shift with Page Up/Down, Home and End, as terminals have always had.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScrollKey {
    PageUp,
    PageDown,
    Top,
    Bottom,
}

/// The scrollback key a keystroke is, if it is one.
pub(crate) fn scroll(keystroke: &Keystroke) -> Option<ScrollKey> {
    let m = &keystroke.modifiers;
    if !m.shift || m.control || m.alt || m.platform {
        return None;
    }
    match keystroke.key.as_str() {
        "pageup" => Some(ScrollKey::PageUp),
        "pagedown" => Some(ScrollKey::PageDown),
        "home" => Some(ScrollKey::Top),
        "end" => Some(ScrollKey::Bottom),
        _ => None,
    }
}

/// The bytes the arrow key `up` (or down) sends in the current mode — what a
/// wheel turns into on the alternate screen, where a pager or an editor scrolls
/// itself.
pub(crate) fn arrow_key(up: bool, mode: TermMode) -> &'static [u8] {
    arrow(
        mode.contains(TermMode::APP_CURSOR),
        if up { b'A' } else { b'B' },
    )
}

/// A cursor key in the encoding the current mode asks for.
fn arrow(application: bool, key: u8) -> &'static [u8] {
    match (application, key) {
        (true, b'A') => b"\x1bOA",
        (true, b'B') => b"\x1bOB",
        (true, b'C') => b"\x1bOC",
        (true, b'D') => b"\x1bOD",
        (true, b'H') => b"\x1bOH",
        (true, b'F') => b"\x1bOF",
        (false, b'H') => b"\x1b[H",
        (false, b'F') => b"\x1b[F",
        (false, b'A') => b"\x1b[A",
        (false, b'B') => b"\x1b[B",
        (false, b'C') => b"\x1b[C",
        _ => b"\x1b[D",
    }
}

/// The control character a key stands for while `Ctrl` is held: the letters in
/// order from `Ctrl+A` (0x01), and the punctuation a terminal has always had
/// codes for.
fn control(key: &str) -> Option<u8> {
    if key == "space" {
        return Some(0);
    }
    let mut letters = key.chars();
    let letter = letters.next()?;
    if letters.next().is_some() {
        return None;
    }
    match letter {
        'a'..='z' => Some(letter as u8 - b'a' + 1),
        '@' | ' ' => Some(0),
        '[' => Some(27),
        '\\' => Some(28),
        ']' => Some(29),
        '^' => Some(30),
        '_' => Some(31),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(text: &str) -> Keystroke {
        Keystroke::parse(text).expect("a keystroke")
    }

    /// Shift with the paging keys is the scrollback's; without Shift, or with
    /// another modifier, the key is the shell's.
    #[test]
    fn shift_paging_keys_scroll_and_others_do_not() {
        assert_eq!(scroll(&key("shift-pageup")), Some(ScrollKey::PageUp));
        assert_eq!(scroll(&key("shift-pagedown")), Some(ScrollKey::PageDown));
        assert_eq!(scroll(&key("shift-home")), Some(ScrollKey::Top));
        assert_eq!(scroll(&key("shift-end")), Some(ScrollKey::Bottom));
        assert_eq!(scroll(&key("pageup")), None);
        assert_eq!(scroll(&key("ctrl-shift-pageup")), None);
        assert_eq!(scroll(&key("shift-a")), None);
        // Unshifted, they still reach the shell.
        assert_eq!(
            bytes(&key("pageup"), TermMode::empty()).unwrap(),
            b"\x1b[5~"
        );
    }

    /// The wheel's arrows follow the cursor-key mode the program asked for.
    #[test]
    fn wheel_arrows_follow_the_cursor_mode() {
        assert_eq!(arrow_key(true, TermMode::empty()), b"\x1b[A");
        assert_eq!(arrow_key(false, TermMode::APP_CURSOR), b"\x1bOB");
    }
}

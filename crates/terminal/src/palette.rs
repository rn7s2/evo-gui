//! The theme's answer for the terminal's colours.
//!
//! The grid names colours the way a terminal does — the sixteen ANSI names, the
//! 256-colour palette, and true colour — while the window has the app's own
//! palette on its theme. This is the one place the two meet, and the mapping is
//! deliberately a *mapping*: the terminal's palette is the app's colours, so a
//! shell listing a directory is drawn in the same reds and greens as the rest of
//! the window, in either mode, and retuning the app's palette retunes the
//! terminal with it.
//!
//! Two things do not map exactly, and are decided here rather than being left to
//! each caller:
//!
//! * **Black and white.** The app's palette has no black and no white, only a
//!   foreground and a muted one. `Black` is drawn as the muted foreground and
//!   `White` as the foreground, so an `ls` that paints ordinary files "white" is
//!   drawn in the reading colour and a dim prompt in the quiet one.
//! * **Bold.** A bold cell is drawn as the bright half of the palette, not as a
//!   heavier face: the grid is monospaced, so a face swap could move every cell
//!   after the run, and the bright variant is what a terminal's bold means to a
//!   program anyway (`git diff` and `ls --color` ask for it and would otherwise
//!   get the same colour as the plain text).

use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use gpui_kit::component::Theme;
use gpui_kit::{rgb, Hsla};

/// The first sixteen indices, in the order the ANSI names come in.
const ANSI: [NamedColor; 16] = [
    NamedColor::Black,
    NamedColor::Red,
    NamedColor::Green,
    NamedColor::Yellow,
    NamedColor::Blue,
    NamedColor::Magenta,
    NamedColor::Cyan,
    NamedColor::White,
    NamedColor::BrightBlack,
    NamedColor::BrightRed,
    NamedColor::BrightGreen,
    NamedColor::BrightYellow,
    NamedColor::BrightBlue,
    NamedColor::BrightMagenta,
    NamedColor::BrightCyan,
    NamedColor::BrightWhite,
];

/// The colour `color` is drawn in, given the window's theme.
pub(crate) fn ink(color: Color, bold: bool, theme: &Theme) -> Hsla {
    match color {
        Color::Named(named) => named_ink(if bold { named.to_bright() } else { named }, theme),
        Color::Spec(spec) => true_ink(spec),
        Color::Indexed(index) => indexed_ink(index, bold, theme),
    }
}

/// One of the terminal's names, as a colour on the theme.
fn named_ink(named: NamedColor, theme: &Theme) -> Hsla {
    match named {
        NamedColor::Black | NamedColor::BrightBlack => theme.muted_foreground,
        NamedColor::Red | NamedColor::DimRed => theme.red,
        NamedColor::Green | NamedColor::DimGreen => theme.green,
        NamedColor::Yellow | NamedColor::DimYellow => theme.yellow,
        NamedColor::Blue | NamedColor::DimBlue => theme.blue,
        NamedColor::Magenta | NamedColor::DimMagenta => theme.magenta,
        NamedColor::Cyan | NamedColor::DimCyan => theme.cyan,
        NamedColor::White => theme.foreground,
        NamedColor::BrightRed => theme.red_light,
        NamedColor::BrightGreen => theme.green_light,
        NamedColor::BrightYellow => theme.yellow_light,
        NamedColor::BrightBlue => theme.blue_light,
        NamedColor::BrightMagenta => theme.magenta_light,
        NamedColor::BrightCyan => theme.cyan_light,
        NamedColor::BrightWhite | NamedColor::Foreground | NamedColor::BrightForeground => {
            theme.foreground
        }
        NamedColor::Background => theme.background,
        NamedColor::Cursor => theme.caret,
        NamedColor::DimBlack | NamedColor::DimWhite | NamedColor::DimForeground => {
            theme.muted_foreground
        }
    }
}

/// A colour the program named itself (`ESC[38:2:…`): drawn as itself.
fn true_ink(spec: Rgb) -> Hsla {
    let (r, g, b) = (u32::from(spec.r), u32::from(spec.g), u32::from(spec.b));
    rgb(r << 16 | g << 8 | b).into()
}

/// One of the 256 indices: the sixteen names, then the 6×6×6 cube, then the
/// twenty-four greys — the layout every terminal has used since xterm.
fn indexed_ink(index: u8, bold: bool, theme: &Theme) -> Hsla {
    match index {
        0..=15 => {
            let mut named = ANSI[index as usize];
            if bold {
                named = named.to_bright();
            }
            named_ink(named, theme)
        }
        16..=231 => {
            let level = |n: u8| {
                if n == 0 {
                    0
                } else {
                    55 + 40 * u32::from(n)
                }
            };
            let cube = u32::from(index - 16);
            let (r, g, b) = (cube / 36, cube % 36 / 6, cube % 6);
            rgb(level(r as u8) << 16 | level(g as u8) << 8 | level(b as u8)).into()
        }
        _ => {
            let grey = 8 + 10 * u32::from(index - 232);
            rgb(grey << 16 | grey << 8 | grey).into()
        }
    }
}

//! The design's own tokens: the palette and the numbers (vibe-wonders document
//! 28, `doc.tsx`'s `L` and `DARK`).
//!
//! This is the one place the design's colours and sizes are written down. The app
//! maps the palette onto gpui-component's `ThemeColor`, so a UI crate reads most of
//! it through `cx.theme()`; what the kit has no token for — the tab strip's own
//! surfaces, the table rules, the window's desktop — is here, next to the numbers
//! every UI crate needs (the strip's height, the header's, the reading measure,
//! the inset). A crate wraps a number in `px()`; nothing here depends on gpui.
//!
//! `doc.tsx`'s `color-mix(in srgb, A p%, B)` is [`Rgb::mix`]: a linear mix, `p` of
//! `A` (`0.11` is eleven percent `A`).

/// An sRGB colour, `0xRRGGBB`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Rgb {
        Rgb { r, g, b }
    }

    /// `0xRRGGBB`.
    pub const fn hex(value: u32) -> Rgb {
        Rgb {
            r: (value >> 16) as u8,
            g: ((value >> 8) & 0xFF) as u8,
            b: (value & 0xFF) as u8,
        }
    }

    /// The design's `color-mix(in srgb, self p%, other)`: `p` of `self`, the rest
    /// of `other`, mixed in sRGB.
    pub fn mix(self, other: Rgb, p: f32) -> Rgb {
        let p = p.clamp(0.0, 1.0);
        let q = 1.0 - p;
        let f = |a: u8, b: u8| {
            (f32::from(a) * p + f32::from(b) * q)
                .round()
                .clamp(0.0, 255.0) as u8
        };
        Rgb::new(f(self.r, other.r), f(self.g, other.g), f(self.b, other.b))
    }

    /// The colour as `0xRRGGBB`, which is what gpui's own `rgba` takes: a UI
    /// crate turns a token into a colour it can draw with one line, without the
    /// tokens themselves depending on a renderer.
    pub const fn to_u32(self) -> u32 {
        (self.r as u32) << 16 | (self.g as u32) << 8 | self.b as u32
    }

    /// The same colour at `alpha`, for the rgba a renderer takes.
    pub fn with_alpha(self, alpha: f32) -> [f32; 4] {
        [
            f32::from(self.r) / 255.0,
            f32::from(self.g) / 255.0,
            f32::from(self.b) / 255.0,
            alpha.clamp(0.0, 1.0),
        ]
    }
}

/// One mode's colours, named as the design names them.
///
/// `bg` is the deepest surface (the editor), `sidebar` the chrome one step above
/// it, `input` the lightest; `tab_*` are the strip's own. `success` is the green
/// the design's status pills use, which `doc.tsx` does not carry itself (it is in
/// `Rows.css`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub bg: Rgb,
    pub sidebar: Rgb,
    pub input: Rgb,
    pub fg: Rgb,
    pub muted: Rgb,
    pub muted_fg: Rgb,
    pub border: Rgb,
    pub primary: Rgb,
    pub primary_fg: Rgb,
    pub secondary: Rgb,
    pub info: Rgb,
    pub warning: Rgb,
    pub destructive: Rgb,
    pub success: Rgb,
    pub title_bar: Rgb,
    pub tab_bar: Rgb,
    pub tab_strip: Rgb,
    pub tab_active: Rgb,
    pub tab_hover: Rgb,
    pub tab_divider: Rgb,
    pub tab_ink: Rgb,
    pub desktop: Rgb,
}

impl Palette {
    /// A table's own rule: `color-mix(in srgb, var(--fg) 17%, var(--bg))`.
    pub fn rule(&self) -> Rgb {
        self.fg.mix(self.bg, 0.17)
    }

    /// The softer rule inside a table: `color-mix(in srgb, var(--fg) 10%, var(--bg))`.
    pub fn rule_soft(&self) -> Rgb {
        self.fg.mix(self.bg, 0.10)
    }

    /// The colour a control takes when the pointer is on it, for the tokens the
    /// design leaves to the kit (`primary` and `secondary` have no hover or
    /// pressed state in `doc.tsx`): a step of the foreground into the surface.
    pub fn hovered(&self, surface: Rgb) -> Rgb {
        self.fg.mix(surface, 0.10)
    }

    /// The same, pressed.
    pub fn pressed(&self, surface: Rgb) -> Rgb {
        self.fg.mix(surface, 0.18)
    }
}

/// `doc.tsx`'s `L`: RL's Dimmed Light 2026 — the deepest surface is the editor,
/// the chrome sits above it, and inputs are the lightest.
pub const LIGHT: Palette = Palette {
    bg: Rgb::hex(0xEFE9DF),
    sidebar: Rgb::hex(0xF6F2EA),
    input: Rgb::hex(0xFBF8F2),
    fg: Rgb::hex(0x202020),
    muted: Rgb::hex(0xE6E0D5),
    muted_fg: Rgb::hex(0x606060),
    border: Rgb::hex(0xDDD7CB),
    primary: Rgb::hex(0x0069CC),
    primary_fg: Rgb::hex(0xFFFFFF),
    secondary: Rgb::hex(0xE6E0D5),
    info: Rgb::hex(0x0069CC),
    warning: Rgb::hex(0x667309),
    destructive: Rgb::hex(0xAD0707),
    // `Rows.css`'s status pills: `#2F7A3A`.
    success: Rgb::hex(0x2F7A3A),
    title_bar: Rgb::hex(0xF6F2EA),
    tab_bar: Rgb::hex(0xF6F2EA),
    tab_strip: Rgb::hex(0xE6E0D5),
    tab_active: Rgb::hex(0xF6F2EA),
    tab_hover: Rgb::hex(0xEFE9DF),
    tab_divider: Rgb::hex(0xCFC8BA),
    tab_ink: Rgb::hex(0x606060),
    desktop: Rgb::hex(0xC9C3B8),
};

/// `doc.tsx`'s `DARK`: gpui-component's own dark tokens, plus the dark strip.
pub const DARK: Palette = Palette {
    bg: Rgb::hex(0x0A0A0A),
    sidebar: Rgb::hex(0x141414),
    input: Rgb::hex(0x171717),
    fg: Rgb::hex(0xFAFAFA),
    muted: Rgb::hex(0x262626),
    muted_fg: Rgb::hex(0xA3A3A3),
    border: Rgb::hex(0x262626),
    primary: Rgb::hex(0xFAFAFA),
    primary_fg: Rgb::hex(0x171717),
    secondary: Rgb::hex(0x262626),
    info: Rgb::hex(0x38BDF8),
    warning: Rgb::hex(0xEAB308),
    destructive: Rgb::hex(0xEF4444),
    // The light green is for a light surface; on this one the pill's own green
    // would be unreadable, so the dark theme keeps the same hue a step lighter.
    success: Rgb::hex(0x4CAF6A),
    title_bar: Rgb::hex(0x171717),
    tab_bar: Rgb::hex(0x171717),
    tab_strip: Rgb::hex(0x1F1F1F),
    tab_active: Rgb::hex(0x141414),
    tab_hover: Rgb::hex(0x2A2A2A),
    tab_divider: Rgb::hex(0x3A3A3A),
    tab_ink: Rgb::hex(0xB5B5B5),
    desktop: Rgb::hex(0x2B2F36),
};

/// The light palette; the mode is the caller's to know.
pub fn palette(dark: bool) -> &'static Palette {
    if dark {
        &DARK
    } else {
        &LIGHT
    }
}

// --- the numbers (`doc.tsx`'s knob defaults) --------------------------------

/// The tab strip's height (`标签栏.高度`).
pub const STRIP_HEIGHT: f32 = 42.;
/// A tab's height, bottom-aligned in the strip (`标签栏.标签高度`).
pub const TAB_HEIGHT: f32 = 38.;
/// A tab's top corner radius (`标签栏.圆角`).
pub const TAB_RADIUS: f32 = 8.;
/// A tab's widest basis, and its floor: `flex: 0 1 240px; min-width: 72px`.
pub const TAB_BASIS: f32 = 240.;
pub const TAB_MIN_WIDTH: f32 = 72.;
/// The macOS traffic lights' area at the strip's head: `84px`, lights 16 in.
pub const TRAFFIC_WIDTH: f32 = 84.;
pub const TRAFFIC_INSET: f32 = 16.;
pub const TRAFFIC_GAP: f32 = 8.;
pub const TRAFFIC_DIAMETER: f32 = 13.;
/// A tab's own padding and gap: `padding: 0 8px 0 14px; gap: 9px`.
pub const TAB_PAD: (f32, f32) = (14., 8.);
pub const TAB_GAP: f32 = 9.;
/// The tab label's size: `font-size: 13.5px`.
pub const TAB_FONT: f32 = 13.5;
/// The slot the busy dot or idle ring sits in, and the dot itself.
pub const TAB_SLOT: f32 = 16.;
pub const DOT: f32 = 9.;
/// The idle ring's border, and its opacity.
pub const IDLE_RING: f32 = 1.3;
pub const IDLE_OPACITY: f32 = 0.55;
/// The busy dot's own ring, and the pulse: `标签栏.工作周期` 1600 ms.
pub const BUSY_RING: f32 = 1.;
pub const BUSY_PERIOD_MS: f64 = 1600.;
/// A close button: a 22px circle; on hover, the foreground 11% into the surface.
pub const CLOSE: f32 = 22.;
pub const CLOSE_ICON: f32 = 10.;
pub const CLOSE_HOVER_MIX: f32 = 0.11;
/// The `+`: 30px, radius 6, 8 from the last tab and centred in the strip's lower
/// edge (`margin: 0 0 calc((tab-h - 30px) / 2) 8px`).
pub const ADD: f32 = 30.;
pub const ADD_RADIUS: f32 = 6.;
pub const ADD_ICON: f32 = 16.;
pub const ADD_GAP: f32 = 8.;
/// The strip's own padding around its row: `padding: 0 8px 0 4px`.
pub const TAB_ROW_PAD: (f32, f32) = (4., 8.);
/// How far a tab's divider is inset from the top and the bottom: `top: 25%;
/// bottom: 25%`.
pub const TAB_DIVIDER_INSET: f32 = 0.25;
/// The close button's `×` and the `+`: the design's stroke widths, at the sizes
/// the CSS gives them (10px for the `×`, 16 for the `+`, whose markup is a 14px
/// glyph the CSS scales).
pub const CLOSE_STROKE: f32 = 1.3;
pub const ADD_STROKE: f32 = 1.6;
/// The header row below the strip: the tab page's own top line (`--header`).
pub const HEADER_HEIGHT: f32 = 38.;
/// The reading measure of prose and tables (`布局.正文测量`).
pub const MEASURE: f32 = 800.;
/// The page's own padding (`布局.内边距`).
pub const INSET: f32 = 16.;
/// The base font size and the monospace one (`排版.基准字号`, `排版.等宽字号`).
pub const FONT_BASE: f32 = 16.;
pub const FONT_MONO: f32 = 13.;
/// The general radius and the large one (`形状.圆角`, `形状.大圆角`).
pub const RADIUS: f32 = 6.;
pub const RADIUS_LG: f32 = 8.;
/// The fonts: the system UI font, and `Menlo`, the design's first monospace.
pub const MONO_FONT: &str = "Menlo";

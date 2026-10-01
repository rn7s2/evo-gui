//! composer — the box under the transcript (`Workspace.css`'s `.composer-box`): the
//! coordinator's input, the goal and todo strips that stand over it, the model drawer
//! that folds out of it, the chips that state what the selected agent is working with,
//! and the one Send/Stop button.
//!
//! The box sits on the transcript's reading measure, at the foot of the conversation
//! column, so what is written lines up with what is read. Everything inside it is the
//! *selected agent's own* state (CONTRACT §4.2): the chips are the topic's status
//! segments in the server's words and order, the goal strip is the topic's goal and the
//! todo strip its todos, and the model drawer ticks the model the topic reports.
//! Nothing is composed here, and nothing survives a selection — another agent is
//! another set of facts.
//!
//! The effort ladder is the server's too (`catalog`'s `thinking_levels`, §5.6): a
//! client never writes the rungs down.
//!
//! Two of the design's controls are the coordinator's alone. `model.set` and
//! `thinking.set` act on the session (CONTRACT §5), and a lane's model is the swarm's,
//! fixed when it starts — so a lane's drawer states what that lane runs and says that
//! this box is not what changes it. Nothing is offered that the server would refuse.
//!
//! The inline completion the caret's word raises is the third thing evo owns: the
//! candidates are the commands `GET /catalog` lists, and — inside `/eval` — the
//! symbols the running image answers with. Nothing here invents one; what the popup
//! does with what comes back is [`complete`].
//!
//! What a message carries beside its words is the box's own too: the `+` at the foot
//! asks the system's picker for files, `⌘V` with an image on the clipboard attaches it,
//! and a drop on the box attaches what was dragged onto it. All three land in one
//! strip — the third of the box's strips, below the goal and the plan — and all three
//! go out through [`ComposerEvent::Send`]'s `attachments` ([`attachments`]). The picker
//! is injectable, so a test can answer it without a dialog.
//!
//! The composer does no I/O of its own but one read: telling an image from a file, by
//! the file's first bytes as well as its name, happens when an attachment is added
//! rather than on every frame. It emits [`ComposerEvent`] and the owner posts the
//! request, then reports the outcome with [`Composer::request_finished`]: that is what
//! keeps a failed send's draft alive — its attachments with it — and what keeps the
//! button disabled only while its own request is in flight.

use std::cell::Cell;
use std::collections::HashMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::base::input::Position;
use gpui_kit::base::TextSelection;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{InputEvent, Textarea, TextareaState},
    scroll::ScrollableElement,
    v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, Size,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    anchored, deferred, div, img, point, px, radians, AbsoluteLength, Anchor, Animation,
    AnimationExt as _, AnyElement, App, AsyncApp, Bounds, BoxShadow, ClickEvent, ClipboardEntry,
    ClipboardItem, Context, ElementId, Entity, EventEmitter, ExternalPaths, FocusHandle,
    FontWeight, Global, HighlightStyle, Image, ImageSource, IntoElement, KeyBinding, Keystroke,
    KeystrokeEvent, ObjectFit, PathPromptOptions, Pixels, Point, Render, ScrollHandle,
    SharedString, StyledText, Subscription, Task, TestSupportExt as _, WeakEntity, Window,
};
use session::{ordered_segments, GoalInfo, Segment, Todo, TodoStatus, TopicState};
use store::design::{self, Palette, INSET, MEASURE, RADIUS};
use widgets::effort::{cubic_bezier, Motion};
use widgets::{paint, Chip, EffortSlider};

pub mod attachments;
mod complete;

pub use attachments::{Attachment, AttachmentKind, Outgoing};
use complete::Popup;
pub use complete::{Answer, Candidate, CompletionKind, Question};

/// The input grows from the design's two lines to as much as half the conversation
/// pane, and scrolls inside itself past that (`AutoTextarea.tsx`).
///
/// The rows are the kit's own unit — a row is one line of the input's text, so two
/// rows are the design's two lines and the box they make (60px against the design's
/// 62px `min-height`) — and the composer is told the pane's height instead, working
/// in rows.
const MIN_ROWS: usize = 2;
/// How much of the pane's height the input may take: `Math.floor(pane / 2)`.
const ROOM_SHARE: f32 = 0.5;

/// What the input is for, and the two keys that submit it.
///
/// One placeholder for every agent: `input.send` posts to the session (CONTRACT §5),
/// so whatever transcript is above the box, what is typed here reaches the
/// coordinator — and the box says so rather than naming the lane it is shown under.
/// A lane is driven by the coordinator, never by the box.
const PLACEHOLDER: &str =
    "Message the coordinator\u{2026}\n(Enter to send, Shift+Enter for newline)";

/// The same box in a single-agent session (§7.2): one `evo-agent` is the whole
/// session, so there is no coordinator to address — what is typed here reaches the
/// agent the page calls `Main`, and the box says so.
const PLACEHOLDER_AGENT: &str =
    "Message the agent\u{2026}\n(Enter to send, Shift+Enter for newline)";

/// The box's own furniture: `12px` radius, one hairline, and the shadow under it
/// (`.composer-box`).
const BOX_RADIUS: Pixels = px(12.);
const BOX_BORDER: Pixels = px(1.);
/// The curve the box's children carry where they meet one of its corners: the
/// box's 12px radius less its own hairline, so a child's corner and the inner
/// edge of the border are the same curve. A child left square — the todo strip,
/// the drawer, the input's own surface — paints its rectangle into the corner,
/// and the box's near-white fill shows as a wedge against the strip's colour
/// (the design clips its children to the rounded box; a squared child is a
/// corner the page can see).
const BOX_INNER_RADIUS: Pixels = px(11.);
/// The focus ring: `box-shadow: 0 0 0 3px color-mix(in srgb, var(--primary) 12%,
/// transparent)`, and the border it comes with — `55%` of the primary into the
/// border.
const RING: Pixels = px(3.);
const RING_MIX: f32 = 12.;
const FOCUS_BORDER_MIX: f32 = 55.;
/// The ink the box's own shadow is drawn in: `rgba(60,40,10,.05)`.
const SHADOW_INK: paint::Rgb = paint::Rgb::new(0x3C, 0x28, 0x0A);

/// The action button's height, and the row it shares with the chips
/// (`.composer-send{height:28px}`, `.composer-foot{padding:6px 8px 8px 14px;gap:12px}`).
///
/// Two files carry a `.composer-foot` rule — `Composer.css` (6px gap, 10px of left
/// padding) and `Workspace.css` (12px, 14px), which the page loads last and which is
/// therefore what the design draws: measured off the rendered page, the first chip
/// starts 14px inside the box and the chips stand 12px apart.
const ACTION_HEIGHT: Pixels = px(28.);
const ACTION_RADIUS: Pixels = px(RADIUS);
const FOOT_GAP: Pixels = px(12.);
const FOOT_PAD: (f32, f32, f32, f32) = (6., 8., 8., 14.);

/// The strips' rows: a 32px title row, and a body that scrolls past 156px
/// (`.todo-strip-row`, `.todo-strip-list{max-height:156px}`).
const STRIP_ROW: Pixels = px(32.);
const STRIP_LIST_MAX: Pixels = px(156.);
/// One todo, its box, and the box's own 14px frame with its 3px radius.
const TODO_ROW: Pixels = px(18.);
const TODO_BOX: Pixels = px(14.);
const TODO_BOX_RADIUS: Pixels = px(3.);
const TODO_BOX_INSET: Pixels = px(6.);
/// The chevron's turn when a strip is folded: `.chev{transition:transform .12s ease}`.
const CHEVRON_TURN: std::time::Duration = std::time::Duration::from_millis(120);

/// One attachment's tile: a 112px thumbnail 72px tall, the name on one line under it,
/// and the row of them 10px apart.
///
/// The numbers are chosen against the todo list's own 156px cap (`STRIP_LIST_MAX`),
/// which the panel keeps: a tile is 92px tall and a row of them 102, so the cap shows
/// one row whole and half of the next — a reader can see there is more without the
/// panel taking the box.
const TILE: Pixels = px(112.);
const TILE_GAP: Pixels = px(10.);
const THUMB: Pixels = px(72.);
const THUMB_RADIUS: Pixels = px(6.);
const TILE_NAME: Pixels = px(16.);
/// The mark that takes an attachment off the message: an 18px round control at the
/// tile's top-right, 4px inside it.
const REMOVE: Pixels = px(18.);
const REMOVE_INSET: Pixels = px(4.);
/// The extensions §5.5's `images` takes. A file with one of these names is an image of
/// the turn whatever its bytes say — and a file whose bytes say image is one whatever
/// its name says.
const IMAGE_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];
/// The name a pasted image is given, with the format it arrived in for its extension:
/// the clipboard has no file name to offer.
const PASTED_NAME: &str = "pasted image";

/// The drawer's rows and its model items.
const DRAWER_ROW: Pixels = px(32.);
const DRAWER_ITEM: Pixels = px(30.);
const DRAWER_ITEM_RADIUS: Pixels = px(RADIUS);
const DRAWER_EFFORT_ROW: Pixels = px(36.);
const DRAWER_LABEL_MIN: Pixels = px(112.);
/// What the effort row says for a model the catalog gives no levels: the level is not
/// shown, because there is none to show — the model takes no effort setting at all.
const NO_EFFORT: &str = "not offered by this model";
/// How many of the drawer's rows the models take before they scroll: seven of
/// them is the region's whole height, so a catalog long enough to need a scroll
/// bar costs the box these rows and not one row more — the title above and the
/// effort below stay where they are.
const MODEL_LIST_ROWS: usize = 7;

/// Where an item's hover and its chosen fill come from: the ink a few percent into
/// the surface the drawer sits on (`--sidebar`), as the design's rows do it.
const ITEM_HOVER_MIX: f32 = 6.;
const ITEM_CHOSEN_MIX: f32 = 9.;

/// The completion popup: a drawer's item list, but floating over the box at the
/// caret rather than folded out of it, so it wears the drawer's own surface, a
/// radius of its own and the shadow the box has.
const POPUP_RADIUS: Pixels = px(10.);
const POPUP_PAD: f32 = 6.;
const POPUP_ROW: f32 = 28.;
/// The row's own inset, and the gap between its label and its description: the
/// label starts at `POPUP_PAD + POPUP_ROW_PAD + the border` from the popup's left
/// edge, which is what the popup is placed by.
const POPUP_ROW_PAD: f32 = 8.;
const POPUP_ROW_GAP: f32 = 10.;
/// How far the popup's own edges stand off the caret's line, and the window's.
const POPUP_GAP: f32 = 6.;
const POPUP_MARGIN: f32 = 8.;
const POPUP_FONT: Pixels = px(12.5);
/// The row the counter under the list is set on, when the list is longer than the
/// popup shows.
const POPUP_COUNTER: f32 = 18.;
/// The list's own fold: the rows the popup shows at once, and nothing else — the
/// popup's padding and its counter stand outside it.
const POPUP_LIST_MAX: Pixels = px(POPUP_ROW * complete::MAX_ROWS as f32);
/// How wide the popup stands: as wide as its widest row, and no wider — a list of
/// short names is a narrow list — between a floor that keeps it a list and a
/// ceiling that keeps it out of the way of what it is drawn over. Past the ceiling
/// (and past the window) a description is truncated, which is the one case a row
/// has to give something up.
const POPUP_MIN_W: f32 = 240.;
const POPUP_MAX_W: f32 = 560.;
/// Where a row's label starts, from the popup's left edge: the popup's own padding,
/// the row's, and the popup's hairline.
const POPUP_LABEL_INSET: f32 = POPUP_PAD + POPUP_ROW_PAD + 1.;

/// How long the caret rests before the server is asked what it is on.
///
/// The op is the read an input box asks on every keystroke — evo's own words for it
/// — but a keystroke is not a question until the caret has stopped for it, and a
/// question is asked once: this is about a frame, and it is what keeps a burst of
/// typing from being a burst of round trips.
const COMPLETE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(16);

/// The input's own type: `.composer-box textarea{font-size:14px;line-height:20px}`
/// — a size of its own, not the theme's base, and the line the autogrow counts in.
const INPUT_FONT: Pixels = px(14.);
const INPUT_LINE: Pixels = px(20.);

/// The input's box, as the design writes it: `padding:11px 14px 4px` and
/// `min-height:62px`. The kit's own input padding is a function of its size (a
/// `Large` one carries 10px above and below and 12px beside), and no size it offers
/// is this shape — so the input wears the size whose own padding is the least
/// (`XSmall`: none vertically, 4px beside) and the wrapper carries the rest: 11
/// above, 10 + the kit's 4 = 14 beside, 4 below.
///
/// The resting height is the design's 62 (its 55px of text and padding under a
/// `min-height` of 62), and the input grows past it with the text.
const INPUT_PAD: (f32, f32, f32, f32) = (11., 10., 4., 10.);
const INPUT_MIN: Pixels = px(62.);

/// Sizes drawn from the design's CSS rather than from a shared token: the chrome
/// text of a strip or a drawer, and the item text under it.
const STRIP_FONT: Pixels = px(12.5);
const ITEM_FONT: Pixels = px(13.);
const DETAIL_FONT: Pixels = px(12.);

/// Key context of the composer, so `Esc` reaches the composer even though the
/// textarea holds the focus and handles `Escape` first.
const KEY_CONTEXT: &str = "Composer";

/// How many prompts one composer remembers for its ↑/↓ history; the oldest fall off
/// past this. In memory, per tab: a composer is not a shell, and nothing here is
/// written down.
const HISTORY_LIMIT: usize = 64;

/// The action button's element id: one button, addressed by name.
pub const BUTTON_ID: &str = "composer-action";
/// The `+` beside it: what a test clicks to open the picker.
pub const ATTACH_ID: &str = "composer-attach";

/// The system's file picker, as the composer asks for one: the files the reader chose,
/// `None` when they put the dialog away — or when the platform could not open one.
///
/// Injectable ([`Composer::set_picker`]) so a test never opens a real dialog, and so a
/// capture can be of what the picker's own answer makes of the box.
pub type PathPicker =
    Arc<dyn Fn(&mut Window, &mut App) -> Task<Option<Vec<PathBuf>>> + Send + Sync>;

/// The platform's own picker: files, any number of them, and no folders — an attachment
/// is something evo can carry, and a folder is not.
fn system_picker(_window: &mut Window, cx: &mut App) -> Task<Option<Vec<PathBuf>>> {
    let dialog = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: true,
        prompt: None,
    });
    cx.spawn(async move |_cx: &mut AsyncApp| match dialog.await {
        Ok(Ok(paths)) => paths,
        // A dialog that was put away, or one that could not be opened at all: nothing
        // was chosen, and nothing is attached.
        _ => None,
    })
}

gpui_kit::actions!(composer, [Interrupt]);

/// One chip on the foot row: a fact the topic published, and whether it opens
/// something.
#[derive(Clone, Debug, PartialEq)]
struct ChipFact {
    /// The segment's own name — the chip's element id, and what a test addresses.
    name: String,
    text: SharedString,
    /// The dim second half: the effort level beside a model.
    dim: Option<SharedString>,
    /// Whether the chip opens the model drawer.
    opens: bool,
}

/// The facts one agent's topic reports, as this box draws them.
#[derive(Clone, Debug, Default, PartialEq)]
struct Agent {
    chips: Vec<ChipFact>,
    todos: Vec<Todo>,
    /// The model the topic reports: what the drawer ticks.
    model: Option<(String, String)>,
    /// The effort level the topic reports, as it is (`state.thinking`).
    thinking: Option<String>,
    /// The goal, for its strip: the status the title row states, the objective it folds
    /// out, and the budget evo tracks while it tracks one.
    goal: Option<GoalInfo>,
}

/// What the composer asks its owner to do. Each is one op the owner sends
/// (`session::OpRequest`): the composer names the action, never the endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposerEvent {
    /// Post this message as the coordinator's turn. It lands at the running turn's
    /// next boundary, so a message sent while the agent works is queued, not lost.
    /// The op is `input.send`: its images ride in `images`, and its other files are
    /// named by path in the text (see [`attachments`]).
    Send(Outgoing),
    /// Stop the coordinator's own run (the TUI's esc) — `run.interrupt` with scope
    /// `session`. The draft is untouched.
    Interrupt,
    /// Stop the whole swarm — `run.interrupt` with scope `swarm`: every lane, and the
    /// coordinator with it (CONTRACT §7.5).
    StopSwarm,
    /// Change the coordinator's model (`model.set`, CONTRACT §5). A lane's model is
    /// the swarm's, so this is emitted for the coordinator alone.
    ModelSet { id: String, provider: String },
    /// Change the coordinator's effort (`thinking.set`).
    ThinkingSet(String),
    /// Run this message as the slash command it is (`command.run`, §5.5): a
    /// message that is one command from its start is the command's, not the
    /// agent's — `/eval` included, whose content is a form for the image.
    Command { name: String, args: String },
    /// Ask what the caret is on, and what could fill it (`complete`, §5.6). The
    /// question is the text the caret is in and where it is in it — a byte offset —
    /// and the answer comes back through [`Composer::set_completion`], named by the
    /// same pair: an answer is only ever an answer about the text it was asked about.
    Complete { text: String, cursor: usize },
}

/// What the one button says — and therefore what clicking it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionFace {
    Send,
    /// The swarm is busy or held while its lanes work: the one action that stops it all.
    StopSwarm,
    /// The same button in a single-agent session (§7.2), where there is no swarm to
    /// stop: it stops this agent's own run, which is the only run there is.
    Stop,
}

impl ActionFace {
    /// The button's visible label — the design's own words, glyph and all:
    /// `.composer-send` holds one text run, `↑ Send`, at the button's own 13px, not a
    /// glyph beside a word. Stop's square is a character for the same reason.
    pub fn label(self) -> &'static str {
        match self {
            Self::Send => "\u{2191} Send",
            Self::StopSwarm => "\u{25a0} Stop swarm",
            Self::Stop => "\u{25a0} Stop",
        }
    }

    /// What the button is *called*, for a reader that cannot see the glyph it leads
    /// with: the word alone.
    pub fn name(self) -> &'static str {
        match self {
            Self::Send => "Send",
            Self::StopSwarm => "Stop swarm",
            Self::Stop => "Stop",
        }
    }
}

/// Where the caret goes when a prompt is recalled: the end of it, on the last line.
/// Columns are counted in characters, which is what the input's cursor positions are.
fn end_position(text: &str) -> Position {
    let line = text.matches('\n').count();
    let character = text
        .rsplit('\n')
        .next()
        .map_or(0, |last| last.chars().count());
    Position::new(line as u32, character as u32)
}

/// How wide one line of text is, as the text system measures it: what the popup is
/// sized by before any of it is laid out.
fn text_width(text: &str, size: Pixels, weight: FontWeight, window: &Window) -> Pixels {
    text_width_in(text, None, size, weight, window)
}

/// [`text_width`] in a given font family — the monospace face a Lisp symbol is drawn
/// in — or the window's own when `family` is `None`.
fn text_width_in(
    text: &str,
    family: Option<&SharedString>,
    size: Pixels,
    weight: FontWeight,
    window: &Window,
) -> Pixels {
    if text.is_empty() {
        return px(0.);
    }
    let mut style = window.text_style();
    if let Some(family) = family {
        style.font_family = family.clone();
    }
    style.font_size = AbsoluteLength::Pixels(size);
    style.font_weight = weight;
    let run = style.to_run(text.len());
    window
        .text_system()
        .shape_line(SharedString::from(text.to_string()), size, &[run], None)
        .width
}

/// Whether a keystroke is the plain `Enter` that submits a draft — the chord the
/// input's own `Enter` is bound to, with no modifier on it.
fn is_submit(keystroke: &Keystroke) -> bool {
    keystroke.key == "enter" && !keystroke.modifiers.modified()
}

/// Whether a keystroke is this platform's copy shortcut — the chord the input's own
/// `Copy` is bound to, and the one the window's `Copy` answers.
fn is_copy_shortcut(keystroke: &Keystroke) -> bool {
    if keystroke.key != "c" || keystroke.modifiers.shift || keystroke.modifiers.alt {
        return false;
    }
    if cfg!(target_os = "macos") {
        keystroke.modifiers.platform
    } else {
        keystroke.modifiers.control
    }
}

/// Whether a keystroke is this platform's paste shortcut, the chord the input's own
/// paste is bound to.
fn is_paste_shortcut(keystroke: &Keystroke) -> bool {
    if keystroke.key != "v" || keystroke.modifiers.shift || keystroke.modifiers.alt {
        return false;
    }
    if cfg!(target_os = "macos") {
        keystroke.modifiers.platform
    } else {
        keystroke.modifiers.control
    }
}

/// The name a tile says under its thumbnail: the file's own, and the whole path when it
/// has none (`/`), because a tile with no name is a tile nobody can tell from another.
fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// What one path is, as an attachment: an **image** of the turn when its name says so or
/// its own first bytes do, and a **file** — a path the agent is handed — otherwise.
///
/// Both signals, because either one can be missing. A screenshot's name is whatever took
/// it chose (`Screenshot 2026-10-02 at 14.03.11.png` ✓, `shot-2026-10-02` ✗), so the
/// magic is read; and a name is what the reader sees, so a `.png` that is not one is
/// still an image here — it is what they meant, and the refusal that comes back names
/// the file (§5.5).
fn kind_of(path: &Path) -> AttachmentKind {
    if has_image_extension(path) || looks_like_an_image(path) {
        AttachmentKind::ImageFile(path.to_path_buf())
    } else {
        AttachmentKind::File(path.to_path_buf())
    }
}

/// Whether the path's own name ends in one of the five image extensions.
fn has_image_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            IMAGE_EXTENSIONS
                .iter()
                .any(|image| extension.eq_ignore_ascii_case(image))
        })
}

/// Whether the file's first bytes are one of the five formats' own magic numbers.
///
/// A file that cannot be opened, or is too short to say, is not an image by this
/// reading: the picker's answer is a path, and a path that is not there is something the
/// reader finds out about when the message is sent.
fn looks_like_an_image(path: &Path) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = [0u8; 12];
    let Ok(read) = file.read(&mut head) else {
        return false;
    };
    image_magic(&head[..read])
}

/// The magic number PNG, JPEG, GIF and WebP all lead with — the sniff behind
/// [`kind_of`], and the same five extensions [`IMAGE_EXTENSIONS`] names.
fn image_magic(head: &[u8]) -> bool {
    /// `\x89PNG\r\n\x1a\n`.
    const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    head.starts_with(&PNG)
        // JPEG's start of image, then the first marker.
        || head.starts_with(&[0xFF, 0xD8, 0xFF])
        || head.starts_with(b"GIF87a")
        || head.starts_with(b"GIF89a")
        // WebP is a RIFF container whose form type is `WEBP` at offset 8.
        || (head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WEBP")
}

/// Whether one-shot bindings have been installed (the keys are global to the app).
struct KeysBound;
impl Global for KeysBound {}

/// The chips the topic's own segments make (CONTRACT §4.2).
///
/// The server's registry built them, so this walks them and folds the two that belong
/// together: `thinking` is the dim half of the `model` chip — `stub-a medium`, the way
/// the design draws it. The `model` chip opens the model drawer; everything else is a
/// chip of its own, in the server's order. Right-hand segments are the swarm's own
/// summary (`2 lanes`), which the lane column already states, so they are not repeated
/// here.
///
/// A `goal` segment is not a chip: the goal has a strip of its own above the foot
/// (`Composer::goal_strip`), and the segment's words name the goal's id — evo's handle
/// on the goal, not a reader's fact. The strip states the status and the objective
/// instead, from `state.goal`.
fn chips_of(segments: &[Segment]) -> Vec<ChipFact> {
    let (left, _right) = ordered_segments(segments);
    let effort = left
        .iter()
        .find(|segment| segment.name == "thinking")
        .map(|segment| SharedString::from(segment.text.clone()));
    let has_model = left.iter().any(|segment| segment.name == "model");
    let mut chips = Vec::new();
    for segment in left {
        let name = segment.name.as_str();
        if name == "goal" {
            continue;
        }
        // The effort rides on the model chip; with no model to ride on it is a fact
        // of its own rather than a dropped one.
        if name == "thinking" && has_model {
            continue;
        }
        chips.push(ChipFact {
            name: segment.name.clone(),
            text: SharedString::from(segment.text.clone()),
            dim: (name == "model").then(|| effort.clone()).flatten(),
            opens: name == "model",
        });
    }
    chips
}

/// The composer.
pub struct Composer {
    input: Entity<TextareaState>,
    /// The selected agent's own facts: the chips, the todos, the model, the goal.
    agent: Agent,
    /// The agent's name, as the drawer's title says it (`Coordinator`, `lane 3`).
    name: SharedString,
    /// Whether this box may change the model and the effort: `model.set` and
    /// `thinking.set` act on the session, so only the coordinator's are its to send.
    settable: bool,
    /// The ladder `thinking.set` accepts, in the server's order (`catalog`'s
    /// `thinking_levels`, CONTRACT §5.6).
    levels: Vec<SharedString>,
    /// The models the catalog lists, for the drawer (§5.6).
    models: Vec<ModelRow>,
    /// The commands the catalog lists: what a `/word` word completes against.
    /// The registry is the server's — a client never writes a command down (§8).
    commands: Vec<Candidate>,
    /// The inline completion popup, while the caret's word has candidates, and how
    /// wide it stands — measured when the rows are new, since a row cannot change
    /// without the popup being rebuilt.
    popup: Option<Popup>,
    popup_width: Option<Pixels>,
    /// The popup's own scroll position, kept across frames, and the prefix `Esc`
    /// has put the popup away for: it comes back when the word moves on.
    popup_scroll: ScrollHandle,
    dismissed: Option<String>,
    /// What the caret is on: the question out, and the server's last answer
    /// (see [`Completion`]).
    completion: Completion,
    /// Whether the model drawer is folded out, as the model chip folds it.
    model_open: bool,
    /// Whether the goal strip's objective is unfolded.
    goal_open: bool,
    /// Whether the todo list is unfolded.
    todos_open: bool,
    /// Whether the attachments strip is unfolded: the reader's own choice, kept per
    /// composer, and — like the other two strips — not something a press outside the box
    /// folds (`Composer::close_drawer_at`).
    attachments_open: bool,
    /// Whether each strip's chevron was drawn pointing up the last time it was drawn,
    /// and how many times it has turned: `.chev{transition:transform .12s ease}` needs
    /// to know what it is turning from, and a new id to turn under.
    goal_chevron_up: bool,
    goal_chevron_turns: u64,
    chevron_up: bool,
    chevron_turns: u64,
    attachments_chevron_up: bool,
    attachments_chevron_turns: u64,
    /// The swarm's own busy flag: what the action button's face follows.
    busy: bool,
    /// Whether this box is a swarm's (§7.2): what the button says while it is busy, and
    /// therefore what a click on it stops.
    swarm: bool,
    /// True while this composer's own request is in flight — the only reason the
    /// button is disabled.
    in_flight: bool,
    /// The prompts this tab has sent, oldest first: what ↑/↓ walks.
    history: Vec<String>,
    /// Where in `history` the input is, while it is showing a recalled prompt.
    walking: Option<usize>,
    /// The text the walk put in the input, so an edit of it is visible before the
    /// input's own `Change` event has had a chance to arrive.
    recalled: String,
    /// The draft as `Enter` found it (see [`Composer::new`]).
    pending_send: Option<String>,
    /// The tallest the input may grow: half the conversation pane, as the page
    /// measured it. The floor is the design's 62px, which the kit's two rows are.
    room: Pixels,
    /// The todo list's own scroll position, kept across frames (and per composer,
    /// so two tabs' lists do not share one).
    todos_scroll: ScrollHandle,
    /// The goal's objective scrolls under the strip's own cap, and its handle is the
    /// composer's for the same reason the todo list's is.
    goal_scroll: ScrollHandle,
    /// The drawer's model list, when the catalog is long enough to scroll it: the
    /// region's own position, kept per composer like the strips'.
    models_scroll: ScrollHandle,
    /// The tile panel's own scroll position, per composer like the todo list's.
    attachments_scroll: ScrollHandle,
    /// What the draft carries beside its words, in the order they were added, and the id
    /// the next one takes — the composer's own counter, so a tile can be taken off while
    /// others are added around it ([`Attachment::id`]).
    attachments: Vec<Attachment>,
    next_attachment: u64,
    /// The bytes of a pasted image, already framed for drawing: built when they arrive,
    /// since the panel is drawn again on every keystroke and a copy of an image's bytes
    /// per frame is not the box's work. Keyed by attachment id, like the tiles.
    pasted: HashMap<u64, Arc<Image>>,
    /// The effort slider's own motion: the level's move along the rail, the press,
    /// the hover and the focus fades. One per composer, handed back every render.
    effort_motion: Rc<Motion>,
    /// Where the box was painted last frame: what "outside the box" is measured
    /// against when the page folds an open drawer on a press (`useOutsideClose`).
    box_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// The rail's keyboard focus, so the arrows move the effort while it is held.
    effort_focus: FocusHandle,
    /// The file picker the `+` asks: the platform's own dialog, or one a test or the
    /// capture example put here.
    picker: PathPicker,
    /// The picker's own request, while the reader is choosing: held so that the answer
    /// still lands when the update that asked is over — a dropped task is a cancelled
    /// one.
    picking: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// What the drawer's effort row has to offer: the chosen model's own rungs, or the fact
/// that it takes no effort setting at all.
#[derive(Clone, Debug, PartialEq, Eq)]
enum DrawerLevels {
    /// The levels to draw, in the order they are offered.
    Rungs(Vec<SharedString>),
    /// The catalog names none for this model — `effort_levels: []` — which is the model's
    /// own answer, and not the same as no ladder being published.
    None,
}

/// One model the drawer offers, as `GET /catalog` describes it (§5.6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelRow {
    pub id: String,
    pub provider: String,
    /// The dim line under the name: the context window, the modalities, and the levels
    /// this registration takes.
    pub detail: String,
    /// The levels this registration takes, in the catalog's own order, empty when it
    /// takes no effort setting at all — what the drawer's ladder is drawn from.
    pub effort_levels: Vec<String>,
    /// Whether evo can reach it right now; the rest are listed with why not.
    pub reason: Option<String>,
}

/// What the caret is on, as the server last said: the question it is waiting on, and
/// the answer to the last one.
///
/// The word is the server's (`complete`); what the popup does with it is
/// [`Composer::refresh`]. Two things this keeps straight:
///
/// * **one question at a time.** A keystroke while one is out is not a second
///   question: the answer asks again for the text as it stands then, so the
///   question is always about the caret as it is, and never twice about the same
///   text ([`Composer::ask`] is what decides);
/// * **the answer belongs to its question.** An answer names a word in the text it
///   was asked about, so it is held against that text ([`complete::held`]) and
///   never against whatever is in the box a keystroke later.
#[derive(Default)]
struct Completion {
    /// The question a reply is owed for, while one is out.
    asked: Option<Question>,
    /// The last answer, and the question it answers.
    answer: Option<(Question, Answer)>,
    /// The debounce itself. Held so the next keystroke can drop it — a dropped
    /// task is a cancelled timer.
    timer: Option<Task<()>>,
}

impl EventEmitter<ComposerEvent> for Composer {}

impl Composer {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::init(cx);

        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(MIN_ROWS, MIN_ROWS)
                .placeholder(PLACEHOLDER)
                // `Enter` submits, `Shift+Enter` inserts a newline.
                .submit_on_enter(true)
        });
        let subscription = cx.subscribe(&input, |this, input, event: &InputEvent, cx| {
            match event {
                // `Shift+Enter` already inserted its newline in the state.
                InputEvent::PressEnter { shift: false, .. } => {
                    // The draft as the keypress found it, not as the input holds it
                    // when this event lands at the end of the update.
                    let draft = match this.pending_send.take() {
                        Some(draft) => draft,
                        // An event nobody pressed for is still the input's text.
                        None => input.read(cx).value().to_string(),
                    };
                    this.send(draft, cx);
                }
                InputEvent::Change => {
                    // An edit is what ends a walk through the history: the reader has
                    // taken the recalled prompt and made it their own draft.
                    this.walking = None;
                    // And an edit is what the completion popup is about: the caret's
                    // word has just changed under it.
                    this.refresh(cx);
                    cx.notify();
                }
                _ => {}
            }
        });

        // The keys the input handles itself but has no use for here: a window
        // selection is not the input's to copy, and an empty composer has no caret to
        // walk through its own prompts.
        let weak_input = input.downgrade();
        let weak_self = cx.weak_entity();
        let interceptor = cx.intercept_keystrokes(move |event, window, cx| {
            if let Some(composer) = weak_self.upgrade() {
                composer.update(cx, |composer, cx| {
                    composer.intercept(&weak_input, event, window, cx)
                });
            }
        });

        Self {
            input,
            agent: Agent::default(),
            name: "main".into(),
            settable: true,
            levels: Vec::new(),
            models: Vec::new(),
            commands: Vec::new(),
            popup: None,
            popup_width: None,
            popup_scroll: ScrollHandle::new(),
            dismissed: None,
            completion: Completion::default(),
            model_open: false,
            goal_open: false,
            todos_open: false,
            attachments_open: false,
            goal_chevron_up: false,
            goal_chevron_turns: 0,
            chevron_up: false,
            chevron_turns: 0,
            attachments_chevron_up: false,
            attachments_chevron_turns: 0,
            busy: false,
            in_flight: false,
            history: Vec::new(),
            walking: None,
            recalled: String::new(),
            pending_send: None,
            room: px(320.),
            todos_scroll: ScrollHandle::new(),
            goal_scroll: ScrollHandle::new(),
            models_scroll: ScrollHandle::new(),
            attachments_scroll: ScrollHandle::new(),
            attachments: Vec::new(),
            next_attachment: 1,
            pasted: HashMap::new(),
            effort_motion: Rc::new(Motion::new()),
            box_bounds: Rc::new(Cell::new(Bounds::default())),
            effort_focus: cx.focus_handle(),
            picker: Arc::new(system_picker),
            picking: None,
            swarm: true,
            _subscriptions: vec![subscription, interceptor],
        }
    }

    /// The prompts this composer has sent, oldest first — the tab's own history for
    /// ↑/↓, and nothing that outlives the process.
    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Installs the composer's key bindings; idempotent, and called by every
    /// composer, so an owner never has to remember it.
    pub fn init(cx: &mut App) {
        if cx.has_global::<KeysBound>() {
            return;
        }
        cx.set_global(KeysBound);
        cx.bind_keys([KeyBinding::new("escape", Interrupt, Some(KEY_CONTEXT))]);
    }

    /// The agent this box is showing: everything the box draws comes from its topic's
    /// state, so one call is the whole update (CONTRACT §4.2).
    ///
    /// `name` is what the drawer's title calls the agent, and `settable` is whether
    /// the model and the effort may be changed from here — true for the coordinator,
    /// whose session is what `model.set` and `thinking.set` act on.
    pub fn set_agent(
        &mut self,
        state: &TopicState,
        name: &str,
        settable: bool,
        cx: &mut Context<Self>,
    ) {
        let agent = Agent {
            chips: chips_of(&state.segments),
            todos: state.todos.clone(),
            model: state
                .model
                .as_ref()
                .map(|model| (model.id.clone(), model.provider.clone())),
            thinking: state.thinking.clone(),
            goal: state.goal.clone(),
        };
        let name: SharedString = name.into();
        let mut changed = false;
        if self.agent != agent {
            self.agent = agent;
            changed = true;
        }
        if self.name != name || self.settable != settable {
            self.name = name;
            self.settable = settable;
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    /// The models the drawer offers, the effort ladder `thinking.set` accepts and
    /// the commands a `/word` completes against: all of it the server's
    /// (`GET /catalog`, §5.6), and all of it in one call.
    pub fn set_catalog(
        &mut self,
        levels: Vec<String>,
        models: Vec<ModelRow>,
        commands: Vec<Candidate>,
        cx: &mut Context<Self>,
    ) {
        let levels: Vec<SharedString> = levels.into_iter().map(SharedString::from).collect();
        if self.levels != levels || self.models != models || self.commands != commands {
            self.levels = levels;
            self.models = models;
            self.commands = commands;
            // A registry that arrived while a word was being typed is a new set of
            // candidates for it.
            self.refresh(cx);
            cx.notify();
        }
    }

    /// Have the `+` ask `picker` for files instead of the platform's own dialog.
    ///
    /// Tests and the capture example inject one: a dialog is a window nobody in a
    /// headless run could answer, and what the box does with the answer — image or file,
    /// tile, count, removal — is what those runs are about.
    pub fn set_picker(&mut self, picker: PathPicker, cx: &mut Context<Self>) {
        self.picker = picker;
        cx.notify();
    }

    /// What the draft carries beside its words, in the order they were added.
    pub fn attachments(&self) -> &[Attachment] {
        &self.attachments
    }

    /// The server's answer to the question this box asked: what the caret is on, and
    /// what could fill it.
    ///
    /// `text` and `cursor` are the question the answer belongs to — the pair that went
    /// out through [`ComposerEvent::Complete`] — and the answer is stored against
    /// them, so the popup can tell a word the caret has merely typed into from a text
    /// this answer says nothing about.
    ///
    /// An answer with no word in it is an answer like any other — a caret on prose, a
    /// server that does not offer the op, a tab whose engine is gone — and it is what
    /// releases the question, so the next one can be asked.
    pub fn set_completion(
        &mut self,
        text: &str,
        cursor: usize,
        answer: Answer,
        cx: &mut Context<Self>,
    ) {
        let question = Question {
            text: text.to_string(),
            cursor,
        };
        if self.completion.asked.as_ref() == Some(&question) {
            self.completion.asked = None;
        }
        self.completion.answer = Some((question, answer));
        self.refresh(cx);
    }

    /// The word under the caret, read again: what the popup shows, and what it
    /// hides.
    ///
    /// Every edit lands here, and so does every key that may have moved the caret, so
    /// the popup is always drawn from the caret's own word — the server's answer to
    /// the last question, held against the text as it stands now. An edit also asks
    /// ([`Composer::ask`]): a word with no answer yet is a word with no popup, for the
    /// one round trip it takes.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let (text, caret) = {
            let input = self.input.read(cx);
            (input.value().to_string(), input.cursor())
        };
        self.ask(&text, caret, cx);
        // A prompt recalled from the history is not the reader's input: what the
        // caret sits on raises no popup until it is edited. Sending from the popup
        // would be worse than an absent suggestion — the list would capture the
        // ↑/↓ the history walks with.
        let held = self
            .completion
            .answer
            .as_ref()
            .and_then(|(question, answer)| {
                if self.walking.is_some() {
                    return None;
                }
                complete::held(answer, question, &text, caret)
            });
        let Some(held) = held else {
            self.popup = None;
            self.popup_width = None;
            return;
        };
        // Esc puts the popup away for the word it was on: a word that has moved
        // on is a new one, and asks again.
        if self.dismissed.as_deref() == Some(held.prefix.as_str()) {
            self.popup = None;
            self.popup_width = None;
            return;
        }
        self.dismissed = None;
        // A command word ranks the *catalog's* own commands — the list is drawn with
        // the names it begins first and the ones it is a subsequence of, which is
        // what a `/lo` finding `/reload` is for. The server's own answer to the same
        // word is the prefix-matched half of that same catalog (§5.6), so the rows
        // are its document either way; a symbol can only come from the answer, since
        // only the image knows its own names.
        let rows = match held.kind {
            CompletionKind::Command => complete::matches(&self.commands, &held.prefix),
            CompletionKind::Symbol => complete::prefix_matches(held.items, &held.prefix),
        };
        // Nothing to offer, or nothing left to choose: a popup whose only
        // candidate is the word already typed shows the reader their own input
        // back, and would capture the ↑/↓ that browse the history with it.
        if rows.is_empty() || complete::settled(&held.prefix, &rows) {
            self.popup = None;
            self.popup_width = None;
            return;
        }
        // The highlight stays where it was while the same word is still being
        // typed; a new word starts at the top of its own list.
        let index = match &self.popup {
            Some(popup) if popup.prefix == held.prefix && popup.kind == held.kind => popup.index,
            _ => 0,
        };
        let mut popup = Popup::new(&held, rows);
        popup.index = index.min(popup.rows.len() - 1);
        self.popup = Some(popup);
        // New rows, and a new width to measure: the popup is as wide as what it holds.
        self.popup_width = None;
    }

    /// Ask what the caret is on, unless the answer already in hand is about exactly
    /// this text and caret.
    ///
    /// Three things keep this from being a request per keystroke per frame:
    ///
    /// * an answer that is *current* — the same text, the same caret — is not asked
    ///   about again, so the answer to a question does not raise its own question;
    /// * one question is out at a time: a keystroke while one is out asks nothing, and
    ///   the answer asks again for the text as it stands then, which is the newest one
    ///   worth asking;
    /// * the debounce is reset by every keystroke, so a burst of typing is one
    ///   question.
    fn ask(&mut self, text: &str, caret: usize, cx: &mut Context<Self>) {
        let current = self
            .completion
            .answer
            .as_ref()
            .is_some_and(|(question, _)| question.text == text && question.cursor == caret);
        if current || self.completion.asked.is_some() {
            return;
        }
        self.completion.timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COMPLETE_DEBOUNCE).await;
            let _ = this.update(cx, |composer, cx| {
                composer.completion.timer = None;
                // The caret may have moved while the timer ran: the question follows
                // the caret, not the keystroke that raised it.
                composer.ask_now(cx);
            });
        }));
    }

    /// Put the question to the server now: the caret's own text, and where it is.
    ///
    /// Nothing is asked while a question is out — the answer comes back through
    /// [`Composer::set_completion`], which reads the caret again — and the question is
    /// remembered as asked, so a tab that cannot send it can answer it with nothing
    /// and leave the popup answering again.
    ///
    /// Nor is a caret already answered asked about again: the timer this runs from
    /// belongs to a keystroke that may have been answered since, and asking would be
    /// both a round trip nobody needs and the loss of the rows on screen — the answer
    /// in hand is dropped for the one being asked about the same text.
    fn ask_now(&mut self, cx: &mut Context<Self>) {
        if self.completion.asked.is_some() {
            return;
        }
        let (text, cursor) = {
            let input = self.input.read(cx);
            (input.value().to_string(), input.cursor())
        };
        let answered = self
            .completion
            .answer
            .as_ref()
            .is_some_and(|(question, _)| question.text == text && question.cursor == cursor);
        if answered {
            return;
        }
        self.completion.asked = Some(Question {
            text: text.clone(),
            cursor,
        });
        cx.emit(ComposerEvent::Complete { text, cursor });
    }

    /// Move the popup's highlight by `delta` rows, wrapping at both ends.
    fn walk(&mut self, delta: isize, cx: &mut Context<Self>) {
        cx.stop_propagation();
        let Some(popup) = self.popup.as_mut() else {
            return;
        };
        popup.walk(delta);
        // A highlight below the list's own fold is no highlight at all: the list
        // follows the keys.
        self.popup_scroll.scroll_to_item(popup.index);
        cx.notify();
    }

    /// Put the popup away until the word changes: the reader has said they are
    /// not choosing from it. This is what the popup's `Esc` means — not the
    /// coordinator's own interrupt, which a second `Esc` still is.
    fn dismiss(&mut self, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.dismissed = self.popup.as_ref().map(|popup| popup.prefix.clone());
        self.popup = None;
        cx.notify();
    }

    /// Take the highlighted candidate into the input: the whole `/command` word,
    /// or just the symbol token under the caret.
    fn accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        // The word as the text stands now, not as it stood when the popup was
        // built: a caret moved by a click since then is still this key's word.
        self.refresh(cx);
        let Some(popup) = self.popup.take() else {
            return;
        };
        let Some(candidate) = popup.chosen() else {
            return;
        };
        let name = candidate.name.clone();
        let text = self.input.read(cx).value().to_string();
        let (text, caret) = complete::accepted(&text, popup.kind, &popup.word, &name);
        let caret = end_position(&text[..caret]);
        self.input.update(cx, |input, cx| {
            input.set_value(text.as_str(), window, cx);
            input.set_cursor_position(caret, window, cx);
        });
        cx.notify();
    }

    /// Unfold or fold the goal's objective, which is also the chevron's turn: the angle
    /// it is coming from is the one it was last drawn at, and the turn is re-keyed so
    /// the animation runs once per press (`.chev{transition:transform .12s ease}`).
    ///
    /// Folding its own objective is the *only* thing a press outside the box does to
    /// the goal: the strip is where the goal is read, not a drawer that comes and goes.
    fn toggle_goal(&mut self, cx: &mut Context<Self>) {
        self.goal_chevron_up = self.goal_open;
        self.goal_chevron_turns = self.goal_chevron_turns.wrapping_add(1);
        self.goal_open = !self.goal_open;
        cx.notify();
    }

    /// Unfold or fold the todo list, which is also the chevron's turn: the angle it
    /// is coming from is the one it was last drawn at, and the turn is re-keyed so
    /// the animation runs once per press (`.chev{transition:transform .12s ease}`).
    fn toggle_todos(&mut self, cx: &mut Context<Self>) {
        self.chevron_up = self.todos_open;
        self.chevron_turns = self.chevron_turns.wrapping_add(1);
        self.todos_open = !self.todos_open;
        cx.notify();
    }

    /// Unfold or fold the attachments, which is also that strip's chevron turn.
    fn toggle_attachments(&mut self, cx: &mut Context<Self>) {
        self.attachments_chevron_up = self.attachments_open;
        self.attachments_chevron_turns = self.attachments_chevron_turns.wrapping_add(1);
        self.attachments_open = !self.attachments_open;
        cx.notify();
    }

    /// Ask the picker for files and attach what the reader chose — the `+`'s whole work.
    ///
    /// The dialog is asked for in this update (a platform dialog is opened by the
    /// platform, and its answer arrives whenever the reader gives it), and the answer
    /// lands in a task of its own: the box does not block on a reader who is still
    /// choosing, and the composer holds that task so the answer is still delivered.
    fn pick_attachments(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picker = self.picker.clone();
        let chosen = picker(window, cx);
        self.picking = Some(cx.spawn_in(window, async move |this, cx| {
            let Some(paths) = chosen.await else {
                return;
            };
            this.update_in(cx, |composer, _window, cx| composer.attach(paths, cx))
                .ok();
        }));
    }

    /// Attach the files at `paths`, in the order they were chosen.
    fn attach(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        for path in paths {
            let name = name_of(&path);
            let kind = kind_of(&path);
            self.push_attachment(name, kind, None);
        }
        cx.notify();
    }

    /// Attach the image the clipboard is holding, if it holds one, and answer whether it
    /// did — `⌘V`'s own work.
    ///
    /// One image entry, whichever comes first: a clipboard can carry several
    /// representations of what was copied, and the one the reader copied is the one they
    /// mean. Nothing else in a clipboard is an attachment here — text is the input's own
    /// paste, untouched, and a copied file is what the picker and a drop are for.
    fn attach_pasted_image(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(image) = cx.read_from_clipboard().and_then(|item| {
            item.entries().iter().find_map(|entry| match entry {
                ClipboardEntry::Image(image) => Some(image.clone()),
                _ => None,
            })
        }) else {
            return false;
        };
        let name = format!("{PASTED_NAME}.{}", image.format.extension());
        let kind = AttachmentKind::ImageBytes {
            media_type: image.format.mime_type().to_string(),
            bytes: Arc::new(image.bytes.clone()),
        };
        self.push_attachment(name, kind, Some(Arc::new(image)));
        cx.notify();
        true
    }

    /// Put one attachment on the draft, giving it the next id: where all three ways of
    /// adding one land — the picker, a paste, a drop — so the strip's own rule is written
    /// once.
    ///
    /// It also unfolds the strip on the *first* attachment: the tiles are where a reader
    /// checks what they just picked, and where a file picked by mistake is taken back, so
    /// a strip that appeared shut would hide the answer to the dialog they had just
    /// answered. After that the strip is theirs — adding a fourth does not re-open one
    /// they folded.
    fn push_attachment(&mut self, name: String, kind: AttachmentKind, image: Option<Arc<Image>>) {
        let id = self.next_attachment;
        self.next_attachment += 1;
        if let Some(image) = image {
            self.pasted.insert(id, image);
        }
        if self.attachments.is_empty() {
            self.attachments_open = true;
        }
        self.attachments.push(Attachment { id, name, kind });
    }

    /// Take one attachment off the message — the tile's own `×`.
    fn remove_attachment(&mut self, id: u64, cx: &mut Context<Self>) {
        self.attachments.retain(|attachment| attachment.id != id);
        self.pasted.remove(&id);
        cx.notify();
    }

    /// A strip's chevron: the design's one glyph — an up chevron — drawn at 12px and
    /// turned over the design's 120ms when the strip is folded (`open` is 0°, the
    /// folded state the 180° the CSS rotates it to). `key` and `drawn` are the strip's
    /// own: which one it is, what angle it was last drawn at, and how many turns it has
    /// taken, so the animation runs once per press.
    fn chevron(&self, open: bool, key: &'static str, drawn: bool, turns: u64) -> AnyElement {
        // Up is the open strip: 0°. Folded is the 180° the CSS turns it to.
        let angle = |up: bool| if up { 0. } else { std::f32::consts::PI };
        let icon = Icon::new(IconName::ChevronUp).size(px(12.));
        if drawn == open {
            return icon.rotate(radians(angle(open))).into_any_element();
        }
        let (from, to) = (angle(drawn), angle(open));
        let turn = Animation::new(CHEVRON_TURN).with_easing(cubic_bezier(0.25, 0.1, 0.25, 1.));
        icon.with_animation((key, turns), turn, move |icon, t: f32| {
            icon.rotate(radians(from + (to - from) * t))
        })
        .into_any_element()
    }

    /// Fold the model drawer back if `at` is a press outside the box, which is what
    /// the design's `pointerdown` listener on the document does. The page asks this
    /// on every press it sees, so a press on the transcript, the lanes or the band
    /// folds it, and one in the box — on the input, a chip, a strip, the drawer
    /// itself — leaves it.
    ///
    /// The strips are not the drawer's: where their own rows put them is where they
    /// stay, whoever presses where.
    pub fn close_drawer_at(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        if self.box_bounds.get().contains(&at) {
            return;
        }
        self.close_drawer(cx);
    }

    /// Fold the model drawer back, as selecting another agent does.
    pub fn close_drawer(&mut self, cx: &mut Context<Self>) {
        if self.model_open {
            self.model_open = false;
            cx.notify();
        }
    }

    /// Fold the model drawer out, showing the model it is on.
    ///
    /// The catalog can be long — every provider registered is thirty rows — so the
    /// models scroll inside a region seven rows tall; the row this box runs is
    /// often not among the seven it opens on, so it is brought into view.
    ///
    /// The reveal is arithmetic rather than `ScrollHandle::scroll_to_item`, which
    /// answers a reveal against the layout the frame before it is asked in left
    /// behind: the frame the drawer opens in is the first the region exists in, so
    /// an ask in it is an ask about a region that was not there — the list opens
    /// where it was left, at its top, with the ticked row under the fold (the same
    /// trap `WorkspaceView::reveal_selected_tab` documents for the strip, which
    /// waits a frame for it). Every row is `DRAWER_ITEM` tall and the region shows
    /// `MODEL_LIST_ROWS` of them, so the least scroll that puts the chosen row in
    /// whole is a sum the composer can do itself, in the frame the drawer opens in.
    fn open_drawer(&mut self, cx: &mut Context<Self>) {
        self.model_open = true;
        if self.models.len() > MODEL_LIST_ROWS {
            if let Some(offset) = self.reveal_chosen_model() {
                self.models_scroll.set_offset(point(px(0.), offset));
            }
        }
        cx.notify();
    }

    /// The offset that shows the ticked model: `None` when the box runs a model the
    /// catalog does not list, and zero when the region already holds it whole.
    fn reveal_chosen_model(&self) -> Option<Pixels> {
        let (id, provider) = self.agent.model.as_ref()?;
        let index = self
            .models
            .iter()
            .position(|model| model.id == *id && model.provider == *provider)?;
        let region = DRAWER_ITEM * MODEL_LIST_ROWS;
        let bottom = DRAWER_ITEM * index + DRAWER_ITEM;
        Some(if bottom <= region {
            px(0.)
        } else {
            region - bottom
        })
    }

    /// How tall the conversation pane is: what the input may grow to half of
    /// (`AutoTextarea.tsx`). The page measures itself and says so on every frame it
    /// is rendered at, so a window resize re-fits the input.
    pub fn set_pane_height(&mut self, pane: Pixels, cx: &mut Context<Self>) {
        let room = px(f32::from(pane) * ROOM_SHARE);
        if self.room == room {
            return;
        }
        self.room = room;
        let rows = rows_for(room);
        self.input
            .update(cx, |input, cx| input.set_auto_grow(MIN_ROWS, rows, cx));
        cx.notify();
    }

    /// Whether anything is going on — the window's own reading of the model: the
    /// coordinator running or held, a lane working. It is what the button's face
    /// follows.
    pub fn set_swarm_busy(&mut self, busy: bool, cx: &mut Context<Self>) {
        if self.busy != busy {
            self.busy = busy;
            cx.notify();
        }
    }

    /// Whether this box belongs to a swarm (§7.2). One agent has no swarm behind it: the
    /// button says `Stop` rather than `Stop swarm`, and it stops that agent's own run —
    /// the session-scoped interrupt, which is the only one an `evo-agent` server knows.
    /// A swarm is what the box opens as.
    pub fn set_swarm(&mut self, swarm: bool, cx: &mut Context<Self>) {
        if self.swarm != swarm {
            self.swarm = swarm;
            cx.notify();
        }
    }

    /// What the box says while it is empty: what is typed here goes to the coordinator,
    /// or to the agent, depending on the program this session is (§7.2).
    fn placeholder(&self) -> &'static str {
        if self.swarm {
            PLACEHOLDER
        } else {
            PLACEHOLDER_AGENT
        }
    }

    /// Report the outcome of this composer's own request.
    ///
    /// `ok` clears the draft — the input is emptied only after the server took the
    /// text. A failed send (or an interrupt) leaves it alone, and leaves what it carries
    /// attached: a refusal names what was wrong (`image could not be read: …`), so the
    /// reader fixes that and sends the same message again.
    pub fn request_finished(&mut self, ok: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.in_flight = false;
        if ok {
            self.input
                .update(cx, |input, cx| input.set_value("", window, cx));
            // The attachments went with the message, so the next one starts empty.
            self.attachments.clear();
            self.pasted.clear();
        }
        cx.notify();
    }

    /// Put `text` in the input for editing, the caret at its end.
    ///
    /// This is what a command that hands a message back does with it — `/rewind`, and
    /// `/tree` on a user message: it moves the session's leaf above the message and
    /// gives the text back so it can be edited and resubmitted. Nothing here is a
    /// send, and nothing is remembered as one.
    pub fn set_draft(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.in_flight = false;
        self.walking = None;
        let caret = end_position(text);
        self.input.update(cx, |input, cx| {
            input.set_value(text, window, cx);
            input.set_cursor_position(caret, window, cx);
        });
        self.refresh(cx);
        cx.notify();
    }

    /// The button's face: `Send` while nothing is going on, and `Stop swarm` while the
    /// swarm is busy or held for its lanes — the one action that means the whole swarm
    /// (CONTRACT §7.5). The per-lane Stop lives in the lane column, where a lane is
    /// named.
    pub fn face(&self) -> ActionFace {
        match (self.busy, self.swarm) {
            (false, _) => ActionFace::Send,
            (true, true) => ActionFace::StopSwarm,
            (true, false) => ActionFace::Stop,
        }
    }

    /// Whether the button wears the kit's disabled face — which is what the render
    /// hands to `.disabled(..)`, and the one testable form of it, since a `Button`
    /// reports no disabled flag to an element snapshot.
    ///
    /// Only a request of this composer's own in flight greys it. An empty draft is
    /// not a disabled button (`.composer-send` is drawn in the primary face at rest);
    /// it is a button with nothing to send, and the click and `Enter` do nothing.
    fn is_action_disabled(&self) -> bool {
        self.in_flight
    }

    /// Put the caret in the input, as opening a tab does.
    pub fn focus_input(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Emit `Send` for a message — its words, or what it carries — unless a request is
    /// already in flight.
    ///
    /// A draft with attachments and no text is a message: the files are what it says
    /// (§5.5), and a blank box with a picture attached is not a blank box.
    fn send(&mut self, draft: String, cx: &mut Context<Self>) {
        if self.in_flight || (draft.trim().is_empty() && self.attachments.is_empty()) {
            return;
        }
        self.in_flight = true;
        // A prompt is remembered the moment it is sent, not when the server takes
        // it: the reader's ↑ should bring back what they just sent even if the
        // request is still on its way. A message with no words is not a prompt —
        // there is nothing for ↑ to bring back.
        if !draft.trim().is_empty() {
            self.remember(&draft);
        }
        self.walking = None;
        // A message that *is* a slash command is the command's, not the agent's:
        // it goes to `command.run`, and the server decides — including whether
        // the command exists at all. Anything else, `/` and all, is the reader's
        // words to the coordinator. A message carrying attachments is not a
        // command: a command has nowhere to put a file, and the reader attached
        // one to what they wrote.
        let event = match self
            .attachments
            .is_empty()
            .then(|| complete::command_message(&draft))
            .flatten()
        {
            Some((name, args)) => ComposerEvent::Command {
                name: name.to_string(),
                args: args.to_string(),
            },
            None => ComposerEvent::Send(Outgoing {
                text: draft,
                attachments: self.attachments.clone(),
            }),
        };
        cx.emit(event);
        cx.notify();
    }

    /// Remember a prompt this tab sent, so ↑ can bring it back.
    fn remember(&mut self, prompt: &str) {
        // Sending the same prompt again — the retry after a refusal, most often — is
        // one entry, not two.
        if self.history.last().map(String::as_str) == Some(prompt) {
            return;
        }
        if self.history.len() == HISTORY_LIMIT {
            self.history.remove(0);
        }
        self.history.push(prompt.to_string());
    }

    /// Walk the sent prompts: ↑ back in time, ↓ forward, past the newest one and out
    /// of the walk.
    ///
    /// The input a walk starts from is empty, which is the draft it comes back to.
    fn recall(&mut self, back: bool, window: &mut Window, cx: &mut Context<Self>) {
        let next = match (self.walking, back) {
            // The oldest prompt is where ↑ stops: it is not a way out of the walk.
            (Some(index), true) => Some(index.saturating_sub(1)),
            (Some(index), false) => (index + 1 < self.history.len()).then_some(index + 1),
            (None, true) => self.history.len().checked_sub(1),
            (None, false) => None,
        };
        self.walking = next;
        let text = next.map_or_else(String::new, |index| self.history[index].clone());
        self.recalled = text.clone();
        let caret = end_position(&text);
        self.input.update(cx, |input, cx| {
            input.set_value(text.as_str(), window, cx);
            // A multi-line `set_value` leaves the caret at the start; a recalled
            // prompt is read and typed onto from its end.
            input.set_cursor_position(caret, window, cx);
        });
        cx.notify();
    }

    /// Take the keys the input handles without doing what the reader means, while its
    /// caret is in this composer.
    ///
    /// The input owns ↑/↓ (caret movement) and the copy shortcut (its own selection),
    /// and handles both itself rather than letting either through. An empty composer
    /// has no caret to move and nothing to copy, though: ↑ belongs to the tab's prompt
    /// history, and a window selection — the reader's, made in the transcript — is
    /// what the shortcut was aimed at.
    fn intercept(
        &mut self,
        input: &WeakEntity<TextareaState>,
        event: &KeystrokeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(input) = input.upgrade() else {
            return;
        };
        if !input
            .read(cx)
            .presentation()
            .focus_handle()
            .is_focused(window)
        {
            return;
        }

        // A recalled prompt the reader has typed in is their draft now, whatever the
        // input's own `Change` event has yet to say about it.
        if self
            .walking
            .is_some_and(|index| input.read(cx).value().as_ref() != self.history[index].as_str())
        {
            self.walking = None;
        }

        let keystroke = &event.keystroke;

        // The popup is a list, and while it is up the keys that walk a list are
        // its own: ↑/↓ pick a row rather than move the caret or browse the
        // history, Tab and Enter take the row, and Esc puts the list away —
        // never the coordinator's interrupt, which is what Esc means with no
        // popup up. Nothing here reaches the input, so a row taken with Enter
        // is not also a draft sent.
        if self.popup.is_some() {
            match keystroke.key.as_str() {
                "up" => return self.walk(-1, cx),
                "down" => return self.walk(1, cx),
                "tab" => return self.accept(window, cx),
                "enter" if is_submit(keystroke) => return self.accept(window, cx),
                "escape" => return self.dismiss(cx),
                _ => {}
            }
        }

        // `⌘V` with an image on the clipboard attaches it rather than pasting it: the
        // input can draw nothing of a picture, and the reader means to send it. Text on
        // the clipboard is the input's own paste, and this leaves it alone — it is taken
        // only when there is an image to take.
        if is_paste_shortcut(keystroke) && self.attach_pasted_image(cx) {
            cx.stop_propagation();
            return;
        }

        // A plain `Enter` submits, but the input submits at the end of this update:
        // the reader's words are taken now, at the key, so that whatever rewrites the
        // input in between is not what goes out.
        if is_submit(keystroke) {
            self.pending_send = Some(input.read(cx).value().to_string());
        }

        let empty = input.read(cx).value().is_empty();
        match keystroke.key.as_str() {
            "up" if self.walking.is_some() || empty => {
                self.recall(true, window, cx);
                cx.stop_propagation();
            }
            "down" if self.walking.is_some() => {
                self.recall(false, window, cx);
                cx.stop_propagation();
            }
            // With nothing of its own selected the input has no copy to make; the
            // window's selection is the one the reader means.
            "c" if is_copy_shortcut(keystroke)
                && !input.read(cx).is_copyable()
                && self.copy_window_selection(window, cx) =>
            {
                cx.stop_propagation();
            }
            _ => {}
        }

        // The caret may end up somewhere else than this key left it — an arrow, a
        // Home, a word-delete — and the popup is about where the caret is. It is
        // read again once this key's update is over, which is when the input has
        // moved it. A key that types something says so through the input's own
        // `Change`.
        if keystroke.key_char.is_none() {
            cx.defer_in(window, |this, _window, cx| this.refresh(cx));
        }
    }

    /// Copy what the window has selected — a selection the reader made outside the
    /// input — the way the window's own copy does. Answers whether there was anything
    /// to copy.
    fn copy_window_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let text = TextSelection::selected_text(window, cx).trim().to_string();
        if text.is_empty() {
            return false;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        true
    }

    /// Emit `Interrupt`, unless a request is already in flight.
    fn interrupt(&mut self, cx: &mut Context<Self>) {
        if self.in_flight {
            return;
        }
        self.in_flight = true;
        cx.emit(ComposerEvent::Interrupt);
        cx.notify();
    }

    fn interrupt_action(&mut self, _: &Interrupt, _: &mut Window, cx: &mut Context<Self>) {
        self.interrupt(cx);
    }

    /// The title row both strips wear (`.todo-strip-row`): 32px, the design's paddings
    /// and 10px gap, the muted ink the row's own words turn into the foreground under
    /// the pointer, and the chevron it is handed at its right-hand end.
    ///
    /// `leading` is what the strip says at its left, in its own order; the chevron comes
    /// last, pushed to the row's end (`margin-left:auto`). A strip builds its own
    /// ([`Composer::chevron`]), because the angle it turns from and the turn it is on are
    /// the strip's own.
    fn strip_row(
        &self,
        id: &'static str,
        aria: SharedString,
        leading: Vec<AnyElement>,
        chevron: AnyElement,
        palette: &'static Palette,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> AnyElement {
        h_flex()
            .id(id)
            .test_support()
            .aria_label(aria)
            .h(STRIP_ROW)
            .w_full()
            .items_center()
            .gap(px(10.))
            .pl(px(14.))
            .pr(px(10.))
            // The design keeps the arrow over every control of its own — a macOS app's
            // chrome does not turn the pointer into a hand
            // (`.todo-strip-row{cursor:default}`) — and only the effort slider, a
            // control the pointer does track, asks for one.
            .cursor_default()
            .text_size(STRIP_FONT)
            // `.todo-strip-row:hover{color:var(--fg)}`: the row's own words take the
            // ink; whatever it sets in the ink is already the ink.
            .text_color(paint::color(palette.muted_fg))
            .hover(move |row| row.text_color(paint::color(palette.fg)))
            .on_click(on_click)
            .children(leading)
            .child(
                div()
                    .ml_auto()
                    .flex_none()
                    .text_color(paint::color(palette.muted_fg))
                    .child(chevron),
            )
            .into_any_element()
    }

    /// The goal strip: the title row, always, and the objective under it while it is
    /// open.
    ///
    /// The strip is where the goal is read, so it is not a drawer: a press outside the
    /// box does not fold it (`Composer::close_drawer_at`) and the model drawer opening
    /// does not either. Its own row is the one thing that folds its objective, and the
    /// composer keeps that, per tab.
    ///
    /// An agent with no goal has no strip: a state with no goal is the only thing that
    /// means there is no goal (§4.2). An agent with one has the status on the row, the
    /// objective folded out under it, and its budget beside the status while evo tracks
    /// one. The goal's **id** (`g-b7ba`) is evo's own handle on it, not a reader's fact:
    /// the status line's `goal` segment spells it out and is not drawn as a chip
    /// (`chips_of`), and nothing here writes it either.
    fn goal_strip(
        &self,
        palette: &'static Palette,
        cx: &Context<Self>,
        top: bool,
    ) -> Option<AnyElement> {
        let goal = self.agent.goal.as_ref()?;
        let open = self.goal_open;
        // The design's own title: `Goal` at the 500 weight in the ink, and the status in
        // the dim half beside it, where the drawer put it (`drawer-dim`).
        let mut leading: Vec<AnyElement> = vec![
            div()
                .flex_none()
                .font_weight(widgets::text::MEDIUM)
                .text_color(paint::color(palette.fg))
                .child(SharedString::from("Goal"))
                .into_any_element(),
            div()
                .flex_none()
                .child(SharedString::from(format!("({})", goal.status)))
                .into_any_element(),
        ];
        // The budget, beside the status, when evo is tracking one: the count a goal is
        // run against is a fact of its own, and a goal with no budget has no count to
        // draw rather than a zero standing for one.
        let mut aria = format!("Goal ({})", goal.status);
        if goal.budget.is_some() {
            let tokens = SharedString::from(goal.tokens_label());
            aria.push_str(&format!(" · {tokens}"));
            leading.push(div().flex_none().child(tokens).into_any_element());
        }
        let mut strip = v_flex()
            .id("goal-strip")
            .test_support()
            .w_full()
            .flex_none()
            .bg(paint::color(palette.sidebar))
            // It is the box's first child, so its corners are the box's top corners: the
            // same curve as the border's inner edge, never a square that leaves the box's
            // own fill showing in the wedge.
            .when(top, |this| this.rounded_t(BOX_INNER_RADIUS))
            .border_b_1()
            .border_color(paint::color(palette.border))
            .child(self.strip_row(
                "goal-strip-row",
                SharedString::from(aria),
                leading,
                self.chevron(
                    open,
                    "goal-chevron",
                    self.goal_chevron_up,
                    self.goal_chevron_turns,
                ),
                palette,
                cx.listener(|this, _, _, cx| this.toggle_goal(cx)),
            ));
        if open {
            // The objective is read as prose, and reads the way the input's own lines
            // do (`.goal-text{font-size:13px;line-height:20px}`). A long one scrolls
            // under the strip's own cap, in the host-and-bar shape the todo list uses —
            // the host holds the height, the bar is its sibling, and the handle lives on
            // the composer so two tabs' strips do not share a scroll position.
            strip = strip.child(
                div()
                    .id(("goal-text-host", cx.entity_id()))
                    .relative()
                    .w_full()
                    .flex_none()
                    .child(
                        div()
                            .id("goal-text")
                            .test_support()
                            // The block's own words, as a reader that cannot see them
                            // hears them.
                            .aria_label(SharedString::from(goal.objective.clone()))
                            .w_full()
                            .max_h(STRIP_LIST_MAX)
                            .overflow_y_scroll()
                            .track_scroll(&self.goal_scroll)
                            .px(px(14.))
                            .pb(px(4.))
                            .text_size(ITEM_FONT)
                            .line_height(INPUT_LINE)
                            .text_color(paint::color(palette.fg))
                            .child(SharedString::from(goal.objective.clone())),
                    )
                    .vertical_scrollbar(&self.goal_scroll),
            );
        }
        Some(strip.into_any_element())
    }

    /// The todo strip: the title row, always, and the list under it while it is open.
    ///
    /// An agent with no todos has no strip — `Todos 0/0` over nothing is a row of
    /// chrome that says only that there is nothing to say.
    fn todo_strip(
        &self,
        palette: &'static Palette,
        cx: &Context<Self>,
        top: bool,
    ) -> Option<AnyElement> {
        if self.agent.todos.is_empty() {
            return None;
        }
        let done = self
            .agent
            .todos
            .iter()
            .filter(|todo| todo.status == TodoStatus::Done)
            .count();
        let open = self.todos_open;
        let count = SharedString::from(format!("Todos {done}/{}", self.agent.todos.len()));
        // With no goal strip above it this is the box's first child, and its corners are
        // the box's top corners.
        let mut strip = v_flex()
            .id("todo-strip")
            .test_support()
            .w_full()
            .flex_none()
            .bg(paint::color(palette.sidebar))
            .when(top, |this| this.rounded_t(BOX_INNER_RADIUS))
            .border_b_1()
            .border_color(paint::color(palette.border))
            .child(self.strip_row(
                "todo-strip-row",
                count.clone(),
                // The count is the design's own `.todo-strip-count`: the ink, at the 500
                // weight, over the row's muted words.
                vec![div()
                    .flex_none()
                    .font_weight(widgets::text::MEDIUM)
                    .text_color(paint::color(palette.fg))
                    .child(count)
                    .into_any_element()],
                self.chevron(open, "todo-chevron", self.chevron_up, self.chevron_turns),
                palette,
                cx.listener(|this, _, _, cx| this.toggle_todos(cx)),
            ));
        if open {
            // The rows scroll inside the list, and the bar is the kit's, on a host
            // that does not scroll (its own absolute overlay) — the same shape
            // gpui-component's own popup menu uses. The list keeps the id the tests
            // and probes find it by, and its own handle, so two tabs' todo lists do
            // not share a scroll position.
            strip = strip.child(
                div()
                    .id(("todo-list-host", cx.entity_id()))
                    .relative()
                    .w_full()
                    .flex_none()
                    .child(
                        div()
                            .id("todo-list")
                            .test_support()
                            .w_full()
                            .max_h(STRIP_LIST_MAX)
                            .overflow_y_scroll()
                            .track_scroll(&self.todos_scroll)
                            .pb(px(4.))
                            .px(px(14.))
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .children(
                                self.agent
                                    .todos
                                    .iter()
                                    .enumerate()
                                    .map(|(index, todo)| todo_row(index, todo, palette)),
                            ),
                    )
                    .vertical_scrollbar(&self.todos_scroll),
            );
        }
        Some(strip.into_any_element())
    }

    /// The attachments strip: the title row, always, and the tiles under it while it is
    /// open.
    ///
    /// It is the third of the box's strips and stands below the other two, because it is
    /// the *draft's* own state where those are the topic's: the goal and the plan are
    /// what the agent is working on, and this is what the message being written carries
    /// — so it stands closest to the input the message is written in, and the drawer,
    /// which folds out of a chip in the foot rather than standing as state, comes after
    /// all three of them.
    ///
    /// An empty draft has no strip: there is nothing to say, and a row of chrome saying
    /// `Attachments 0` is worse than none.
    fn attachments_strip(
        &self,
        palette: &'static Palette,
        cx: &Context<Self>,
        top: bool,
    ) -> Option<AnyElement> {
        if self.attachments.is_empty() {
            return None;
        }
        let open = self.attachments_open;
        // The design's own count, in the ink over the row's muted words
        // (`.todo-strip-count`), spelled as the user's own reading of it:
        // `Attachments 3`.
        let count = SharedString::from(format!("Attachments {}", self.attachments.len()));
        let mut strip = v_flex()
            .id("attachments-strip")
            .test_support()
            .w_full()
            .flex_none()
            .bg(paint::color(palette.sidebar))
            .when(top, |this| this.rounded_t(BOX_INNER_RADIUS))
            .border_b_1()
            .border_color(paint::color(palette.border))
            .child(self.strip_row(
                "attachments-strip-row",
                count.clone(),
                vec![div()
                    .flex_none()
                    .font_weight(widgets::text::MEDIUM)
                    .text_color(paint::color(palette.fg))
                    .child(count)
                    .into_any_element()],
                self.chevron(
                    open,
                    "attachments-chevron",
                    self.attachments_chevron_up,
                    self.attachments_chevron_turns,
                ),
                palette,
                cx.listener(|this, _, _, cx| this.toggle_attachments(cx)),
            ));
        if open {
            // The panel scrolls past the todo list's own cap, on the same host-and-bar
            // shape and with a handle of its own, so two tabs' panels do not share a
            // scroll position.
            strip = strip.child(
                div()
                    .id(("attachments-host", cx.entity_id()))
                    .relative()
                    .w_full()
                    .flex_none()
                    .child(
                        div()
                            .id("attachments-panel")
                            .test_support()
                            .w_full()
                            .max_h(STRIP_LIST_MAX)
                            .overflow_y_scroll()
                            .track_scroll(&self.attachments_scroll)
                            .px(px(14.))
                            .pb(px(4.))
                            .child(
                                h_flex()
                                    .w_full()
                                    .items_start()
                                    .flex_wrap()
                                    .gap(TILE_GAP)
                                    .children(self.attachments.iter().enumerate().map(
                                        |(index, attachment)| {
                                            self.attachment_tile(index, attachment, palette, cx)
                                        },
                                    )),
                            ),
                    )
                    .vertical_scrollbar(&self.attachments_scroll),
            );
        }
        Some(strip.into_any_element())
    }

    /// One attachment, as a tile: its thumbnail, the name under it, and the `×` that
    /// takes it off the message.
    ///
    /// The name is one line with an ellipsis (`truncate`) and the whole of it on hover,
    /// because a name is what a reader tells two files apart by and the tile is narrower
    /// than most names. The `×` is drawn always rather than under the pointer: it is the
    /// one way back from a file picked by mistake, and a control that is only there for a
    /// reader who already knows it is there is not a control.
    fn attachment_tile(
        &self,
        index: usize,
        attachment: &Attachment,
        palette: &'static Palette,
        cx: &Context<Self>,
    ) -> AnyElement {
        let id = attachment.id;
        let name = SharedString::from(attachment.name.clone());
        v_flex()
            .id(ElementId::from(format!("attachment-{index}")))
            .test_support()
            .aria_label(name.clone())
            .w(TILE)
            .flex_none()
            .gap(px(4.))
            .child(
                div()
                    .id(ElementId::from(format!("attachment-thumb-{index}")))
                    .test_support()
                    .relative()
                    .w(TILE)
                    .h(THUMB)
                    .flex_none()
                    // The frame, and the picture's own curve with it: a child is not
                    // clipped to a rounded parent (gpui's clip is the rectangle), so a
                    // picture left square paints its corners over the frame's curve —
                    // the same wedge the box's own children would leave on the box.
                    .rounded(THUMB_RADIUS)
                    .overflow_hidden()
                    .bg(paint::color(palette.input))
                    .child(self.thumbnail(attachment, palette))
                    .child(
                        div()
                            .id(ElementId::from(format!("attachment-remove-{index}")))
                            .test_support()
                            .aria_label(SharedString::from(format!("Remove {name}")))
                            // Over the picture, at the tile's top-right: a card of the
                            // input surface with the chips' own hairline, so the mark is
                            // legible on any picture in either mode.
                            .absolute()
                            .top(REMOVE_INSET)
                            .right(REMOVE_INSET)
                            .size(REMOVE)
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(f32::from(REMOVE) / 2.))
                            .bg(paint::color(palette.input))
                            .border_1()
                            .border_color(paint::color(palette.border))
                            .hover(move |mark| mark.border_color(paint::color(palette.muted_fg)))
                            .tooltip(|window, cx| {
                                widgets::tooltip::text(
                                    "attachment-remove-tooltip",
                                    "Remove",
                                    px(240.),
                                    window,
                                    cx,
                                )
                            })
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.remove_attachment(id, cx)),
                            )
                            .child(
                                Icon::new(IconName::Close)
                                    .size(px(10.))
                                    .text_color(paint::color(palette.fg)),
                            ),
                    ),
            )
            .child(
                div()
                    .id(ElementId::from(format!("attachment-name-{index}")))
                    .test_support()
                    .w_full()
                    .min_w_0()
                    .truncate()
                    .h(TILE_NAME)
                    .text_size(DETAIL_FONT)
                    .text_color(paint::color(palette.muted_fg))
                    .tooltip({
                        let name = name.clone();
                        move |window, cx| {
                            widgets::tooltip::text(
                                ElementId::from(format!("attachment-name-tooltip-{index}")),
                                name.clone(),
                                px(320.),
                                window,
                                cx,
                            )
                        }
                    })
                    .child(name),
            )
            .into_any_element()
    }

    /// A tile's picture: the image itself, scaled to fit the tile's box, or the file's
    /// own glyph when the attachment is not an image.
    ///
    /// The image is drawn with `Contain` over the frame's whole box, so a picture of any
    /// shape is shown whole — a thumbnail says which file this is, and a crop of a
    /// screenshot can hide the part that does. The curve is the frame's
    /// ([`THUMB_RADIUS`]), carried by the picture itself.
    fn thumbnail(&self, attachment: &Attachment, palette: &'static Palette) -> AnyElement {
        let source = match &attachment.kind {
            // A pasted image's bytes, framed when they arrived: the frame is the one
            // gpui's own asset system decodes from, and re-making it here would copy the
            // bytes on every frame the box is drawn in.
            AttachmentKind::ImageBytes { .. } => self
                .pasted
                .get(&attachment.id)
                .map(|image| ImageSource::Image(image.clone())),
            AttachmentKind::ImageFile(path) => Some(ImageSource::from(path.clone())),
            AttachmentKind::File(_) => None,
        };
        match source {
            Some(source) => img(source)
                .size_full()
                .object_fit(ObjectFit::Contain)
                .rounded(THUMB_RADIUS)
                .into_any_element(),
            None => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Icon::new(IconName::File)
                        .size(px(24.))
                        .text_color(paint::color(palette.muted_fg)),
                )
                .into_any_element(),
        }
    }

    /// The drawer the model chip opened, folded out inside the box under the strips.
    fn drawer_panel(
        &self,
        palette: &'static Palette,
        window: &Window,
        cx: &Context<Self>,
        top: bool,
    ) -> Option<AnyElement> {
        if !self.model_open {
            return None;
        }
        let title: AnyElement = h_flex()
            .gap(px(6.))
            .child(SharedString::from(format!("{} model", self.name)))
            .into_any_element();
        let body: AnyElement = self.model_body(palette, window, cx);
        Some(
            v_flex()
                .id("composer-drawer")
                .test_support()
                .w_full()
                .flex_none()
                .bg(paint::color(palette.sidebar))
                // `top` when no strip is drawn above it: then the drawer's own corners
                // are the box's, and they take the box's curve.
                .when(top, |this| this.rounded_t(BOX_INNER_RADIUS))
                .border_b_1()
                .border_color(paint::color(palette.border))
                .child(
                    // The title row folds the drawer back, the way a strip's row does.
                    h_flex()
                        .id("drawer-row")
                        .test_support()
                        .h(DRAWER_ROW)
                        .w_full()
                        .items_center()
                        .gap(px(10.))
                        .pl(px(14.))
                        .pr(px(10.))
                        .cursor_default()
                        .text_size(STRIP_FONT)
                        .text_color(paint::color(palette.muted_fg))
                        .hover(move |row| row.text_color(paint::color(palette.fg)))
                        .on_click(cx.listener(|this, _, _, cx| this.close_drawer(cx)))
                        .child(
                            div()
                                .flex_none()
                                // `.drawer-title{font-weight:500}`.
                                .font_weight(widgets::text::MEDIUM)
                                .text_color(paint::color(palette.fg))
                                .child(title),
                        )
                        .child(
                            div()
                                .ml_auto()
                                .flex_none()
                                // The design's own toggle: the up chevron a strip
                                // turns to when it is open — a way back, not a
                                // way further out.
                                .child(Icon::new(IconName::ChevronUp).size(px(12.))),
                        ),
                )
                .child(body)
                .into_any_element(),
        )
    }

    /// How tall the popup stands, as its own drawing measures it: the shell's
    /// padding, the rows it shows at once, and the counter when the list is longer
    /// than that. What the placement decides with — a couple of pixels either way
    /// cannot change which side of the caret has the room.
    fn completion_height(&self) -> Pixels {
        let Some(popup) = self.popup.as_ref() else {
            return px(0.);
        };
        let shown = popup.rows.len().min(complete::MAX_ROWS) as f32;
        let counter = if popup.rows.len() > complete::MAX_ROWS {
            POPUP_COUNTER
        } else {
            0.
        };
        px(POPUP_PAD * 2. + POPUP_ROW * shown + counter)
    }

    /// The completion popup: the caret's word, and what would finish it (§7.3).
    ///
    /// A drawer's own language — the sidebar surface, one hairline, the box's
    /// radius and the warm shadow — but floating at the caret rather than folded
    /// out of the box: a word being typed does not move the box's own content
    /// one row up and down as suggestions come and go.
    fn completion_popup(
        &self,
        palette: &'static Palette,
        width: Pixels,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let popup = self.popup.as_ref()?;
        let hover = paint::color(paint::mix(palette.fg, ITEM_HOVER_MIX, palette.sidebar));
        let chosen_fill = paint::color(paint::mix(palette.fg, ITEM_CHOSEN_MIX, palette.sidebar));
        let weak = cx.entity().downgrade();

        let rows = popup.rows.iter().enumerate().map(|(index, candidate)| {
            let chosen = index == popup.index;
            let label = SharedString::from(popup.label(candidate));
            // The characters the word matched are drawn heavier, so a row says why it
            // is on the list: a `/lo` that offers `/reload` is a subsequence match, and
            // which letters it hit is the whole of the explanation.
            //
            // `/catalog`'s own `args_hint` is not drawn here, and today it is `null` for
            // every command evo registers (`evo-agent src/serve/catalog.lisp`): there is
            // nothing to put beside the label, and a client that invented one would be
            // writing the argument syntax evo owns.
            let highlights = complete::matched_ranges(&candidate.name, &popup.prefix)
                .into_iter()
                .map(|range| {
                    (
                        range,
                        HighlightStyle {
                            font_weight: Some(widgets::text::MEDIUM),
                            color: Some(paint::color(palette.fg)),
                            ..Default::default()
                        },
                    )
                })
                .collect::<Vec<_>>();
            let description = SharedString::from(candidate.description.clone());
            let mut row = h_flex()
                .id(ElementId::from(format!("completion-row-{index}")))
                .test_support()
                // The row as a reader that cannot see it hears it: the label, then
                // the line beside it — `{label} · {description}` on one line, the way
                // the drawer names a model.
                .aria_label(SharedString::from(format!("{label} · {description}")))
                .flex_none()
                .h(px(POPUP_ROW))
                .w_full()
                .items_center()
                .gap(px(POPUP_ROW_GAP))
                .px(px(POPUP_ROW_PAD))
                .rounded(DRAWER_ITEM_RADIUS)
                .text_size(POPUP_FONT)
                .text_color(paint::color(palette.fg))
                .cursor_default()
                // `.drawer-item.chosen` is written after `.drawer-item:hover`: the
                // chosen row keeps its own fill under the pointer.
                .when(!chosen, |row| row.hover(move |row| row.bg(hover)))
                // The label's own box is what the popup is placed by: its left edge is
                // the x the typed word begins at.
                .child(
                    div()
                        .id(ElementId::from(format!("completion-label-{index}")))
                        .test_support()
                        .flex_none()
                        // A symbol is Lisp being typed into the image: code, drawn in
                        // the theme's monospace face as an editor draws its completions.
                        // A command is a word of the UI, in the UI's own face.
                        .when(popup.kind == CompletionKind::Symbol, |label| {
                            label.font_family(cx.theme().mono_font_family.clone())
                        })
                        .child(StyledText::new(label).with_highlights(highlights)),
                )
                .child(
                    // `flex_1` rather than its content width: the popup is as wide as
                    // its widest row, so this one has exactly its own room — and the
                    // room past the popup's ceiling is where a description truncates.
                    div()
                        .id(ElementId::from(format!("completion-description-{index}")))
                        .test_support()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(paint::color(palette.muted_fg))
                        .child(description),
                );
            // A press is a choice: the row the pointer is on is the row taken.
            let weak = weak.clone();
            row = row.on_click(move |_, window, cx| {
                if let Some(composer) = weak.upgrade() {
                    composer.update(cx, |this, cx| {
                        if let Some(popup) = this.popup.as_mut() {
                            popup.index = index;
                        }
                        this.accept(window, cx);
                    });
                }
            });
            if chosen {
                row = row.bg(chosen_fill);
            }
            row
        });

        let list = v_flex()
            .id("completion-list")
            .test_support()
            .w_full()
            .max_h(POPUP_LIST_MAX)
            .overflow_y_scroll()
            .track_scroll(&self.popup_scroll)
            .children(rows);

        // The TUI's own overflow line: without it a list longer than the popup
        // shows gives no sign that it is longer. The index is the reader's, one
        // based, and the count is how many there are.
        let counter = (popup.rows.len() > complete::MAX_ROWS).then(|| {
            h_flex()
                .id("completion-counter")
                .test_support()
                .flex_none()
                .h(px(POPUP_COUNTER))
                .w_full()
                .items_center()
                .gap(px(POPUP_ROW_GAP))
                .px(px(POPUP_ROW_PAD))
                .border_t_1()
                .border_color(paint::faded(palette.border, 0.7))
                .text_size(POPUP_FONT)
                .text_color(paint::color(palette.muted_fg))
                .child(SharedString::from(format!(
                    "… {}/{}",
                    popup.index + 1,
                    popup.rows.len()
                )))
        });

        Some(
            v_flex()
                .id("completion-popup")
                .test_support()
                .w(width)
                .flex_none()
                .p(px(POPUP_PAD))
                .rounded(POPUP_RADIUS)
                .border(BOX_BORDER)
                .border_color(paint::color(palette.border))
                .bg(paint::color(palette.sidebar))
                .shadow(vec![BoxShadow::new(
                    px(0.),
                    px(2.),
                    paint::wash(SHADOW_INK, 8.),
                )
                .blur_radius(px(6.))])
                .child(list)
                .children(counter)
                .into_any_element(),
        )
    }

    /// How wide the popup stands: the widest row of the *whole* list — every
    /// candidate, not only the rows it is showing, so walking it never resizes it —
    /// measured in the size and weight the rows are drawn in, between
    /// [`POPUP_MIN_W`] and [`POPUP_MAX_W`].
    ///
    /// Measured once per list, when the rows are new: a row cannot change without the
    /// popup being rebuilt, and text shaping is not work to repeat every frame.
    fn measure_popup(&mut self, window: &Window, cx: &App) -> Pixels {
        let Some(popup) = self.popup.as_ref() else {
            return px(POPUP_MIN_W);
        };
        let mono =
            (popup.kind == CompletionKind::Symbol).then(|| cx.theme().mono_font_family.clone());
        let widest = popup
            .rows
            .iter()
            .map(|candidate| {
                // The label is measured in the heavier face it is drawn in where the
                // word matched — the widest it can be — and the description as it is.
                text_width_in(
                    &popup.label(candidate),
                    mono.as_ref(),
                    POPUP_FONT,
                    widgets::text::MEDIUM,
                    window,
                ) + px(POPUP_ROW_GAP)
                    + text_width(
                        &candidate.description,
                        POPUP_FONT,
                        FontWeight::default(),
                        window,
                    )
            })
            .fold(px(0.), |widest, row| widest.max(row));
        // A pixel of slack: what is measured must fit in what is drawn, and the
        // measurement and the layout are two different shapes of the same text.
        let width = widest + px(2. * POPUP_LABEL_INSET + 1.);
        width.clamp(px(POPUP_MIN_W), px(POPUP_MAX_W))
    }

    /// Where the caret's word begins, as the box the popup is placed by: the x its
    /// rows line their labels up with, and the line they stand over.
    ///
    /// The word's own beginning, not the caret: a caret at the end of `/ev` would put
    /// the list two characters to the right of what it completes.
    fn word_bounds(&self, cx: &App) -> Option<Bounds<Pixels>> {
        let word = self.popup.as_ref()?.word.clone();
        let input = self.input.read(cx);
        input
            .range_to_bounds(&(word.start..word.start))
            // Nothing laid out yet, or the word is scrolled out of sight: the caret's
            // own line is still the line to stand over.
            .or_else(|| {
                input.cursor_layout().map(|(caret, line_height)| {
                    Bounds::new(caret.origin, gpui_kit::size(caret.size.width, line_height))
                })
            })
    }

    /// The model drawer: the catalog's models, and the effort ladder under them.
    ///
    /// For a lane the list states what that lane runs and nothing here clicks: a
    /// lane's model is the swarm's, fixed when the swarm starts, so the row says so
    /// rather than offering a change the server would refuse.
    fn model_body(
        &self,
        palette: &'static Palette,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let chosen = self.agent.model.clone();
        let mut body = v_flex().w_full().px(px(6.)).pb(px(6.));
        if !self.settable {
            body = body.child(
                div()
                    .px(px(8.))
                    .pb(px(6.))
                    .text_size(DETAIL_FONT)
                    .text_color(paint::color(palette.muted_fg))
                    .child(SharedString::from(format!(
                        "{} runs the swarm's lane model: evo fixes it when the swarm starts.",
                        self.name
                    ))),
            );
        }
        let settable = self.settable;
        let weak = cx.entity().downgrade();
        let items = self.models.iter().map(|model| {
            let is_chosen = chosen
                .as_ref()
                .is_some_and(|(id, provider)| *id == model.id && *provider == model.provider);
            let (id, provider) = (model.id.clone(), model.provider.clone());
            // The row's own words, as a reader that cannot see them hears them: the
            // registration, then the line beside it.
            let spoken = SharedString::from(format!(
                "{} · {} {}",
                model.provider,
                model.id,
                model.reason.as_ref().unwrap_or(&model.detail)
            ));
            let hover = paint::color(paint::mix(palette.fg, ITEM_HOVER_MIX, palette.sidebar));
            let active = paint::color(paint::mix(palette.fg, ITEM_CHOSEN_MIX, palette.sidebar));
            let mut row = h_flex()
                .id(ElementId::from(format!("drawer-model-{}", model.id)))
                .test_support()
                .aria_label(spoken)
                .h(DRAWER_ITEM)
                .w_full()
                .items_center()
                .gap(px(12.))
                .px(px(8.))
                .rounded(DRAWER_ITEM_RADIUS)
                .text_size(ITEM_FONT)
                .text_color(paint::color(palette.fg))
                .child(
                    h_flex()
                        .flex_none()
                        .child(
                            div()
                                .text_color(paint::color(palette.muted_fg))
                                // `<span class="drawer-dim">{provider} ·</span> {id}`:
                                // the design writes the name as one line of text — a
                                // space either side of the dot — so the space after it
                                // is the font's own, not a gap of the app's.
                                .child(format!("{} · ", model.provider)),
                        )
                        .child(SharedString::from(model.id.clone())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(DETAIL_FONT)
                        .text_color(paint::color(palette.muted_fg))
                        // A registration evo cannot reach is listed with why not, in
                        // evo's own words, in the detail slot's place: the context
                        // window is not the fact that matters about a model that
                        // could not be set.
                        .child(SharedString::from(
                            model.reason.clone().unwrap_or_else(|| model.detail.clone()),
                        )),
                )
                .child(div().w(px(14.)).flex_none().child(if is_chosen {
                    Icon::new(IconName::Check).size(px(14.)).into_any_element()
                } else {
                    div().into_any_element()
                }));
            if settable {
                let weak = weak.clone();
                let (id, provider) = (id.clone(), provider.clone());
                row = row
                    .cursor_default()
                    // `.drawer-item.chosen` is written after `.drawer-item:hover`, so
                    // the ticked row keeps its own fill under the pointer.
                    .when(!is_chosen, |row| row.hover(move |row| row.bg(hover)))
                    .on_click(move |_, _, cx| {
                        if let Some(composer) = weak.upgrade() {
                            composer.update(cx, |this, cx| this.choose_model(&id, &provider, cx));
                        }
                    });
            }
            if is_chosen {
                row = row.bg(active);
            }
            row
        });
        // A catalog longer than the drawer may draw: the models scroll inside a
        // region exactly `MODEL_LIST_ROWS` rows tall, in the host-and-bar shape the
        // todo list and the goal's objective use — the host holds the height and
        // the bar, the box inside it scrolls, and the handle is the composer's, so
        // two tabs' drawers do not share a position. The title row above and the
        // effort below are outside it and stay where they are.
        body = if self.models.len() > MODEL_LIST_ROWS {
            body.child(
                div()
                    .id(("drawer-models-host", cx.entity_id()))
                    .relative()
                    .w_full()
                    .flex_none()
                    .child(
                        div()
                            .id("drawer-models")
                            .test_support()
                            .w_full()
                            .h(DRAWER_ITEM * MODEL_LIST_ROWS)
                            .overflow_y_scroll()
                            .track_scroll(&self.models_scroll)
                            .children(items),
                    )
                    .vertical_scrollbar(&self.models_scroll),
            )
        } else {
            body.children(items)
        };
        body.child(self.effort_row(palette, window, cx))
            .into_any_element()
    }

    /// The rungs the drawer's effort row offers, and whose they are.
    ///
    /// A ladder is the **model's** (`/catalog.models[].effort_levels`): a provider offers
    /// `low, high, max`, another the whole run, and a model with no effort parameter at
    /// all offers none. The session's own `thinking_levels` is what stands when no model
    /// is chosen, or when the catalog names no levels for the chosen one — which is a
    /// catalog written before evo published them.
    fn drawer_levels(&self) -> DrawerLevels {
        if let Some((id, provider)) = self.agent.model.as_ref() {
            if let Some(model) = self
                .models
                .iter()
                .find(|model| &model.id == id && &model.provider == provider)
            {
                return if model.effort_levels.is_empty() {
                    // The catalog answers for this registration, and its answer is that
                    // it has no effort setting: the row says so rather than offering the
                    // session's ladder for a model that would clamp it.
                    DrawerLevels::None
                } else {
                    DrawerLevels::Rungs(
                        model
                            .effort_levels
                            .iter()
                            .cloned()
                            .map(SharedString::from)
                            .collect(),
                    )
                };
            }
        }
        DrawerLevels::Rungs(self.levels.clone())
    }

    /// The effort row: the level's name, and the rail that changes it.
    fn effort_row(
        &self,
        palette: &'static Palette,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let level = self.agent.thinking.clone().unwrap_or_default();
        let levels = self.drawer_levels();
        let stated = match &levels {
            // The model takes no effort setting: the row keeps its place — a control
            // that came and went with every model would move the drawer under the
            // reader — and says what the model is instead of drawing rungs for it.
            DrawerLevels::None => SharedString::from(NO_EFFORT),
            _ => SharedString::from(level.clone()),
        };
        let row = h_flex()
            .id("drawer-effort")
            .test_support()
            // The row as a reader that cannot see it hears it: the level it states, or
            // that the model has none to state.
            .aria_label(SharedString::from(format!("Effort {stated}")))
            .h(DRAWER_EFFORT_ROW)
            .w_full()
            .items_center()
            .gap(px(16.))
            .px(px(8.))
            .mt_1()
            .border_t_1()
            .border_color(paint::faded(palette.border, 0.7))
            .text_size(ITEM_FONT)
            .child(
                h_flex()
                    .flex_none()
                    .min_w(DRAWER_LABEL_MIN)
                    // `Effort <span class="drawer-dim">medium</span>`: the level is
                    // the dim half, as the model chip's effort is.
                    .gap(px(4.))
                    .child("Effort")
                    .child(
                        div()
                            .id("drawer-effort-level")
                            .test_support()
                            .text_color(paint::color(palette.muted_fg))
                            .child(stated),
                    ),
            );
        let DrawerLevels::Rungs(levels) = levels else {
            // The model's own answer: no rungs to offer.
            return row.into_any_element();
        };
        if levels.is_empty() {
            // The server published no ladder: say what the agent runs and change
            // nothing, rather than draw rungs a client made up.
            return row.into_any_element();
        }
        if !self.settable {
            // A lane's effort belongs to the swarm, as its model does.
            return row.into_any_element();
        }
        let index = levels
            .iter()
            .position(|name| name.as_str() == level)
            .unwrap_or(0);
        let weak = cx.entity().downgrade();
        let ladder = levels.clone();
        row.child(
            div()
                .id("composer-effort")
                .test_support()
                .w(px(200.))
                .flex_none()
                .ml_auto()
                .child(
                    EffortSlider::with_levels(
                        "composer-effort-rail",
                        levels.iter().cloned(),
                        index,
                        self.effort_motion.clone(),
                    )
                    .palette(palette)
                    .focus(self.effort_focus.clone())
                    .on_change(move |level: usize, _, cx: &mut App| {
                        if let Some(composer) = weak.upgrade() {
                            composer.update(cx, |this, cx| {
                                // The rungs the rail was drawn with are the ones its own
                                // press means: the model's ladder, not the session's.
                                if let Some(name) = ladder.get(level).map(|name| name.to_string()) {
                                    this.choose_effort(&name, cx);
                                }
                            });
                        }
                    })
                    .render(window),
                ),
        )
        .into_any_element()
    }

    /// Send `model.set` for a model the drawer picked.
    fn choose_model(&mut self, id: &str, provider: &str, cx: &mut Context<Self>) {
        if !self.settable || self.in_flight {
            return;
        }
        if self
            .agent
            .model
            .as_ref()
            .is_some_and(|(chosen, chosen_provider)| chosen == id && chosen_provider == provider)
        {
            return;
        }
        self.in_flight = true;
        cx.emit(ComposerEvent::ModelSet {
            id: id.to_string(),
            provider: provider.to_string(),
        });
        cx.notify();
    }

    /// Send `thinking.set` for a rung the slider picked.
    fn choose_effort(&mut self, level: &str, cx: &mut Context<Self>) {
        if !self.settable || self.in_flight {
            return;
        }
        if self.agent.thinking.as_deref() == Some(level) {
            return;
        }
        self.in_flight = true;
        cx.emit(ComposerEvent::ThinkingSet(level.to_string()));
        cx.notify();
    }

    /// The foot row: the chips that state what the agent is working with, and the one
    /// action button at the end of them.
    fn foot(&self, palette: &'static Palette, cx: &Context<Self>) -> AnyElement {
        let mut row = h_flex()
            .id("composer-foot")
            .test_support()
            .w_full()
            .items_center()
            .gap(FOOT_GAP)
            .pt(px(FOOT_PAD.0))
            .pr(px(FOOT_PAD.1))
            .pb(px(FOOT_PAD.2))
            .pl(px(FOOT_PAD.3));
        let open = self.model_open;
        let weak = cx.entity().downgrade();
        for chip in &self.agent.chips {
            // The slot is the composer's: it carries the chip's own words as its
            // accessible name (the widget draws the pill, which states no name of its
            // own), and it is what a test addresses the chip by.
            let label = match &chip.dim {
                Some(dim) => format!("{} {dim}", chip.text),
                None => chip.text.to_string(),
            };
            let mut slot = div()
                .id(ElementId::from(format!("composer-chip-{}", chip.name)))
                .test_support()
                .flex_none()
                .aria_label(label);
            let mut pill = Chip::new(chip.name.clone(), chip.text.clone()).palette(palette);
            if let Some(dim) = chip.dim.clone() {
                pill = pill.dim(dim);
            }
            if chip.opens {
                let weak = weak.clone();
                pill = pill.open(open).interactive(move |_, cx| {
                    // A chip is a button: clicking it while the drawer it opens is
                    // folded out folds the drawer back, and otherwise opens it.
                    if let Some(composer) = weak.upgrade() {
                        composer.update(cx, |this, cx| {
                            if this.model_open {
                                this.close_drawer(cx);
                            } else {
                                this.open_drawer(cx);
                            }
                        });
                    }
                });
            }
            slot = slot.child(pill.render());
            row = row.child(slot);
        }
        row.child(div().flex_1())
            // The `+`, immediately left of the one action button, and nothing between
            // them but the row's own gap.
            .child(self.attach_button(palette, cx))
            .child(self.action_button(cx))
            .into_any_element()
    }

    /// The `+` of the foot row: the chip's own language — a 26px pill one step of the
    /// ink into the input surface, its glyph in the ink — instead of a second action
    /// button, because it is not the row's action: what it opens is a dialog, and what it
    /// makes is an attachment the action then carries.
    ///
    /// The design has no `+` to copy (`doc.tsx`'s `.composer-foot` is the chips and the
    /// one button), so it is drawn as what the row already has: the chips' height, their
    /// pill, their three fills, and their rest ink — with the muted spelling of it, since
    /// the `+` is a way in rather than a fact.
    fn attach_button(&self, palette: &'static Palette, cx: &Context<Self>) -> AnyElement {
        let hover = widgets::chip::fill(palette, true, false);
        div()
            .id(ATTACH_ID)
            .test_support()
            .flex_none()
            .aria_label("Add attachments")
            .flex()
            .items_center()
            .justify_center()
            .h(px(widgets::chip::HEIGHT))
            .px(px(widgets::chip::PAD))
            .rounded(px(widgets::chip::RADIUS))
            .bg(widgets::chip::fill(palette, false, false))
            .text_color(paint::color(palette.muted_fg))
            // The row's controls keep the arrow the window draws
            // (`.chip{cursor:default}`), and this one is one of them.
            .cursor_default()
            .hover(move |button| button.bg(hover).text_color(paint::color(palette.fg)))
            .tooltip(|window, cx| {
                widgets::tooltip::text("attach-tooltip", "Add attachments", px(240.), window, cx)
            })
            .on_click(cx.listener(|this, _, window, cx| this.pick_attachments(window, cx)))
            .child(Icon::new(IconName::Plus).size(px(14.)))
            .into_any_element()
    }

    fn action_button(&self, cx: &Context<Self>) -> impl IntoElement {
        let face = self.face();
        Button::new(BUTTON_ID)
            .h(ACTION_HEIGHT)
            .px(px(12.))
            .rounded(ACTION_RADIUS)
            .text_size(px(13.))
            // The design's label is text — `↑ Send`, glyph and all — so there is no
            // icon element beside the word and no gap the design does not have. What a
            // reader hears is still the word: the glyph is decoration.
            .label(face.label())
            .accessibility_label(face.name())
            // A request of this composer's own in flight is the only thing that greys
            // it. An empty draft is not a disabled button: the design draws
            // `.composer-send` in the primary face at rest, and a blank draft simply
            // has nothing to send (or to stop), so the click and `Enter` do nothing.
            .disabled(self.is_action_disabled())
            .on_click(cx.listener(|this, _, _, cx| match this.face() {
                ActionFace::Send => {
                    let draft = this.input.read(cx).value().to_string();
                    this.send(draft, cx);
                }
                ActionFace::StopSwarm => {
                    this.in_flight = true;
                    cx.emit(ComposerEvent::StopSwarm);
                    cx.notify();
                }
                // The same click in a single-agent session: there is no swarm behind
                // this box, so the one thing to stop is the agent's own run — the
                // session-scoped interrupt, `run.interrupt{scope:session}` (§7.2).
                ActionFace::Stop => {
                    this.in_flight = true;
                    cx.emit(ComposerEvent::Interrupt);
                    cx.notify();
                }
            }))
            // One button, the design's own: primary in both faces, because it is the
            // same button with a different word on it (`.composer-send`). The design
            // writes no `:hover` for it, so the pointer does not repaint it; the kit's
            // primary face is the one part of that this button still wears — its
            // hover and pressed fills are the theme's own steps — and it keeps the
            // disabled face the in-flight state is shown with.
            .primary()
    }
}

/// How many of the input's rows fit in `room`, with the design's floor.
fn rows_for(room: Pixels) -> usize {
    // A row of this input is one line of its own type (`INPUT_LINE`): the kit grows
    // in rows, and its row is the line the text is set on. The design measures
    // pixels — `height = min(scrollHeight, pane / 2)` includes the textarea's own
    // padding — so the wrapper's padding comes off the room before the rows are
    // counted, and the cap is the pane's half as the design means it.
    let text = f32::from(room) - INPUT_PAD.0 - INPUT_PAD.2;
    ((text / f32::from(INPUT_LINE)).floor() as usize).max(MIN_ROWS)
}

/// One todo: its 14px box, and its text.
fn todo_row(index: usize, todo: &Todo, palette: &'static Palette) -> AnyElement {
    let (ink, struck) = match todo.status {
        TodoStatus::Done => (palette.muted_fg, true),
        TodoStatus::InProgress => (palette.fg, false),
        TodoStatus::Pending => (palette.muted_fg, false),
    };
    let text = div()
        .min_w_0()
        .truncate()
        .text_color(paint::color(ink))
        .child(SharedString::from(todo.text.clone()));
    h_flex()
        .id(ElementId::from(format!("todo-{index}")))
        .test_support()
        .aria_label(SharedString::from(todo.text.clone()))
        .h(TODO_ROW)
        .w_full()
        .items_center()
        .gap(px(8.))
        .text_size(ITEM_FONT)
        .child(todo_box(todo.status, palette))
        .child(if struck {
            text.line_through()
                .text_decoration_color(paint::faded(palette.muted_fg, 0.55))
                .into_any_element()
        } else {
            text.into_any_element()
        })
        .into_any_element()
}

/// The todo's box: one 14px frame for every state — filled with a tick when done, a
/// smaller square inside while in progress, empty when pending (`.todo-box`).
fn todo_box(status: TodoStatus, palette: &'static Palette) -> AnyElement {
    let ink = match status {
        TodoStatus::Done => palette.muted_fg,
        TodoStatus::InProgress => palette.fg,
        TodoStatus::Pending => palette.muted_fg,
    };
    let frame = match status {
        TodoStatus::Pending => paint::faded(ink, 0.7),
        _ => paint::color(ink),
    };
    let mut box_ = div()
        .size(TODO_BOX)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(TODO_BOX_RADIUS)
        .border(px(1.2))
        .border_color(frame);
    match status {
        TodoStatus::Done => {
            box_ = box_.bg(paint::color(ink)).child(
                Icon::new(IconName::Check)
                    .size(px(10.))
                    .text_color(paint::color(palette.sidebar)),
            );
        }
        TodoStatus::InProgress => {
            box_ = box_.child(
                div()
                    .size(TODO_BOX_INSET)
                    .rounded(px(1.5))
                    .bg(paint::color(ink)),
            );
        }
        TodoStatus::Pending => {}
    }
    box_.into_any_element()
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = design::palette(cx.theme().mode.is_dark());

        // The box's placeholder follows the program (§7.2). It is set here rather than
        // where the program arrives because an input state's placeholder is fixed when
        // it is built — before anyone knows which program the tab will run — and this is
        // the first moment there is a window to set it with. The comparison is what keeps
        // it to the once: after this frame the input already says the right thing.
        let placeholder = self.placeholder();
        if self.input.read(cx).presentation().placeholder().as_ref() != placeholder {
            self.input.update(cx, |input, cx| {
                input.set_placeholder(placeholder, window, cx)
            });
        }

        // The caret is what "focused" means here: the ring belongs to the box, which
        // the input does not own.
        let focused = self
            .input
            .read(cx)
            .presentation()
            .focus_handle()
            .is_focused(window);
        let border = if focused {
            paint::color(paint::mix(
                palette.primary,
                FOCUS_BORDER_MIX,
                palette.border,
            ))
        } else {
            paint::color(palette.border)
        };
        let mut shadows =
            vec![BoxShadow::new(px(0.), px(1.), paint::wash(SHADOW_INK, 5.)).blur_radius(px(2.))];
        if focused {
            shadows.push(
                BoxShadow::new(px(0.), px(0.), paint::wash(palette.primary, RING_MIX))
                    .spread_radius(RING),
            );
        }

        // The strips stand over the input in the box's own order: the goal first, then
        // the plan, then what the draft carries, then the drawer a chip folded out. The
        // first of them is the box's first child and takes the box's top corners.
        let goal = self.goal_strip(palette, cx, true);
        let strip = self.todo_strip(palette, cx, goal.is_none());
        let attach = self.attachments_strip(palette, cx, goal.is_none() && strip.is_none());
        let drawer = self.drawer_panel(
            palette,
            window,
            cx,
            goal.is_none() && strip.is_none() && attach.is_none(),
        );
        let foot = self.foot(palette, cx);
        // With neither strip nor drawer open the input's wrapper is the box's first
        // child: its own top corners are the box's, and it is the only child painting
        // there.
        let body_at_top = goal.is_none() && strip.is_none() && attach.is_none() && drawer.is_none();

        // The completion popup floats at the caret, out of the box's own clipping: a
        // deferred draw is painted after its ancestors, so neither the box's fold nor
        // the transcript under it can cut it. Its *layout* is still the box's, which
        // is why the anchored element it draws is positioned absolutely — taken out
        // of the flow, it moves nothing.
        // The popup floats over the caret's *word* and is as wide as its own rows,
        // and both are decided here, where the window and the input's own layout are.
        // A deferred draw is painted after its ancestors, so neither the box's fold nor
        // the transcript under it can cut it; its layout is still the box's, which is
        // why the anchored element it draws is positioned absolutely — out of the flow,
        // it moves nothing.
        let popup = if self.popup.is_some() {
            let width = match self.popup_width {
                Some(width) => width,
                None => {
                    let width = self.measure_popup(window, cx);
                    self.popup_width = Some(width);
                    width
                }
            };
            // Never wider than the window it floats in, never hanging off either side.
            let window_width = window.bounds().size.width;
            let width = width.min(window_width - px(2. * POPUP_MARGIN));
            // The word's own beginning, as the input last laid it out; failing that —
            // an input not yet laid out, a word scrolled out of the box — the box's own
            // left edge, on the first line of it.
            let (word_left, top, bottom) = match self.word_bounds(cx) {
                Some(bounds) => (bounds.left(), bounds.top(), bounds.bottom()),
                None => {
                    let bounds = self.input.read(cx).input_bounds();
                    let line = bounds.top() + INPUT_LINE;
                    (bounds.left(), line, line)
                }
            };
            // The popup is placed by its rows' *labels*: every label starts at the x
            // the typed word does, so the list reads as an expansion of the word rather
            // than a box that happens to be near it.
            let left = (word_left - px(POPUP_LABEL_INSET))
                .max(px(POPUP_MARGIN))
                .min(window_width - width - px(POPUP_MARGIN));
            // Over the word's own line, a gap off it — where the room is: under it when
            // the window runs out above, as it does for a box at the top of one.
            let (anchor, at) = if top - px(POPUP_GAP) - self.completion_height() >= px(0.) {
                (Anchor::BottomLeft, point(left, top - px(POPUP_GAP)))
            } else {
                (Anchor::TopLeft, point(left, bottom + px(POPUP_GAP)))
            };
            let popup = self
                .completion_popup(palette, width, cx)
                .expect("the popup is up");
            Some(deferred(
                anchored().anchor(anchor).position(at).child(popup),
            ))
        } else {
            None
        };

        // Where the box was painted: the page asks this against the press it sees, so
        // that a press in the box and a press outside it are told apart by where they
        // landed rather than by who handled them first (the input, a chip and the
        // drawer all take their own presses).
        let measure = {
            let box_bounds = self.box_bounds.clone();
            move |painted: Vec<Bounds<Pixels>>, _window: &mut Window, _cx: &mut App| {
                if let Some(bounds) = painted.first().copied() {
                    box_bounds.set(bounds);
                }
            }
        };

        // The dock: the box sits on the transcript's reading measure, at the foot of
        // the conversation, and the page owns everything above it.
        div()
            .on_children_prepainted(measure)
            .id("composer")
            .test_support()
            .flex_none()
            .w_full()
            .flex()
            .justify_center()
            .px(px(INSET))
            .pt(px(4.))
            .pb(px(INSET))
            .child(
                v_flex()
                    .id("composer-box")
                    .test_support()
                    .w_full()
                    .max_w(px(MEASURE - 2. * INSET))
                    .rounded(BOX_RADIUS)
                    .border(BOX_BORDER)
                    .border_color(border)
                    .bg(paint::color(palette.input))
                    .shadow(shadows)
                    .overflow_hidden()
                    // Esc, with the caret anywhere in the box, is the coordinator's
                    // own interrupt (the TUI's).
                    .key_context(KEY_CONTEXT)
                    .on_action(cx.listener(Self::interrupt_action))
                    // Files dragged onto the box are attachments, the same as the
                    // picker's own answer: the third way a reader adds one, and the one
                    // that needs no dialog at all. Nothing else is dropped on: what a
                    // drag carries may be anything, and only paths are a file.
                    .can_drop(|dragged, _, _| dragged.is::<ExternalPaths>())
                    .on_drop(cx.listener(|this, paths: &ExternalPaths, _window, cx| {
                        this.attach(paths.0.iter().cloned().collect(), cx)
                    }))
                    .children(goal)
                    .children(strip)
                    .children(attach)
                    .children(drawer)
                    .child(
                        // Everything under the strip: the input and the foot, on the
                        // box's own surface, curved with the box's inner radius so the
                        // box's own fill reaches the border on every corner it meets. A
                        // child of the box is not clipped to the box's rounded bounds
                        // (gpui's clip is the rectangle), so a square corner here paints
                        // its own colour over the corner's wedge and the box reads as a
                        // square with a rounded hairline through it.
                        v_flex()
                            .w_full()
                            .flex_none()
                            .bg(paint::color(palette.input))
                            .rounded_b(BOX_INNER_RADIUS)
                            .when(body_at_top, |this| this.rounded_t(BOX_INNER_RADIUS))
                            .child(
                                // The input's own box: the design's padding, and the
                                // design's 62px floor under it (`INPUT_PAD`, `INPUT_MIN`).
                                // The kit grows the *rows* inside, so the padding stays
                                // outside them and a taller draft grows this wrapper with
                                // it.
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .pt(px(INPUT_PAD.0))
                                    .pr(px(INPUT_PAD.1))
                                    .pb(px(INPUT_PAD.2))
                                    .pl(px(INPUT_PAD.3))
                                    .min_h(INPUT_MIN)
                                    .child(
                                        // `XSmall`: the kit's own padding is the least
                                        // one it has, so the design's is not compounded
                                        // with it.
                                        Textarea::new(&self.input)
                                            .with_size(Size::XSmall)
                                            .appearance(false)
                                            .bordered(false)
                                            .text_size(INPUT_FONT)
                                            .line_height(INPUT_LINE)
                                            .w_full()
                                            .min_w_0(),
                                    ),
                            )
                            .child(foot),
                    )
                    // The popup, if the caret's word has one: the box's last child,
                    // drawn over everything the box holds.
                    .children(popup),
            )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::base::{TextView, TextViewState};
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::FocusHandle;
    use gpui_kit::{
        point, px, AnyWindowHandle, Bounds, EntityId, TestAppContext, WindowBounds, WindowOptions,
    };
    use std::cell::RefCell;
    use std::rc::Rc;

    /// The conversation pane the design draws the box in: the reading measure at its
    /// widest, so the box fills it exactly (`800 - 2 * 16`).
    const PANE: gpui_kit::Size<gpui_kit::Pixels> = gpui_kit::Size {
        width: px(MEASURE),
        height: px(720.),
    };

    /// The chip's element id, as the box names it.
    fn chip_id(name: &str) -> String {
        format!("composer-chip-{name}")
    }

    /// A topic state whose segments are the named ones, in this order, with the text a
    /// server would publish for each.
    fn state_with(names: &[&str]) -> TopicState {
        state_with_level(names, "high")
    }

    /// The same, with the effort the topic reports.
    fn state_with_level(names: &[&str], level: &str) -> TopicState {
        let segments: Vec<serde_json::Value> = names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let text = match *name {
                    "model" => "stub-a".to_string(),
                    "thinking" => level.to_string(),
                    "context" => "ctx 48k/936k (5%)".to_string(),
                    "cache_stats" => "97% cached".to_string(),
                    "goal" => "goal a1b2c3d4 (active) 12k/50k".to_string(),
                    other => other.to_string(),
                };
                serde_json::json!({
                    "name": name,
                    "order": index as i64,
                    "side": "left",
                    "text": text,
                    "data": {},
                })
            })
            .collect();
        TopicState::from_json(&serde_json::json!({
            "model": {"id": "stub-a", "provider": "openai", "ready": true},
            "thinking": level,
            "goal": {"goal_id": "a1b2c3d4", "objective": "ship the redesign",
                     "status": "active", "budget": 50000, "tokens": 12000},
            "todos": [
                {"text": "port the view model", "status": "done"},
                {"text": "trim the workspace", "status": "in_progress"},
                {"text": "re-take the screens", "status": "pending"},
            ],
            "segments": segments,
        }))
    }

    /// The models a catalog would list, one of them chosen already — and the three
    /// answers a catalog gives about effort: `stub-a` takes the session's whole ladder,
    /// `stub-b`'s provider offers two rungs of its own, and `stub-c` takes none at all.
    fn catalog_models() -> Vec<ModelRow> {
        vec![
            ModelRow {
                id: "stub-a".to_string(),
                provider: "openai".to_string(),
                detail: "200k ctx · vision · effort low, medium, high, xhigh, max".to_string(),
                effort_levels: catalog_levels(),
                reason: None,
            },
            ModelRow {
                id: "stub-b".to_string(),
                provider: "openai".to_string(),
                detail: "936k ctx · effort low, max".to_string(),
                effort_levels: rungs(&["low", "max"]),
                reason: None,
            },
            ModelRow {
                id: "stub-c".to_string(),
                provider: "openai".to_string(),
                detail: "1M ctx".to_string(),
                effort_levels: Vec::new(),
                reason: None,
            },
        ]
    }

    /// The rungs a provider's own ladder names.
    fn rungs(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    /// The ladder evo's own registration declares (CONTRACT §5.6).
    fn catalog_levels() -> Vec<String> {
        ["low", "medium", "high", "xhigh", "max"]
            .iter()
            .map(|level| level.to_string())
            .collect()
    }

    /// The commands the server's registry lists, in its own order (§5.6) — the
    /// popup's candidates, never the client's.
    fn catalog_commands() -> Vec<Candidate> {
        [
            ("help", "commands and keys"),
            ("lore", "durable guidance"),
            ("memory", "what is remembered"),
            ("reload", "reload the image's own code"),
            ("eval", "evaluate one form in the live image"),
        ]
        .iter()
        .map(|(name, description)| Candidate {
            name: name.to_string(),
            description: description.to_string(),
        })
        .collect()
    }

    /// A catalog longer than the drawer draws: ten registrations, `stub-a` first.
    fn many_models() -> Vec<ModelRow> {
        (0..10u8)
            .map(|n| ModelRow {
                id: format!("stub-{}", char::from(b'a' + n)),
                provider: "openai".to_string(),
                detail: format!("{}k ctx", (n as u32 + 1) * 100),
                effort_levels: catalog_levels(),
                reason: None,
            })
            .collect()
    }

    /// The same topic, running another of the catalog's registrations: what the
    /// drawer ticks.
    fn state_running(id: &str) -> TopicState {
        TopicState::from_json(&serde_json::json!({
            "model": {"id": id, "provider": "openai", "ready": true},
            "thinking": "high",
            "segments": [
                {"name": "model", "order": 100, "side": "left", "text": id, "data": {}},
                {"name": "thinking", "order": 200, "side": "left", "text": "high",
                 "data": {}},
            ],
        }))
    }

    struct Fixture {
        window: AnyWindowHandle,
        composer: Entity<Composer>,
        events: Rc<RefCell<Vec<ComposerEvent>>>,
        /// Kept alive: dropping it would stop the recording.
        _subscription: Subscription,
    }

    impl Fixture {
        fn events(&self) -> Vec<ComposerEvent> {
            self.events.borrow().clone()
        }

        fn draft(&self, cx: &App) -> String {
            self.composer.read(cx).input.read(cx).value().to_string()
        }

        fn draft_now(&self, cx: &TestAppContext) -> String {
            cx.read(|cx| self.draft(cx))
        }

        fn input_frame(&self, cx: &App) -> (&'static str, EntityId) {
            ("input", self.composer.read(cx).input.entity_id())
        }

        /// Answer the `+` with these paths, as the platform's dialog would: the stand-in
        /// these tests inject so that no dialog is ever opened where nobody could answer
        /// it.
        fn use_picker(&self, paths: Vec<PathBuf>, cx: &mut App) {
            let picker: PathPicker = Arc::new(move |_, _| Task::ready(Some(paths.clone())));
            self.composer
                .update(cx, |composer, cx| composer.set_picker(picker, cx));
        }

        /// Shape the picker's answer, press the `+`, and let the answer land: the whole of
        /// what a reader does to attach files.
        fn pick_files(&self, paths: Vec<PathBuf>, cx: &mut TestAppContext) {
            cx.update(|cx| self.use_picker(paths, cx));
            self.act(cx, |window, cx| {
                window.render_frame(cx);
                window.click(ATTACH_ID, cx);
            });
            cx.run_until_parked();
        }

        /// What the draft is carrying, as the box holds it.
        fn attached(&self, cx: &TestAppContext) -> Vec<Attachment> {
            cx.read(|cx| self.composer.read(cx).attachments().to_vec())
        }

        fn busy(&self, busy: bool, cx: &mut App) {
            self.composer
                .update(cx, |composer, cx| composer.set_swarm_busy(busy, cx));
        }

        /// The selected agent's own state, as the page hands it in.
        fn set_agent(&self, state: &TopicState, cx: &mut App) {
            self.composer.update(cx, |composer, cx| {
                composer.set_agent(state, "Coordinator", true, cx)
            });
        }

        /// The registry's commands, as the catalog hands them in: the popup's own
        /// candidates, and the width they give it.
        fn set_commands(&self, commands: Vec<Candidate>, cx: &mut App) {
            self.composer.update(cx, |composer, cx| {
                composer.set_catalog(catalog_levels(), catalog_models(), commands, cx)
            });
        }

        /// The server's catalog: the effort ladder, the models the drawer offers,
        /// and the commands a `/word` completes against.
        fn set_catalog(&self, cx: &mut App) {
            self.composer.update(cx, |composer, cx| {
                composer.set_catalog(catalog_levels(), catalog_models(), catalog_commands(), cx)
            });
        }

        /// Focus the input the way a user does, then type into it.
        fn type_draft(&self, text: &str, window: &mut Window, cx: &mut App) {
            window.click(self.input_frame(cx), cx);
            window.input(text, cx);
        }

        /// The tab's half of a completion, for the tests that are about the popup: let
        /// the debounce run so the composer asks, then answer the question it asked.
        ///
        /// The rule here is the *test's* stand-in for a server, and it is the plain one
        /// these tests are written in. What a server really answers — the `/eval`
        /// content's symbols, a path that is not a command, a caret in the middle of a
        /// word — is asserted in `session::completion` and against a real `evo-agent`;
        /// a test that is about one of those hands the composer its own [`Answer`].
        fn answer(&self, cx: &mut TestAppContext) {
            cx.executor().advance_clock(COMPLETE_DEBOUNCE);
            cx.run_until_parked();
            cx.update(|cx| {
                let Some(ComposerEvent::Complete { text, cursor }) = self.events().last().cloned()
                else {
                    return;
                };
                let answer = stand_in_answer(&text, cursor, &catalog_commands());
                self.composer.update(cx, |composer, cx| {
                    composer.set_completion(&text, cursor, answer, cx)
                });
            });
        }

        /// The composer's caret, as the input reports it (a byte offset).
        fn caret(&self, cx: &App) -> usize {
            self.composer.read(cx).input.read(cx).cursor()
        }

        /// The prompts this tab has sent, oldest first.
        fn history(&self, cx: &TestAppContext) -> Vec<String> {
            cx.read(|cx| self.composer.read(cx).history().to_vec())
        }

        /// Send what is in the input the way a reader does, and let the server
        /// take it.
        ///
        /// Each step is its own app update: the composer hears about `Enter`
        /// when the update it arrived in returns, and it reads the draft from
        /// the input then.
        fn send_prompt(&self, prompt: &str, cx: &mut TestAppContext) {
            self.act(cx, |window, cx| self.type_draft(prompt, window, cx));
            self.act(cx, |window, cx| window.press("enter", cx));
            self.act(cx, |window, cx| {
                self.composer.update(cx, |composer, cx| {
                    composer.request_finished(true, window, cx)
                });
            });
        }

        /// Replace the whole draft, as pasting over a selected draft does.
        fn set_draft(&self, text: &str, window: &mut Window, cx: &mut App) {
            self.composer.update(cx, |composer, cx| {
                composer
                    .input
                    .update(cx, |input, cx| input.set_value(text, window, cx));
            });
        }

        /// Run `f` against the composer's window.
        ///
        /// Entity events reach subscribers when the app update returns, so
        /// assertions on [`Fixture::events`] belong *after* this call.
        fn act<R>(&self, cx: &mut TestAppContext, f: impl FnOnce(&mut Window, &mut App) -> R) -> R {
            cx.update_window(self.window, |_, window, cx| f(window, cx))
                .expect("composer window")
        }
    }

    gpui_kit::actions!(composer_probe, [ProbeUp, ProbeCopy]);

    /// Something else in the window that claims ↑ and the copy shortcut for
    /// itself: a list row, a settings field, a transcript row.
    struct Probe {
        focus: FocusHandle,
        ups: usize,
        copies: usize,
    }

    impl Probe {
        fn new(cx: &mut Context<Self>) -> Self {
            Self {
                focus: cx.focus_handle(),
                ups: 0,
                copies: 0,
            }
        }
    }

    impl Render for Probe {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("probe")
                .test_support()
                .key_context("Probe")
                .track_focus(&self.focus)
                .on_action(cx.listener(|this, _: &ProbeUp, _, cx| {
                    this.ups += 1;
                    cx.notify();
                }))
                .on_action(cx.listener(|this, _: &ProbeCopy, _, cx| {
                    this.copies += 1;
                    cx.notify();
                }))
                .size_full()
        }
    }

    /// The composer, and something else that wants the same keys.
    struct Beside {
        probe: Entity<Probe>,
        composer: Entity<Composer>,
    }

    impl Render for Beside {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            v_flex()
                .size_full()
                .child(div().h(px(200.)).child(self.probe.clone()))
                .child(div().h(px(160.)).child(self.composer.clone()))
        }
    }

    /// The composer takes ↑ and the copy shortcut only while its caret is in it:
    /// the rest of the window keeps both (§7.3).
    #[gpui_kit::test]
    fn the_composer_takes_its_keys_only_while_its_caret_is_in_it(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (window, beside) = cx.update(|cx| {
            cx.bind_keys([
                KeyBinding::new("up", ProbeUp, Some("Probe")),
                KeyBinding::new("cmd-c", ProbeCopy, Some("Probe")),
            ]);
            gpui_kit::open_window(window_options(), cx, |window, cx| {
                cx.new(|cx| Beside {
                    probe: cx.new(Probe::new),
                    composer: cx.new(|cx| Composer::new(window, cx)),
                })
            })
            .expect("a window with a composer and a probe")
        });
        let (probe, composer) = beside.read_with(cx, |beside, _| {
            (beside.probe.clone(), beside.composer.clone())
        });

        // A prompt of this tab's, so a leaked ↑ would show up in the draft.
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click(("input", composer.read(cx).input.entity_id()), cx);
            window.input("an earlier prompt", cx);
        })
        .expect("the composer's window");
        cx.update_window(window, |_, window, cx| window.press("enter", cx))
            .expect("the composer's window");
        cx.update_window(window, |_, window, cx| {
            composer.update(cx, |composer, cx| {
                composer.request_finished(true, window, cx)
            })
        })
        .expect("the composer's window");
        assert_eq!(
            composer.read_with(cx, |composer, _| composer.history().len()),
            1,
            "the tab has one prompt to recall"
        );

        // With the focus elsewhere in the window, ↑ and ⌘C are that element's:
        // the composer neither answers them nor keeps them from answering.
        cx.update_window(window, |_, window, cx| {
            window.click("probe", cx);
            assert!(
                probe.read(cx).focus.is_focused(window),
                "the probe holds the focus"
            );
            window.press("up", cx);
            window.press("cmd-c", cx);
        })
        .expect("the probe's window");

        assert_eq!(
            probe.read_with(cx, |probe, _| probe.ups),
            1,
            "the probe's ↑"
        );
        assert_eq!(
            probe.read_with(cx, |probe, _| probe.copies),
            1,
            "the probe's copy"
        );
        assert_eq!(
            composer.read_with(cx, |composer, cx| composer
                .input
                .read(cx)
                .value()
                .to_string()),
            "",
            "and the composer recalled nothing over its own draft"
        );

        // With the caret back in the input the same keys are the composer's, and
        // the element that had them does not hear them.
        cx.update_window(window, |_, window, cx| {
            window.click(("input", composer.read(cx).input.entity_id()), cx);
            window.press("up", cx);
        })
        .expect("the composer's window");
        assert_eq!(
            composer.read_with(cx, |composer, cx| composer
                .input
                .read(cx)
                .value()
                .to_string()),
            "an earlier prompt",
            "the composer's ↑ recalls this tab's prompt"
        );
        assert_eq!(
            probe.read_with(cx, |probe, _| probe.ups),
            1,
            "and the probe's ↑ did not fire"
        );
    }

    /// A window the width of the composer's column (§7.3).
    fn window_options() -> WindowOptions {
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(0.), px(0.)),
                size: PANE,
            })),
            ..Default::default()
        }
    }

    fn open(cx: &mut TestAppContext) -> Fixture {
        cx.update(gpui_kit::init);
        let (window, composer) = cx.update(|cx| {
            gpui_kit::open_window(window_options(), cx, |window, cx| {
                cx.new(|cx| Composer::new(window, cx))
            })
            .expect("composer window")
        });

        let events = Rc::new(RefCell::new(Vec::new()));
        let recorded = events.clone();
        let subscription = cx.update(|cx| {
            cx.subscribe(&composer, move |_, event: &ComposerEvent, _| {
                recorded.borrow_mut().push(event.clone());
            })
        });

        Fixture {
            window,
            composer,
            events,
            _subscription: subscription,
        }
    }

    /// The design's box (`Workspace.css`'s `.composer-box`): the input, and the foot
    /// row of chips and the one button under it, inside one hairline on the
    /// transcript's reading measure.
    #[gpui_kit::test]
    fn the_box_holds_the_input_and_the_foot_row_on_the_reading_measure(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);

            let box_ = window.find("composer-box").bounds();
            let input = window.find(f.input_frame(cx));
            assert!(input.visible(), "the box holds the input");
            assert_eq!(input.label(), Some(PLACEHOLDER));
            assert!(
                input.bounds().top() >= box_.top() && input.bounds().bottom() <= box_.bottom(),
                "the input is inside the box: {:?} in {box_:?}",
                input.bounds()
            );

            // The box is on the reading measure, centred in the column: the design's
            // `max-width: calc(var(--measure) - 2 * var(--inset))`.
            assert_eq!(
                box_.size.width,
                px(MEASURE - 2. * INSET),
                "the box is the measure, less the page's own insets"
            );
            assert_eq!(box_.left(), px(INSET), "and it is centred on it: {box_:?}");

            // The foot row: the chips, then the button at the row's right-hand end,
            // inside the box.
            let button = window.find(BUTTON_ID).bounds();
            assert_eq!(button.size.height, ACTION_HEIGHT);
            assert_eq!(
                button.right(),
                box_.right() - px(FOOT_PAD.1) - BOX_BORDER,
                "the button ends on the foot's own inset, inside the box's hairline: \
                 {button:?} in {box_:?}"
            );
            assert!(
                button.top() >= input.bounds().bottom(),
                "the foot is under the input: input {:?}, button {button:?}",
                input.bounds()
            );
            assert!(button.bottom() <= box_.bottom(), "{button:?} vs {box_:?}");
        });
    }

    #[gpui_kit::test]
    fn enter_sends_the_draft_and_the_owner_clears_it_only_on_ok(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            f.type_draft("hello", window, cx);
            assert_eq!(f.draft(cx), "hello");
            window.press("enter", cx);
        });

        // The composer never clears itself on send.
        assert_eq!(f.events(), vec![ComposerEvent::Send("hello".into())]);
        assert_eq!(f.draft_now(cx), "hello");

        // A failed request keeps the draft, so the prompt is not lost.
        f.act(cx, |window, cx| {
            f.composer.update(cx, |composer, cx| {
                composer.request_finished(false, window, cx)
            })
        });
        assert_eq!(f.draft_now(cx), "hello");

        // The retry sends again; this time the server takes the text.
        f.act(cx, |window, cx| window.press("enter", cx));
        assert_eq!(f.events().len(), 2, "the retry sends the kept draft again");

        f.act(cx, |window, cx| {
            f.composer.update(cx, |composer, cx| {
                composer.request_finished(true, window, cx)
            })
        });
        assert_eq!(
            f.draft_now(cx),
            "",
            "an accepted send is what clears the input"
        );
        assert_eq!(
            f.events(),
            vec![
                ComposerEvent::Send("hello".into()),
                ComposerEvent::Send("hello".into())
            ]
        );
    }

    /// The design's own words: `.composer-send` holds one text run — `↑ Send`, the
    /// arrow a character at the button's 13px — and Stop reads the same way. What a
    /// reader hears is the word alone.
    #[test]
    fn each_face_reads_as_the_design_writes_it() {
        assert_eq!(ActionFace::Send.label(), "\u{2191} Send");
        assert_eq!(ActionFace::StopSwarm.label(), "\u{25a0} Stop swarm");
        assert_eq!(ActionFace::Send.name(), "Send");
        assert_eq!(ActionFace::StopSwarm.name(), "Stop swarm");
        // The single-agent face says what it stops: one agent's own run, not a swarm's.
        assert_eq!(ActionFace::Stop.label(), "\u{25a0} Stop");
        assert_eq!(ActionFace::Stop.name(), "Stop");
    }

    /// §7.2: a single-agent session's box. Busy, the button says `Stop` — there is no
    /// swarm behind it to stop — and the click is the session-scoped interrupt, which is
    /// the only scope an `evo-agent` server knows. The same click in a swarm's box stops
    /// the whole swarm, as it always has.
    #[gpui_kit::test]
    fn a_single_agents_box_stops_its_own_run(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            // A button reports the name a reader hears, not the glyph it leads with
            // (`each_face_reads_as_the_design_writes_it` is where the drawn label is
            // pinned).
            assert_eq!(
                window.find(BUTTON_ID).label(),
                Some(ActionFace::Send.name()),
                "nothing is going on"
            );

            f.composer.update(cx, |composer, cx| {
                composer.set_swarm(false, cx);
                composer.set_swarm_busy(true, cx);
            });
            window.render_frame(cx);
            assert_eq!(
                window.find(BUTTON_ID).label(),
                Some(ActionFace::Stop.name())
            );

            window.click(BUTTON_ID, cx);
        });
        assert_eq!(
            f.events(),
            vec![ComposerEvent::Interrupt],
            "the scope is the session's own"
        );
    }

    /// §7.2: the box's placeholder names the agent this session has — a swarm's
    /// coordinator, or the one agent, which the page calls `Main`. The input state is
    /// built before anyone knows which program the tab will run, so the box takes the
    /// words on with the program, at the frame that has a window to set them with.
    #[gpui_kit::test]
    fn the_box_names_the_program_it_belongs_to(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find(f.input_frame(cx)).label(),
                Some(PLACEHOLDER),
                "a swarm's box is addressed to its coordinator"
            );

            f.composer
                .update(cx, |composer, cx| composer.set_swarm(false, cx));
            window.render_frame(cx);
            assert_eq!(
                window.find(f.input_frame(cx)).label(),
                Some(PLACEHOLDER_AGENT),
                "one agent's box is addressed to the agent"
            );

            // And back: the swarm's own words are not lost on the way through.
            f.composer
                .update(cx, |composer, cx| composer.set_swarm(true, cx));
            window.render_frame(cx);
            assert_eq!(window.find(f.input_frame(cx)).label(), Some(PLACEHOLDER));
        });
    }

    #[gpui_kit::test]
    fn shift_enter_inserts_a_newline_instead_of_sending(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            f.type_draft("first", window, cx);
            window.press("shift-enter", cx);
            assert_eq!(f.draft(cx), "first\n");
        });

        assert!(f.events().is_empty(), "Shift+Enter must not send");
    }

    #[gpui_kit::test]
    fn escape_interrupts_and_leaves_the_draft_alone(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            f.type_draft("half a prompt", window, cx);
            f.busy(true, cx);
            window.press("escape", cx);
        });

        assert_eq!(f.events(), vec![ComposerEvent::Interrupt]);
        // `Esc` interrupts; it is not the input's "clear" escape.
        assert_eq!(f.draft_now(cx), "half a prompt");
    }

    #[gpui_kit::test]
    fn enter_sends_while_running_so_text_can_be_queued(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            f.type_draft("queued", window, cx);
            f.busy(true, cx);
            window.render_frame(cx);
            window.press("enter", cx);
        });

        assert_eq!(f.events(), vec![ComposerEvent::Send("queued".into())]);
    }

    /// One button, and what it *is called* follows the swarm: the word alone — the
    /// glyph the design leads it with is decoration, and a reader that cannot see it
    /// hears `Send` or `Stop swarm`, never the arrow.
    #[gpui_kit::test]
    fn the_one_button_follows_the_activity_and_is_never_send_and_stop(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find(BUTTON_ID).label(), Some("Send"));

            f.busy(true, cx);
            window.render_frame(cx);
            assert_eq!(window.find(BUTTON_ID).label(), Some("Stop swarm"));

            f.busy(true, cx);
            window.render_frame(cx);
            assert_eq!(window.find(BUTTON_ID).label(), Some("Stop swarm"));

            f.busy(false, cx);
            window.render_frame(cx);
            assert_eq!(window.find(BUTTON_ID).label(), Some("Send"));
        });
    }

    /// A blank draft is not a disabled button: the design draws `.composer-send` in
    /// the primary face at rest, so the button looks the same with nothing typed — it
    /// just has nothing to send, and neither the click nor `Enter` does anything.
    #[gpui_kit::test]
    fn a_blank_draft_is_the_same_button_and_does_nothing(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find(BUTTON_ID).label(), Some("Send"));
            assert!(
                !f.composer.read(cx).is_action_disabled(),
                "no draft, and the button is still the design's own face"
            );

            // Nothing typed: nothing to send, so the click is inert.
            window.click(BUTTON_ID, cx);

            // Whitespace only is blank too.
            f.set_draft("   ", window, cx);
            window.render_frame(cx);
            assert!(!f.composer.read(cx).is_action_disabled());
            window.click(BUTTON_ID, cx);
            window.press("enter", cx);
        });

        assert!(f.events().is_empty(), "a blank draft has nothing to send");

        f.act(cx, |window, cx| {
            f.set_draft("real", window, cx);
            window.render_frame(cx);
            assert!(!f.composer.read(cx).is_action_disabled());
            window.click(BUTTON_ID, cx);
        });

        assert_eq!(f.events(), vec![ComposerEvent::Send("real".into())]);
    }

    #[gpui_kit::test]
    fn the_button_is_disabled_only_while_its_own_request_is_in_flight(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            f.set_draft("one", window, cx);
            window.render_frame(cx);
            assert!(!f.composer.read(cx).is_action_disabled());
            window.click(BUTTON_ID, cx);
        });
        assert_eq!(f.events().len(), 1, "the first click sends");

        // In flight: the button wears the kit's disabled face and the click does
        // nothing at all.
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert!(
                f.composer.read(cx).is_action_disabled(),
                "the greyed face is only ever its own request in flight"
            );
            window.click(BUTTON_ID, cx);
        });
        assert_eq!(f.events().len(), 1);

        // The request is over, so the button works again.
        f.act(cx, |window, cx| {
            f.composer.update(cx, |composer, cx| {
                composer.request_finished(true, window, cx)
            });
            f.set_draft("two", window, cx);
            window.render_frame(cx);
            window.click(BUTTON_ID, cx);
        });

        assert_eq!(
            f.events(),
            vec![
                ComposerEvent::Send("one".into()),
                ComposerEvent::Send("two".into())
            ]
        );
    }

    /// While the swarm is busy the button stops it — the whole swarm, which is the one
    /// action a person has over it (CONTRACT §7.5) — and never sends or clears the draft.
    #[gpui_kit::test]
    fn stop_swarm_stops_it_and_never_sends_or_clears(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            f.type_draft("still working", window, cx);
            f.busy(true, cx);
            window.render_frame(cx);

            assert_eq!(window.find(BUTTON_ID).label(), Some("Stop swarm"));
            window.click(BUTTON_ID, cx);
        });

        assert_eq!(f.events(), vec![ComposerEvent::StopSwarm]);
        assert_eq!(f.draft_now(cx), "still working");

        // The reply keeps the draft: it was a stop, not a send.
        f.act(cx, |window, cx| {
            f.composer.update(cx, |composer, cx| {
                composer.request_finished(false, window, cx)
            })
        });
        assert_eq!(f.draft_now(cx), "still working");
    }

    /// Esc is the coordinator's own run, not the swarm's: `run.interrupt` with scope
    /// `session`, which the owner sends.
    #[gpui_kit::test]
    fn escape_interrupts_the_coordinators_own_run(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            f.type_draft("still working", window, cx);
            f.busy(true, cx);
            window.render_frame(cx);
            window.dispatch_keystroke(Keystroke::parse("escape").expect("escape"), cx);
        });

        assert_eq!(f.events(), vec![ComposerEvent::Interrupt]);
        assert_eq!(f.draft_now(cx), "still working");
    }

    /// The foot row is the topic's own chips (CONTRACT §4.2): the model with its
    /// effort beside it, then the context and the cache, in the order the server
    /// published them.
    ///
    /// The goal is not one of them: it has the strip of its own above the foot, and the
    /// server's `goal` segment — the one place its id is spelled out — is not drawn.
    #[gpui_kit::test]
    fn the_foot_row_is_the_topics_own_chips(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            let state = state_with(&["model", "thinking", "context", "cache_stats", "goal"]);
            f.set_agent(&state, cx);
            window.render_frame(cx);

            let model = window.find(chip_id("model"));
            let ctx = window.find(chip_id("context"));
            let cache = window.find(chip_id("cache_stats"));
            assert!(model.visible() && ctx.visible() && cache.visible());
            assert_eq!(
                model.label(),
                Some("stub-a high"),
                "the model, then the effort"
            );
            assert_eq!(ctx.label(), Some("ctx 48k/936k (5%)"));
            assert_eq!(cache.label(), Some("97% cached"));
            assert!(
                window.try_find(chip_id("goal")).is_none(),
                "the goal is the strip's, not a chip's"
            );

            // One row, left to right, in the server's order.
            let x = |id: &str| window.find(chip_id(id)).bounds().left();
            assert!(x("model") < x("context") && x("context") < x("cache_stats"));
            for id in ["model", "context", "cache_stats"] {
                assert_eq!(
                    window.find(chip_id(id)).bounds().size.height,
                    px(widgets::chip::HEIGHT)
                );
            }
        });
    }

    /// The core's own cache segment is a chip of its own (§4.2, the design's `cache
    /// chip`): whatever the topic publishes is drawn, and nothing is hidden.
    ///
    /// The document is the wire's own: `cache_stats` (the enum-string name, order 350,
    /// side left), the text evo paints, and the `cache_stats` state key beside it — a
    /// key this crate's view model does not read, which must not cost the segment
    /// beside it.
    #[gpui_kit::test]
    fn the_servers_own_cache_segment_is_a_chip_of_its_own(cx: &mut TestAppContext) {
        let state = TopicState::from_json(&serde_json::json!({
            "status": "idle",
            "model": {"id": "stub-a", "provider": "openai", "ready": true},
            "thinking": "medium",
            "cache_stats": {"input": 0, "cache_read": 0, "cache_write": 0},
            "segments": [
                {"name": "model", "order": 100, "side": "left", "text": "stub-a", "data": {}},
                {"name": "thinking", "order": 200, "side": "left", "text": "medium",
                 "data": {}},
                {"name": "context", "order": 300, "side": "left", "text": "ctx 0k/200k (0%)",
                 "data": {}},
                {"name": "cache_stats", "order": 350, "side": "left", "text": "0% cached",
                 "data": {"input": 0, "cache_read": 0, "cache_write": 0}},
                {"name": "goal", "order": 400, "side": "left", "text": "goal a1b2c3d4 (active)",
                 "data": {}},
            ],
        }));
        assert!(
            state
                .segments
                .iter()
                .any(|segment| segment.name == "cache_stats"),
            "the state key the view model does not know costs nothing beside it"
        );

        let f = open(cx);
        f.act(cx, |window, cx| {
            f.set_agent(&state, cx);
            window.render_frame(cx);
            let cache = window.find(chip_id("cache_stats"));
            assert!(cache.visible(), "the cache is a chip of its own");
            assert_eq!(cache.label(), Some("0% cached"), "in the server's words");
            assert_eq!(cache.bounds().size.height, px(widgets::chip::HEIGHT));
            // The server's own order: after the context. The `goal` segment beside it is
            // the one segment the box does not draw (the goal strip is), and the id its
            // words carry is nowhere on this row.
            let x = |id: &str| window.find(chip_id(id)).bounds().left();
            assert!(x("context") < x("cache_stats"), "after `context`");
            assert!(
                window.try_find(chip_id("goal")).is_none(),
                "the goal's own segment is not a chip"
            );
        });
    }

    /// Evo's own `goal` segment is the one place the goal's id is written down
    /// (`goal g-b7ba (active) 12k/50k`), and the box does not draw it: the goal is a
    /// strip, and a chip is a status segment.
    #[test]
    fn the_goal_segment_is_not_a_chip() {
        let state = state_with(&["model", "thinking", "context", "goal"]);
        assert!(
            state
                .segments
                .iter()
                .any(|segment| segment.name == "goal" && segment.text.contains("a1b2c3d4")),
            "the fixture's status line spells the goal's id out"
        );

        let chips = chips_of(&state.segments);
        assert!(
            chips.iter().all(|chip| chip.name != "goal"),
            "the goal's segment is not a chip: {:?}",
            chips.iter().map(|chip| &chip.name).collect::<Vec<_>>()
        );
        assert!(
            chips
                .iter()
                .all(|chip| !chip.text.contains("a1b2c3d4") && !chip.text.contains("goal")),
            "and no chip carries the id the segment spelled out"
        );
    }

    /// The goal's strip: the title row states the goal — `Goal`, its status in the dim
    /// half, its budget beside it — and the objective folds out under it.
    ///
    /// The goal's id is evo's own handle on it (`g-b7ba`), never a reader's fact: not
    /// the chip's words (there is no goal chip), not the row's, and not the objective's.
    #[gpui_kit::test]
    fn the_goal_strip_states_the_goal_and_folds_out_its_objective(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            let state = state_with(&["model", "thinking", "goal"]);
            assert!(
                state.goal.is_some(),
                "the topic's state carries the goal; its status line spells out the id"
            );
            f.set_agent(&state, cx);
            window.render_frame(cx);

            assert!(
                window.try_find(chip_id("goal")).is_none(),
                "the state's goal is a strip, not a chip"
            );
            let row = window.find("goal-strip-row");
            assert!(row.visible(), "the goal has a strip of its own");
            assert_eq!(row.label(), Some("Goal (active) · 12k/50k"));
            assert!(
                !row.label().unwrap_or_default().contains("a1b2c3d4"),
                "the row never names the goal's id"
            );
            assert!(
                window.try_find("goal-text").is_none(),
                "the objective is folded away to begin with, as the todo list is"
            );

            window.click("goal-strip-row", cx);
            window.render_frame(cx);
            let text = window.find("goal-text");
            assert!(text.visible(), "and folds out under its own row");
            assert_eq!(
                text.label(),
                Some("ship the redesign"),
                "the block is the objective, whole and nothing else"
            );
            let row_ = window.find("goal-strip-row").bounds();
            assert!(
                text.bounds().top() >= row_.bottom(),
                "the objective is under the title row: {:?} vs {row_:?}",
                text.bounds()
            );

            // The strips stack: the goal over the plan, both over the input.
            let todos = window.find("todo-strip-row").bounds();
            let input = window.find(f.input_frame(cx)).bounds();
            assert!(
                row_.bottom() <= todos.top() && todos.bottom() <= input.top(),
                "the goal strip stands over the todo strip: {row_:?}, {todos:?}, {input:?}"
            );

            // The row folds the objective back, and only the row does.
            window.click("goal-strip-row", cx);
            window.render_frame(cx);
            assert!(window.try_find("goal-text").is_none());
        });
    }

    /// The strip is not a drawer: the press outside the box that folds the model drawer
    /// back leaves the goal where its own row put it, and so does the drawer opening.
    #[gpui_kit::test]
    fn the_goal_strip_is_not_folded_by_a_press_outside_the_box(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            f.set_catalog(cx);
            let state = state_with(&["model", "thinking", "goal"]);
            f.set_agent(&state, cx);
            window.render_frame(cx);

            // The goal's objective, and the model's drawer beside it.
            window.click("goal-strip-row", cx);
            window.render_frame(cx);
            window.click(chip_id("model"), cx);
            window.render_frame(cx);
            assert!(window.find("goal-text").visible());
            assert!(window.find("composer-drawer").visible());

            // A press on the transcript — anywhere outside the box — folds the drawer
            // and nothing else.
            f.composer.update(cx, |composer, cx| {
                composer.close_drawer_at(point(px(0.), px(0.)), cx)
            });
            window.render_frame(cx);
            assert!(
                window.try_find("composer-drawer").is_none(),
                "the model drawer folds back"
            );
            assert!(
                window.find("goal-text").visible(),
                "the goal's strip stays where it is"
            );

            // And folding the drawer out again leaves it there too.
            window.click(chip_id("model"), cx);
            window.render_frame(cx);
            assert!(window.find("composer-drawer").visible());
            assert!(window.find("goal-text").visible());
        });
    }

    /// An agent with no goal has no goal strip, as §4.2 means it: the box says nothing
    /// about a goal the topic does not carry.
    #[gpui_kit::test]
    fn an_agent_with_no_goal_has_no_goal_strip(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            let mut state = state_with(&["model", "thinking"]);
            state.goal = None;
            f.set_agent(&state, cx);
            window.render_frame(cx);
            assert!(window.try_find("goal-strip-row").is_none());
            assert!(
                window.find("composer-box").visible(),
                "the box is still there"
            );
        });
    }

    /// A chip that opens something is a button: clicking it folds the drawer out
    /// inside the box, clicking it again folds it back.
    #[gpui_kit::test]
    fn the_model_chip_folds_the_model_drawer_out_and_back(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            f.set_catalog(cx);
            let state = state_with(&["model", "thinking"]);
            f.set_agent(&state, cx);
            window.render_frame(cx);
            assert!(
                window.try_find("composer-drawer").is_none(),
                "nothing is open to begin with"
            );

            window.click(chip_id("model"), cx);
            window.render_frame(cx);
            let drawer = window.find("composer-drawer").bounds();
            let box_ = window.find("composer-box").bounds();
            assert!(
                drawer.top() >= box_.top() && drawer.bottom() <= box_.bottom(),
                "the drawer folds out inside the box: {drawer:?} in {box_:?}"
            );
            assert!(
                window.find("drawer-model-stub-a").visible(),
                "the catalog's models are in it"
            );
            assert!(
                window.find("composer-effort").visible(),
                "and the effort rail"
            );

            // The same chip folds it back.
            window.click(chip_id("model"), cx);
            window.render_frame(cx);
            assert!(window.try_find("composer-drawer").is_none());
        });
    }

    /// The drawer's models are the catalog's, and picking one is a `model.set`; the
    /// rail picks a rung of the server's ladder and that is a `thinking.set`.
    #[gpui_kit::test]
    fn the_drawer_sends_model_set_and_thinking_set(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            f.set_catalog(cx);
            // The agent runs the ladder's first rung, so a press on the middle of the
            // rail is a rung it is not on.
            let state = state_with_level(&["model", "thinking"], "low");
            f.set_agent(&state, cx);
            window.render_frame(cx);
            window.click(chip_id("model"), cx);
            window.render_frame(cx);
        });
        f.act(cx, |window, cx| window.click("drawer-model-stub-b", cx));
        assert_eq!(
            f.events(),
            vec![ComposerEvent::ModelSet {
                id: "stub-b".to_string(),
                provider: "openai".to_string(),
            }]
        );

        // The reply lands (the owner reports it), and the rail is clicked: a press on
        // the rail picks the rung under the pointer.
        f.act(cx, |window, cx| {
            f.composer.update(cx, |composer, cx| {
                composer.request_finished(false, window, cx)
            });
            window.click("composer-effort", cx);
        });
        assert!(
            matches!(f.events().last(), Some(ComposerEvent::ThinkingSet(_))),
            "the rail picked a rung: {:?}",
            f.events()
        );
    }

    /// The rail's rungs are the **chosen model's own** (`/catalog.models[].effort_levels`),
    /// not the session's: the same press is a different level on a model whose provider
    /// offers two rungs than on one that takes the whole ladder — and a model the catalog
    /// gives no levels says so, with no rail to press at all.
    #[gpui_kit::test]
    fn the_drawer_offers_the_chosen_models_own_levels(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            f.set_catalog(cx);
            f.set_agent(&state_running("stub-a"), cx);
            window.render_frame(cx);
            window.click(chip_id("model"), cx);
            window.render_frame(cx);
        });

        // Three quarters along the rail: the fourth of `stub-a`'s five rungs, which the
        // topic's own `high` is not. The press is answered the way the tab answers it —
        // one setting is in flight at a time, so until the reply lands the next press
        // is not the reader's to make.
        let press = |f: &Fixture, cx: &mut TestAppContext| {
            f.act(cx, |window, cx| {
                let at = three_quarters_of_the_rail(window);
                window.click_at("composer-effort", at, cx);
                f.composer.update(cx, |composer, cx| {
                    composer.request_finished(false, window, cx)
                });
            })
        };
        press(&f, cx);
        assert_eq!(
            f.events().last(),
            Some(&ComposerEvent::ThinkingSet("xhigh".to_string())),
            "the session's ladder: {:?}",
            f.events()
        );

        // The same press on a model whose provider offers two rungs is that ladder's
        // last: `max`, not `xhigh` — the rail has no rung the model does not take.
        f.act(cx, |window, cx| {
            f.set_agent(&state_running("stub-b"), cx);
            window.render_frame(cx);
        });
        press(&f, cx);
        assert_eq!(
            f.events().last(),
            Some(&ComposerEvent::ThinkingSet("max".to_string())),
            "the model's own ladder: {:?}",
            f.events()
        );

        // A model that takes no effort setting at all: the row keeps its place and says
        // so, and there is no rail to offer rungs it does not have.
        f.act(cx, |window, cx| {
            f.set_agent(&state_running("stub-c"), cx);
            window.render_frame(cx);
            assert_eq!(
                window.find("drawer-effort").label(),
                Some(format!("Effort {NO_EFFORT}").as_str()),
                "the row states it"
            );
            assert!(
                window.try_find("composer-effort").is_none(),
                "and offers no rail"
            );
        });
    }

    /// A catalog longer than the drawer may draw puts the models in a region exactly
    /// seven rows tall that scrolls: the title row above it and the effort row below
    /// it stay put, and the model this box runs is in view from the frame the drawer
    /// opens in.
    #[gpui_kit::test]
    fn a_long_catalog_scrolls_under_a_seven_row_region(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            f.composer.update(cx, |composer, cx| {
                composer.set_catalog(catalog_levels(), many_models(), catalog_commands(), cx)
            });
            // The tenth registration: the row a drawer of seven would not show.
            let state = state_running("stub-j");
            f.set_agent(&state, cx);
            window.render_frame(cx);
            window.click(chip_id("model"), cx);
            window.render_frame(cx);

            let region = window.find("drawer-models").bounds();
            assert_eq!(
                region.size.height,
                DRAWER_ITEM * MODEL_LIST_ROWS,
                "seven rows, whatever the catalog holds: {region:?}"
            );
            let title = window.find("drawer-row").bounds();
            assert!(
                title.bottom() <= region.top(),
                "the title row is above the region: {title:?} vs {region:?}"
            );
            let effort = window.find("drawer-effort");
            assert!(
                effort.visible() && effort.bounds().top() >= region.bottom(),
                "and the effort row is still under it, in the drawer: {:?} vs \
                 {region:?}",
                effort.bounds()
            );

            // The reveal is the composer's own arithmetic, done in the frame the
            // region is born in: `scroll_to_item` asked here is answered against the
            // frame before it, when the region was not there, and leaves the list at
            // its top with the ticked row under the fold.
            let chosen = window.find("drawer-model-stub-j");
            assert!(
                chosen.visible()
                    && chosen.bounds().top() >= region.top()
                    && chosen.bounds().bottom() <= region.bottom(),
                "the model this box runs is in view: {:?} in {region:?}",
                chosen.bounds()
            );
        });
        let max = cx.read(|cx| f.composer.read(cx).models_scroll.max_offset().y);
        assert!(
            max > px(0.),
            "the region has three rows under it to scroll to: {max:?}"
        );

        // A model the region already shows is not scrolled for: the reveal is the
        // least scroll that puts the row in whole, not a jump to its own row.
        f.act(cx, |window, cx| {
            window.click(chip_id("model"), cx);
            window.render_frame(cx);
            let state = state_running("stub-c");
            f.set_agent(&state, cx);
            window.render_frame(cx);
            window.click(chip_id("model"), cx);
            window.render_frame(cx);
            assert_eq!(
                f.composer.read(cx).models_scroll.offset().y,
                px(0.),
                "the third row is one the region opens on"
            );
            let region = window.find("drawer-models").bounds();
            let chosen = window.find("drawer-model-stub-c").bounds();
            assert!(
                chosen.top() >= region.top() && chosen.bottom() <= region.bottom(),
                "and it is in view: {chosen:?} in {region:?}"
            );
        });
    }

    /// Seven models or fewer: the drawer is the rows it has — no region, no bar, and
    /// no height the catalog cannot fill.
    #[gpui_kit::test]
    fn a_short_catalog_is_laid_out_whole(cx: &mut TestAppContext) {
        let f = open(cx);
        for count in [5u8, 7] {
            f.act(cx, |window, cx| {
                let models = many_models()[..count as usize].to_vec();
                f.composer.update(cx, |composer, cx| {
                    composer.set_catalog(catalog_levels(), models, catalog_commands(), cx)
                });
                let state = state_running("stub-e");
                f.set_agent(&state, cx);
                window.render_frame(cx);
                window.click(chip_id("model"), cx);
                window.render_frame(cx);

                assert!(
                    window.try_find("drawer-models").is_none(),
                    "{count} models are not a region"
                );
                let first = window.find("drawer-model-stub-a").bounds();
                let last = window
                    .find(format!(
                        "drawer-model-stub-{}",
                        char::from(b'a' + count - 1)
                    ))
                    .bounds();
                assert_eq!(
                    last.bottom() - first.top(),
                    DRAWER_ITEM * count as usize,
                    "{count} rows are the block's whole height, as they were before \
                     the region"
                );
                let effort = window.find("drawer-effort");
                assert!(
                    effort.visible() && effort.bounds().top() >= last.bottom(),
                    "and the effort row is under them: {:?} vs {last:?}",
                    effort.bounds()
                );

                // Folded back for the next count's pass.
                window.click(chip_id("model"), cx);
                window.render_frame(cx);
            });
        }
    }

    /// A lane's drawer states what that lane runs and offers no change: `model.set`
    /// and `thinking.set` act on the session, and a lane's model is the swarm's.
    #[gpui_kit::test]
    fn a_lanes_drawer_states_what_it_runs_and_changes_nothing(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            f.set_catalog(cx);
            let state = state_with(&["model", "thinking"]);
            f.composer.update(cx, |composer, cx| {
                composer.set_agent(&state, "lane 3", false, cx)
            });
            window.render_frame(cx);
            window.click(chip_id("model"), cx);
            window.render_frame(cx);
        });
        assert!(
            f.act(cx, |window, _| window.find("composer-drawer").visible()),
            "the drawer opens"
        );
        // Nothing here is a button: the crate's own event log stays empty whichever
        // model is pressed.
        f.act(cx, |window, cx| window.click("drawer-model-stub-b", cx));
        assert_eq!(
            f.events(),
            vec![],
            "a lane's model is not this box's to set"
        );
        f.act(cx, |window, _| {
            assert!(
                window.find("drawer-effort").visible(),
                "the effort the lane runs is stated"
            );
            assert!(
                window.try_find("composer-effort").is_none(),
                "and there is no rail to change it with"
            );
        });
    }

    /// A lane's foot is the lane's own state: the segments its topic publishes, in
    /// the server's words — the same chips the coordinator's box shows, drawn from
    /// whatever the selected agent's topic carries. Nothing is composed here, so a
    /// lane whose topic has not reached this box yet has an empty foot.
    #[gpui_kit::test]
    fn a_lanes_own_state_is_what_its_foot_shows(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            let state = state_with(&["model", "thinking", "context", "cache_stats"]);
            f.composer.update(cx, |composer, cx| {
                composer.set_agent(&state, "lane 1", false, cx)
            });
            window.render_frame(cx);
            let model = window.find(chip_id("model"));
            let ctx = window.find(chip_id("context"));
            let cache = window.find(chip_id("cache_stats"));
            assert!(
                model.visible() && ctx.visible() && cache.visible(),
                "the lane's own segments are its chips"
            );
            assert_eq!(model.label(), Some("stub-a high"), "its model and effort");
            assert_eq!(ctx.label(), Some("ctx 48k/936k (5%)"));
            assert_eq!(cache.label(), Some("97% cached"));
        });
    }

    /// The todo strip: one title row with the count, and the list under it while it is
    /// open — folded to begin with, as the design's default.
    #[gpui_kit::test]
    fn the_todo_strip_counts_the_todos_and_folds_them_out(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            let state = state_with(&["model"]);
            f.set_agent(&state, cx);
            window.render_frame(cx);
            let row = window.find("todo-strip-row");
            assert_eq!(row.label(), Some("Todos 1/3"), "the strip's own count");
            assert!(
                window.try_find("todo-list").is_none(),
                "the list is folded away to begin with"
            );

            window.click("todo-strip-row", cx);
            window.render_frame(cx);
            let list = window.find("todo-list").bounds();
            assert!(
                list.size.height <= px(156.),
                "the list caps at 156px: {list:?}"
            );
            let first = window.find("todo-0").bounds();
            let row_ = window.find("todo-strip-row").bounds();
            assert!(
                first.top() >= row_.bottom(),
                "the items are under the title row: {first:?} vs {row_:?}"
            );
            assert_eq!(window.find("todo-0").label(), Some("port the view model"));
        });
    }

    /// A plan longer than the list's cap scrolls inside it: the list is the design's
    /// 156px whatever the server sends, and a wheel moves the rows under it rather
    /// than growing the box.
    #[gpui_kit::test]
    fn a_long_todo_list_caps_and_scrolls(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            let todos: Vec<serde_json::Value> = (0..12)
                .map(|n| serde_json::json!({"text": format!("step {n}"), "status": "pending"}))
                .collect();
            let state = TopicState::from_json(&serde_json::json!({
                "model": {"id": "stub-a", "provider": "openai", "ready": true},
                "segments": [
                    {"name": "model", "order": 100, "side": "left", "text": "stub-a",
                     "data": {}}
                ],
                "todos": todos,
            }));
            f.set_agent(&state, cx);
            window.render_frame(cx);
            window.click("todo-strip-row", cx);
            window.render_frame(cx);

            let list = window.find("todo-list").bounds();
            assert!(
                list.size.height <= px(156.),
                "the list caps at the design's 156px: {list:?}"
            );
            let first = window.find("todo-0").bounds().top();
            // A wheel down is a negative y delta in gpui's own units.
            window.scroll(
                "todo-list",
                gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-60.))),
                cx,
            );
            window.render_frame(cx);
            window.render_frame(cx);
            assert_eq!(
                window.find("todo-list").bounds().size.height,
                list.size.height,
                "scrolling does not grow the list"
            );
            assert!(
                window.find("todo-0").bounds().top() < first,
                "the rows move under the cap: {} was {first:?}",
                window.find("todo-0").bounds().top()
            );
        });
    }

    /// An agent with no todos has no strip: a title row over nothing would be chrome
    /// that says only that there is nothing to say.
    #[gpui_kit::test]
    fn an_agent_with_no_todos_has_no_strip(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            let mut state = state_with(&["model", "thinking"]);
            state.todos.clear();
            f.composer.update(cx, |composer, cx| {
                composer.set_agent(&state, "Coordinator", true, cx)
            });
            window.render_frame(cx);
            assert!(window.try_find("todo-strip-row").is_none());
            assert!(
                window.find("composer-box").visible(),
                "the box is still there"
            );
        });
    }

    // -----------------------------------------------------------------------
    // What the message carries: the picker, the tiles, the paste, the drop
    // -----------------------------------------------------------------------

    /// A directory of the test's own, removed when the test ends.
    ///
    /// The picker's answer is real paths and telling an image from a file reads the file
    /// itself, so these tests need files that are there.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir = std::env::temp_dir().join(format!(
                "composer-attachments-{}-{name}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("a scratch directory");
            Scratch(dir)
        }

        /// A file of these bytes, answering the path it is at.
        fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, bytes).expect("a scratch file");
            path
        }

        /// A file of this name and no file behind it: the path a picker answers with for
        /// something that has since gone, or for a name read without the bytes.
        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }

        /// A real PNG of that size — the bytes an image of the turn really is, magic
        /// number and all.
        fn png(&self, name: &str, width: u32, height: u32) -> PathBuf {
            let image = image::RgbaImage::from_fn(width, height, |x, y| {
                image::Rgba([(x * 6) as u8, (y * 6) as u8, 0x40, 0xFF])
            });
            let mut bytes = std::io::Cursor::new(Vec::new());
            image
                .write_to(&mut bytes, image::ImageFormat::Png)
                .expect("encode a PNG");
            self.write(name, &bytes.into_inner())
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Every extension §5.5's images take, a name that lies, a file with no name to go
    /// by, and the two things that are not images at all: what `kind_of` reads, and why
    /// it reads both signals.
    #[test]
    fn an_image_is_told_from_a_file_by_its_name_and_by_its_own_bytes() {
        let scratch = Scratch::new("kinds");
        // A real PNG, and the four other formats' magic numbers, written where their
        // names say nothing at all.
        let png = scratch.png("shot.png", 24, 12);
        let unnamed_png = scratch.write("shot-2026-10-02", &std::fs::read(&png).unwrap());
        let webp = scratch.write("a.webp", b"RIFF\x24\x00\x00\x00WEBPVP8 \x00\x00\x00\x00");
        let gif = scratch.write("b.gif", b"GIF89a\x01\x00\x01\x00\x00");
        let jpeg = scratch.write("c", &[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]);
        // A name that lies, and a name that knows: extension is what the reader sees, and
        // the refusal that comes back names the file (§5.5).
        let lying = scratch.write("screenshot.png", b"this is not a picture\n");
        let text = scratch.write("notes.txt", b"plain words, no magic in them\n");
        // A file that is not there: a path, and nothing to sniff — the name decides.
        let gone = scratch.path("gone.png");
        let gone_text = scratch.path("gone.txt");

        for (path, image) in [
            (&png, true),
            (&unnamed_png, true),
            (&webp, true),
            (&gif, true),
            (&jpeg, true),
            (&lying, true),
            (&gone, true),
            (&text, false),
            (&gone_text, false),
        ] {
            let kind = kind_of(path);
            assert_eq!(
                matches!(kind, AttachmentKind::ImageFile(_)),
                image,
                "{path:?} as an attachment: {kind:?}"
            );
            assert_eq!(
                matches!(kind, AttachmentKind::File(_)),
                !image,
                "and it is one or the other: {kind:?}"
            );
        }
    }

    /// The `+` is the foot row's own control — a chip's pill, a chip's height, a chip's
    /// three fills — standing immediately left of the one action button, and what it asks
    /// the picker for is what the draft then carries.
    #[gpui_kit::test]
    fn the_plus_asks_the_picker_and_the_draft_carries_what_it_answers(cx: &mut TestAppContext) {
        let scratch = Scratch::new("plus");
        let shot = scratch.png("shot.png", 40, 24);
        let notes = scratch.write("notes.txt", b"plain words\n");
        let f = open(cx);

        f.act(cx, |window, cx| {
            window.render_frame(cx);
            let plus = window.find(ATTACH_ID);
            assert_eq!(
                plus.bounds().size.height,
                px(widgets::chip::HEIGHT),
                "the chips' own height"
            );
            assert_eq!(
                plus.bounds().size.width,
                px(2. * widgets::chip::PAD + 14.),
                "a pill around one glyph, with the chips' own padding"
            );
            assert_eq!(plus.label(), Some("Add attachments"), "and its name");
            let button = window.find(BUTTON_ID).bounds();
            assert!(
                plus.bounds().right() <= button.left(),
                "left of the one button: {:?} vs {button:?}",
                plus.bounds()
            );
            assert_eq!(
                plus.bounds().center().y,
                button.center().y,
                "on the button's own line"
            );
        });

        f.pick_files(vec![shot.clone(), notes.clone()], cx);

        let attached = f.attached(cx);
        assert_eq!(attached.len(), 2, "both files are on the draft");
        assert_eq!(attached[0].name, "shot.png");
        assert!(
            matches!(&attached[0].kind, AttachmentKind::ImageFile(path) if *path == shot),
            "the picture is an image of the turn: {:?}",
            attached[0].kind
        );
        assert!(attached[0].is_image());
        assert_eq!(attached[1].name, "notes.txt");
        assert!(
            matches!(&attached[1].kind, AttachmentKind::File(path) if *path == notes),
            "the text file is a file: {:?}",
            attached[1].kind
        );
        assert!(!attached[1].is_image());
        assert_ne!(
            attached[0].id, attached[1].id,
            "each has its own id, so one can be taken off around the other"
        );
    }

    /// With no picker injected the `+` asks the platform's own dialog: files, several of
    /// them, and no folders — an attachment is something evo can carry, and a folder is
    /// not.
    #[gpui_kit::test]
    fn the_plus_asks_the_platform_for_files_when_no_picker_is_injected(cx: &mut TestAppContext) {
        let scratch = Scratch::new("platform");
        let shot = scratch.png("shot.png", 8, 8);
        let f = open(cx);

        f.act(cx, |window, cx| {
            window.render_frame(cx);
            window.click(ATTACH_ID, cx);
        });
        assert!(
            cx.did_prompt_for_paths(),
            "the platform's own dialog was opened"
        );
        cx.simulate_path_prompt_response(move |options| {
            assert!(options.files, "files are what it offers");
            assert!(!options.directories, "and not folders");
            assert!(options.multiple, "any number of them");
            Some(vec![shot])
        });
        cx.run_until_parked();

        assert_eq!(f.attached(cx).len(), 1, "the dialog's answer is attached");
    }

    /// The strip states the count, opens on the first attachment — the tiles are where a
    /// reader checks the answer they just gave a dialog — and folds and unfolds from its
    /// own row. It stands under the goal's and the plan's strips, closest to the input
    /// the message is written in.
    #[gpui_kit::test]
    fn the_attachments_strip_counts_them_and_folds_out_under_the_other_strips(
        cx: &mut TestAppContext,
    ) {
        let scratch = Scratch::new("strip");
        let f = open(cx);
        f.act(cx, |window, cx| {
            let state = state_with(&["model"]);
            f.set_agent(&state, cx);
            window.render_frame(cx);
            assert!(
                window.try_find("attachments-strip-row").is_none(),
                "an empty draft has no strip"
            );
        });

        f.pick_files(
            vec![
                scratch.png("shot.png", 40, 24),
                scratch.write("notes.txt", b"plain words\n"),
            ],
            cx,
        );

        f.act(cx, |window, cx| {
            window.render_frame(cx);

            let row = window.find("attachments-strip-row");
            assert_eq!(row.label(), Some("Attachments 2"), "the count is the row");
            let goal = window.find("goal-strip-row").bounds();
            let todos = window.find("todo-strip-row").bounds();
            let attach = row.bounds();
            let input = window.find(f.input_frame(cx)).bounds();
            assert!(
                goal.bottom() <= todos.top()
                    && todos.bottom() <= attach.top()
                    && attach.bottom() <= input.top(),
                "the box's own order: the goal, then the plan, then what the draft \
                 carries, then the input: {goal:?} {todos:?} {attach:?} {input:?}"
            );
            assert!(
                window.find("attachment-0").visible() && window.find("attachment-1").visible(),
                "and the panel is open: the reader sees what they picked"
            );
            assert_eq!(
                window.find("attachment-0").label(),
                Some("shot.png"),
                "a tile is the file's name"
            );
            assert_eq!(window.find("attachment-1").label(), Some("notes.txt"));

            // The tile's own numbers: a 112px thumbnail 72px tall, the name under it, and
            // a panel that caps at the todo list's own 156px — a row of tiles and a half,
            // so a reader can see there is more.
            let thumb = window.find("attachment-thumb-0");
            assert_eq!(thumb.bounds().size.width, TILE);
            assert_eq!(thumb.bounds().size.height, THUMB);
            let name = window.find("attachment-name-0");
            assert!(
                name.bounds().top() >= thumb.bounds().bottom(),
                "the name is under the thumbnail: {name:?} vs {:?}",
                thumb.bounds()
            );
            let panel = window.find("attachments-panel").bounds();
            let first = window.find("attachment-0").bounds();
            let second = window.find("attachment-1").bounds();
            assert_eq!(
                first.top(),
                second.top(),
                "two tiles are one row: {first:?} vs {second:?}"
            );
            assert!(
                panel.size.height <= STRIP_LIST_MAX,
                "the panel is inside the todo list's own cap: {panel:?}"
            );
            assert!(
                panel.bottom() <= window.find(f.input_frame(cx)).bounds().top(),
                "and it is above the input: {panel:?}"
            );

            // Folded and unfolded from the row itself.
            window.click("attachments-strip-row", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("attachments-panel").is_none(),
                "the row folds the panel away"
            );
            assert_eq!(
                window.find("attachments-strip-row").label(),
                Some("Attachments 2"),
                "and the count stays on the row"
            );
            window.click("attachments-strip-row", cx);
            window.render_frame(cx);
            assert!(window.find("attachments-panel").visible());
        });
    }

    /// More tiles than the panel shows at once: it keeps the todo list's own 156px cap
    /// and the tiles move under it, so a draft with a dozen attachments is one panel and
    /// not a box that grows past the conversation.
    #[gpui_kit::test]
    fn a_long_row_of_tiles_caps_and_scrolls(cx: &mut TestAppContext) {
        let scratch = Scratch::new("cap");
        let f = open(cx);
        let paths: Vec<PathBuf> = (0..12)
            .map(|n| scratch.png(&format!("shot-{n:02}.png"), 32, 20))
            .collect();
        f.pick_files(paths, cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            let panel = window.find("attachments-panel").bounds();
            assert_eq!(
                panel.size.height, STRIP_LIST_MAX,
                "the panel stops at the todo list's own cap: {panel:?}"
            );
            let first = window.find("attachment-0").bounds().top();
            window.scroll(
                "attachments-panel",
                gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-60.))),
                cx,
            );
            window.render_frame(cx);
            window.render_frame(cx);
            assert_eq!(
                window.find("attachments-panel").bounds().size.height,
                panel.size.height,
                "scrolling does not grow the panel"
            );
            assert!(
                window.find("attachment-0").bounds().top() < first,
                "the tiles move under the cap: {} was {first:?}",
                window.find("attachment-0").bounds().top()
            );
        });
    }

    /// A tile's own `×` takes that one attachment off the message — and only that one, so
    /// a file picked by mistake does not cost the files picked with it.
    #[gpui_kit::test]
    fn a_tiles_x_takes_one_attachment_off_the_message(cx: &mut TestAppContext) {
        let scratch = Scratch::new("remove");
        let f = open(cx);
        f.pick_files(
            vec![
                scratch.png("one.png", 24, 16),
                scratch.write("two.txt", b"two\n"),
                scratch.png("three.png", 24, 16),
            ],
            cx,
        );
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find("attachments-strip-row").label(),
                Some("Attachments 3")
            );

            window.click("attachment-remove-1", cx);
            window.render_frame(cx);

            let names: Vec<String> = [window.find("attachment-0"), window.find("attachment-1")]
                .iter()
                .map(|tile| tile.label().unwrap_or_default().to_string())
                .collect();
            assert_eq!(
                names,
                vec!["one.png".to_string(), "three.png".to_string()],
                "the middle one is gone, the others are where they were"
            );
            assert_eq!(
                window.find("attachments-strip-row").label(),
                Some("Attachments 2"),
                "and the count follows"
            );

            // The last one off leaves no strip at all: an empty draft has nothing to say.
            window.click("attachment-remove-0", cx);
            window.render_frame(cx);
            window.click("attachment-remove-0", cx);
            window.render_frame(cx);
            assert!(
                window.try_find("attachments-strip-row").is_none(),
                "nothing attached, no strip"
            );
        });
    }

    /// A press outside the box folds the model drawer and nothing else: the strips are
    /// where a reader reads, and the tiles stand until they are taken off.
    #[gpui_kit::test]
    fn a_press_outside_the_box_leaves_the_attachments_open(cx: &mut TestAppContext) {
        let scratch = Scratch::new("outside");
        let f = open(cx);
        f.pick_files(vec![scratch.png("shot.png", 24, 16)], cx);
        f.act(cx, |window, cx| {
            f.set_catalog(cx);
            let state = state_with(&["model"]);
            f.set_agent(&state, cx);
            window.render_frame(cx);
            window.click(chip_id("model"), cx);
            window.render_frame(cx);
            assert!(window.find("composer-drawer").visible());

            f.composer.update(cx, |composer, cx| {
                composer.close_drawer_at(point(px(-4.), px(-4.)), cx)
            });
            window.render_frame(cx);
            assert!(
                window.try_find("composer-drawer").is_none(),
                "the drawer is what a press outside folds"
            );
            assert!(
                window.find("attachments-panel").visible(),
                "the tiles are still there"
            );
        });
    }

    /// `⌘V` with an image on the clipboard attaches it and leaves the input alone: a
    /// picture is not text, and the reader means to send it. Text on the clipboard is the
    /// input's own paste, untouched.
    #[gpui_kit::test]
    fn a_pasted_image_is_an_attachment_and_pasted_text_is_still_text(cx: &mut TestAppContext) {
        let scratch = Scratch::new("paste");
        let shot = scratch.png("shot.png", 32, 20);
        let png = std::fs::read(&shot).expect("the PNG's own bytes");
        let f = open(cx);

        f.act(cx, |window, cx| {
            window.render_frame(cx);
            window.click(f.input_frame(cx), cx);
        });
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_image(
            &gpui_kit::Image::from_bytes(gpui_kit::ImageFormat::Png, png),
        ));
        f.act(cx, |window, cx| window.press("cmd-v", cx));

        let attached = f.attached(cx);
        assert_eq!(attached.len(), 1, "the picture is attached");
        assert_eq!(
            attached[0].name, "pasted image.png",
            "and named for what it is"
        );
        assert!(
            matches!(
                &attached[0].kind,
                AttachmentKind::ImageBytes { media_type, bytes }
                    if media_type == "image/png" && !bytes.is_empty()
            ),
            "as bytes with their own media type: {:?}",
            attached[0].kind
        );
        assert_eq!(f.draft_now(cx), "", "and none of it went into the draft");

        // The same key with text on the clipboard is the input's own paste.
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("words\n".to_string()));
        f.act(cx, |window, cx| window.press("cmd-v", cx));
        assert_eq!(f.draft_now(cx), "words\n", "text pastes as text");
        assert_eq!(f.attached(cx).len(), 1, "and text is not an attachment");
    }

    /// Files dropped on the box are attachments: the third way in, and the one that needs
    /// no dialog at all.
    #[gpui_kit::test]
    fn files_dropped_on_the_box_are_attachments(cx: &mut TestAppContext) {
        let scratch = Scratch::new("drop");
        let shot = scratch.png("shot.png", 24, 16);
        let notes = scratch.write("notes.txt", b"plain words\n");
        let f = open(cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            let at = window.find("composer-box").bounds().center();
            // The drag carries the paths the platform read off the drop; the collection
            // is the platform's own, built the way it builds one.
            let mut paths = ExternalPaths::default();
            paths.0.extend([shot.clone(), notes.clone()]);
            window.dispatch_event(
                gpui_kit::PlatformInput::FileDrop(gpui_kit::FileDropEvent::Entered {
                    position: at,
                    paths,
                }),
                cx,
            );
            window.dispatch_event(
                gpui_kit::PlatformInput::FileDrop(gpui_kit::FileDropEvent::Submit { position: at }),
                cx,
            );
            window.render_frame(cx);
        });

        let attached = f.attached(cx);
        assert_eq!(attached.len(), 2, "the drop is two attachments");
        assert!(
            matches!(&attached[0].kind, AttachmentKind::ImageFile(path) if *path == shot)
                && matches!(&attached[1].kind, AttachmentKind::File(path) if *path == notes),
            "read like the picker's own answer: {attached:?}"
        );
        assert_eq!(
            f.act(cx, |window, _| window
                .find("attachments-strip-row")
                .label()
                .map(str::to_string)),
            Some("Attachments 2".to_string())
        );
    }

    /// A message with what it carries and no words is a message (§5.5): the files are
    /// what it says, so `Enter` in an empty box with an attachment on it sends.
    #[gpui_kit::test]
    fn an_empty_box_that_carries_an_attachment_is_a_message(cx: &mut TestAppContext) {
        let scratch = Scratch::new("empty");
        let f = open(cx);
        f.pick_files(vec![scratch.write("notes.txt", b"plain words\n")], cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            // The caret is in the box, as it is when a reader who just answered a dialog
            // presses Enter.
            window.click(f.input_frame(cx), cx);
            window.press("enter", cx);
        });

        assert_eq!(
            f.events(),
            vec![ComposerEvent::Send(Outgoing {
                text: String::new(),
                attachments: f.attached(cx),
            })],
            "an empty box with a file on it is not a blank box"
        );
        assert_eq!(
            f.act(cx, |window, _| window
                .find(BUTTON_ID)
                .label()
                .map(str::to_string)),
            Some(ActionFace::Send.name().to_string()),
            "and the one button is still Send"
        );
    }

    /// The send carries them, and only the server's own `ok` takes them off: a refusal
    /// that names what it did not like (`image could not be read: …`) leaves the message
    /// whole — the words and what they carry — for the reader to fix and send again.
    #[gpui_kit::test]
    fn a_send_carries_the_attachments_and_only_ok_takes_them_off(cx: &mut TestAppContext) {
        let scratch = Scratch::new("send");
        let f = open(cx);
        f.pick_files(
            vec![
                scratch.png("one.png", 24, 16),
                scratch.write("two.txt", b"two\n"),
            ],
            cx,
        );
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            window.click(f.input_frame(cx), cx);
            window.input("have a look at these", cx);
            window.press("enter", cx);
        });

        // What went out is what the box was holding.
        let sent = f.attached(cx);
        assert_eq!(sent.len(), 2);
        assert_eq!(
            f.events(),
            vec![ComposerEvent::Send(Outgoing {
                text: "have a look at these".to_string(),
                attachments: sent.clone(),
            })],
            "the message carries them"
        );

        // The refusal: nothing is taken off the draft — neither the words nor the files.
        f.act(cx, |window, cx| {
            f.composer.update(cx, |composer, cx| {
                composer.request_finished(false, window, cx)
            });
            window.render_frame(cx);
        });
        assert_eq!(f.attached(cx).len(), 2, "a refusal keeps the attachments");
        assert_eq!(f.draft_now(cx), "have a look at these", "and the words");
        assert_eq!(
            f.act(cx, |window, _| window
                .find("attachments-strip-row")
                .label()
                .map(str::to_string)),
            Some("Attachments 2".to_string()),
            "with the strip still standing"
        );

        // Sent again, and taken: the input is cleared, and so is what it carried.
        f.act(cx, |window, cx| window.press("enter", cx));
        f.act(cx, |window, cx| {
            f.composer.update(cx, |composer, cx| {
                composer.request_finished(true, window, cx)
            });
            window.render_frame(cx);
        });
        assert!(f.attached(cx).is_empty(), "an accepted send takes them off");
        assert_eq!(f.draft_now(cx), "", "with the draft");
        assert!(
            f.act(cx, |window, _| window
                .try_find("attachments-strip-row")
                .is_none()),
            "and no strip is left over nothing"
        );
    }

    /// A message carrying an attachment is not a slash command, whatever its first word
    /// looks like: a command has nowhere to put a file, and the reader attached one to
    /// what they wrote.
    #[gpui_kit::test]
    fn a_message_that_carries_an_attachment_is_not_a_command(cx: &mut TestAppContext) {
        let scratch = Scratch::new("command");
        let f = open(cx);
        f.pick_files(vec![scratch.write("notes.txt", b"plain words\n")], cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            window.click(f.input_frame(cx), cx);
            window.input("/compact", cx);
            window.press("enter", cx);
        });
        assert!(
            matches!(f.events().last(), Some(ComposerEvent::Send(_))),
            "it goes as a message: {:?}",
            f.events()
        );

        // And with nothing attached it is the command it reads as.
        f.act(cx, |window, cx| {
            f.composer.update(cx, |composer, cx| {
                composer.request_finished(true, window, cx)
            });
            window.render_frame(cx);
            window.click(f.input_frame(cx), cx);
            window.input("/compact", cx);
            window.press("enter", cx);
        });
        assert_eq!(
            f.events().last(),
            Some(&ComposerEvent::Command {
                name: "compact".to_string(),
                args: String::new(),
            }),
            "a bare command is the command's: {:?}",
            f.events()
        );
    }

    /// The input grows with what is typed and stops at half the pane
    /// (`AutoTextarea.tsx`): the page hands in its own height, and a shorter pane
    /// makes a shorter ceiling.
    #[gpui_kit::test]
    fn the_input_grows_to_half_the_pane(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            f.composer
                .update(cx, |composer, cx| composer.set_pane_height(px(600.), cx));
            window.render_frame(cx);
            let short = window.find(f.input_frame(cx)).bounds().size.height;

            f.composer
                .update(cx, |composer, cx| composer.set_pane_height(px(2400.), cx));
            window.render_frame(cx);
            let tall = window.find(f.input_frame(cx)).bounds().size.height;
            assert!(
                tall >= short,
                "a taller pane allows a taller input: {short:?} then {tall:?}"
            );
        });
    }

    #[gpui_kit::test]
    fn enter_sends_the_draft_as_the_keypress_found_it(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            f.type_draft("the prompt as it was typed", window, cx);
            window.press("enter", cx);
            // The rest of the same update touches the input.
            f.set_draft("something else entirely", window, cx);
        });

        assert_eq!(
            f.events(),
            vec![ComposerEvent::Send("the prompt as it was typed".into())],
            "the prompt is what was in the input at the keypress"
        );
        assert_eq!(
            f.draft_now(cx),
            "something else entirely",
            "and the input is left as the update left it"
        );
    }

    #[gpui_kit::test]
    fn the_up_arrow_walks_the_prompts_this_tab_sent(cx: &mut TestAppContext) {
        let f = open(cx);
        f.send_prompt("first prompt", cx);
        f.send_prompt("second prompt", cx);

        assert_eq!(f.draft_now(cx), "", "an accepted send clears the input");
        assert_eq!(f.history(cx), ["first prompt", "second prompt"]);

        f.act(cx, |window, cx| {
            // ↑ brings back the last thing sent, caret at its end; ↑ again, the
            // one before it, and no further.
            window.press("up", cx);
            assert_eq!(f.draft(cx), "second prompt");
            assert_eq!(f.caret(cx), "second prompt".len());
            window.press("up", cx);
            assert_eq!(f.draft(cx), "first prompt");
            window.press("up", cx);
            assert_eq!(f.draft(cx), "first prompt", "there is nothing older");

            // ↓ walks forward again, and past the newest prompt is the empty
            // draft the walk started from.
            window.press("down", cx);
            assert_eq!(f.draft(cx), "second prompt");
            window.press("down", cx);
            assert_eq!(f.draft(cx), "");
        });

        // A draft of the reader's own owns ↑: the caret walks through it, and
        // nothing is recalled over it.
        f.act(cx, |window, cx| {
            f.type_draft("first line", window, cx);
            window.press("shift-enter", cx);
            window.input("second line", cx);
            let end = f.caret(cx);

            window.press("up", cx);
            assert_eq!(f.draft(cx), "first line\nsecond line");
            assert!(
                f.caret(cx) < end,
                "the input moved the caret up a line: {} -> {}",
                end,
                f.caret(cx)
            );
        });

        // A prompt of several lines comes back whole, with the caret at its end
        // — where the reader left off — and not at the start `set_value` leaves
        // it at.
        const TWO_LINES: &str = "first line\nsecond line";
        f.act(cx, |window, cx| f.set_draft(TWO_LINES, window, cx));
        f.act(cx, |window, cx| window.press("enter", cx));
        f.act(cx, |window, cx| {
            f.composer.update(cx, |composer, cx| {
                composer.request_finished(true, window, cx)
            })
        });
        assert_eq!(f.history(cx).last().map(String::as_str), Some(TWO_LINES));
        f.act(cx, |window, cx| {
            window.press("up", cx);
            assert_eq!(f.draft(cx), TWO_LINES);
            assert_eq!(f.caret(cx), TWO_LINES.len(), "the caret is at the end");
        });

        assert_eq!(f.events().len(), 3, "walking the history sends nothing");
    }

    #[gpui_kit::test]
    fn a_prompt_typed_over_ends_the_walk(cx: &mut TestAppContext) {
        let f = open(cx);
        f.send_prompt("an earlier prompt", cx);

        f.act(cx, |window, cx| {
            window.press("up", cx);
            assert_eq!(f.draft(cx), "an earlier prompt");
        });

        // Editing a recalled prompt makes it the reader's own draft: ↓ has no
        // walk left to walk, so it does not throw the draft away.
        f.act(cx, |window, cx| {
            window.input("!", cx);
            assert_eq!(f.draft(cx), "an earlier prompt!");
            window.press("down", cx);
            assert_eq!(f.draft(cx), "an earlier prompt!");
            assert_eq!(f.caret(cx), "an earlier prompt!".len());
        });
    }

    #[gpui_kit::test]
    fn sending_the_same_prompt_again_is_one_entry(cx: &mut TestAppContext) {
        let f = open(cx);
        f.send_prompt("try this", cx);

        // A refused send leaves the draft; the retry is the same prompt.
        f.act(cx, |window, cx| window.press("enter", cx));
        f.act(cx, |window, cx| {
            f.composer.update(cx, |composer, cx| {
                composer.request_finished(false, window, cx)
            })
        });
        f.act(cx, |window, cx| window.press("enter", cx));
        f.act(cx, |window, cx| {
            f.composer.update(cx, |composer, cx| {
                composer.request_finished(true, window, cx)
            })
        });

        assert_eq!(f.history(cx), ["try this"], "a retry is not a new prompt");
    }

    #[gpui_kit::test]
    fn a_refused_send_keeps_the_draft_and_the_caret_at_its_end(cx: &mut TestAppContext) {
        const DRAFT: &str = "first line\nsecond line";
        let f = open(cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            f.type_draft("first line", window, cx);
            window.press("shift-enter", cx);
            window.input("second line", cx);
            assert_eq!(f.draft(cx), DRAFT);
        });
        f.act(cx, |window, cx| window.press("enter", cx));
        f.act(cx, |window, cx| {
            f.composer.update(cx, |composer, cx| {
                composer.request_finished(false, window, cx)
            });
            window.render_frame(cx);

            assert_eq!(f.draft(cx), DRAFT, "a refused send keeps the draft");
            assert_eq!(
                f.caret(cx),
                DRAFT.len(),
                "and the caret is where the reader left it, at the end"
            );

            // The next keystroke continues the draft rather than prepending to it.
            window.input("!", cx);
            assert_eq!(f.draft(cx), format!("{DRAFT}!"));
        });

        assert_eq!(f.events(), vec![ComposerEvent::Send(DRAFT.into())]);
    }

    /// One window, two tabs' composers, one of them mounted — what the tab page
    /// does when the reader switches tabs: the other tab's composer stays alive
    /// off-tree, with its own draft (the workspace keeps one per `TabContent`,
    /// crates/workspace/src/tab.rs).
    struct Tabs {
        composers: [Entity<Composer>; 2],
        showing: usize,
    }

    impl Render for Tabs {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .child(self.composers[self.showing].clone())
        }
    }

    #[gpui_kit::test]
    fn a_draft_belongs_to_its_tab(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (window, tabs) = cx.update(|cx| {
            gpui_kit::open_window(window_options(), cx, |window, cx| {
                cx.new(|cx| Tabs {
                    composers: [
                        cx.new(|cx| Composer::new(window, cx)),
                        cx.new(|cx| Composer::new(window, cx)),
                    ],
                    showing: 0,
                })
            })
            .expect("a window with two composers")
        });
        let first = tabs.read_with(cx, |tabs, _| tabs.composers[0].clone());
        let second = tabs.read_with(cx, |tabs, _| tabs.composers[1].clone());

        let type_into =
            |composer: &Entity<Composer>, text: &str, window: &mut Window, cx: &mut App| {
                let input = ("input", composer.read(cx).input.entity_id());
                window.click(input, cx);
                window.input(text, cx);
            };

        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            type_into(&first, "half a prompt", window, cx);
        })
        .expect("the first tab's window");

        // Switch to the second tab, type there, and come back.
        tabs.update(cx, |tabs, cx| {
            tabs.showing = 1;
            cx.notify();
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                second.read(cx).input.read(cx).value(),
                "",
                "a fresh tab's composer is empty"
            );
            type_into(&second, "the other tab", window, cx);

            tabs.update(cx, |tabs, cx| {
                tabs.showing = 0;
                cx.notify();
            });
            window.render_frame(cx);
        })
        .expect("the second tab's window");

        assert_eq!(
            first.read_with(cx, |composer, cx| composer
                .input
                .read(cx)
                .value()
                .to_string()),
            "half a prompt",
            "the tab that was left behind kept its draft"
        );
        assert_eq!(
            second.read_with(cx, |composer, cx| composer
                .input
                .read(cx)
                .value()
                .to_string()),
            "the other tab",
            "and the tab that was opened has its own"
        );

        // The draft is still the input's, and typing continues in it.
        cx.update_window(window, |_, window, cx| {
            type_into(&first, " and more", window, cx);
        })
        .expect("the first tab's window");
        assert_eq!(
            first.read_with(cx, |composer, cx| composer
                .input
                .read(cx)
                .value()
                .to_string()),
            "half a prompt and more",
            "typing goes on where the surviving draft left off"
        );
    }

    #[gpui_kit::test]
    fn a_large_paste_lands_in_one_piece(cx: &mut TestAppContext) {
        let f = open(cx);
        // Prompts run long: a pasted log, a diff, a stack trace.
        let pasted: String = (0..1_200)
            .map(|line| format!("line {line} of a prompt pasted whole into the composer\n"))
            .collect();
        assert!(pasted.len() > 50_000, "the paste is at least 50 KB");

        f.act(cx, |window, cx| {
            window.render_frame(cx);
            window.click(f.input_frame(cx), cx);
        });
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(pasted.clone()));

        let started = std::time::Instant::now();
        f.act(cx, |window, cx| window.press("cmd-v", cx));
        let elapsed = started.elapsed();

        assert_eq!(f.draft_now(cx), pasted, "the whole paste lands, unclipped");
        println!(
            "[composer] a {} byte paste into the input took {elapsed:?}",
            pasted.len()
        );
    }

    /// A window with selectable text beside a composer: what the reader selects
    /// outside the input is not the input's to copy.
    struct Reader {
        text: Entity<TextViewState>,
        composer: Entity<Composer>,
    }

    impl Render for Reader {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            v_flex()
                .size_full()
                .gap_2()
                .child(
                    div()
                        .h(px(80.))
                        .child(TextView::new(&self.text).selectable(true)),
                )
                .child(div().h(px(160.)).child(self.composer.clone()))
        }
    }

    /// A window with a selectable text view and a composer, the text already
    /// dragged over, and the caret moved into the composer the way the tab page
    /// moves it — no mouse-down in the input, so the selection survives.
    fn reader_fixture(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Composer>) {
        cx.update(gpui_kit::init);
        let source = "Select me outside the input, then copy me.";
        let (window, reader) = cx.update(|cx| {
            gpui_kit::open_window(window_options(), cx, |window, cx| {
                cx.new(|cx| Reader {
                    text: cx.new(|cx| TextViewState::markdown(source, cx)),
                    composer: cx.new(|cx| Composer::new(window, cx)),
                })
            })
            .expect("a window with text and a composer")
        });
        let (composer, bounds) = reader.read_with(cx, |reader, cx| {
            (reader.composer.clone(), reader.text.read(cx).bounds())
        });

        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.drag(
                bounds.origin + point(px(1.), px(4.)),
                bounds.origin + point(bounds.size.width - px(1.), px(4.)),
                cx,
            );
            composer.update(cx, |composer, cx| composer.focus_input(window, cx));
        })
        .expect("the reader's window");

        (window, composer)
    }

    #[gpui_kit::test]
    fn the_copy_shortcut_takes_the_windows_selection_when_the_input_has_none(
        cx: &mut TestAppContext,
    ) {
        let (window, composer) = reader_fixture(cx);

        cx.update_window(window, |_, window, cx| window.press("cmd-c", cx))
            .expect("the reader's window");

        let copied = cx.read_from_clipboard().and_then(|item| item.text());
        let copied = copied.unwrap_or_default();
        assert!(
            copied.starts_with("Select me outside the input"),
            "the reader's selection is what ⌘C took: {copied:?}"
        );
        assert_eq!(
            composer.read_with(cx, |composer, cx| composer
                .input
                .read(cx)
                .value()
                .to_string()),
            "",
            "the input's own text is untouched"
        );
    }

    #[gpui_kit::test]
    fn the_inputs_own_selection_wins_the_copy(cx: &mut TestAppContext) {
        let (window, composer) = reader_fixture(cx);

        cx.update_window(window, |_, window, cx| {
            window.click(("input", composer.read(cx).input.entity_id()), cx);
            window.input("a draft of my own", cx);
            window.press("cmd-a", cx);
            window.press("cmd-c", cx);
        })
        .expect("the reader's window");

        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("a draft of my own".to_string()),
            "the input copies its own selection, not the window's"
        );
    }

    // --- the inline completion popup (§7.3) -------------------------------------

    /// The popup's rows as a reader sees them: each label, in order.
    fn popup_labels(f: &Fixture, cx: &App) -> Vec<String> {
        f.composer
            .read(cx)
            .popup
            .as_ref()
            .map(|popup| {
                popup
                    .rows
                    .iter()
                    .map(|row| popup.label(row))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }

    /// The x the typed word begins at: what the popup's rows line up with.
    fn word_left(f: &Fixture, cx: &App) -> Pixels {
        f.composer
            .read(cx)
            .word_bounds(cx)
            .expect("the word's own box, as the input laid it out")
            .left()
    }

    /// The x row `index`'s label begins at.
    /// A press three quarters along the effort rail, in the coordinates the slider's own
    /// box takes: the rail sits inside it, so the offset is measured from the slider.
    fn three_quarters_of_the_rail(window: &Window) -> gpui_kit::Point<Pixels> {
        let slider = window.find("composer-effort").bounds();
        let rail = window.find("composer-effort-rail-rail").bounds();
        gpui_kit::point(
            rail.left() + rail.size.width * 0.75 - slider.left(),
            rail.center().y - slider.top(),
        )
    }

    fn label_left(window: &Window, index: usize) -> Pixels {
        window
            .find(format!("completion-label-{index}"))
            .bounds()
            .left()
    }

    /// Whether an event is the box asking what the caret is on: a question, not
    /// something the box did to the message.
    fn is_a_question(event: &ComposerEvent) -> bool {
        matches!(event, ComposerEvent::Complete { .. })
    }

    /// Whether an event is the box sending its draft as a message: what a key that the
    /// popup owns must never do.
    fn sends_the_message(event: &ComposerEvent) -> bool {
        matches!(
            event,
            ComposerEvent::Send(_) | ComposerEvent::Command { .. }
        )
    }

    /// What the stand-in server answers for `text` at `cursor`: the caret's `/command`
    /// word — the whitespace-delimited word it is in, when that word starts with a
    /// slash — with the catalog's commands as candidates, and nothing otherwise.
    fn stand_in_answer(text: &str, cursor: usize, items: &[Candidate]) -> Answer {
        let start = text[..cursor]
            .rfind(char::is_whitespace)
            .map_or(0, |index| index + 1);
        let end = text[cursor..]
            .find(char::is_whitespace)
            .map_or(text.len(), |index| cursor + index);
        match text[start..end].starts_with('/') {
            true => Answer {
                kind: Some(CompletionKind::Command),
                name: start + 1..end,
                items: items.to_vec(),
            },
            false => Answer::none(),
        }
    }

    /// The popup's own width.
    fn popup_width(window: &Window) -> Pixels {
        window.find("completion-popup").bounds().size.width
    }

    /// Which row the popup has highlighted, out of how many.
    fn popup_index(f: &Fixture, cx: &App) -> Option<(usize, usize)> {
        f.composer
            .read(cx)
            .popup
            .as_ref()
            .map(|popup| (popup.index, popup.rows.len()))
    }

    /// Whether the popup is drawn in this frame.
    fn popup_drawn(window: &Window) -> bool {
        window
            .try_find("completion-popup")
            .is_some_and(|popup| popup.visible())
    }

    /// The composer at the foot of a page, the way §7.3 holds it: the conversation
    /// above it is what puts the caret's own line low in the window — which is where
    /// "the popup stands over the caret" is a question with an answer.
    struct FooterPage {
        composer: Entity<Composer>,
    }

    impl Render for FooterPage {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .flex()
                .flex_col()
                .size_full()
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .child("the conversation the box is docked under"),
                )
                .child(self.composer.clone())
        }
    }

    fn open_footer(cx: &mut TestAppContext) -> Fixture {
        cx.update(gpui_kit::init);
        let (window, page) = cx.update(|cx| {
            gpui_kit::open_window(window_options(), cx, |window, cx| {
                let composer = cx.new(|cx| Composer::new(window, cx));
                cx.new(|_| FooterPage { composer })
            })
            .expect("the page's window")
        });
        let composer = cx.read(|cx| page.read(cx).composer.clone());

        let events = Rc::new(RefCell::new(Vec::new()));
        let recorded = events.clone();
        let subscription = cx.update(|cx| {
            cx.subscribe(&composer, move |_, event: &ComposerEvent, _| {
                recorded.borrow_mut().push(event.clone());
            })
        });
        Fixture {
            window,
            composer,
            events,
            _subscription: subscription,
        }
    }

    #[gpui_kit::test]
    fn a_slash_word_raises_the_registrys_commands_over_the_caret(cx: &mut TestAppContext) {
        let f = open_footer(cx);
        f.act(cx, |_window, cx| f.set_catalog(cx));
        f.act(cx, |window, cx| f.type_draft("/", window, cx));
        f.answer(cx);

        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert!(popup_drawn(window), "a slash raises the popup");
            assert_eq!(
                popup_labels(&f, cx),
                vec!["/help", "/lore", "/memory", "/reload", "/eval"],
                "the registry's own commands, in its own order"
            );

            // It stands over the caret's own line, a small gap off it, starting where
            // the word does — and it is a deferred draw, so neither the box it belongs
            // to nor the page over it can clip it.
            let (caret, _) = f
                .composer
                .read(cx)
                .input
                .read(cx)
                .cursor_layout()
                .expect("the caret is laid out");
            let popup = window.find("completion-popup").bounds();
            assert!(
                popup.bottom() <= caret.top(),
                "the popup stands over the line being typed: {popup:?} vs {caret:?}"
            );
            assert!(
                caret.top() - popup.bottom() <= px(POPUP_GAP + 2.),
                "and a gap off it, not a distance: {popup:?} vs {caret:?}"
            );
            assert!(
                (label_left(window, 0) - word_left(&f, cx)).abs() <= px(1.),
                "the first row's label starts where the typed word does: \
                 {:?} vs {:?}",
                label_left(window, 0),
                word_left(&f, cx)
            );
            // The rows' labels are one column: what is lined up is the list, not one
            // row of it.
            assert_eq!(label_left(window, 0), label_left(window, 1));
            assert!(
                popup.top() >= px(0.) && popup.bottom() <= px(PANE.height.into()),
                "and the whole of it is inside the window: {popup:?}"
            );
        });
    }

    /// A page with the box at its foot, the catalog in it, and the caret in the input
    /// after typing `text`.
    fn typed_footer(cx: &mut TestAppContext, text: &str) -> Fixture {
        let f = open_footer(cx);
        f.act(cx, |_window, cx| f.set_catalog(cx));
        f.act(cx, |window, cx| f.type_draft(text, window, cx));
        f.answer(cx);
        f
    }

    /// The rows are a column that starts where the word does — at the message's start,
    /// mid-prose with the caret inside the word, and over a `/eval` token — so the list
    /// reads as the word being expanded rather than a box beside it.
    #[gpui_kit::test]
    fn the_rows_line_up_with_the_word_being_completed(cx: &mut TestAppContext) {
        // At the message's start.
        let f = typed_footer(cx, "/");
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            let word = word_left(&f, cx);
            assert!(
                (label_left(window, 0) - word).abs() <= px(1.),
                "the label starts at the word: {:?} vs {word:?}",
                label_left(window, 0)
            );
            let popup_left = popup_left_edge(window);
            assert!(
                (label_left(window, 0) - popup_left - px(POPUP_LABEL_INSET)).abs() <= px(1.),
                "a row's own inset in from the popup's edge: {:?} vs {popup_left:?}",
                label_left(window, 0)
            );
        });

        // Mid-prose, with the caret at the end of the word rather than the message:
        // the caret settles where the reader put it, and *then* the server is asked —
        // a caret that has moved is a question about the text it moved in.
        let g = open_footer(cx);
        g.act(cx, |_window, cx| g.set_catalog(cx));
        g.act(cx, |window, cx| {
            g.type_draft("please run /he now", window, cx)
        });
        g.act(cx, |window, cx| {
            for _ in 0..4 {
                window.press("left", cx);
            }
        });
        g.answer(cx);
        g.act(cx, |window, cx| {
            window.render_frame(cx);
            let word = word_left(&g, cx);
            assert!(
                (label_left(window, 0) - word).abs() <= px(1.),
                "the label starts at the word, not at the caret: \
                 {:?} vs {word:?}",
                label_left(window, 0)
            );
        });

        // Inside `/eval`: the word is the token, not the command word before it — a
        // symbol answer, which is the image's own.
        let h = typed_footer(cx, "/eval (evo.eval:");
        h.act(cx, |_window, cx| {
            h.composer.update(cx, |composer, cx| {
                composer.set_completion(
                    "/eval (evo.eval:",
                    16,
                    Answer {
                        kind: Some(CompletionKind::Symbol),
                        name: 7..16,
                        items: vec![Candidate {
                            name: "evo.eval:completions-for".to_string(),
                            description: "function".to_string(),
                        }],
                    },
                    cx,
                )
            })
        });
        h.act(cx, |window, cx| {
            window.render_frame(cx);
            let word = word_left(&h, cx);
            assert!(
                (label_left(window, 0) - word).abs() <= px(1.),
                "the label starts at the token: {:?} vs {word:?}",
                label_left(window, 0)
            );
        });
    }

    /// The popup's own left edge: what a label sits inside of.
    fn popup_left_edge(window: &Window) -> Pixels {
        window.find("completion-popup").bounds().left()
    }

    /// A word far enough right that the popup would hang off the window: it is pulled
    /// back instead, and stays whole.
    #[gpui_kit::test]
    fn a_word_near_the_windows_edge_keeps_the_popup_inside_it(cx: &mut TestAppContext) {
        // Long enough that the word being completed starts near the right edge, short
        // enough to still be on one line of the box.
        let text = format!("{} /", "x".repeat(70));
        let f = typed_footer(cx, &text);
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            let window_width = window.bounds().size.width;
            let popup = window.find("completion-popup").bounds();
            let word = word_left(&f, cx);
            assert!(
                word - px(POPUP_LABEL_INSET) + popup.size.width > window_width - px(POPUP_MARGIN),
                "the word is far enough right that the popup would overflow: \
                 {word:?} + {popup:?} in {window_width:?}"
            );
            assert!(
                popup.right() <= window_width - px(POPUP_MARGIN),
                "so it is pulled back inside the window: {popup:?} in {window_width:?}"
            );
            assert!(
                popup.left() >= px(POPUP_MARGIN - 1.),
                "and never off the other side: {popup:?}"
            );
            assert_eq!(
                popup.size.width,
                window.find("completion-popup").bounds().size.width,
                "its width is its rows', not the room left over"
            );
        });
    }

    /// The popup is as wide as its widest row — the whole list's, so walking it never
    /// resizes it — and past the ceiling a description is what gives.
    #[gpui_kit::test]
    fn the_popup_is_as_wide_as_its_own_rows(cx: &mut TestAppContext) {
        let f = typed_footer(cx, "/");
        let short = f.act(cx, |window, cx| {
            window.render_frame(cx);
            popup_width(window)
        });

        // A description long enough to need a wider popup, and short enough to fit under
        // the ceiling: the popup grows to hold it rather than truncating it, and the
        // server's own words are drawn whole.
        let long = "ask the agent to refine what it remembers";
        f.act(cx, |_window, cx| {
            f.set_commands(
                vec![
                    Candidate {
                        name: "global-memory".to_string(),
                        description: long.to_string(),
                    },
                    Candidate {
                        name: "help".to_string(),
                        description: "commands and keys".to_string(),
                    },
                ],
                cx,
            )
        });
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            let wide = popup_width(window);
            assert!(
                wide > short,
                "a longer row is a wider popup: {wide:?} vs {short:?}"
            );
            assert!(wide <= px(POPUP_MAX_W), "and no wider than the ceiling");
            // The description is drawn whole: its own box is as wide as the text is.
            let drawn = window.find("completion-description-0").bounds().size.width;
            let text = text_width(long, POPUP_FONT, FontWeight::default(), window);
            assert!(
                drawn >= text - px(1.),
                "the server's own words are not cut: {drawn:?} vs {text:?}"
            );
        });

        // Past the ceiling there is nothing left to give but the words themselves: the
        // popup stops at the ceiling and the description is what truncates.
        let enormous = "x".repeat(400);
        f.act(cx, |_window, cx| {
            f.set_commands(
                vec![Candidate {
                    name: "helped".to_string(),
                    description: enormous.clone(),
                }],
                cx,
            )
        });
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert_eq!(
                popup_width(window),
                px(POPUP_MAX_W),
                "the popup stops at its ceiling"
            );
            let drawn = window.find("completion-description-0").bounds().size.width;
            let text = text_width(&enormous, POPUP_FONT, FontWeight::default(), window);
            assert!(
                drawn < text,
                "and the row it cannot hold is truncated: {drawn:?} vs {text:?}"
            );
        });

        // A list whose widest row is not the first: walking it does not resize it.
        f.act(cx, |_window, cx| {
            f.set_commands(
                vec![
                    Candidate {
                        name: "new".to_string(),
                        description: "start a new session".to_string(),
                    },
                    Candidate {
                        name: "global-memory".to_string(),
                        description: long.to_string(),
                    },
                ],
                cx,
            )
        });
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            let before = popup_width(window);
            window.press("down", cx);
            window.render_frame(cx);
            assert_eq!(
                popup_width(window),
                before,
                "the width is the list's, whatever row is highlighted"
            );
            assert_eq!(popup_index(&f, cx).map(|(index, _)| index), Some(1));
        });
    }

    /// With no room over the caret — the box at the top of the window — the popup goes
    /// under it instead of hanging off the window's edge.
    #[gpui_kit::test]
    fn a_popup_with_no_room_above_stands_under_the_caret(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |_window, cx| f.set_catalog(cx));
        f.act(cx, |window, cx| f.type_draft("/", window, cx));
        f.answer(cx);

        f.act(cx, |window, cx| {
            window.render_frame(cx);
            let (caret, _) = f
                .composer
                .read(cx)
                .input
                .read(cx)
                .cursor_layout()
                .expect("the caret is laid out");
            let popup = window.find("completion-popup").bounds();
            assert!(
                popup.top() >= caret.bottom(),
                "the popup is under the caret when there is nothing over it: \
                 {popup:?} vs {caret:?}"
            );
            assert!(popup.bottom() <= px(PANE.height.into()), "{popup:?}");
        });
    }

    #[gpui_kit::test]
    fn the_word_is_completed_and_only_a_whole_command_is_one(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |_window, cx| f.set_catalog(cx));
        f.act(cx, |window, cx| f.type_draft("/he", window, cx));
        f.answer(cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert_eq!(popup_labels(&f, cx), vec!["/help"]);
        });

        // Tab takes the word — and takes nothing else with it: nothing is sent.
        f.act(cx, |window, cx| window.press("tab", cx));
        assert_eq!(f.draft_now(cx), "/help ");
        assert_eq!(
            cx.read(|cx| f.caret(cx)),
            6,
            "the caret is past the word it took"
        );
        assert!(
            !f.events().iter().any(sends_the_message),
            "taking a row is not sending: {:?}",
            f.events()
        );

        // Enter sends it, and one slash command from the message's start is the
        // command layer's, not the coordinator's.
        f.act(cx, |window, cx| window.press("enter", cx));
        assert_eq!(
            f.events()
                .iter()
                .filter(|event| !is_a_question(event))
                .collect::<Vec<_>>(),
            vec![&ComposerEvent::Command {
                name: "help".to_string(),
                args: String::new(),
            }]
        );
    }

    #[gpui_kit::test]
    fn enter_takes_a_row_instead_of_sending_while_the_popup_is_up(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |_window, cx| f.set_catalog(cx));
        f.act(cx, |window, cx| f.type_draft("/lo", window, cx));
        f.answer(cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert_eq!(
                popup_labels(&f, cx),
                vec!["/lore", "/reload"],
                "the word's own prefix first, then the names it is a subsequence of"
            );
        });

        f.act(cx, |window, cx| window.press("enter", cx));
        assert_eq!(f.draft_now(cx), "/lore ", "Enter took the highlighted row");
        assert!(
            !f.events().iter().any(sends_the_message),
            "and the draft was not sent instead: {:?}",
            f.events()
        );
    }
    #[gpui_kit::test]
    fn the_arrows_walk_the_rows_and_wrap(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |_window, cx| f.set_catalog(cx));
        // A prompt this tab sent, so ↑ would have something to recall: with the
        // popup up, it walks rows instead.
        f.send_prompt("an earlier prompt", cx);
        f.act(cx, |window, cx| f.type_draft("/", window, cx));
        f.answer(cx);

        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert_eq!(popup_index(&f, cx), Some((0, 5)), "the first row to start");
            window.press("down", cx);
            assert_eq!(popup_index(&f, cx), Some((1, 5)));
            window.press("up", cx);
            assert_eq!(popup_index(&f, cx), Some((0, 5)));
            window.press("up", cx);
            assert_eq!(popup_index(&f, cx), Some((4, 5)), "↑ off the top wraps");
            window.press("down", cx);
            assert_eq!(popup_index(&f, cx), Some((0, 5)), "and ↓ off the end");
        });
        assert_eq!(f.draft_now(cx), "/", "the caret stayed on its word");
    }

    #[gpui_kit::test]
    fn escape_puts_the_popup_away_and_does_not_interrupt(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |_window, cx| f.set_catalog(cx));
        f.act(cx, |window, cx| f.type_draft("/", window, cx));
        f.answer(cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert!(window.find("completion-popup").visible());
        });

        f.act(cx, |window, cx| window.press("escape", cx));
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert!(!popup_drawn(window), "Esc puts the popup away");
        });
        assert!(
            !f.events().iter().any(sends_the_message),
            "closing the popup is not the coordinator's interrupt: {:?}",
            f.events()
        );

        // The word moving on is a new word, and asks again.
        f.act(cx, |window, cx| f.type_draft("h", window, cx));
        f.answer(cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert!(popup_drawn(window));
            assert_eq!(popup_labels(&f, cx), vec!["/help"]);
        });
    }

    /// The question is the caret's own text and where it is in it, asked once, after
    /// the caret has rested — and an answer about a text the caret has left is not a
    /// row here, however many candidates it carries.
    #[gpui_kit::test]
    fn the_caret_is_asked_about_once_and_a_stale_answer_raises_no_row(cx: &mut TestAppContext) {
        let f = open_footer(cx);
        f.act(cx, |_window, cx| f.set_catalog(cx));
        f.act(cx, |window, cx| f.type_draft("/eval (zz", window, cx));
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert!(
                popup_labels(&f, cx).is_empty(),
                "the server has not answered yet"
            );
        });
        assert!(
            f.events().is_empty(),
            "and nothing has been asked yet: the caret has to rest on the word"
        );

        // The debounce is what asks, once: the caret's text, and where it is in it.
        cx.executor().advance_clock(COMPLETE_DEBOUNCE);
        cx.run_until_parked();
        assert_eq!(
            f.events(),
            vec![ComposerEvent::Complete {
                text: "/eval (zz".to_string(),
                cursor: 9,
            }]
        );
        // Nothing more is asked while that question is out.
        f.act(cx, |_window, cx| {
            f.composer.update(cx, |composer, cx| composer.refresh(cx))
        });
        cx.executor().advance_clock(COMPLETE_DEBOUNCE);
        cx.run_until_parked();
        assert_eq!(f.events().len(), 1, "one question at a time");

        // The answer names the token, and its rows are the popup's: what the image
        // calls them, and what they are.
        f.act(cx, |_window, cx| {
            f.composer.update(cx, |composer, cx| {
                composer.set_completion(
                    "/eval (zz",
                    9,
                    Answer {
                        kind: Some(CompletionKind::Symbol),
                        name: 7..9,
                        items: vec![Candidate {
                            name: "zzz".to_string(),
                            description: "function".to_string(),
                        }],
                    },
                    cx,
                )
            })
        });
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert_eq!(popup_labels(&f, cx), vec!["zzz"]);
            assert!(
                window.find("completion-row-0").label().is_some(),
                "the row is drawn, and reads as a row"
            );
        });

        // An answer about a text this is not — the caret has left that one — raises
        // nothing, and leaves the word it *is* about alone.
        f.act(cx, |_window, cx| {
            f.composer.update(cx, |composer, cx| {
                composer.set_completion(
                    "/eval (z",
                    8,
                    Answer {
                        kind: Some(CompletionKind::Symbol),
                        name: 7..8,
                        items: vec![Candidate {
                            name: "car".to_string(),
                            description: "function".to_string(),
                        }],
                    },
                    cx,
                )
            })
        });
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert!(
                popup_labels(&f, cx).is_empty(),
                "an answer about another text raises no row: {:?}",
                popup_labels(&f, cx)
            );
        });
    }

    /// An answer can land before the keystroke's own timer is due — the caret's text is
    /// answered, and the box has no reason to ask again. The timer firing afterwards is
    /// not a reason either: what is on screen is the answer this caret is waiting for.
    #[gpui_kit::test]
    fn an_answer_that_beats_the_debounce_is_kept(cx: &mut TestAppContext) {
        let f = open_footer(cx);
        f.act(cx, |_window, cx| f.set_catalog(cx));
        f.act(cx, |window, cx| f.type_draft("/eval (zz", window, cx));
        // The answer, before the caret's rest is up — what the tab hands back for a word
        // the server has already answered, or one it cannot ask about at all.
        f.act(cx, |_window, cx| {
            f.composer.update(cx, |composer, cx| {
                composer.set_completion(
                    "/eval (zz",
                    9,
                    Answer {
                        kind: Some(CompletionKind::Symbol),
                        name: 7..9,
                        items: vec![Candidate {
                            name: "zzz".to_string(),
                            description: "function".to_string(),
                        }],
                    },
                    cx,
                )
            })
        });
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert_eq!(popup_labels(&f, cx), vec!["zzz"]);
        });

        // The timer that keystroke left behind comes due: it asks nothing — this caret
        // has its answer — and the rows stay where they are.
        cx.executor().advance_clock(COMPLETE_DEBOUNCE * 2);
        cx.run_until_parked();
        assert!(
            f.events().is_empty(),
            "an answered caret is not asked about twice: {:?}",
            f.events()
        );
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert_eq!(popup_labels(&f, cx), vec!["zzz"]);
        });
    }

    #[gpui_kit::test]
    fn recalled_history_raises_no_popup_until_it_is_edited(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |_window, cx| f.set_catalog(cx));
        // A prompt this tab sent that *would* raise a popup as new input: the word
        // `he` mid-message. It is sent with Esc first, which is what a reader does
        // when they mean the line rather than the row.
        f.act(cx, |window, cx| f.type_draft("please /he", window, cx));
        f.answer(cx);
        f.act(cx, |window, cx| {
            window.press("escape", cx);
            window.press("enter", cx);
        });
        assert!(
            matches!(f.events().last(), Some(ComposerEvent::Send(_))),
            "the same text is sent as the reader's words: {:?}",
            f.events()
        );

        // Recalled, it asks for nothing: a suggestion list is what new input asks
        // for, and ↑ would have no way back out of a popup that captured it.
        f.act(cx, |window, cx| window.press("up", cx));
        assert_eq!(f.draft_now(cx), "please /he", "↑ recalled the prompt");
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert!(
                !popup_drawn(window),
                "what history put in the input asks for nothing"
            );
        });

        // Editing it makes the input the reader's own again, and the word completes.
        f.act(cx, |window, cx| f.type_draft("l", window, cx));
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert!(popup_drawn(window));
            assert_eq!(popup_labels(&f, cx), vec!["/help"]);
        });
    }
}

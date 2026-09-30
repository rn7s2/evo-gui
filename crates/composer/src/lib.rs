//! composer — the box under the transcript (`Workspace.css`'s `.composer-box`): the
//! coordinator's input, the todo strip and the drawers that fold out of it, the chips
//! that state what the selected agent is working with, and the one Send/Stop button.
//!
//! The box sits on the transcript's reading measure, at the foot of the conversation
//! column, so what is written lines up with what is read. Everything inside it is the
//! *selected agent's own* state (CONTRACT §4.2): the chips are the topic's status
//! segments in the server's words and order, the todos are the topic's todos, and the
//! model drawer ticks the model the topic reports. Nothing is composed here, and
//! nothing survives a selection — another agent is another set of facts.
//!
//! The effort ladder is the server's too (`catalog`'s `thinking_levels`, §5.6): a
//! client never writes the rungs down.
//!
//! Two of the design's controls are the coordinator's alone. `model.set` and
//! `thinking.set` act on the session (CONTRACT §5), and a lane's model is the swarm's,
//! fixed when it starts — so a lane's drawer states what that lane runs and says that
//! this box is not what changes it. Nothing is offered that the server would refuse.
//!
//! The composer does no I/O. It emits [`ComposerEvent`] and the owner posts the
//! request, then reports the outcome with [`Composer::request_finished`]: that is what
//! keeps a failed send's draft alive, and what keeps the button disabled only while
//! its own request is in flight.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::base::input::Position;
use gpui_kit::base::TextSelection;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{InputEvent, Textarea, TextareaState},
    v_flex, ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, Size, StyledExt as _,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, px, radians, Animation, AnimationExt as _, AnyElement, App, Bounds, BoxShadow,
    ClipboardItem, Context, ElementId, Entity, EventEmitter, FocusHandle, Global, IntoElement,
    KeyBinding, Keystroke, KeystrokeEvent, Pixels, Point, Render, ScrollHandle, SharedString,
    Subscription, TestSupportExt as _, WeakEntity, Window,
};
use session::{ordered_segments, Segment, Todo, TodoStatus, TopicState};
use store::design::{self, Palette, INSET, MEASURE, RADIUS};
use widgets::effort::cubic_bezier;
use widgets::{paint, Chip, EffortSlider};

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

/// The box's own furniture: `12px` radius, one hairline, and the shadow under it
/// (`.composer-box`).
const BOX_RADIUS: Pixels = px(12.);
const BOX_BORDER: Pixels = px(1.);
/// The focus ring: `box-shadow: 0 0 0 3px color-mix(in srgb, var(--primary) 12%,
/// transparent)`, and the border it comes with — `55%` of the primary into the
/// border.
const RING: Pixels = px(3.);
const RING_MIX: f32 = 12.;
const FOCUS_BORDER_MIX: f32 = 55.;
/// The ink the box's own shadow is drawn in: `rgba(60,40,10,.05)`.
const SHADOW_INK: paint::Rgb = paint::Rgb::new(0x3C, 0x28, 0x0A);

/// The action button's height, and the row it shares with the chips
/// (`.composer-send{height:28px}`, `.composer-foot{padding:6px 8px 8px 10px}`).
const ACTION_HEIGHT: Pixels = px(28.);
const ACTION_RADIUS: Pixels = px(RADIUS);
const FOOT_GAP: Pixels = px(6.);
const FOOT_PAD: (f32, f32, f32, f32) = (6., 8., 8., 10.);

/// The todo strip's rows: a 32px title row, and a list that scrolls past 156px
/// (`.todo-strip-row`, `.todo-strip-list{max-height:156px}`).
const STRIP_ROW: Pixels = px(32.);
const STRIP_LIST_MAX: Pixels = px(156.);
/// One todo, its box, and the box's own 14px frame with its 3px radius.
const TODO_ROW: Pixels = px(18.);
const TODO_BOX: Pixels = px(14.);
const TODO_BOX_RADIUS: Pixels = px(3.);
const TODO_BOX_INSET: Pixels = px(6.);
/// The chevron's turn when the todo strip is folded: `.chev{transition:transform
/// .12s ease}`.
const CHEVRON_TURN: std::time::Duration = std::time::Duration::from_millis(120);

/// The drawer's rows and its model items.
const DRAWER_ROW: Pixels = px(32.);
const DRAWER_ITEM: Pixels = px(30.);
const DRAWER_ITEM_RADIUS: Pixels = px(RADIUS);
const DRAWER_EFFORT_ROW: Pixels = px(36.);
const DRAWER_LABEL_MIN: Pixels = px(112.);

/// Where an item's hover and its chosen fill come from: the ink a few percent into
/// the surface the drawer sits on (`--sidebar`), as the design's rows do it.
const ITEM_HOVER_MIX: f32 = 6.;
const ITEM_CHOSEN_MIX: f32 = 9.;

/// The input's own type: `.composer-box textarea{font-size:14px;line-height:20px}`
/// — a size of its own, not the theme's base, and the line the autogrow counts in.
const INPUT_FONT: Pixels = px(14.);
const INPUT_LINE: Pixels = px(20.);

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
    /// The drawer this chip opens, when it opens one.
    opens: Option<Drawer>,
}

/// The fold-out a chip opens, inside the box (`.drawer`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Drawer {
    Model,
    Goal,
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
    /// The goal, for the drawer's body: its status and the objective in full.
    goal: Option<(String, String)>,
}

/// What the composer asks its owner to do. Each is one op the owner sends
/// (`session::OpRequest`): the composer names the action, never the endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposerEvent {
    /// Post this text as the coordinator's turn. It lands at the running turn's next
    /// boundary, so text sent while the agent works is queued, not lost. The op is
    /// `input.send`.
    Send(String),
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
}

/// What the one button says — and therefore what clicking it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionFace {
    Send,
    /// The swarm is busy or held while its lanes work: the one action that stops it all.
    StopSwarm,
}

impl ActionFace {
    /// The button's visible label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Send => "Send",
            Self::StopSwarm => "\u{25a0} Stop swarm",
        }
    }

    /// The glyph leading the label: an up arrow to send. Stop's square is part of its
    /// label instead — the icon set the app bundles carries no plain square, and the
    /// screen's own glyph draws one at the label's own size.
    pub fn icon(self) -> Option<IconName> {
        match self {
            Self::Send => Some(IconName::ArrowUp),
            Self::StopSwarm => None,
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

/// Whether one-shot bindings have been installed (the keys are global to the app).
struct KeysBound;
impl Global for KeysBound {}

/// The chips the topic's own segments make (CONTRACT §4.2).
///
/// The server's registry built them, so this walks them and folds the two that belong
/// together: `thinking` is the dim half of the `model` chip — `stub-a medium`, the way
/// the design draws it. A `goal` segment opens the goal drawer, when the topic
/// carries the goal itself; everything else is a chip of its own, in the server's
/// order. Right-hand segments are the swarm's own summary (`2 lanes`), which the lane
/// column already states, so they are not repeated here.
fn chips_of(segments: &[Segment], goal: Option<&(String, String)>) -> Vec<ChipFact> {
    let has_goal = goal.is_some();
    let (left, _right) = ordered_segments(segments);
    let effort = left
        .iter()
        .find(|segment| segment.name == "thinking")
        .map(|segment| SharedString::from(segment.text.clone()));
    let has_model = left.iter().any(|segment| segment.name == "model");
    let mut chips = Vec::new();
    for segment in left {
        let name = segment.name.as_str();
        // The effort rides on the model chip; with no model to ride on it is a fact
        // of its own rather than a dropped one.
        if name == "thinking" && has_model {
            continue;
        }
        let opens = match name {
            "model" => Some(Drawer::Model),
            "goal" if has_goal => Some(Drawer::Goal),
            _ => None,
        };
        chips.push(ChipFact {
            name: segment.name.clone(),
            text: SharedString::from(segment.text.clone()),
            dim: (name == "model").then(|| effort.clone()).flatten(),
            opens,
        });
    }
    // A goal the topic's state carries is a goal even when the status line has no
    // segment for it — a state without a goal is the only thing that means there is
    // no goal (§4.2). The chip is the design's own: `goal`, its status beside it in
    // the dim half, opening the drawer that holds the objective.
    if let Some((status, _)) = goal.filter(|_| !chips.iter().any(|chip| chip.name == "goal")) {
        chips.push(ChipFact {
            name: "goal".to_string(),
            text: SharedString::from("goal"),
            dim: Some(SharedString::from(format!("({status})"))),
            opens: Some(Drawer::Goal),
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
    /// The fold-out that is open, if any.
    drawer: Option<Drawer>,
    /// Whether the todo list is unfolded.
    todos_open: bool,
    /// Whether the chevron was drawn pointing up the last time it was drawn, and
    /// how many times it has turned: `.chev{transition:transform .12s ease}` needs
    /// to know what it is turning from, and a new id to turn under.
    chevron_up: bool,
    chevron_turns: u64,
    /// The swarm's own busy flag: what the action button's face follows.
    busy: bool,
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
    /// The todo list's own scroll position, kept across frames.
    todos_scroll: ScrollHandle,
    /// Where the box was painted last frame: what "outside the box" is measured
    /// against when the page folds an open drawer on a press (`useOutsideClose`).
    box_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// The rail's keyboard focus, so the arrows move the effort while it is held.
    effort_focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

/// One model the drawer offers, as `GET /catalog` describes it (§5.6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelRow {
    pub id: String,
    pub provider: String,
    /// The dim line under the name: the context window and what else evo reports.
    pub detail: String,
    /// Whether evo can reach it right now; the rest are listed with why not.
    pub reason: Option<String>,
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
            drawer: None,
            todos_open: false,
            chevron_up: false,
            chevron_turns: 0,
            busy: false,
            in_flight: false,
            history: Vec::new(),
            walking: None,
            recalled: String::new(),
            pending_send: None,
            room: px(320.),
            todos_scroll: ScrollHandle::new(),
            box_bounds: Rc::new(Cell::new(Bounds::default())),
            effort_focus: cx.focus_handle(),
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
        let goal = state
            .goal
            .as_ref()
            .map(|goal| (goal.status.clone(), goal.objective.clone()));
        let agent = Agent {
            chips: chips_of(&state.segments, goal.as_ref()),
            todos: state.todos.clone(),
            model: state
                .model
                .as_ref()
                .map(|model| (model.id.clone(), model.provider.clone())),
            thinking: state.thinking.clone(),
            goal,
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

    /// The models the drawer offers, and the effort ladder `thinking.set` accepts:
    /// both are the server's (`GET /catalog`, §5.6), and both come in one call.
    pub fn set_catalog(
        &mut self,
        levels: Vec<String>,
        models: Vec<ModelRow>,
        cx: &mut Context<Self>,
    ) {
        let levels: Vec<SharedString> = levels.into_iter().map(SharedString::from).collect();
        if self.levels != levels || self.models != models {
            self.levels = levels;
            self.models = models;
            cx.notify();
        }
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

    /// The strip's chevron: the design's one glyph — an up chevron — drawn at 12px
    /// and turned over the design's 120ms when the strip is folded (`up` is 0°, the
    /// folded state the 180° the CSS rotates it to).
    fn chevron(&self, open: bool) -> AnyElement {
        // Up is the open strip: 0°. Folded is the 180° the CSS turns it to.
        let angle = |up: bool| if up { 0. } else { std::f32::consts::PI };
        let icon = Icon::new(IconName::ChevronUp).size(px(12.));
        if self.chevron_up == open {
            return icon.rotate(radians(angle(open))).into_any_element();
        }
        let (from, to) = (angle(self.chevron_up), angle(open));
        let turn = Animation::new(CHEVRON_TURN).with_easing(cubic_bezier(0.25, 0.1, 0.25, 1.));
        icon.with_animation(
            ("todo-chevron", self.chevron_turns),
            turn,
            move |icon, t: f32| icon.rotate(radians(from + (to - from) * t)),
        )
        .into_any_element()
    }

    /// Fold the open drawer back if `at` is a press outside the box, which is what
    /// the design's `pointerdown` listener on the document does. The page asks this
    /// on every press it sees, so a press on the transcript, the lanes or the band
    /// folds it, and one in the box — on the input, a chip, the strip, the drawer
    /// itself — leaves it.
    pub fn close_drawer_at(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        if self.box_bounds.get().contains(&at) {
            return;
        }
        self.close_drawer(cx);
    }

    /// Fold the open drawer back, as selecting another agent does.
    pub fn close_drawer(&mut self, cx: &mut Context<Self>) {
        if self.drawer.take().is_some() {
            cx.notify();
        }
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

    /// Report the outcome of this composer's own request.
    ///
    /// `ok` clears the draft — the input is emptied only after the server took the
    /// text. A failed send (or an interrupt) leaves it alone.
    pub fn request_finished(&mut self, ok: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.in_flight = false;
        if ok {
            self.input
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        cx.notify();
    }

    /// The button's face: `Send` while nothing is going on, and `Stop swarm` while the
    /// swarm is busy or held for its lanes — the one action that means the whole swarm
    /// (CONTRACT §7.5). The per-lane Stop lives in the lane column, where a lane is
    /// named.
    pub fn face(&self) -> ActionFace {
        if self.busy {
            ActionFace::StopSwarm
        } else {
            ActionFace::Send
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

    /// Emit `Send` for a non-blank draft, unless a request is already in flight.
    fn send(&mut self, draft: String, cx: &mut Context<Self>) {
        if self.in_flight || draft.trim().is_empty() {
            return;
        }
        self.in_flight = true;
        // A prompt is remembered the moment it is sent, not when the server takes
        // it: the reader's ↑ should bring back what they just sent even if the
        // request is still on its way.
        self.remember(&draft);
        self.walking = None;
        cx.emit(ComposerEvent::Send(draft));
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

    /// The todo strip: the title row, always, and the list under it while it is open.
    ///
    /// An agent with no todos has no strip — `Todos 0/0` over nothing is a row of
    /// chrome that says only that there is nothing to say.
    fn todo_strip(&self, palette: &'static Palette, cx: &Context<Self>) -> Option<AnyElement> {
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
        let stripe = paint::color(palette.sidebar);
        let ink = paint::color(palette.muted_fg);
        let mut strip = v_flex()
            .id("todo-strip")
            .test_support()
            .w_full()
            .flex_none()
            .bg(stripe)
            .border_b_1()
            .border_color(paint::color(palette.border))
            .child(
                h_flex()
                    .id("todo-strip-row")
                    .test_support()
                    .aria_label(SharedString::from(format!(
                        "Todos {done}/{}",
                        self.agent.todos.len()
                    )))
                    .h(STRIP_ROW)
                    .w_full()
                    .items_center()
                    .gap(px(10.))
                    .pl(px(14.))
                    .pr(px(10.))
                    // The design keeps the arrow over every control of its own — a
                    // macOS app's chrome does not turn the pointer into a hand
                    // (`.todo-strip-row{cursor:default}`) — and only the effort
                    // slider, a control the pointer does track, asks for one.
                    .cursor_default()
                    .text_size(STRIP_FONT)
                    // `.todo-strip-row:hover{color:var(--fg)}`: the row's own words
                    // take the ink; the count is already the ink.
                    .text_color(ink)
                    .hover(move |row| row.text_color(paint::color(palette.fg)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.toggle_todos(cx);
                    }))
                    .child(
                        div()
                            .flex_none()
                            .font_medium()
                            .text_color(paint::color(palette.fg))
                            .child(SharedString::from(format!(
                                "Todos {done}/{}",
                                self.agent.todos.len()
                            ))),
                    )
                    .child(
                        div()
                            .ml_auto()
                            .flex_none()
                            .text_color(paint::color(palette.muted_fg))
                            .child(self.chevron(open)),
                    ),
            );
        if open {
            strip = strip.child(
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
            );
        }
        Some(strip.into_any_element())
    }

    /// The drawer a chip opened, folded out inside the box under the todo row.
    fn drawer_panel(
        &self,
        palette: &'static Palette,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let drawer = self.drawer?;
        let title: AnyElement = match drawer {
            Drawer::Model => h_flex()
                .gap(px(6.))
                .child(SharedString::from(format!("{} model", self.name)))
                .into_any_element(),
            Drawer::Goal => {
                let status = self
                    .agent
                    .goal
                    .as_ref()
                    .map(|(status, _)| status.clone())
                    .unwrap_or_default();
                h_flex()
                    .gap(px(6.))
                    .child("Goal")
                    .child(
                        div()
                            .text_color(paint::color(palette.muted_fg))
                            .child(SharedString::from(format!("({status})"))),
                    )
                    .into_any_element()
            }
        };
        let body: AnyElement = match drawer {
            Drawer::Model => self.model_body(palette, window, cx),
            Drawer::Goal => self.goal_body(palette).into_any_element(),
        };
        Some(
            v_flex()
                .id("composer-drawer")
                .test_support()
                .w_full()
                .flex_none()
                .bg(paint::color(palette.sidebar))
                .border_b_1()
                .border_color(paint::color(palette.border))
                .child(
                    // The title row folds the drawer back, the way the todo row does.
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
                                .font_medium()
                                .text_color(paint::color(palette.fg))
                                .child(title),
                        )
                        .child(
                            div()
                                .ml_auto()
                                .flex_none()
                                .child(Icon::new(IconName::ChevronDown).size(px(12.))),
                        ),
                )
                .child(body)
                .into_any_element(),
        )
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
            let hover = paint::color(paint::mix(palette.fg, ITEM_HOVER_MIX, palette.sidebar));
            let active = paint::color(paint::mix(palette.fg, ITEM_CHOSEN_MIX, palette.sidebar));
            let mut row = h_flex()
                .id(ElementId::from(format!("drawer-model-{}", model.id)))
                .test_support()
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
                        .gap(px(5.))
                        .child(
                            div()
                                .text_color(paint::color(palette.muted_fg))
                                .child(format!("{} ·", model.provider)),
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
                        .child(SharedString::from(model.detail.clone())),
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
                    .hover(move |row| row.bg(hover))
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
        body = body.children(items);
        body.child(self.effort_row(palette, window, cx))
            .into_any_element()
    }

    /// The effort row: the level's name, and the rail that changes it.
    fn effort_row(
        &self,
        palette: &'static Palette,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let level = self.agent.thinking.clone().unwrap_or_default();
        let label = format!("Effort {level}");
        let row = h_flex()
            .id("drawer-effort")
            .test_support()
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
                div()
                    .flex_none()
                    .min_w(DRAWER_LABEL_MIN)
                    .child(SharedString::from(label.clone())),
            );
        if self.levels.is_empty() {
            // The server published no ladder: say what the agent runs and change
            // nothing, rather than draw rungs a client made up.
            return row.into_any_element();
        }
        if !self.settable {
            // A lane's effort belongs to the swarm, as its model does.
            return row.into_any_element();
        }
        let levels = self.levels.clone();
        let index = levels
            .iter()
            .position(|name| name.as_str() == level)
            .unwrap_or(0);
        let weak = cx.entity().downgrade();
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
                    )
                    .palette(palette)
                    .focus(self.effort_focus.clone())
                    .on_change(move |level: usize, _, cx: &mut App| {
                        if let Some(composer) = weak.upgrade() {
                            composer.update(cx, |this, cx| {
                                if let Some(name) =
                                    this.levels.get(level).map(|name| name.to_string())
                                {
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

    /// The goal drawer: the objective in full, under its status.
    fn goal_body(&self, palette: &'static Palette) -> impl IntoElement {
        let objective = self
            .agent
            .goal
            .as_ref()
            .map(|(_, objective)| objective.clone())
            .unwrap_or_default();
        div()
            .id("drawer-goal-text")
            .test_support()
            .px(px(8.))
            .pb(px(4.))
            .text_size(ITEM_FONT)
            .text_color(paint::color(palette.fg))
            .child(SharedString::from(objective))
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
        let open = self.drawer;
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
            if let Some(drawer) = chip.opens {
                let weak = weak.clone();
                pill = pill.open(open == Some(drawer)).interactive(move |_, cx| {
                    // A chip is a button: clicking the open one folds the drawer
                    // back, clicking the other one switches to it.
                    if let Some(composer) = weak.upgrade() {
                        composer.update(cx, |this, cx| {
                            if this.drawer == Some(drawer) {
                                this.close_drawer(cx);
                            } else {
                                this.drawer = Some(drawer);
                                cx.notify();
                            }
                        });
                    }
                });
            }
            slot = slot.child(pill.render());
            row = row.child(slot);
        }
        row.child(div().flex_1())
            .child(self.action_button(cx))
            .into_any_element()
    }

    fn action_button(&self, cx: &Context<Self>) -> impl IntoElement {
        let face = self.face();
        let mut button = Button::new(BUTTON_ID)
            .h(ACTION_HEIGHT)
            .px(px(12.))
            .rounded(ACTION_RADIUS)
            .text_size(px(13.))
            .label(face.label())
            // The glyph is decoration: what the button is called is the word.
            .accessibility_label(face.label())
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
            }));
        if let Some(icon) = face.icon() {
            button = button.icon(icon);
        }

        // One button, the design's own: primary in both faces, because it is the
        // same button with a different word on it (`.composer-send`).
        button.primary()
    }
}

/// How many of the input's rows fit in `room`, with the design's floor.
fn rows_for(room: Pixels) -> usize {
    // A row of this input is one line of its own type (`INPUT_LINE`): the kit grows
    // in rows, and its row is the line the text is set on. The row box carries the
    // textarea's own padding (`input_py`, 10px a side) on top of the rows, which is
    // the whole of the difference between this cap and the design's — the design
    // measures pixels, so its half-pane includes that padding and this one is at
    // most one line over it.
    ((f32::from(room) / f32::from(INPUT_LINE)).floor() as usize).max(MIN_ROWS)
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
        let input_background = cx.theme().input_background();

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

        let strip = self.todo_strip(palette, cx);
        let drawer = self.drawer_panel(palette, window, cx);
        let foot = self.foot(palette, cx);

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
                    .bg(input_background)
                    .shadow(shadows)
                    .overflow_hidden()
                    // Esc, with the caret anywhere in the box, is the coordinator's
                    // own interrupt (the TUI's).
                    .key_context(KEY_CONTEXT)
                    .on_action(cx.listener(Self::interrupt_action))
                    .children(strip)
                    .children(drawer)
                    .child(
                        div().w_full().min_w_0().child(
                            Textarea::new(&self.input)
                                .with_size(Size::Large)
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
                    "cache-stats" => "97% cached".to_string(),
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

    /// The models a catalog would list, one of them chosen already.
    fn catalog_models() -> Vec<ModelRow> {
        vec![
            ModelRow {
                id: "stub-a".to_string(),
                provider: "openai".to_string(),
                detail: "200k ctx · vision · effort".to_string(),
                reason: None,
            },
            ModelRow {
                id: "stub-b".to_string(),
                provider: "openai".to_string(),
                detail: "936k ctx".to_string(),
                reason: None,
            },
        ]
    }

    /// The ladder evo's own registration declares (CONTRACT §5.6).
    fn catalog_levels() -> Vec<String> {
        ["low", "medium", "high", "xhigh", "max"]
            .iter()
            .map(|level| level.to_string())
            .collect()
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

        /// The server's catalog: the effort ladder and the models the drawer offers.
        fn set_catalog(&self, cx: &mut App) {
            self.composer.update(cx, |composer, cx| {
                composer.set_catalog(catalog_levels(), catalog_models(), cx)
            });
        }

        /// Focus the input the way a user does, then type into it.
        fn type_draft(&self, text: &str, window: &mut Window, cx: &mut App) {
            window.click(self.input_frame(cx), cx);
            window.input(text, cx);
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

    #[test]
    fn each_face_has_a_label_and_its_own_glyph() {
        use gpui_kit::component::IconNamed as _;

        assert_eq!(ActionFace::Send.label(), "Send");
        assert_eq!(ActionFace::StopSwarm.label(), "\u{25a0} Stop swarm");
        assert_eq!(
            ActionFace::Send.icon().map(|icon| icon.path().to_string()),
            Some("icons/arrow-up.svg".to_string()),
            "Send leads with an up arrow"
        );
        assert!(
            ActionFace::StopSwarm.icon().is_none(),
            "Stop's square is a glyph in its label"
        );
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

    #[gpui_kit::test]
    fn the_one_button_follows_the_activity_and_is_never_send_and_stop(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find(BUTTON_ID).label(), Some("Send"));

            f.busy(true, cx);
            window.render_frame(cx);
            assert_eq!(window.find(BUTTON_ID).label(), Some("\u{25a0} Stop swarm"));

            f.busy(true, cx);
            window.render_frame(cx);
            assert_eq!(window.find(BUTTON_ID).label(), Some("\u{25a0} Stop swarm"));

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

            assert_eq!(window.find(BUTTON_ID).label(), Some("\u{25a0} Stop swarm"));
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
    /// effort beside it, then the context, the cache and the goal, in the order the
    /// server published them — and the goal chip opens the goal drawer.
    #[gpui_kit::test]
    fn the_foot_row_is_the_topics_own_chips(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            let state = state_with(&["model", "thinking", "context", "cache-stats", "goal"]);
            f.set_agent(&state, cx);
            window.render_frame(cx);

            let model = window.find(chip_id("model"));
            let ctx = window.find(chip_id("context"));
            let cache = window.find(chip_id("cache-stats"));
            let goal = window.find(chip_id("goal"));
            assert!(model.visible() && ctx.visible() && cache.visible() && goal.visible());
            assert_eq!(
                model.label(),
                Some("stub-a high"),
                "the model, then the effort"
            );
            assert_eq!(ctx.label(), Some("ctx 48k/936k (5%)"));
            assert_eq!(cache.label(), Some("97% cached"));
            assert_eq!(goal.label(), Some("goal a1b2c3d4 (active) 12k/50k"));

            // One row, left to right, in the server's order.
            let x = |id: &str| window.find(chip_id(id)).bounds().left();
            assert!(x("model") < x("context") && x("context") < x("cache-stats"));
            assert!(x("cache-stats") < x("goal"));
            for id in ["model", "context", "cache-stats", "goal"] {
                assert_eq!(
                    window.find(chip_id(id)).bounds().size.height,
                    px(widgets::chip::HEIGHT)
                );
            }
        });
    }

    /// A goal the topic's state carries is a goal even when the status line has no
    /// segment for it: the chip appears last — the design's own `goal`, its status in
    /// the dim half — and opens the drawer holding the objective (§4.2).
    #[gpui_kit::test]
    fn a_goal_the_state_carries_is_a_chip_without_a_segment(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            let state = state_with(&["model", "thinking"]);
            assert!(
                state.goal.is_some(),
                "the topic's state carries the goal, and its status line does not"
            );
            f.set_agent(&state, cx);
            window.render_frame(cx);

            let goal = window.find(chip_id("goal"));
            assert!(goal.visible(), "the state's goal has a chip of its own");
            assert_eq!(goal.label(), Some("goal (active)"));
            assert!(
                goal.bounds().left() > window.find(chip_id("model")).bounds().right(),
                "last in the row, where the design draws it"
            );

            window.click(chip_id("goal"), cx);
            window.render_frame(cx);
            assert!(
                window.find("composer-drawer").visible(),
                "and opens the drawer the design puts the objective in"
            );
            assert!(window.find("drawer-goal-text").visible());
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
}

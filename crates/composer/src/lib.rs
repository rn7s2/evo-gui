//! composer — the right column of the tab page (§7.3): the coordinator's input,
//! and one status row under it carrying the session readout on the left and the
//! single Send/Stop button flush right.
//!
//! The composer does no I/O. It emits [`ComposerEvent`] and the owner posts the
//! request, then reports the outcome with [`Composer::request_finished`]: that
//! is what keeps a failed send's draft alive, and what keeps the button
//! disabled only while its own request is in flight.

use gpui_kit::base::input::Position;
use gpui_kit::base::TextSelection;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{InputEvent, Textarea, TextareaState},
    tooltip::Tooltip,
    v_flex, ActiveTheme as _, Disableable as _, IconName, Sizable as _, Size,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, px, App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, Global,
    InteractiveElement as _, IntoElement, KeyBinding, Keystroke, KeystrokeEvent,
    ParentElement as _, Pixels, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, TestSupportExt as _, WeakEntity, Window,
};
use session::Segment;

/// The input grows from two rows to eight; past that it scrolls.
const MIN_ROWS: usize = 2;
const MAX_ROWS: usize = 8;

/// The input is a card: a chat input, not a form field.
const INPUT_RADIUS: Pixels = px(10.);
const INPUT_BORDER: Pixels = px(1.);
/// The focus ring: a band around the card, in the focus colour, while the caret
/// is in the input. The band is a wash, not a second outline — the card marks
/// focus with one hairline, and this is the soft edge around it.
const FOCUS_RING: Pixels = px(3.);
const FOCUS_RING_INK: f32 = 0.12;

/// What the input is for, and the two keys that submit it.
///
/// The hint is a line of its own: a column this narrow cannot show the whole
/// sentence at once, and the placeholder of a multi-line input is drawn line by
/// line (`placeholder_line_runs` splits on newlines), so it stays readable here
/// instead of being clipped at the input's edge.
const PLACEHOLDER: &str =
    "Message the coordinator\u{2026}\n(Enter to send, Shift+Enter for newline)";

/// The action button's height: a compact control sharing the readout's line.
const ACTION_HEIGHT: Pixels = px(28.);
/// The readout's size: the TUI's dim status line, small enough to stay one line.
const READOUT_SIZE: Pixels = px(12.);
/// The width the readout's tooltip wraps to: the column's own width.
const READOUT_TOOLTIP_WIDTH: Pixels = px(340.);

/// Key context of the composer, so `Esc` reaches the composer even though the
/// textarea holds the focus and handles `Escape` first.
const KEY_CONTEXT: &str = "Composer";

/// How many prompts one composer remembers for its ↑/↓ history; the oldest fall
/// off past this. In memory, per tab: a composer is not a shell, and nothing
/// here is written down.
const HISTORY_LIMIT: usize = 64;

/// The action button's element id: one button, addressed by name.
pub const BUTTON_ID: &str = "composer-action";
/// The readout's element id; it also carries the full line as its tooltip.
pub const READOUT_ID: &str = "composer-readout";

gpui_kit::actions!(composer, [Interrupt]);

/// What the composer asks its owner to do. Each is one op the owner sends
/// (`session::OpRequest`): the composer names the action, never the endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposerEvent {
    /// Post this text as the coordinator's turn. It lands at the running turn's
    /// next boundary, so text sent while the agent works is queued, not lost.
    /// The op is `input.send`.
    Send(String),
    /// Stop the coordinator's own run (the TUI's esc) — `run.interrupt` with scope
    /// `session`. The draft is untouched.
    Interrupt,
    /// Stop the whole swarm — `run.interrupt` with scope `swarm`: every lane, and the
    /// coordinator with it (CONTRACT §7.5).
    StopSwarm,
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

    /// The glyph leading the label: an up arrow to send. Stop's square is part of
    /// its label instead — the icon set the app bundles carries no plain square,
    /// and the screen's own glyph draws one at the label's own size.
    pub fn icon(self) -> Option<IconName> {
        match self {
            Self::Send => Some(IconName::ArrowUp),
            Self::StopSwarm => None,
        }
    }
}

/// Where the caret goes when a prompt is recalled: the end of it, on the last
/// line. Columns are counted in characters, which is what the input's cursor
/// positions are.
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

/// Whether a keystroke is this platform's copy shortcut — the chord the input's
/// own `Copy` is bound to, and the one the window's `Copy` answers.
fn is_copy_shortcut(keystroke: &Keystroke) -> bool {
    if keystroke.key != "c" || keystroke.modifiers.shift || keystroke.modifiers.alt {
        return false;
    }
    #[cfg(target_os = "macos")]
    {
        keystroke.modifiers.platform
    }
    #[cfg(not(target_os = "macos"))]
    {
        keystroke.modifiers.control
    }
}

/// Marks the app-wide key binding as installed, so any number of composers
/// share one binding.
struct KeysBound;
impl Global for KeysBound {}

/// The coordinator's composer.
pub struct Composer {
    input: Entity<TextareaState>,
    readout: SharedString,
    /// The right-hand segments (`2 lanes`), at the row's end before the button.
    trailing: Option<SharedString>,
    /// Whether this composer draws the status readout on its own row. The app
    /// turns it off: the line belongs to the tab page now, under the transcript,
    /// where it can speak for the agent being shown rather than for the
    /// coordinator alone (§7.3). A composer on its own — the demo — keeps it.
    show_readout: bool,
    /// The swarm's own busy flag: what the action button's face follows.
    busy: bool,
    /// True while this composer's own request is in flight — the only reason
    /// the button is disabled.
    in_flight: bool,
    /// The prompts this tab has sent, oldest first: what ↑/↓ walks.
    history: Vec<String>,
    /// Where in `history` the input is, while it is showing a recalled prompt.
    /// `None` means the input holds the reader's own draft.
    walking: Option<usize>,
    /// The text the walk put in the input, so an edit of it — the reader typing
    /// over a recalled prompt — is visible before the input's own `Change` event
    /// has had a chance to arrive.
    recalled: String,
    /// The draft as `Enter` found it.
    ///
    /// The input's own `PressEnter` reaches this composer at the end of the
    /// update the key arrived in, and the text would be read off the input then:
    /// anything that rewrites the input in between — a prefill, a mention the
    /// reader picked — must not change the prompt that goes out.
    pending_send: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ComposerEvent> for Composer {}

impl Composer {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::init(cx);

        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(MIN_ROWS, MAX_ROWS)
                .placeholder(PLACEHOLDER)
                // `Enter` submits, `Shift+Enter` inserts a newline.
                .submit_on_enter(true)
        });
        let subscription = cx.subscribe(&input, |this, input, event: &InputEvent, cx| {
            match event {
                // `Shift+Enter` already inserted its newline in the state.
                InputEvent::PressEnter { shift: false, .. } => {
                    // The draft as the keypress found it, not as the input holds
                    // it when this event lands at the end of the update.
                    let draft = match this.pending_send.take() {
                        Some(draft) => draft,
                        // An event nobody pressed for is still the input's text.
                        None => input.read(cx).value().to_string(),
                    };
                    this.send(draft, cx);
                }
                InputEvent::Change => {
                    // An edit is what ends a walk through the history: the reader
                    // has taken the recalled prompt and made it their own draft.
                    this.walking = None;
                    cx.notify();
                }
                _ => {}
            }
        });

        // The keys the input handles itself but has no use for here: a window
        // selection is not the input's to copy, and an empty composer has no
        // caret to walk through its own prompts.
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
            readout: SharedString::default(),
            trailing: None,
            show_readout: true,
            busy: false,
            in_flight: false,
            history: Vec::new(),
            walking: None,
            recalled: String::new(),
            pending_send: None,
            _subscriptions: vec![subscription, interceptor],
        }
    }

    /// The prompts this composer has sent, oldest first — the tab's own history
    /// for ↑/↓, and nothing that outlives the process.
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

    /// The status line the owner wants shown: the topic's own `segments`, rendered as
    /// they are (CONTRACT §4.2). Nothing is composed here — the server's segment registry
    /// built each piece, so the TUI and this row cannot drift apart.
    ///
    /// The left side is the line itself; the right side sits at the row's end, before the
    /// action button. Long lines are ellipsized and carried whole in a tooltip.
    pub fn set_segments(&mut self, left: &[Segment], right: &[Segment], cx: &mut Context<Self>) {
        let join = |segments: &[Segment]| {
            segments
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>()
                .join(" · ")
        };
        let readout: SharedString = join(left).into();
        let trailing: Option<SharedString> = match right {
            [] => None,
            segments => Some(join(segments).into()),
        };
        if self.readout != readout || self.trailing != trailing {
            self.readout = readout;
            self.trailing = trailing;
            cx.notify();
        }
    }

    /// The whole line the row shows, as the tooltip and the accessible name carry it.
    pub fn readout(&self) -> &str {
        &self.readout
    }

    /// Whether the readout shares the action row. Off, the row is the button
    /// alone, at its right-hand end (§7.3).
    pub fn set_show_readout(&mut self, show: bool, cx: &mut Context<Self>) {
        if self.show_readout != show {
            self.show_readout = show;
            cx.notify();
        }
    }

    /// Whether the swarm is doing anything — `TabModel::is_swarm_busy`, which is the
    /// coordinator's own status *and* whether it is held while its lanes work. It is what
    /// the button's face follows.
    pub fn set_swarm_busy(&mut self, busy: bool, cx: &mut Context<Self>) {
        if self.busy != busy {
            self.busy = busy;
            cx.notify();
        }
    }

    /// Report the outcome of this composer's own request.
    ///
    /// `ok` clears the draft — the input is emptied only after the server took
    /// the text. A failed send (or an interrupt) leaves it alone.
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
    /// (CONTRACT §7.5). The per-lane Stop lives in the agent list, where a lane is named.
    pub fn face(&self) -> ActionFace {
        if self.busy {
            ActionFace::StopSwarm
        } else {
            ActionFace::Send
        }
    }

    /// Whether the button accepts a click right now: enabled only when the face
    /// has something to do and this composer has no request in flight.
    pub fn is_action_enabled(&self, cx: &App) -> bool {
        if self.in_flight {
            return false;
        }
        match self.face() {
            ActionFace::Send => !self.input.read(cx).value().trim().is_empty(),
            ActionFace::StopSwarm => true,
        }
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
        // Sending the same prompt again — the retry after a refusal, most often
        // — is one entry, not two.
        if self.history.last().map(String::as_str) == Some(prompt) {
            return;
        }
        if self.history.len() == HISTORY_LIMIT {
            self.history.remove(0);
        }
        self.history.push(prompt.to_string());
    }

    /// Walk the sent prompts: ↑ back in time, ↓ forward, past the newest one and
    /// out of the walk.
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

    /// Take the keys the input handles without doing what the reader means,
    /// while its caret is in this composer.
    ///
    /// The input owns ↑/↓ (caret movement) and the copy shortcut (its own
    /// selection), and handles both itself rather than letting either through.
    /// An empty composer has no caret to move and nothing to copy, though: ↑
    /// belongs to the tab's prompt history, and a window selection — the
    /// reader's, made in the transcript — is what the shortcut was aimed at.
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

        // A recalled prompt the reader has typed in is their draft now, whatever
        // the input's own `Change` event has yet to say about it.
        if self
            .walking
            .is_some_and(|index| input.read(cx).value().as_ref() != self.history[index].as_str())
        {
            self.walking = None;
        }

        let keystroke = &event.keystroke;

        // A plain `Enter` submits, but the input submits at the end of this
        // update: the reader's words are taken now, at the key, so that whatever
        // rewrites the input in between is not what goes out.
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
            // With nothing of its own selected the input has no copy to make;
            // the window's selection is the one the reader means.
            "c" if is_copy_shortcut(keystroke)
                && !input.read(cx).is_copyable()
                && self.copy_window_selection(window, cx) =>
            {
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    /// Copy what the window has selected — a selection the reader made outside
    /// the input — the way the window's own copy does. Answers whether there was
    /// anything to copy.
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

    fn readout_element(&self, cx: &Context<Self>) -> impl IntoElement {
        let full = self.readout.clone();
        let tooltip = full.clone();
        div()
            .id(READOUT_ID)
            .test_support()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_size(READOUT_SIZE)
            .text_color(cx.theme().muted_foreground)
            // The whole line as the element's accessible name: the visible cell is
            // truncated to the column, and this is what a reader who cannot see the
            // ellipsis (or a test) is meant to read.
            .aria_label(full.clone())
            // The whole line, wrapped rather than a single line wider than the
            // window it is read in.
            .tooltip(move |window, cx| {
                // The whole line, in a card the width of the column it belongs
                // to: a status line is wider than the window it is read in, and
                // an unwrapped tooltip runs off the screen.
                let line = tooltip.clone();
                Tooltip::element(move |_, _| div().w(READOUT_TOOLTIP_WIDTH).child(line.clone()))
                    .build(window, cx)
            })
            .child(full)
    }

    fn action_button(&self, cx: &Context<Self>) -> impl IntoElement {
        let face = self.face();
        let mut button = Button::new(BUTTON_ID)
            .small()
            .h(ACTION_HEIGHT)
            .label(face.label())
            // The glyph is decoration: what the button is called is the word.
            .accessibility_label(face.label())
            .disabled(!self.is_action_enabled(cx))
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

        match face {
            ActionFace::Send => button.primary(),
            ActionFace::StopSwarm => button.secondary(),
        }
    }
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let readout = self.show_readout.then(|| self.readout_element(cx));
        let button = self.action_button(cx);
        // The caret is what "focused" means here: the ring belongs to the card,
        // which the input does not own. Focus is the theme's focus colour, and
        // only a hairline of it: the band around the card carries the weight.
        let focused = self
            .input
            .read(cx)
            .presentation()
            .focus_handle()
            .is_focused(window);
        let edge = if focused { theme.ring } else { theme.border };

        // The composer is the top of its column (§7.3): the input, its status
        // row, and nothing below them — the column's height belongs to the tab
        // page, not to the composer.
        v_flex()
            .size_full()
            .items_stretch()
            .gap_2()
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(Self::interrupt_action))
            .child(
                // A chat input: a rounded card with one hairline edge, and the
                // hint that this is where a message is typed — the focus
                // colour, as a soft band around the card while the caret is in
                // it. The input draws none of this itself, so the card can
                // round further than the theme's default radius and pad the
                // text by the size's own 12px without a second inset around it.
                div()
                    .w_full()
                    .min_w_0()
                    .rounded(INPUT_RADIUS + FOCUS_RING)
                    .p(FOCUS_RING)
                    .when(focused, |this| this.bg(theme.ring.alpha(FOCUS_RING_INK)))
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .rounded(INPUT_RADIUS)
                            .border(INPUT_BORDER)
                            .border_color(edge)
                            .bg(theme.input_background())
                            .child(
                                Textarea::new(&self.input)
                                    .with_size(Size::Large)
                                    .appearance(false)
                                    .bordered(false)
                                    .w_full()
                                    .min_w_0(),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_center()
                    .gap_2()
                    // The readout is the flexible cell of this row; without it the
                    // button is the row, and stays where it was (§7.3).
                    .when_some(readout, |row, readout| row.child(readout))
                    // The right-hand segments (`2 lanes`) sit where they belong: at the
                    // end of the row, before the action.
                    .when_some(self.trailing.clone(), |row, trailing| {
                        row.child(
                            div()
                                .id("composer-readout-right")
                                .test_support()
                                .flex_none()
                                .text_size(READOUT_SIZE)
                                .text_color(theme.muted_foreground)
                                .child(trailing),
                        )
                    })
                    .when(!self.show_readout, |row| row.justify_end())
                    .child(button),
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

    /// The right column is ~360 px wide (§7.3), so the tests use one.
    const COLUMN: gpui_kit::Size<gpui_kit::Pixels> = gpui_kit::Size {
        width: px(360.),
        height: px(320.),
    };

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
                size: COLUMN,
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

    #[gpui_kit::test]
    fn the_input_is_a_card_at_the_top_and_the_action_is_one_compact_button(
        cx: &mut TestAppContext,
    ) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);

            // The input opens the column, and what it is for is its own
            // placeholder — including the two keys that submit it (§7.3).
            let input = window.find(f.input_frame(cx));
            assert!(input.visible(), "the input is the top of the column");
            assert!(
                input.bounds().origin.y <= px(4.),
                "the input opens the column — only its ring and border sit above it: {:?}",
                input.bounds()
            );
            assert_eq!(input.label(), Some(PLACEHOLDER));

            // Under it, one status row: the readout on the left, the action
            // button flush right, both on one line of the button's height.
            let readout = window.find(READOUT_ID).bounds();
            let button = window.find(BUTTON_ID).bounds();
            assert!(
                button.origin.y >= input.bounds().bottom(),
                "the button left the row under the input: input {:?}, button {:?}",
                input.bounds(),
                button
            );
            assert_eq!(button.size.height, ACTION_HEIGHT);
            // The readout is the row's flexible cell: it takes every pixel left
            // over from the button, so a long status line is elided by the
            // button's own gap and not by slack in the layout.
            assert_eq!(
                button.left() - readout.right(),
                px(8.),
                "the readout stops short of the action: {readout:?} vs {button:?}"
            );

            // The rest of the column is empty: the composer owns the top of it,
            // not its height (§7.3).
            assert!(
                button.bottom() < window.bounds().size.height / 2.,
                "the composer filled the column: button {button:?} in {:?}",
                window.bounds()
            );
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

    #[gpui_kit::test]
    fn send_is_enabled_by_a_non_blank_draft_and_by_nothing_else(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find(BUTTON_ID).label(), Some("Send"));

            // Nothing typed: nothing to send, so the click is inert.
            assert!(!f.composer.read(cx).is_action_enabled(cx));
            window.click(BUTTON_ID, cx);

            // Whitespace only is blank too.
            f.set_draft("   ", window, cx);
            window.render_frame(cx);
            assert!(!f.composer.read(cx).is_action_enabled(cx));
            window.click(BUTTON_ID, cx);
        });

        assert!(f.events().is_empty(), "a blank draft has nothing to send");

        f.act(cx, |window, cx| {
            f.set_draft("real", window, cx);
            window.render_frame(cx);
            assert!(f.composer.read(cx).is_action_enabled(cx));
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
            window.click(BUTTON_ID, cx);
        });
        assert_eq!(f.events().len(), 1, "the first click sends");

        // In flight: the click does nothing at all.
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert!(!f.composer.read(cx).is_action_enabled(cx));
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

    #[gpui_kit::test]
    fn a_long_readout_is_ellipsized_and_keeps_the_button_on_its_line(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            let line = "ark-deepseek-v4.1-flash \u{b7} max \u{b7} ctx 48k/936k (5%) \u{b7} 97% cached \u{b7} \
                        goal a1b2c3d4 (active) 12k/50k \u{b7} \
                        0123456789 0123456789 0123456789 0123456789 0123456789";
            f.composer.update(cx, |composer, cx| {
                composer.set_segments(
                    &[Segment {
                        name: "line".to_string(),
                        order: 1,
                        side: session::Side::Left,
                        text: line.to_string(),
                        data: Default::default(),
                    }],
                    &[],
                    cx,
                )
            });
            window.render_frame(cx);

            let readout = window.find(READOUT_ID);
            let button = window.find(BUTTON_ID);
            assert!(readout.visible() && button.visible());

            // The cell is truncated to the column, so the whole line is its
            // accessible name — what a workspace test asserts the status row says.
            assert_eq!(readout.label(), Some(line), "the readout's own line");

            // One line high and on the same row: readout left, button right.
            assert!(
                readout.bounds().size.height <= px(24.),
                "the readout wrapped: {:?}",
                readout.bounds()
            );
            assert!(
                readout.bounds().origin.y < button.bounds().bottom()
                    && button.bounds().origin.y < readout.bounds().bottom(),
                "the button left the readout's line: readout {:?}, button {:?}",
                readout.bounds(),
                button.bounds()
            );
            assert!(
                readout.bounds().right() <= button.bounds().left(),
                "the readout overlaps the button: {:?} vs {:?}",
                readout.bounds(),
                button.bounds()
            );
        });
    }

    /// §7.3: the app takes the readout off this row — the tab page draws it under
    /// the transcript — and the row is then the action alone, at its right-hand
    /// end: the button does not move to the left, and nothing of the readout is
    /// left behind.
    #[gpui_kit::test]
    fn without_the_readout_the_row_is_the_button_at_its_right(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            f.composer.update(cx, |composer, cx| {
                composer.set_segments(
                    &[Segment {
                        name: "context".to_string(),
                        order: 1,
                        side: session::Side::Left,
                        text: "ctx 48k/936k".to_string(),
                        data: Default::default(),
                    }],
                    &[],
                    cx,
                )
            });
            f.composer
                .update(cx, |composer, cx| composer.set_show_readout(false, cx));
            window.render_frame(cx);

            let button = window.find(BUTTON_ID);
            assert!(button.visible(), "the button is the row");
            assert!(
                window.try_find(READOUT_ID).is_none(),
                "the readout is not drawn here any more"
            );
            // Right-aligned in the column — which here is the whole window, so
            // the row's right-hand end is its width.
            assert!(
                (button.bounds().right() - COLUMN.width).abs() <= px(1.),
                "the button is at the row's right-hand end: {:?}",
                button.bounds()
            );
        });
    }

    /// §9.2: the prompt that goes out is the one the reader had in front of them
    /// when they pressed `Enter`. The input's own event lands at the end of that
    /// update, so a prefill — a mention they picked, a program that rewrites the
    /// input — must not be what is sent.
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

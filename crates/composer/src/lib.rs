//! composer — the right column of the tab page (§7.3): the coordinator's input,
//! and one status row under it carrying the session readout on the left and the
//! single Send/Stop button flush right.
//!
//! The composer does no I/O. It emits [`ComposerEvent`] and the owner posts the
//! request, then reports the outcome with [`Composer::request_finished`]: that
//! is what keeps a failed send's draft alive, and what keeps the button
//! disabled only while its own request is in flight.

use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{InputEvent, Textarea, TextareaState},
    tooltip::Tooltip,
    v_flex, ActiveTheme as _, Disableable as _, IconName, Sizable as _, Size,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, px, App, AppContext as _, Context, Entity, EventEmitter, Global, InteractiveElement as _,
    IntoElement, KeyBinding, ParentElement as _, Pixels, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _, Window,
};
use session::Activity;

/// The input grows from two rows to eight; past that it scrolls.
const MIN_ROWS: usize = 2;
const MAX_ROWS: usize = 8;

/// The input is a card: a chat input, not a form field.
const INPUT_RADIUS: Pixels = px(10.);
const INPUT_BORDER: Pixels = px(1.);
/// The focus ring: a band around the card, in the accent, while the caret is in
/// the input.
const FOCUS_RING: Pixels = px(3.);
const FOCUS_RING_INK: f32 = 0.18;

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

/// The action button's element id: one button, addressed by name.
pub const BUTTON_ID: &str = "composer-action";
/// The readout's element id; it also carries the full line as its tooltip.
pub const READOUT_ID: &str = "composer-readout";

gpui_kit::actions!(composer, [Interrupt]);

/// What the composer asks its owner to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposerEvent {
    /// Post this text as the coordinator's turn. It lands at the running turn's
    /// next boundary, so text sent while the agent works is queued, not lost.
    Send(String),
    /// Interrupt the run (the TUI's esc). The draft is untouched.
    Interrupt,
}

/// What the one button says — and therefore what clicking it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionFace {
    Send,
    Stop,
}

impl ActionFace {
    /// The button's visible label: `Send`, or `Stop` behind its square glyph.
    pub fn label(self) -> &'static str {
        match self {
            Self::Send => "Send",
            Self::Stop => "\u{25a0} Stop",
        }
    }

    /// The glyph leading the label: an up arrow to send. Stop's square is part of
    /// its label instead — the icon set the app bundles carries no plain square,
    /// and the screen's own glyph draws one at the label's own size.
    pub fn icon(self) -> Option<IconName> {
        match self {
            Self::Send => Some(IconName::ArrowUp),
            Self::Stop => None,
        }
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
    activity: Activity,
    /// True while this composer's own request is in flight — the only reason
    /// the button is disabled.
    in_flight: bool,
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
                    let draft = input.read(cx).value().to_string();
                    this.send(draft, cx);
                }
                // The button's enabled state follows whether the draft is blank.
                InputEvent::Change => cx.notify(),
                _ => {}
            }
        });

        Self {
            input,
            readout: SharedString::default(),
            activity: Activity::Idle,
            in_flight: false,
            _subscriptions: vec![subscription],
        }
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

    /// The status line the owner wants shown, segment for segment as the TUI
    /// builds it (§7.3). Long lines are ellipsized and carried whole in a tooltip.
    pub fn set_readout(&mut self, readout: impl Into<SharedString>, cx: &mut Context<Self>) {
        let readout = readout.into();
        if self.readout != readout {
            self.readout = readout;
            cx.notify();
        }
    }

    /// The coordinator's activity, which decides the button's face.
    pub fn set_activity(&mut self, activity: Activity, cx: &mut Context<Self>) {
        if self.activity != activity {
            self.activity = activity;
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

    /// The button's face for the current activity — never Send and Stop at once.
    pub fn face(&self) -> ActionFace {
        match self.activity {
            Activity::Idle => ActionFace::Send,
            Activity::Running | Activity::Compacting => ActionFace::Stop,
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
            ActionFace::Stop => true,
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
        cx.emit(ComposerEvent::Send(draft));
        cx.notify();
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
            .accessibility_label(match face {
                ActionFace::Send => "Send",
                ActionFace::Stop => "Stop",
            })
            .disabled(!self.is_action_enabled(cx))
            .on_click(cx.listener(|this, _, _, cx| match this.face() {
                ActionFace::Send => {
                    let draft = this.input.read(cx).value().to_string();
                    this.send(draft, cx);
                }
                ActionFace::Stop => this.interrupt(cx),
            }));
        if let Some(icon) = face.icon() {
            button = button.icon(icon);
        }

        match face {
            ActionFace::Send => button.primary(),
            ActionFace::Stop => button.secondary(),
        }
    }
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let readout = self.readout_element(cx);
        let button = self.action_button(cx);
        // The caret is what "focused" means here: the ring belongs to the card,
        // which the input does not own.
        let focused = self
            .input
            .read(cx)
            .presentation()
            .focus_handle()
            .is_focused(window);

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
                // hint that this is where a message is typed — the accent, as a
                // ring around the card, while the caret is in it. The input
                // draws none of this itself, so the card can round further than
                // the theme's default radius and pad the text by the size's own
                // 12px without a second inset around it.
                div()
                    .w_full()
                    .min_w_0()
                    .rounded(INPUT_RADIUS + FOCUS_RING)
                    .p(FOCUS_RING)
                    .when(focused, |this| {
                        this.bg(theme.primary.alpha(FOCUS_RING_INK))
                    })
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .rounded(INPUT_RADIUS)
                            .border(INPUT_BORDER)
                            .border_color(if focused {
                                theme.primary
                            } else {
                                theme.border
                            })
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
                    .child(readout)
                    .child(button),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::test::TestWindowExt as _;
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

        fn activity(&self, activity: Activity, cx: &mut App) {
            self.composer
                .update(cx, |composer, cx| composer.set_activity(activity, cx));
        }

        /// Focus the input the way a user does, then type into it.
        fn type_draft(&self, text: &str, window: &mut Window, cx: &mut App) {
            window.click(self.input_frame(cx), cx);
            window.input(text, cx);
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

    fn open(cx: &mut TestAppContext) -> Fixture {
        cx.update(gpui_kit::init);
        let (window, composer) = cx.update(|cx| {
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.), px(0.)),
                    size: COLUMN,
                })),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
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
            assert!(
                readout.right() <= button.left(),
                "the readout runs into the button: {readout:?} vs {button:?}"
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
        assert_eq!(ActionFace::Stop.label(), "\u{25a0} Stop");
        assert_eq!(
            ActionFace::Send.icon().map(|icon| icon.path().to_string()),
            Some("icons/arrow-up.svg".to_string()),
            "Send leads with an up arrow"
        );
        assert!(
            ActionFace::Stop.icon().is_none(),
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
            f.activity(Activity::Running, cx);
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
            f.activity(Activity::Running, cx);
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

            f.activity(Activity::Running, cx);
            window.render_frame(cx);
            assert_eq!(window.find(BUTTON_ID).label(), Some("Stop"));

            f.activity(Activity::Compacting, cx);
            window.render_frame(cx);
            assert_eq!(window.find(BUTTON_ID).label(), Some("Stop"));

            f.activity(Activity::Idle, cx);
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

    #[gpui_kit::test]
    fn stop_interrupts_and_never_sends_or_clears(cx: &mut TestAppContext) {
        let f = open(cx);
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            f.type_draft("still working", window, cx);
            f.activity(Activity::Compacting, cx);
            window.render_frame(cx);

            assert_eq!(window.find(BUTTON_ID).label(), Some("Stop"));
            window.click(BUTTON_ID, cx);
        });

        assert_eq!(f.events(), vec![ComposerEvent::Interrupt]);
        assert_eq!(f.draft_now(cx), "still working");

        // The interrupt's reply keeps the draft: it was an interrupt, not a send.
        f.act(cx, |window, cx| {
            f.composer.update(cx, |composer, cx| {
                composer.request_finished(false, window, cx)
            })
        });
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
            f.composer
                .update(cx, |composer, cx| composer.set_readout(line, cx));
            window.render_frame(cx);

            let readout = window.find(READOUT_ID);
            let button = window.find(BUTTON_ID);
            assert!(readout.visible() && button.visible());

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
}

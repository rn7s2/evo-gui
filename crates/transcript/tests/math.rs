//! Math in a transcript, seen from outside the crate: what a reader gets.
//!
//! These are the review lane's own tests. They use nothing but the published
//! `TranscriptView` and the painted window — no `pub(crate)` seam of the math
//! module is reached into — so they say what the *app* does with a formula, not
//! what one helper inside it returns:
//!
//! * the frame that first meets a display formula draws the **source**, because a
//!   formula is laid out off the frame; once it lands the reader gets the picture;
//! * a formula is a picture, not a text line: measurably taller than the source it
//!   falls back to;
//! * a formula the engine cannot read, one outside the embedded faces, and one
//!   that is empty are all shown as the source the model wrote;
//! * two formulas in one message each get their own sideways scroller;
//! * prose that merely mentions money is not a formula;
//! * a drawn formula is cached: its box is the same on the next frame;
//! * a formula wider than the column is still drawn, inside the column.
//!
//! The renderer is asynchronous and its cache is process-wide, so every test
//! settles the app's executors before it asserts a picture, and **every test uses
//! a formula of its own** — a formula another test had already drawn would be in
//! the cache before the first frame, and the claim the first test watches for
//! would never happen.

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    div, px, AppContext as _, Bounds, Context, ElementId, Entity, IntoElement, ParentElement as _,
    Pixels, Render, SharedString, Styled as _, TestAppContext, VisualTestContext, Window,
};
use serde_json::json;
use session::Item;
use store::design::{INSET, MEASURE};
use transcript::TranscriptView;

/// A window over one transcript, as the tab page arranges it.
struct Host {
    transcript: Entity<TranscriptView>,
}

impl Host {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            transcript: cx.new(TranscriptView::new),
        }
    }
}

impl Render for Host {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.transcript.clone())
    }
}

macro_rules! open {
    ($cx:expr, $items:expr) => {
        open_frames!($cx, $items, 3)
    };
}

macro_rules! open_frames {
    ($cx:expr, $items:expr, $frames:expr) => {{
        $cx.update(gpui_kit::init);
        let (host, cx) = $cx.add_window_view(|_window, cx| Host::new(cx));
        let view = cx.read(|cx| host.read(cx).transcript.clone());
        view.update(cx, |view, cx| view.replace($items, cx));
        frames(cx, $frames);
        (view, cx)
    }};
}

// --- fixtures -------------------------------------------------------------

fn item(value: serde_json::Value) -> Item {
    Item::from_json(&value).expect("a fixture item has an id")
}

/// A finished assistant message, which is drawn as markdown.
fn said(id: &str, text: &str) -> Item {
    item(json!({ "id": id, "ts": 1, "kind": "assistant", "text": text, "status": "final" }))
}

// --- what the window shows ------------------------------------------------

/// The cell a row's text is measured in (the row's own id path, as the row
/// builders name it).
fn row(id: &str) -> ElementId {
    (ElementId::from("transcript-measure"), id.to_string()).into()
}

/// The scrollers a message's display formulas are drawn in, in the order they
/// were written. The math module names each one after its own span in the
/// **document**, and the document is the message after the source pass — which
/// fences a `$$…$$` that is a paragraph of its own, and fences a display alias the
/// same way — so the spans to look for are spans of the normalized text, not of
/// what the model wrote.
fn display_blocks(text: &str) -> Vec<ElementId> {
    let document = transcript::normalize_math::normalize(text);
    let mut ids = Vec::new();
    let mut at = 0;
    while at < document.len() {
        // An inline alias is a `$…$` of its own: in `$\(a\)$$$\(b\)$` the `$$` a
        // scan would see is two markers touching, not a display formula.
        if let Some(end) = inline_marker_end(&document, at) {
            at = end;
            continue;
        }
        if !document[at..].starts_with("$$") {
            at += document[at..].chars().next().map_or(1, char::len_utf8);
            continue;
        }
        // A `$$` standing alone on its line is a fence: it is closed by the next
        // `$$` that stands alone too, and what lies between them is the model's
        // own line, dollars and all. Every other `$$` is closed by the next one,
        // which is where the kit's own inline-math span ends.
        let fenced = stands_alone(&document, at);
        let mut search = at + 2;
        let end = loop {
            let Some(offset) = document[search..].find("$$") else {
                break None;
            };
            let close = search + offset;
            if !fenced || stands_alone(&document, close) {
                break Some(close + 2);
            }
            search = close + 2;
        };
        let Some(end) = end else { break };
        ids.push(ElementId::Name(SharedString::from(format!(
            "transcript-math-block-{at}-{end}"
        ))));
        at = end;
    }
    ids
}

/// Just past the `$\(…\)$` or `$\[…\]$` marker that opens at `at`, if one does.
fn inline_marker_end(document: &str, at: usize) -> Option<usize> {
    let rest = &document[at..];
    let close = if rest.starts_with("$\\(") {
        "\\)$"
    } else if rest.starts_with("$\\[") {
        "\\]$"
    } else {
        return None;
    };
    let body = at + 3;
    document[body..]
        .find(close)
        .map(|offset| body + offset + close.len())
}

/// Whether the `$$` at `at` is a fence: nothing else on its line.
fn stands_alone(document: &str, at: usize) -> bool {
    let line_start = document[..at].rfind('\n').map_or(0, |newline| newline + 1);
    let line_end = document[at..]
        .find('\n')
        .map_or(document.len(), |newline| at + newline);
    document[line_start..at].trim().is_empty() && document[at + 2..line_end].trim().is_empty()
}

fn found(cx: &mut VisualTestContext, id: ElementId) -> bool {
    cx.update(|window, _| window.try_find(id).is_some())
}

fn bounds(cx: &mut VisualTestContext, id: ElementId) -> Bounds<Pixels> {
    cx.update(|window, _| window.find(id).bounds())
}

fn height_of(cx: &mut VisualTestContext, id: &str) -> Pixels {
    bounds(cx, row(id)).size.height
}

fn frames(cx: &mut VisualTestContext, count: usize) {
    for _ in 0..count {
        cx.update(|window, cx| window.render_frame(cx));
    }
}

/// Let the work the renderer asked for finish, and draw what it made: the app's
/// executors are drained and the windows painted, twice — a picture that lands
/// between two draws is drawn by the second.
fn settle(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    frames(cx, 2);
    cx.run_until_parked();
    frames(cx, 2);
}

// --- the tests ------------------------------------------------------------

/// Laying a formula out is not the frame's work: the frame that first meets a
/// display formula draws its source, and the picture is there once the work the
/// renderer asked for has come back.
#[gpui_kit::test]
fn the_frame_that_claims_a_display_formula_draws_its_source(cx: &mut TestAppContext) {
    let text = "before\n\n$$\\frac{claim}{first}$$\n\nafter";
    let (_view, cx) = open_frames!(cx, vec![said("e_1", text)], 1);
    let scroller = display_blocks(text).remove(0);

    assert!(
        !found(cx, scroller.clone()),
        "the frame that meets the formula has no picture yet"
    );
    let before = height_of(cx, "e_1");

    settle(cx);

    assert!(
        found(cx, scroller.clone()),
        "the picture is drawn once the renderer's work has come back"
    );
    let after = height_of(cx, "e_1");
    assert!(
        after > before,
        "and it is the picture, not the source: {before:?} then {after:?}"
    );
}

/// A `$$…$$` formula is drawn as a picture in a scroller of its own, sized inside
/// the row it sits in.
#[gpui_kit::test]
fn a_display_formula_is_drawn_as_a_picture_of_its_own(cx: &mut TestAppContext) {
    let text = "before\n\n$$\\frac{alpha}{beta}$$\n\nafter";
    let (_view, cx) = open!(cx, vec![said("e_1", text)]);
    settle(cx);

    let scroller = display_blocks(text).remove(0);
    assert!(
        found(cx, scroller.clone()),
        "the display formula is drawn in its own scroller"
    );
    let drawn = bounds(cx, scroller);
    assert!(
        drawn.size.height > px(0.) && drawn.size.width > px(0.),
        "the scroller has a box: {drawn:?}"
    );
    let row = bounds(cx, row("e_1"));
    assert!(
        drawn.size.width <= row.size.width + px(1.),
        "it is inside the row: {drawn:?} vs {row:?}"
    );
}

/// The same message with a formula the engine cannot read: no picture is ever
/// made, and the reader keeps the source the model wrote — measurably shorter
/// than the picture, which is what says the two paths really differ.
#[gpui_kit::test]
fn a_formula_the_engine_cannot_read_is_shown_as_its_source(cx: &mut TestAppContext) {
    let drawn_text = "before\n\n$$\\frac{gamma}{delta}$$\n\nafter";
    let broken_text = "before\n\n$$\\frac{epsilon}{$$\n\nafter";
    let (_view, cx) = open!(cx, vec![said("e_1", drawn_text), said("e_2", broken_text)]);
    settle(cx);

    let broken = display_blocks(broken_text).remove(0);
    assert!(
        !found(cx, broken),
        "an unreadable formula is not drawn as a picture"
    );

    let drawn_height = height_of(cx, "e_1");
    let broken_height = height_of(cx, "e_2");
    assert!(
        drawn_height > broken_height,
        "the picture is taller than the source it replaces: {drawn_height:?} vs {broken_height:?}"
    );
    assert!(
        broken_height > px(0.),
        "and the unreadable formula still occupies a line of its own: {broken_height:?}"
    );
}

/// The same for an inline formula: a drawable one makes the line taller than the
/// source an unreadable one falls back to.
#[gpui_kit::test]
fn an_inline_formula_is_drawn_and_an_unreadable_one_falls_back(cx: &mut TestAppContext) {
    let drawn = r"a $\sum_{i=0}^{zeta} \frac{eta_i}{theta}$ b";
    let broken = r"a $\sum_{i=0}^{zeta} \frac{eta_i}{$ b";
    let (_view, cx) = open!(cx, vec![said("e_1", drawn), said("e_2", broken)]);
    settle(cx);

    let drawn_height = height_of(cx, "e_1");
    let broken_height = height_of(cx, "e_2");
    assert!(
        drawn_height > broken_height,
        "a drawn inline sum with a fraction is taller than its source would be: \
         {drawn_height:?} vs {broken_height:?}"
    );
}

/// A formula using characters the embedded faces do not carry is declined, not
/// drawn: drawing it would send the renderer looking for a system font. The
/// reader gets the source, and nothing about the machine decides the picture.
#[gpui_kit::test]
fn a_formula_outside_the_embedded_fonts_is_declined_to_its_source(cx: &mut TestAppContext) {
    let ascii_text = "before\n\n$$\\frac{iota}{kappa}$$\n\nafter";
    let unicode_text = "before\n\n$$\\frac{\u{3B1}}{\u{3B2}}$$\n\nafter";
    let (_view, cx) = open!(cx, vec![said("e_1", ascii_text), said("e_2", unicode_text)]);
    settle(cx);

    let unicode_block = display_blocks(unicode_text).remove(0);
    assert!(
        !found(cx, unicode_block),
        "a formula outside the embedded faces is not drawn"
    );
    assert!(
        height_of(cx, "e_1") > height_of(cx, "e_2"),
        "the ASCII formula is a picture; the other is the source as text"
    );
}

/// Two display formulas in one message are two pictures, each with its own
/// scroller — no shared name, no second one swallowed by the first.
#[gpui_kit::test]
fn two_display_formulas_in_one_message_each_get_their_own_scroller(cx: &mut TestAppContext) {
    let text = "$$lambda + mu$$\n\nand\n\n$$nu + xi$$";
    let (_view, cx) = open!(cx, vec![said("e_1", text)]);
    settle(cx);

    let blocks = display_blocks(text);
    assert_eq!(blocks.len(), 2, "the message holds two display formulas");
    let (first, second) = (blocks[0].clone(), blocks[1].clone());
    assert!(found(cx, first.clone()), "the first formula is drawn");
    assert!(found(cx, second.clone()), "and the second");
    assert_ne!(first, second, "with names of their own");
    let above = bounds(cx, first);
    let below = bounds(cx, second);
    assert!(
        above.top() < below.top(),
        "in the order they were written: {above:?} then {below:?}"
    );
}

/// An empty formula makes no picture and breaks no row: the line it sits in is
/// drawn, and the message after it is still there.
#[gpui_kit::test]
fn an_empty_formula_draws_nothing_and_leaves_the_line_alone(cx: &mut TestAppContext) {
    let (_view, cx) = open!(
        cx,
        vec![said("e_1", "a $$$$ b"), said("e_2", "the next message")]
    );
    settle(cx);

    assert!(
        height_of(cx, "e_1") > px(0.),
        "the line with the empty formula is still drawn"
    );
    assert!(
        height_of(cx, "e_2") > px(0.),
        "and the message after it is untouched"
    );
}

/// Prose that merely mentions money is not a formula: TeX's own rule — no space
/// against either delimiter — leaves it to the kit, which shows the sentence.
#[gpui_kit::test]
fn prose_that_mentions_money_is_not_a_formula(cx: &mut TestAppContext) {
    let prose = "I spent $5 and then $10 more on tokens";
    let plain = "I spent 5 and then 10 more on tokens";
    let (_view, cx) = open!(cx, vec![said("e_1", prose), said("e_2", plain)]);
    settle(cx);

    let prose_height = height_of(cx, "e_1");
    let plain_height = height_of(cx, "e_2");
    let difference = (f32::from(prose_height) - f32::from(plain_height)).abs();
    assert!(
        difference < 2.,
        "the sentence is drawn as the prose it is: {prose_height:?} vs {plain_height:?}"
    );
}

/// A drawn formula keeps its box across frames: the picture is cached, not
/// re-made differently, and the element it is drawn in does not move.
#[gpui_kit::test]
fn a_drawn_formula_keeps_its_box_across_frames(cx: &mut TestAppContext) {
    let text = "$$\\sum_{i=1}^{omicron} \\frac{pi_i}{rho_i}$$";
    let (_view, cx) = open!(cx, vec![said("e_1", text)]);
    settle(cx);
    let scroller = display_blocks(text).remove(0);

    let first = bounds(cx, scroller.clone());
    frames(cx, 3);
    let second = bounds(cx, scroller);
    assert_eq!(
        first, second,
        "a cached picture draws in the same box on the next frame"
    );
}

/// A formula wider than the column is still drawn — in its own scroller, which is
/// what the reader uses to see the whole of it.
#[gpui_kit::test]
fn a_formula_wider_than_the_column_is_still_drawn(cx: &mut TestAppContext) {
    let long = (0..24)
        .map(|i| format!("sigma_{i}"))
        .collect::<Vec<_>>()
        .join(" + ");
    let text = format!("before\n\n$${long}$$\n\nafter");
    let (_view, cx) = open!(cx, vec![said("e_1", &text)]);
    settle(cx);

    let scroller = display_blocks(&text).remove(0);
    assert!(found(cx, scroller.clone()), "the wide formula is drawn");
    let drawn = bounds(cx, scroller);
    let row = bounds(cx, row("e_1"));
    assert!(
        drawn.size.width <= row.size.width + px(1.),
        "and its scroller is the width of the column, not of the formula: \
         {drawn:?} vs {row:?}"
    );
}

// --- TeX's own delimiters -------------------------------------------------

/// `\(…\)` in the middle of a sentence is a formula like any other: drawn once the
/// renderer's work lands, while one with no closer stays the text it was written as.
#[gpui_kit::test]
fn an_inline_alias_in_prose_is_drawn(cx: &mut TestAppContext) {
    let drawn = r"a \(\sum_{i=0}^{omega} \frac{xi_i}{psi}\) b";
    let dangling = r"a \(\sum_{i=0}^{omega} \frac{xi_i}{ b";
    let (_view, cx) = open!(cx, vec![said("e_1", drawn), said("e_2", dangling)]);
    settle(cx);

    let drawn_height = height_of(cx, "e_1");
    let dangling_height = height_of(cx, "e_2");
    assert!(
        drawn_height > dangling_height,
        "the alias is a picture and the dangling one is its own text: \
         {drawn_height:?} vs {dangling_height:?}"
    );
    let escaped = transcript::normalize_math::normalize(dangling);
    assert!(
        escaped.contains("\\("),
        "an alias with no closer keeps its backslash, written as the two Markdown \
         reads as one: {escaped:?}"
    );
    assert!(
        !escaped.contains('$'),
        "and no math is written for it: {escaped:?}"
    );
}

/// `\[…\]` on a line of its own is display math: its own scroller, the width of
/// the column, drawn once the work lands.
#[gpui_kit::test]
fn a_display_alias_on_a_line_of_its_own_is_drawn(cx: &mut TestAppContext) {
    let text = "before\n\n\\[\\frac{tau}{upsilon}\\]\n\nafter";
    let (_view, cx) = open!(cx, vec![said("e_1", text)]);
    settle(cx);

    let scroller = display_blocks(text).remove(0);
    assert!(found(cx, scroller.clone()), "the display alias is drawn");
    let drawn = bounds(cx, scroller);
    let row = bounds(cx, row("e_1"));
    assert!(
        drawn.size.width <= row.size.width + px(1.),
        "inside the column: {drawn:?} vs {row:?}"
    );
    assert!(drawn.size.height > px(0.), "with a box: {drawn:?}");
}

/// Code is code: an alias inside a fence or an inline span is left alone, and the
/// row is the code block it would have been without any math in it.
#[gpui_kit::test]
fn an_alias_inside_code_stays_code(cx: &mut TestAppContext) {
    let with_alias = "```\n\\(phi\\) and \\[chi\\]\n```";
    let without = "```\nplain code, no math at all\n```";
    let (_view, cx) = open!(cx, vec![said("e_1", with_alias), said("e_2", without)]);

    assert_eq!(
        transcript::normalize_math::normalize(with_alias),
        with_alias,
        "nothing inside a fence is rewritten"
    );
    settle(cx);
    let with_height = height_of(cx, "e_1");
    let without_height = height_of(cx, "e_2");
    assert!(
        (f32::from(with_height) - f32::from(without_height)).abs() < 2.,
        "the code block is a code block: {with_height:?} vs {without_height:?}"
    );
}

/// A *closed* alias whose formula this engine cannot read is claimed and shown as
/// the alias itself — delimiters and all — never as the dollars the source pass
/// wrapped it in, and never as a picture.
#[gpui_kit::test]
fn a_closed_alias_that_cannot_be_drawn_is_shown_as_its_alias(cx: &mut TestAppContext) {
    let broken = "before\n\n$$\\[\\notacommand{aleph}\\]$$\n\nafter";
    let text = "before\n\n\\[\\notacommand{aleph}\\]\n\nafter";
    let (_view, cx) = open!(cx, vec![said("e_1", text)]);

    let document = transcript::normalize_math::normalize(text);
    assert_eq!(document, broken, "the alias is wrapped for the parser");
    settle(cx);

    let scroller = display_blocks(text).remove(0);
    assert!(
        !found(cx, scroller),
        "a formula this engine cannot read is not drawn"
    );
    assert!(
        height_of(cx, "e_1") > px(0.),
        "and the source is still drawn"
    );
}

/// A report's fields are markdown too, and they are normalized the same way, at the
/// frame that first shows the row: an alias in a report's own text is a formula.
#[gpui_kit::test]
fn an_alias_in_a_report_field_is_drawn(cx: &mut TestAppContext) {
    let field = "drew\n\n\\[\\varphi_1 \\otimes \\omega_1\\]\n\nand stopped";
    let report = item(json!({
        "id": "e_1", "ts": 1, "kind": "lane_report", "lane": 3, "done": field
    }));
    let (_view, cx) = open!(cx, vec![report]);
    settle(cx);

    let scroller = display_blocks(field).remove(0);
    assert!(
        found(cx, scroller.clone()),
        "the report's own field is normalized at the frame that shows it"
    );
    assert!(bounds(cx, scroller).size.height > px(0.));
}

/// A display alias written across lines is one formula in a block of its own, and
/// the paragraph after it is still drawn: the shape the pass writes now cannot
/// swallow the rest of the message.
#[gpui_kit::test]
fn a_display_alias_across_lines_is_a_block_and_the_message_goes_on(cx: &mut TestAppContext) {
    let text = "before\n\n\\[\n\\frac{tau}{upsilon}\n\\]\n\nafter";
    // The same block without the paragraph after it: the difference between the two
    // rows is that paragraph, and no other. (A different formula, so the two
    // scrollers are not the same element.)
    let alone = "before\n\n\\[\n\\alpha_2\n\\]";
    let (_view, cx) = open!(cx, vec![said("e_1", text), said("e_2", alone)]);
    settle(cx);

    let document = transcript::normalize_math::normalize(text);
    assert!(
        document.starts_with("before\n\n$$\n\\[\n"),
        "the alias is fenced as a block: {document:?}"
    );
    let scroller = display_blocks(&document).remove(0);
    assert!(
        found(cx, scroller.clone()),
        "the alias across lines is drawn as one formula"
    );
    let drawn = bounds(cx, scroller);
    assert!(
        height_of(cx, "e_1") > drawn.size.height,
        "the paragraph after the fence is drawn below it: {:?} vs {drawn:?}",
        height_of(cx, "e_1")
    );
    assert!(
        height_of(cx, "e_1") > height_of(cx, "e_2"),
        "and that paragraph is what makes the difference: {:?} vs {:?}",
        height_of(cx, "e_1"),
        height_of(cx, "e_2")
    );
}

/// The wrapper `$$\[` on the opening line — what the pass used to write — is a math
/// fence to the parser that never closes, and it takes the message with it. Nothing
/// is drawn from it; the block form draws.
#[gpui_kit::test]
fn the_runaway_wrapper_draws_nothing(cx: &mut TestAppContext) {
    let broken = "before\n\n$$\\[\n\\frac{tau}{upsilon}\n\\]$$\n\nafter";
    let fixed =
        transcript::normalize_math::normalize("before\n\n\\[\n\\frac{tau}{upsilon}\n\\]\n\nafter")
            .into_owned();
    let (_view, cx) = open!(cx, vec![said("e_1", broken), said("e_2", &fixed)]);
    settle(cx);

    assert!(
        !found(cx, display_blocks(broken).remove(0)),
        "the runaway wrapper draws no formula"
    );
    assert!(
        found(cx, display_blocks(&fixed).remove(0)),
        "the block form does"
    );
}

/// An alias written across lines that is not a display one standing alone is left
/// exactly as it was: no dollar is written into the message, whatever the line
/// inside it starts with.
#[gpui_kit::test]
fn a_multiline_alias_that_is_not_standalone_is_left_alone(cx: &mut TestAppContext) {
    let texts = [
        "before \\(a\n+ b\\) after",
        "before \\(a\n# b\\) after",
        "before \\(a\nb\\) after",
        "before \\[a\n+ b\\] after",
        "before\n\n\\[\nx\n\\] and more prose",
    ];
    for text in texts {
        let document = transcript::normalize_math::normalize(text);
        assert_eq!(document, text, "left exactly as it was: {text:?}");
        assert!(
            !document.contains('$'),
            "no dollar written into it: {text:?}"
        );
    }
    let (_view, cx) = open!(cx, vec![said("e_1", texts[0]), said("e_2", texts[3])]);
    settle(cx);
    assert!(height_of(cx, "e_1") > px(0.), "the message is still drawn");
    assert!(height_of(cx, "e_2") > px(0.));
}

/// Raw HTML is drawn as the markup it is, so the pass writes nothing into it — and
/// the pass still works either side of it.
#[gpui_kit::test]
fn raw_html_is_left_exactly_as_it_was(cx: &mut TestAppContext) {
    for text in [
        "<pre>\n\\(phi\\)\n</pre>",
        "<code>\\(phi\\)</code>",
        "<PRE class=\"a\">\n\\(phi\\)\n</PRE>",
        "<script type=\"text/x\">\n\\(phi\\)\n</script>",
        "<!-- \\(phi\\) -->",
    ] {
        let document = transcript::normalize_math::normalize(text);
        assert_eq!(
            document, text,
            "raw HTML is left exactly as it was: {text:?}"
        );
        assert!(
            !document.contains('$'),
            "no dollar is written into it: {text:?}"
        );
    }
    let text = "<pre>\n\\(phi\\)\n</pre>\n\nbefore \\[\\frac{a}{b}\\]\n\nafter";
    let (_view, cx) = open!(cx, vec![said("e_1", text)]);
    settle(cx);
    assert!(
        found(cx, display_blocks(text).remove(0)),
        "the alias after the markup is still drawn"
    );
    assert!(height_of(cx, "e_1") > px(0.));
}

/// A destination is not prose: no dollar is ever written into a URL, while a label
/// is prose and an alias in it is drawn.
#[gpui_kit::test]
fn a_link_destination_is_never_rewritten(cx: &mut TestAppContext) {
    for text in [
        r"[a](http://e.com/\(x\))",
        r"[a](<http://e.com/\(x\)>)",
        r#"[a](http://e.com "a \(title\)")"#,
        "[label]: http://e.com/\\(x\\)",
    ] {
        let document = transcript::normalize_math::normalize(text);
        assert_eq!(document, text, "the destination is untouched: {text:?}");
        assert!(
            !document.contains('$'),
            "no dollar in a destination: {text:?}"
        );
    }
    let labelled = r"[a \(x\)](http://e.com)";
    assert_eq!(
        transcript::normalize_math::normalize(labelled),
        r"[a $\(x\)$](http://e.com)",
        "the label is prose, and is scanned"
    );
    let (_view, cx) = open!(cx, vec![said("e_1", labelled)]);
    settle(cx);
    assert!(
        height_of(cx, "e_1") > px(0.),
        "the message with a link is drawn"
    );
}

/// What the pass writes is its own answer: a second pass over a normalized message
/// changes nothing, the block form included.
#[test]
fn normalizing_twice_changes_nothing() {
    for text in [
        "before\n\n\\[\n\\frac{a}{b}\n\\]\n\nafter",
        "<pre>\n\\(x\\)\n</pre>\n\n\\[y^2\\]\n",
        r"[a](http://e.com/\(x\)) then \(y\)",
    ] {
        let once = transcript::normalize_math::normalize(text).into_owned();
        let twice = transcript::normalize_math::normalize(&once).into_owned();
        assert_eq!(once, twice, "{text}");
    }
}

/// A formula that is a paragraph of its own is a **block**: the width of the
/// reading column, whatever its picture's own width — which is what leaves the
/// picture centred in it, the whole column being the block. A formula written
/// inside a sentence is not a block: it hugs its own ink, in the line it is in.
/// The two spellings the source pass writes are one and the same block: a `$$…$$`
/// of its own paragraph, and a display alias fenced the same way.
#[gpui_kit::test]
fn a_formula_of_its_own_is_the_columns_width_and_one_in_a_sentence_is_not(cx: &mut TestAppContext) {
    let standalone = "before\n\n$$\\frac{a_9}{b_9}$$\n\nafter";
    let alias = "before\n\n\\[\n\\frac{c_9}{d_9}\n\\]\n\nafter";
    let in_a_sentence = "before $$\\frac{e_9}{f_9}$$ after";
    let (_view, cx) = open!(
        cx,
        vec![
            said("e_1", standalone),
            said("e_2", alias),
            said("e_3", in_a_sentence)
        ]
    );
    settle(cx);

    let column = MEASURE - 2. * INSET;
    let block = bounds(cx, display_blocks(standalone).remove(0));
    let fenced = bounds(cx, display_blocks(alias).remove(0));
    assert!(
        (f32::from(block.size.width) - column).abs() < 1.,
        "a `$$…$$` of its own is the column's width: {block:?} vs {column}"
    );
    assert!(
        (f32::from(fenced.size.width) - column).abs() < 1.,
        "and a fenced display alias is the same block: {fenced:?} vs {column}"
    );
    assert_eq!(
        block.size.width, fenced.size.width,
        "the two spellings are one block, so their widths are one width"
    );
    assert_eq!(
        block.left(),
        fenced.left(),
        "and stand in the same place in the column"
    );

    let line = bounds(cx, display_blocks(in_a_sentence).remove(0));
    assert!(
        line.size.width > px(0.),
        "the inline one is drawn: {line:?}"
    );
    assert!(
        f32::from(line.size.width) < column - 1.,
        "a formula inside a sentence hugs its own ink rather than the column: \
         {line:?} vs {column}"
    );
}

/// Prose stays prose. An address GFM reads as a link is left exactly as it was
/// written — a dollar in it would be shown to the reader and would break the
/// link's own target — and an alias that never closes keeps its backslash, so the
/// reader is shown the character the model typed rather than the parenthesis
/// Markdown would have made of an escape.
///
/// The rendered characters themselves are not observable from here: no element of
/// a transcript row carries a label or a value (checked against
/// `gpui_kit::base::test_support::snapshots`), so what the pass wrote is asserted
/// through the public transform, and what it means for the reader through the
/// row's height — a formula is a picture, measurably taller than the line it
/// replaces.
#[gpui_kit::test]
fn an_address_is_left_alone_and_an_opening_alias_keeps_its_backslash(cx: &mut TestAppContext) {
    let address = r"see https://example.com/\(not\)math now";
    let plain_address = "see https://example.com/(not)math now";
    let dangling = r"a \(x^2 b and on";
    let plain = "a (x^2 b and on";
    let (_view, cx) = open!(
        cx,
        vec![
            said("e_1", address),
            said("e_2", plain_address),
            said("e_3", dangling),
            said("e_4", plain)
        ]
    );
    settle(cx);

    assert_eq!(
        transcript::normalize_math::normalize(address),
        address,
        "an address is left exactly as it was written"
    );
    let escaped = transcript::normalize_math::normalize(dangling);
    assert!(
        escaped.contains("\\\\("),
        "an opener with nothing to close is written as two backslashes, so one \\
         reaches the reader: {escaped:?}"
    );
    assert!(
        !escaped.contains('$'),
        "and no math is written for it: {escaped:?}"
    );

    let address_height = height_of(cx, "e_1");
    let plain_address_height = height_of(cx, "e_2");
    assert!(
        (f32::from(address_height) - f32::from(plain_address_height)).abs() < 2.,
        "nothing was made of the alias in the address: {address_height:?} vs \
         {plain_address_height:?}"
    );
    let dangling_height = height_of(cx, "e_3");
    let plain_height = height_of(cx, "e_4");
    assert!(
        (f32::from(dangling_height) - f32::from(plain_height)).abs() < 2.,
        "and the line that never closed is text, not a picture: {dangling_height:?} vs \
         {plain_height:?}"
    );
}

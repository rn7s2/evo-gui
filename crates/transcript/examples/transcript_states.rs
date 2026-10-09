//! transcript_states — the transcript's own states, as pictures.
//!
//! The app's screens (`docs/screens.md`) show a tab, a lane at work and a report
//! arriving. They never show what the transcript itself does, which is where the two
//! newest pieces live: the journal is *one virtual list* — `gpui::list` over the whole
//! record, paging older items back by itself — and the reader's font zoom
//! ([`TranscriptZoom`], the View menu) redraws every row at another size.
//!
//! ```sh
//! cargo run -p transcript --example transcript_states --features test-support -- \
//!     --capture /tmp/lane5-captures
//! ```
//!
//! One state can be taken on its own with `--only <state>`, in an app of its own. That
//! matters for the formula in the record: the app lays a formula out *off the frame*
//! (`math::picture` starts a job and draws the source until it lands), and in this
//! headless harness that job settles for the first window's keys only — so the sweep
//! below, which opens a window per state, captures every state's first formulas as the
//! source they are drawn as before the job lands. Run a state with `--only` when the
//! picture of a formula is what matters.
//!
//! The record is [`ITEMS`] items — turns, markdown with headings, lists, a table, a code
//! fence and TeX, tool calls, lane reports, notices, goal rows, a compaction divider and
//! thinking text — and every state below is reached the way a reader reaches it: a wheel
//! over the list, a press on the pill, a message growing under a pinned reader, a card
//! folded open under the pointer, the zoom the menu sets, a window that changes shape.
//! Nothing pokes the view's fields.
//!
//! Hosting is the app's own: the transcript sits in a `flex_1().min_h_0()` box, as
//! `tab_page.rs`'s conversation column embeds it, in a *cached* view whose style is
//! `size_full()` — so what these pictures show is what the app shows.
//!
//! Every state prints: the pane's bounds, which rows of the record the frame built
//! (probed by the row ids the crate registers), the distance of the first and last
//! painted row from the pane's edges, the pill's presence, the pin's own flags, and —
//! with `test-support` — how many rows the last frame built and how many times the view
//! rendered. The view exposes no numeric scroll offset (only `is_following_tail` and
//! `is_away_from_latest`), so the reader's place is those numbers plus the painted rows.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::theme::ThemeMode;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::TestSupportExt as _;
use gpui_kit::{
    div, point, px, size, AnyView, AnyWindowHandle, AppContext as _, Bounds, Context, ElementId,
    Entity, HeadlessAppContext, InteractiveElement as _, IntoElement, ParentElement as _, Pixels,
    Render, ScrollDelta, SharedString, Styled as _, Window, WindowBounds, WindowOptions,
};
use serde_json::{json, Value};
use session::Item;

#[cfg(feature = "test-support")]
use transcript::counted;
use transcript::TranscriptView;
use transcript::TranscriptZoom;

/// The window the pictures are taken in: the conversation column's width, with the lane
/// column the app docks beside it left out.
const WINDOW_SIZE: (f32, f32) = (1000., 800.);

/// How many items the record holds.
const ITEMS: usize = 800;

/// One screen of wheel: the pane is as tall as the window.
const SCREEN: f32 = WINDOW_SIZE.1;

// ---------------------------------------------------------------------------
// The record: what a long session's journal holds.
// ---------------------------------------------------------------------------

fn item(value: Value) -> Item {
    Item::from_json(&value).expect("a fixture item has an id")
}

/// The record under construction. An item's id is where it lands (`e_0000`…), so a
/// state can say which row of the record the pane is showing.
struct Record {
    items: Vec<Item>,
    n: usize,
}

impl Record {
    fn new() -> Self {
        Self {
            items: Vec::new(),
            n: 0,
        }
    }

    fn len(&self) -> usize {
        self.items.len()
    }

    fn add(&mut self, build: impl FnOnce(&str) -> Value) {
        let id = format!("e_{:04}", self.items.len());
        self.items.push(item(build(&id)));
    }
}

/// A user turn: the reader's own words.
fn turn(record: &mut Record, text: &str) {
    record.add(|id| json!({ "id": id, "ts": 1, "kind": "user", "text": text, "status": "sent" }));
}

/// An assistant reply, with the thinking that produced it where a state wants one.
fn reply(record: &mut Record, text: &str, thinking: Option<&str>) {
    record.add(|id| {
        let mut value = json!({
            "id": id, "ts": 1, "kind": "assistant", "text": text, "status": "final",
            "model": "stub-a", "provider": "stub",
            "usage": { "input": 1200, "output": 300, "cache_read": 4000, "cache_write": 0 },
        });
        if let Some(thinking) = thinking {
            value["thinking"] = json!(thinking);
        }
        value
    });
}

/// A tool call: the arguments the name implies, and a result of its own.
fn call(record: &mut Record, name: &str, args: Value, result: String, truncated: bool) {
    record.add(|id| {
        json!({
            "id": id, "ts": 1, "kind": "tool", "call_id": format!("call_{id}"), "name": name,
            "args": args, "status": "ok",
            "result": { "text": result, "chars": result.chars().count(), "truncated": truncated },
            "parent": id,
        })
    });
}

fn report(record: &mut Record, lane: u64) {
    record.add(|id| {
        json!({
            "id": id, "ts": 1, "kind": "lane_report", "lane": lane,
            "done": "ported the item model and its rows",
            "evidence": "cargo test -p session -p transcript: 186 passed",
            "next": "the view crates, then the screens",
            "blocked": "", "requests": "", "goal": "active",
        })
    });
}

fn notice(record: &mut Record, severity: &str, text: &str) {
    record.add(|id| {
        json!({
            "id": id, "ts": 1, "kind": "notice", "severity": severity, "text": text,
            "source": "swarm", "durable": true,
        })
    });
}

fn lane_event(record: &mut Record, lane: u64, event: &str, severity: &str) {
    record.add(|id| {
        json!({
            "id": id, "ts": 1, "kind": "lane_event", "lane": lane, "event": event,
            "detail": "its process exited", "severity": severity,
        })
    });
}

fn goal(record: &mut Record, event: &str, objective: &str, tokens: u64) {
    record.add(|id| {
        json!({
            "id": id, "ts": 1, "kind": "goal", "event": event, "goal_id": "a1b2c3d4",
            "objective": objective, "budget": 50000, "tokens": tokens,
        })
    });
}

fn compaction(record: &mut Record, summary: &str) {
    record.add(|id| {
        json!({
            "id": id, "ts": 1, "kind": "compaction", "summary": summary,
            "tokens_before": 180000, "tokens_after": 9000, "manual": false,
        })
    });
}

fn outcome(record: &mut Record, outcome: &str) {
    record.add(|id| json!({ "id": id, "ts": 1, "kind": "run_outcome", "outcome": outcome }));
}

/// A long message: a heading, lists, a table, a code fence and TeX — inline and
/// displayed — so one row of it is several hundred pixels tall.
fn long_message(topic: &str, n: usize) -> String {
    format!(
        "# {topic}\n\
         \n\
         The measure is $800\\text{{px}}$ wide and the pane is the window less the column \
         beside it, so a row of prose is about \
         $\\frac{{800 - 32}}{{7.5}} \\approx 102$ characters, and the list builds \
         only what it needs:\n\
         \n\
         $$\n\
         \\text{{rows built}} = \\frac{{\\text{{pane height}}}}{{\\text{{row \
         height}}}} + \\text{{overdraw}}\n\
         $$\n\
         \n\
         Pass {n} over the same ground, and what changed: the item index is a map, \
         so a patch is a splice rather than a rescan; each message retains one document, \
         extended with `set_text`; and the journal is one virtual list, so a wheel costs a \
         pane of rows rather than a parse of the whole record.\n\
         \n\
         ## What the pane holds\n\
         \n\
         - the rows the pane reaches, plus a few rows of overdraw\n\
         - one measured height per row, kept across a splice\n\
         - the reader's place, which the pin decides\n\
         \n\
         | step | owner | state |\n\
         | --- | --- | --- |\n\
         | items | session | done |\n\
         | rows | transcript | in progress |\n\
         | screens | app | waiting |\n\
         \n\
         ```rust\n\
         let view = cx.new(TranscriptView::new);\n\
         view.update(cx, |view, cx| view.replace(items, cx));\n\
         ```\n\
         \n\
         The rest is the same work at another {n} rows: read it, move it, and let the list \
         keep the heights it measured.\n"
    )
}

/// The last message of the record: one paragraph and one display formula, and short
/// enough that the whole of it fits the pane — which is what lets the three zoom states
/// line their pictures up on it.
fn math_close() -> String {
    "The last word, in one formula: the pane is the window less the column beside it, so \
     a row of prose is about $\\frac{800 - 32}{7.5} \\approx 102$ characters — and the \
     list builds only what it needs:\n\
     \n\
     $$\n\
     \\text{rows built} = \\frac{\\text{pane height}}{\\text{row height}} + \\
     \\text{overdraw}\n\
     $$\n"
        .to_string()
}

/// A short reply, the kind that lands between two tool calls.
fn short_reply(n: usize) -> String {
    format!(
        "Reading the {n}th file now. The type is already public, so this is a rename and a \
         call site rather than a new module."
    )
}

/// The record: [`ITEMS`] items, in the order a long session produced them.
fn record() -> Vec<Item> {
    let mut record = Record::new();
    while record.len() < ITEMS {
        record.n += 1;
        let n = record.n;
        turn(
            &mut record,
            if n == 1 {
                "port the view model, then take the screens again"
            } else {
                "carry on — the next unit, and keep the record long enough that the list is \
                 what draws it"
            },
        );
        reply(
            &mut record,
            &long_message("Porting the view model", n),
            if n.is_multiple_of(3) {
                Some(
                    "The list measures a row only when it is built, so the heights arrive \
                     as the reader walks. That is the whole design: nothing above the pane \
                     is measured until someone looks at it.",
                )
            } else {
                None
            },
        );
        call(
            &mut record,
            "bash",
            json!({ "command": "cargo test -p session -p transcript", "timeout": 120 }),
            (0..24)
                .map(|i| format!("test the_{i}th_row_is_built ... ok"))
                .collect::<Vec<_>>()
                .join("\n"),
            n.is_multiple_of(2),
        );
        call(
            &mut record,
            "read",
            json!({ "path": format!("/Users/you/coding/evo-gui/crates/transcript/src/rows.rs:{n}") }),
            "//! One row of the journal, and the air between two of them.\n".repeat(6),
            false,
        );
        call(
            &mut record,
            "edit",
            json!({
                "path": format!("/Users/you/coding/evo-gui/crates/transcript/src/rows.rs:{n}"),
                "old_string": "let heights = measure_all(&items);",
                "new_string": "let heights = measure_as_the_pane_reaches(&items);",
            }),
            "the edit landed on line 512".to_string(),
            false,
        );
        reply(&mut record, &short_reply(n), None);
        report(&mut record, (n % 6 + 1) as u64);
        notice(
            &mut record,
            if n.is_multiple_of(4) { "warn" } else { "info" },
            "lane 3 was restarted by its supervisor; the transcript kept its place",
        );
        lane_event(&mut record, (n % 6 + 1) as u64, "restarted", "warn");
        goal(
            &mut record,
            "updated",
            "Ship the redesign: every screen from the design, every state bound to the real \
             view model, and the gate green.",
            (n as u64) * 4000,
        );
        if n.is_multiple_of(5) {
            compaction(
                &mut record,
                "The redesign was planned; the item model, the rows and the list are in.",
            );
        }
        if n.is_multiple_of(7) {
            outcome(&mut record, "completed");
        }
        call(
            &mut record,
            "grep",
            json!({ "pattern": "logical_scroll_top", "path": "crates/transcript/src" }),
            "crates/transcript/src/lib.rs:1261:        self.list.logical_scroll_top()\n".repeat(3),
            false,
        );
        turn(&mut record, "and the pill — does it come back on its own?");
        reply(
            &mut record,
            "It does: `Pin::jumped` puts the reader back on the tail, and the pill goes away \
             on the same frame the list reaches the foot.",
            None,
        );
    }
    record.items.truncate(ITEMS - 1);
    // The record ends on the math message, so every state can put the same formula under
    // the reader's eye and the zoom states are pictures of the same text.
    reply(&mut record, &math_close(), None);
    record.items
}

/// The folder the link states work in, and a file in it that is really there.
///
/// A path in a row's words is a link only when the disk says it is, so a picture of one
/// needs paths that exist. They are short and under `/tmp` on purpose: what the picture is
/// for is where the underlined words are, not what the machine's home is called.
fn links_folder() -> PathBuf {
    let root = PathBuf::from("/tmp/evo-links");
    std::fs::create_dir_all(root.join("notes")).expect("a folder to point at");
    let file = root.join("notes/report.md");
    if !file.exists() {
        std::fs::write(&file, "# the report\n").expect("a file to point at");
    }
    root
}

/// A short record whose rows carry addresses and paths in their *own words*: the reader's
/// turn, an answer's prose with a path in it and one written as code, a notice, a lane's
/// line, a report's field — and a call, whose arguments and its result are data.
fn links_record(root: &Path) -> Vec<Item> {
    let root = root.display().to_string();
    let file = format!("{root}/notes/report.md");
    let notes = format!("{root}/notes");
    let mut record = Record::new();
    turn(
        &mut record,
        &format!("read {file}, then the shape at https://evo.dev/state.json"),
    );
    reply(
        &mut record,
        &format!(
            "Written to `{file}` and to notes/report.md — the schema is at \
             https://evo.dev/state.json, and the notes are in {notes}."
        ),
        None,
    );
    record.add(|id| {
        json!({
            "id": id, "ts": 1, "kind": "notice", "severity": "info", "source": "swarm",
            "durable": true,
            "text": format!("lane 3 was restarted by its supervisor; it had written {file}"),
        })
    });
    record.add(|id| {
        json!({
            "id": id, "ts": 1, "kind": "lane_event", "lane": 3, "event": "restarted",
            "severity": "warn",
            "detail": format!("it could not write {file}; see https://evo.dev/lanes/3"),
        })
    });
    record.add(|id| {
        json!({
            "id": id, "ts": 1, "kind": "lane_report", "lane": 3,
            "done": format!("ported a row's own words, and `{file}` with them"),
            "evidence": "cargo test -p transcript: 134 passed",
            "next": "the captures, in both themes", "blocked": "", "requests": "",
            "goal": "active",
        })
    });
    call(
        &mut record,
        "read",
        json!({ "path": format!("{file}:12") }),
        format!("# the report\n// {file} was read from https://evo.dev/state.json\n"),
        false,
    );
    record.items
}

/// The link states, both of them: a tab that runs in the folder the record's relative
/// paths are measured from, holding a record whose own words carry links.
fn links_setup(cx: &mut HeadlessAppContext, window: AnyWindowHandle, page: &Entity<Page>) -> usize {
    let root = links_folder();
    let view = transcript_of(cx, page);
    let items = links_record(&root);
    let rows = items.len();
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.set_folder(Some(root), cx);
            view.replace(items, cx);
        })
    });
    frames(cx, window, 3);
    rows
}

/// A short record whose reader's turn carried files: the reader's own words, and the
/// files under them as chips — the one that is there, and the one that is gone.
///
/// The message is written the way the app writes it (`session::attachment_turn`): the
/// heading and one absolute path per file, in the reader's own text. The row is what
/// reads it back, so a picture of this state is a picture of that reading.
fn files_record(root: &Path) -> Vec<Item> {
    let here = root.join("notes/report.md");
    // A file that was attached and has since gone: the chip is still the message's, and
    // dimmed rather than dropped.
    let gone = root.join("notes/gone.csv");
    let _ = std::fs::remove_file(&gone);
    let text = session::attachment_turn(
        "summarise this, and say what the columns in the other one are",
        &[session::Attached::File(here), session::Attached::File(gone)],
    )
    .0;
    let mut record = Record::new();
    turn(&mut record, &text);
    reply(
        &mut record,
        "The report is a summary of the run; the columns are in the spreadsheet, which I \
         could not open — it is not there any more.",
        None,
    );
    record.items
}

/// The files state: a message with the files it carried drawn under its words.
fn files_setup(cx: &mut HeadlessAppContext, window: AnyWindowHandle, page: &Entity<Page>) -> usize {
    let root = links_folder();
    let view = transcript_of(cx, page);
    let items = files_record(&root);
    let rows = items.len();
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.set_folder(Some(root), cx);
            view.replace(items, cx);
        })
    });
    frames(cx, window, 3);
    rows
}

/// The folder the picture states work in: a picture in it that is really there, a file
/// that is not a picture, and nothing under the name the record asks about.
///
/// Short and under `/tmp` on purpose, like the links folder: what a picture of the state
/// is for is the picture in the row, not what the machine's home is called.
fn pictures_folder() -> PathBuf {
    let root = PathBuf::from("/tmp/evo-pictures");
    std::fs::create_dir_all(&root).expect("a folder to point at");
    let shot = root.join("shot.png");
    if !shot.exists() {
        let bytes = png_bytes(image::RgbaImage::from_fn(1400, 1000, |x, y| {
            image::Rgba([
                (x * 255 / 1400) as u8,
                (y * 255 / 1000) as u8,
                if x < 8 || y < 8 || x + 8 > 1400 || y + 8 > 1000 {
                    40
                } else {
                    160
                },
                255,
            ])
        }));
        std::fs::write(&shot, bytes).expect("a picture to point at");
    }
    let notes = root.join("notes.txt");
    if !notes.exists() {
        std::fs::write(&notes, "not a picture\n").expect("a file that is not one");
    }
    root
}

/// A short record whose answer points at a picture on this machine — `reference`, the
/// form the state is about — and at three that are not pictures: a file that is not
/// there, a file that is not a picture, and an address. So a picture of the state shows
/// what is drawn and what keeps the fallback.
fn pictures_record(root: &Path, reference: &str) -> Vec<Item> {
    let root = root.display().to_string();
    let mut record = Record::new();
    turn(
        &mut record,
        "what does the run look like? /tmp/evo-pictures is where the shots are",
    );
    reply(
        &mut record,
        &format!(
            "Here it is:\n\n![the run]({reference})\n\n![gone](file://{root}/gone.png) \
             ![notes](file://{root}/notes.txt) ![remote](https://evo.dev/run.png)"
        ),
        None,
    );
    record.items
}

/// How a state is reached: it opens its own record and says how many rows that record
/// holds.
type Setup = fn(&mut HeadlessAppContext, AnyWindowHandle, &Entity<Page>) -> usize;

/// A picture state: it points the message at the file the way `reference` names it, so
/// the two states differ only in the form of the reference — the `file://` URL a model
/// writes, and a path relative to the folder the tab runs in.
fn pictures_setup(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    page: &Entity<Page>,
    reference: fn(&Path) -> String,
) -> usize {
    let root = pictures_folder();
    let view = transcript_of(cx, page);
    let items = pictures_record(&root, &reference(&root));
    let rows = items.len();
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.set_folder(Some(root), cx);
            view.replace(items, cx);
        })
    });
    // The read is a worker's: the picture lands a frame or two after the record does.
    for _ in 0..6 {
        cx.run_until_parked();
        frames(cx, window, 2);
    }
    rows
}

/// The picture state that names the file as a URL.
fn pictures_at_url(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    page: &Entity<Page>,
) -> usize {
    pictures_setup(cx, window, page, picture_url)
}

/// The picture state that names the file relative to the tab's folder.
fn pictures_beside_the_tab(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    page: &Entity<Page>,
) -> usize {
    pictures_setup(cx, window, page, picture_relative)
}

/// The picture's address, the way a model that wrote it would: a `file://` URL of the
/// file itself.
fn picture_url(root: &Path) -> String {
    format!("file://{}/shot.png", root.display())
}

/// The same picture, named the way a message names a file beside the project: a path
/// relative to the folder the tab runs in.
fn picture_relative(_root: &Path) -> String {
    "shot.png".to_string()
}

/// The record the lane-switch states are about: a session that has run for a while,
/// ending on the reader's own words — one row longer than the pane, so a pane showing the
/// foot of the record is a pane full of them — with the answer to them arriving while
/// another lane is on screen.
fn lane_switch_record() -> Vec<Item> {
    let mut record = Record::new();
    for n in 1..=24 {
        turn(
            &mut record,
            &format!(
                "step {n}: carry on — the next unit, and keep the record long enough that \
                 the list is what draws it"
            ),
        );
        reply(
            &mut record,
            "The list measures a row when the pane reaches it, so a row nobody has laid \
             out has no height at all yet.",
            None,
        );
        call(
            &mut record,
            "bash",
            json!({ "command": "cargo test -p transcript --test pane" }),
            "test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured\n".repeat(3),
            false,
        );
    }
    let long = "the reader's own turn, long enough that this one row is longer than the \
                pane it is read in — many times over, so that a reader scrolled up into it \
                is reading it, and not something else. "
        .repeat(18);
    turn(&mut record, &long);
    record.items
}

/// The switch back to a session whose reader had scrolled up into their own message.
///
/// Another lane was on screen, so this transcript was not drawn while the answer arrived;
/// the switch back is `TabModel::select`'s reset worked out in the view — the same items,
/// the answer last, replaced outright. What the picture is of is the reader's place in it:
/// the words the pane was showing before the switch are the words it must show after.
fn switched_back(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    page: &Entity<Page>,
    swarm: bool,
) -> usize {
    let view = transcript_of(cx, page);
    let items = lane_switch_record();
    let rows = items.len();
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            // The program the tab is (§7.2): a swarm's coordinator, or the one agent of
            // a single-agent session. Both take the same switch, and a record draws the
            // same either way.
            view.set_swarm(swarm, cx);
            view.replace(items, cx);
        })
    });
    frames(cx, window, 3);
    // The reader wheels up into their own words: from here the list is theirs, and where
    // it is at is the place the switch must keep.
    for _ in 0..3 {
        wheel(cx, window, SCREEN);
    }
    // The answer arrives while another lane is shown; the switch back re-reads the topic
    // into the view — the same items, the answer last.
    let answer = item(json!({
        "id": item_id(rows), "ts": 2, "kind": "assistant", "status": "final",
        "text": "the answer to the reader's own turn, written while another lane was on \
                 screen: the row the switch must not hide.",
        "model": "stub-a", "provider": "stub",
    }));
    cx.update(|cx| {
        view.update(cx, |view, cx| view.upsert(answer, cx));
    });
    let held = cx.update(|cx| view.read(cx).items(cx).to_vec());
    cx.update(|cx| {
        view.update(cx, |view, cx| view.replace(held, cx));
    });
    frames(cx, window, 3);
    rows + 1
}

// ---------------------------------------------------------------------------
// The host: the app's own embedding, one cached view in a `flex_1` box.
// ---------------------------------------------------------------------------

struct Page {
    transcript: Entity<TranscriptView>,
}

impl Page {
    fn new(items: &[Item], cx: &mut Context<Self>) -> Self {
        let transcript = cx.new(TranscriptView::new);
        transcript.update(cx, |view, cx| view.replace(items.to_vec(), cx));
        Self { transcript }
    }
}

impl Render for Page {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        // The conversation column's own box (§7.3): the transcript fills it, embedded as
        // a cached view whose style is `size_full()` — a cached view is laid out from its
        // style alone, so it must fill the box outright.
        div()
            .id("page")
            .test_support()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                div()
                    .id("transcript-box")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .children(Some(
                        AnyView::from(self.transcript.clone())
                            .cached(gpui_kit::StyleRefinement::default().size_full()),
                    )),
            )
    }
}

fn context() -> HeadlessAppContext {
    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(gpui_kit::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(gpui_kit::init);
    cx
}

/// Open the capture window with the record in it, at its tail, `width` pixels wide: the
/// transcript's pane is what bounds its measure, so how wide the window is is part of what
/// a state is a picture of.
fn open(
    cx: &mut HeadlessAppContext,
    items: &[Item],
    width: f32,
) -> (AnyWindowHandle, Entity<Page>) {
    cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    gpui_kit::point(px(0.), px(0.)),
                    size(px(width), px(WINDOW_SIZE.1)),
                ))),
                focus: false,
                show: false,
                ..Default::default()
            },
            cx,
            |_window, cx| cx.new(|cx| Page::new(items, cx)),
        )
    })
    .expect("open the capture window")
}

// ---------------------------------------------------------------------------
// The things a reader does.
// ---------------------------------------------------------------------------

/// A frame, or three: a state is whatever the last of them left behind.
fn frames(cx: &mut HeadlessAppContext, window: AnyWindowHandle, count: usize) {
    cx.update_window(window, |_, window, cx| {
        for _ in 0..count {
            window.render_frame(cx);
        }
    })
    .expect("the capture window is open");
}

/// A wheel over the list, at its centre: a real `ScrollWheelEvent`, so it lands in the
/// box's own handler the way a reader's does — the one scroll that unpins the list.
fn wheel(cx: &mut HeadlessAppContext, window: AnyWindowHandle, dy: f32) {
    cx.update_window(window, |_, window, cx| {
        window.scroll(
            "transcript-scroll",
            ScrollDelta::Pixels(point(px(0.), px(dy))),
            cx,
        );
    })
    .expect("the capture window is open");
    frames(cx, window, 3);
}

/// The id of a row of the record, built the way `rows.rs` builds it.
fn row_id(id: &str) -> ElementId {
    named_row_id("transcript-row", id)
}

/// The id of one element of a row, built the way `rows.rs` builds it.
fn named_row_id(name: &str, id: &str) -> ElementId {
    (ElementId::from(SharedString::from(name)), id.to_string()).into()
}

/// The id the record's `n`th item was given.
fn item_id(n: usize) -> String {
    format!("e_{n:04}")
}

/// Which rows of the record the last frame built: the list builds the rows the pane
/// reaches, so this is the pane's own window onto the journal.
fn painted(window: &Window, rows: usize) -> (Option<usize>, Option<usize>) {
    let mut first = None;
    let mut last = None;
    for n in 0..rows {
        if window.try_find(row_id(&item_id(n))).is_some() {
            first.get_or_insert(n);
            last = Some(n);
        }
    }
    (first, last)
}

/// Wheel up until the head of the record is what the pane shows, and say how many wheels
/// it took — the list builds and measures a row when the pane reaches it, so walking back
/// through a long record is a walk rather than one jump.
fn wheel_to_the_head(cx: &mut HeadlessAppContext, window: AnyWindowHandle) -> Option<usize> {
    for taken in 1..=400 {
        wheel(cx, window, SCREEN * 2.);
        if painted_in(cx, window).0 == Some(0) {
            return Some(taken);
        }
    }
    None
}

fn painted_in(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
) -> (Option<usize>, Option<usize>) {
    cx.update_window(window, |_, window, _| painted(window, ITEMS))
        .expect("the capture window is open")
}

/// Let the simulated clock move on, so a timer-driven animation — the jump's own steps,
/// the pill's 160ms fade — runs to its end.
fn run_animation(cx: &mut HeadlessAppContext, window: AnyWindowHandle, budget: Duration) {
    let step = Duration::from_millis(20);
    let mut spent = Duration::ZERO;
    while spent < budget {
        cx.advance_clock(step);
        cx.run_until_parked();
        frames(cx, window, 1);
        spent += step;
    }
}

// ---------------------------------------------------------------------------
// What a state says about itself.
// ---------------------------------------------------------------------------

struct Report {
    size: (f32, f32),
    page: (f32, f32),
    rows: usize,
    pane: Bounds<Pixels>,
    built: (Option<usize>, Option<usize>),
    head: Option<f32>,
    foot: Option<f32>,
    pill: bool,
    loading_line: bool,
    following: bool,
    away: bool,
    older: bool,
    loading: bool,
    rows_built: Option<u64>,
    renders: Option<u64>,
}

/// One frame of its own, with the row counter reset, and the state read off it.
fn measure(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    page: &Entity<Page>,
    rows: usize,
) -> Report {
    #[cfg(feature = "test-support")]
    counted::reset();
    frames(cx, window, 1);
    #[cfg(feature = "test-support")]
    let rows_built = Some(counted::rows());
    #[cfg(not(feature = "test-support"))]
    let rows_built = None;

    let page = page.clone();
    cx.update_window(window, |_, window, cx| {
        let transcript = page.read(cx).transcript.clone();
        let view = transcript.read(cx);
        // The box the app gives the transcript, not the cached view inside it: a
        // cached subtree is re-laid out without being re-rendered, so its own
        // registration can be a frame older than the window around it.
        let page = window.find("page").bounds();
        let pane = window.find("transcript-box").bounds();
        let built = painted(window, rows);
        let top = built
            .0
            .and_then(|n| window.try_find(row_id(&item_id(n))))
            .map(|row| f32::from(row.bounds().top() - pane.top()));
        let foot = built
            .1
            .and_then(|n| window.try_find(row_id(&item_id(n))))
            .map(|row| f32::from(pane.bottom() - row.bounds().bottom()));
        Report {
            size: (
                f32::from(window.bounds().size.width),
                f32::from(window.bounds().size.height),
            ),
            page: (f32::from(page.size.width), f32::from(page.size.height)),
            rows,
            pane,
            built,
            head: top,
            foot,
            pill: window.find("transcript-jump").visible(),
            loading_line: window.try_find("transcript-loading-older").is_some(),
            following: view.is_following_tail(cx),
            away: view.is_away_from_latest(cx),
            older: view.has_older(),
            loading: view.is_loading_older(),
            rows_built,
            renders: renders(view),
        }
    })
    .expect("the capture window is open")
}

#[cfg(feature = "test-support")]
fn renders(view: &TranscriptView) -> Option<u64> {
    Some(view.renders())
}

#[cfg(not(feature = "test-support"))]
fn renders(_view: &TranscriptView) -> Option<u64> {
    None
}

fn say(name: &str, report: &Report) {
    let rows_of_the_record = report.rows;
    let rows = match report.rows_built {
        Some(rows) => rows.to_string(),
        None => "n/a (build with --features test-support)".to_string(),
    };
    let renders = match report.renders {
        Some(renders) => renders.to_string(),
        None => "n/a".to_string(),
    };
    let place = match report.built {
        (Some(first), Some(last)) => format!(
            "{}..={last} of {rows_of_the_record}",
            item_id(first).trim_start_matches("e_")
        ),
        _ => "none".to_string(),
    };
    println!(
        "[states] {name:<16} window {}x{} | page {:.0}x{:.0} | pane {:.0}x{:.0} | painted {place} \
         | head {:+.1}px foot {:+.1}px | pill {} | loading line {} | pin following={} away={} \
         older={} loading={} | rows built {rows} | renders {renders}",
        report.size.0,
        report.size.1,
        report.page.0,
        report.page.1,
        f32::from(report.pane.size.width),
        f32::from(report.pane.size.height),
        report.head.unwrap_or(f32::NAN),
        report.foot.unwrap_or(f32::NAN),
        if report.pill { "shown" } else { "hidden" },
        if report.loading_line {
            "shown"
        } else {
            "hidden"
        },
        report.following,
        report.away,
        report.older,
        report.loading,
    );
}

// ---------------------------------------------------------------------------
// The states.
// ---------------------------------------------------------------------------

/// The zoom a state draws at, for the zoom states.
fn set_zoom(cx: &mut HeadlessAppContext, scale: f32) {
    cx.update(|cx| TranscriptZoom(scale).set(cx));
}

fn click(cx: &mut HeadlessAppContext, window: AnyWindowHandle, id: &'static str) {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(id, cx);
    })
    .expect("the capture window is open");
}

/// The view the page hosts.
fn transcript_of(cx: &mut HeadlessAppContext, page: &Entity<Page>) -> Entity<TranscriptView> {
    let page = page.clone();
    cx.update(|cx| page.read(cx).transcript.clone())
}

/// Grow the last message the way a stream of `item.append` ops does, and say whether the
/// reader is still on the tail after each one.
fn stream(cx: &mut HeadlessAppContext, window: AnyWindowHandle, page: &Entity<Page>) {
    let view = transcript_of(cx, page);
    for step in 1..=6 {
        cx.update(|cx| {
            let last = view
                .read(cx)
                .items(cx)
                .last()
                .expect("the record ends on a message")
                .clone();
            let mut raw = last.raw().clone();
            let text = raw.get("text").and_then(Value::as_str).unwrap_or("");
            raw["text"] = json!(format!(
                "{text}\n\nThe {step}th paragraph of the same message, still arriving: the \
                 document is extended, never parsed again.\n"
            ));
            raw["status"] = json!("streaming");
            view.update(cx, |view, cx| {
                view.upsert(item(raw), cx);
            });
        });
        frames(cx, window, 2);
        let following = cx.update(|cx| view.read(cx).is_following_tail(cx));
        println!("[states] stream step {step}: following={following}");
    }
}

// ---------------------------------------------------------------------------
// The two corners of a tool card's own ink.
// ---------------------------------------------------------------------------

/// The heads of the tool rows the pane has built, with where each was painted: the
/// list builds a row when the pane reaches it, so this is the pane's own window onto
/// the cards again.
fn tool_heads(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
) -> Vec<(ElementId, Bounds<Pixels>)> {
    let mut heads = Vec::new();
    for n in 0..ITEMS {
        let head: ElementId = (
            ElementId::from(SharedString::from("transcript-tool")),
            item_id(n),
        )
            .into();
        let found = cx
            .update_window(window, |_, window, _| {
                window.try_find(head.clone()).map(|fact| fact.bounds())
            })
            .expect("the capture window is open");
        if let Some(bounds) = found {
            heads.push((head, bounds));
        }
    }
    heads
}

/// A pane that has stopped moving: opening a card changes the height of a row above
/// the one a reader is looking at, so a head probed before the list has settled is a
/// head in the wrong place.
fn settle(cx: &mut HeadlessAppContext, window: AnyWindowHandle) {
    for _ in 0..6 {
        cx.run_until_parked();
        frames(cx, window, 2);
    }
}

/// Put the pointer on a row, as a reader's does.
fn hover(cx: &mut HeadlessAppContext, window: AnyWindowHandle, head: ElementId) {
    cx.update_window(window, |_, window, cx| window.hover(head, cx))
        .expect("the capture window is open");
    settle(cx, window);
}

fn main() {
    let dir: PathBuf = std::env::args()
        .skip_while(|arg| arg != "--capture")
        .nth(1)
        .unwrap_or_else(|| "/tmp/transcript-states".to_string())
        .into();
    std::fs::create_dir_all(&dir).expect("the capture directory");

    let only: Option<String> = std::env::args().skip_while(|arg| arg != "--only").nth(1);
    let items = record();
    println!("[states] the record: {} items", items.len());

    // The states, in the order they are read: the list's own places, a tool card's own
    // two corners, then the pill and the walk back, then a message growing, then the
    // scrollback's quiet line, then the reader's zoom, then a window that changes shape.
    // A state says how many rows the record it opened holds: the link states replace the
    // long record with a short one whose rows carry links.
    // One entry per state: what it is called, the zoom it draws at, how wide its
    // window is (the reading measure's own bound is the pane, so a picture of the
    // measure needs a pane wider than it), and how it is reached.
    let states: [(&str, f32, f32, Setup); 31] = [
        ("bottom", 1., WINDOW_SIZE.0, |_, _, _| ITEMS),
        ("tool-hover", 1., WINDOW_SIZE.0, |cx, window, _| {
            // A folded card under the pointer: the head's hover ink is the whole of
            // the card's inside, so all four of its corners are the card's own.
            settle(cx, window);
            let shown = tool_heads(cx, window).into_iter().find(|(_, bounds)| {
                bounds.top() >= px(60.) && bounds.bottom() <= px(WINDOW_SIZE.1)
            });
            if let Some((head, _)) = shown {
                hover(cx, window, head);
            }
            ITEMS
        }),
        ("tool-open", 1., WINDOW_SIZE.0, |cx, window, _| {
            // An open card: the head's own ink reaches the top corners, and the body's
            // fill the bottom ones, with the head's underside square between them.
            settle(cx, window);
            let last = tool_heads(cx, window).pop();
            if let Some((head, _)) = last {
                cx.update_window(window, |_, window, cx| window.click(head.clone(), cx))
                    .expect("the capture window is open");
                settle(cx, window);
                hover(cx, window, head);
            }
            ITEMS
        }),
        ("image-row", 1., WINDOW_SIZE.0, |cx, window, page| {
            // A turn that carried a picture, at the tail: the thumbnail sits in a
            // rounded frame, and the picture's own corners must follow it rather than
            // fill the frame's corners with the picture's square edge.
            let view = transcript_of(cx, page);
            let shot = picture_bytes();
            cx.update(|cx| {
                view.update(cx, |view, cx| {
                    view.upsert(
                        session::Item::from_json(&json!({
                            "id": "e_image", "ts": 1, "kind": "user", "status": "sent",
                            "text": "this is what the pane looked like",
                            "images": [{ "name": "pane.png", "media_type": "image/png",
                                         "bytes": shot.len(), "href": "/media/e_image/0" }]
                        }))
                        .expect("an image turn"),
                        cx,
                    );
                    if let Some(frame) = transcript::decode_image(&shot) {
                        view.set_image("e_image", 0, frame, cx);
                    }
                })
            });
            settle(cx, window);
            ITEMS
        }),
        // A turn that carried three pictures, at the reader's own zoom: two of them
        // fetched and drawn as thumbnails, the third one the server could not answer —
        // which is a line saying so, not a hole. This is the state a session resumed
        // from history reaches as its fetches land, one item at a time.
        ("images", 1.5, WINDOW_SIZE.0, |cx, window, page| {
            let view = transcript_of(cx, page);
            let shot = picture_bytes();
            cx.update(|cx| {
                view.update(cx, |view, cx| {
                    view.upsert(
                        session::Item::from_json(&json!({
                            "id": "e_images", "ts": 1, "kind": "user", "status": "sent",
                            "text": "three shots from the run",
                            "images": [
                                { "name": "pane.png", "media_type": "image/png",
                                  "bytes": shot.len(), "href": "/media/e_images/0" },
                                { "name": "lane.png", "media_type": "image/png",
                                  "bytes": shot.len(), "href": "/media/e_images/1" },
                                { "name": "gone.png", "media_type": "image/png",
                                  "bytes": 512, "href": "/media/e_images/2" }
                            ]
                        }))
                        .expect("an image turn"),
                        cx,
                    );
                    if let Some(frame) = transcript::decode_image(&shot) {
                        view.set_image("e_images", 0, frame.clone(), cx);
                        view.set_image("e_images", 1, frame, cx);
                    }
                    // The fetch that came back 404: the row says what it is instead of
                    // leaving the reader with a gap.
                    view.set_image_failed("e_images", 2, cx);
                })
            });
            settle(cx, window);
            ITEMS
        }),
        // One turn carrying the three sizes a picture comes in: the smallest there is, an
        // icon, and a screenshot. A tiny picture used to be a dot — the frame was the
        // picture's own size — and is now a block of its own pixels, baked up to a whole
        // number of them, centred in a box a reader can see.
        ("tiny-pictures", 1., WINDOW_SIZE.0, |cx, window, page| {
            let view = transcript_of(cx, page);
            let dot = checker_bytes(1, 1);
            let icon = checker_bytes(8, 8);
            let shot = picture_bytes();
            cx.update(|cx| {
                view.update(cx, |view, cx| {
                    view.upsert(
                        session::Item::from_json(&json!({
                            "id": "e_tiny", "ts": 1, "kind": "user", "status": "sent",
                            "text": "one pixel, one icon, one screenshot",
                            "images": [
                                { "name": "dot.png", "media_type": "image/png",
                                  "bytes": dot.len(), "href": "/media/e_tiny/0" },
                                { "name": "icon.png", "media_type": "image/png",
                                  "bytes": icon.len(), "href": "/media/e_tiny/1" },
                                { "name": "pane.png", "media_type": "image/png",
                                  "bytes": shot.len(), "href": "/media/e_tiny/2" }
                            ]
                        }))
                        .expect("an image turn"),
                        cx,
                    );
                    for (n, bytes) in [&dot, &icon, &shot].into_iter().enumerate() {
                        if let Some(frame) = transcript::decode_image(bytes) {
                            view.set_image("e_tiny", n as u32, frame, cx);
                        }
                    }
                })
            });
            settle(cx, window);
            ITEMS
        }),
        ("scrolled-up", 1., WINDOW_SIZE.0, |cx, window, _| {
            for _ in 0..3 {
                wheel(cx, window, SCREEN);
            }
            ITEMS
        }),
        ("top", 1., WINDOW_SIZE.0, |cx, window, _| {
            match wheel_to_the_head(cx, window) {
                Some(wheels) => println!(
                    "[states] top: {wheels} wheels of {}px reached the head",
                    SCREEN * 2.
                ),
                None => println!("[states] top: the head was never reached"),
            }
            ITEMS
        }),
        ("jump-back", 1., WINDOW_SIZE.0, |cx, window, page| {
            for _ in 0..3 {
                wheel(cx, window, SCREEN);
            }
            let view = transcript_of(cx, page);
            let away = cx.update(|cx| view.read(cx).is_away_from_latest(cx));
            println!("[states] jump-back: away before the press={away}");
            click(cx, window, "transcript-jump");
            run_animation(cx, window, Duration::from_millis(240));
            ITEMS
        }),
        ("streaming", 1., WINDOW_SIZE.0, |cx, window, page| {
            stream(cx, window, page);
            ITEMS
        }),
        ("loading-older", 1., WINDOW_SIZE.0, |cx, window, page| {
            let view = transcript_of(cx, page);
            cx.update(|cx| view.update(cx, |view, cx| view.set_history(true, true, cx)));
            match wheel_to_the_head(cx, window) {
                Some(wheels) => println!(
                    "[states] loading-older: {wheels} wheels of {}px reached the head",
                    SCREEN * 2.
                ),
                None => println!("[states] loading-older: the head was never reached"),
            }
            ITEMS
        }),
        ("zoom-150-bottom", 1.5, WINDOW_SIZE.0, |cx, window, _| {
            frames(cx, window, 3);
            ITEMS
        }),
        ("zoom-75-bottom", 0.75, WINDOW_SIZE.0, |cx, window, _| {
            frames(cx, window, 3);
            ITEMS
        }),
        ("mid", 1., WINDOW_SIZE.0, |cx, window, _| {
            for _ in 0..2 {
                wheel(cx, window, SCREEN);
            }
            ITEMS
        }),
        ("zoom-150-mid", 1.5, WINDOW_SIZE.0, |cx, window, _| {
            for _ in 0..2 {
                wheel(cx, window, SCREEN);
            }
            ITEMS
        }),
        // A screen and a half up: the message's code fence, whose mono type follows
        // the zoom like the prose around it.
        ("zoom-150-code", 1.5, WINDOW_SIZE.0, |cx, window, _| {
            wheel(cx, window, SCREEN * 1.5);
            ITEMS
        }),
        ("zoom-75-mid", 0.75, WINDOW_SIZE.0, |cx, window, _| {
            for _ in 0..2 {
                wheel(cx, window, SCREEN);
            }
            ITEMS
        }),
        ("resize", 1., WINDOW_SIZE.0, |cx, window, _| {
            cx.update_window(window, |_, window, _| {
                window.resize(size(px(WINDOW_SIZE.0), px(WINDOW_SIZE.1 - 240.)));
            })
            .expect("the capture window is open");
            frames(cx, window, 3);
            ITEMS
        }),
        ("resize-back", 1., WINDOW_SIZE.0, |cx, window, _| {
            cx.update_window(window, |_, window, _| {
                window.resize(size(px(WINDOW_SIZE.0), px(WINDOW_SIZE.1 - 240.)));
            })
            .expect("the capture window is open");
            frames(cx, window, 2);
            cx.update_window(window, |_, window, _| {
                window.resize(size(px(WINDOW_SIZE.0), px(WINDOW_SIZE.1)));
            })
            .expect("the capture window is open");
            frames(cx, window, 3);
            ITEMS
        }),
        // The reading measure at the reader's zoom (§7.2): the same closing message in a
        // pane wider than the measure, so what the picture shows is the measure itself —
        // the paragraph breaks in the same places and the display formula is the same
        // block of the column, four sizes over. Unscaled, the paragraph would be half
        // again as many lines at 200% and its lines half as long at 75%.
        ("zoom-075-wide", 0.75, 1800., |_, _, _| ITEMS),
        ("zoom-100-wide", 1.0, 1800., |_, _, _| ITEMS),
        ("zoom-150-wide", 1.5, 1800., |_, _, _| ITEMS),
        ("zoom-200-wide", 2.0, 1800., |_, _, _| ITEMS),
        // And narrower than the measure, where the column is the pane at every zoom:
        // 150% asks for 1200px and the window has 700.
        ("zoom-150-narrow", 1.5, 700., |_, _, _| ITEMS),
        // The links: every row that draws a reader's or an agent's own words — a turn, an
        // answer's prose, a notice, a lane's line, a report's field — with the addresses
        // and paths in them underlined. The call below it is the contrast: its arguments
        // and its result hold the same kinds of words, and none of them is a link.
        ("links", 1., WINDOW_SIZE.0, links_setup),
        // A turn whose message carried files: the reader's words, and the files under
        // them as chips — the one that is there, and the one that is gone, which is
        // drawn dimmed and presses nothing.
        ("files", 1., WINDOW_SIZE.0, files_setup),
        // The same record with the call opened: what a call carried is drawn as data.
        ("links-call", 1., WINDOW_SIZE.0, |cx, window, page| {
            let rows = links_setup(cx, window, page);
            cx.update_window(window, |_, window, cx| {
                window.click(named_row_id("transcript-tool", &item_id(rows - 1)), cx);
            })
            .expect("the capture window is open");
            frames(cx, window, 3);
            rows
        }),
        // The switch back to a lane whose reader had scrolled up into their own turn,
        // with the answer to it arriving while another lane was shown: the row the pane
        // showed before the switch is the row it must show after (both programs).
        ("lane-switch", 1., WINDOW_SIZE.0, |cx, window, page| {
            switched_back(cx, window, page, true)
        }),
        (
            "lane-switch-single",
            1.,
            WINDOW_SIZE.0,
            |cx, window, page| switched_back(cx, window, page, false),
        ),
        // The picture a message points at on this machine, in the column it is read in,
        // and the same record in a pane narrower than the reading measure: the two the
        // picture's own width is chosen by.
        ("pictures", 1., WINDOW_SIZE.0, pictures_at_url),
        ("pictures-narrow", 1., 620., pictures_beside_the_tab),
    ];

    let mut cx = context();
    for (name, scale, width, setup) in states {
        if only.as_deref().is_some_and(|only| only != name) {
            continue;
        }
        let (window, page) = open(&mut cx, &items, width);
        // The zoom is one global for the whole app (§7.2), so every state says which
        // scale it draws at rather than inheriting the last state's.
        set_zoom(&mut cx, scale);
        frames(&mut cx, window, 3);
        let rows = setup(&mut cx, window, &page);
        // Let anything the state set in motion come to rest before the picture is taken:
        // an animation's steps, and the formulas the app lays out off the frame — a
        // formula a frame first meets draws its source, and the picture lands once the
        // executor has run the job and the document has laid out again.
        for _ in 0..12 {
            std::thread::sleep(Duration::from_millis(20));
            cx.run_until_parked();
            frames(&mut cx, window, 2);
        }

        let report = measure(&mut cx, window, &page, rows);
        say(name, &report);

        for (mode, suffix) in [(ThemeMode::Light, "light"), (ThemeMode::Dark, "dark")] {
            cx.update_window(window, |_, window, cx| {
                gpui_kit::component::Theme::change(mode, Some(window), cx);
            })
            .expect("the capture window is open");
            // A theme is another ink, and a formula's picture is keyed by its ink: the
            // change sends every formula on screen back to the executor, and the frames
            // in between draw its source. Wait for the pictures as after the setup, or
            // the capture shows TeX where the app would show it only for a moment.
            for _ in 0..12 {
                std::thread::sleep(Duration::from_millis(20));
                cx.run_until_parked();
                frames(&mut cx, window, 2);
            }
            let image = cx.capture_screenshot(window).expect("a frame to capture");
            let path = dir.join(format!("{name}-{suffix}.png"));
            image.save(&path).expect("write the picture");
            println!(
                "[states] {name:<16} {}x{} -> {}",
                image.width(),
                image.height(),
                path.display()
            );
        }
    }
}

/// A picture with something to see at its corners: a saturated diagonal gradient, so a
/// corner the frame does not clip shows as a square of colour against the frame's curve.
fn picture_bytes() -> Vec<u8> {
    png_bytes(image::RgbaImage::from_fn(240, 160, |x, y| {
        image::Rgba([(x * 255 / 240) as u8, 90, (y * 255 / 160) as u8, 255])
    }))
}

/// A picture of a given size in two inks: at 8×8 it is pixel art, at 1×1 it is one square
/// of the first ink — the smallest picture there is, and what a thumbnail of one is about.
fn checker_bytes(width: u32, height: u32) -> Vec<u8> {
    png_bytes(image::RgbaImage::from_fn(width, height, |x, y| {
        if (x + y) % 2 == 0 {
            image::Rgba([214, 61, 61, 255])
        } else {
            image::Rgba([244, 244, 245, 255])
        }
    }))
}

fn png_bytes(image: image::RgbaImage) -> Vec<u8> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut bytes, image::ImageFormat::Png)
        .expect("encode the fixture picture");
    bytes.into_inner()
}

//! M2 (§12) at the app level: a swarm that ran once is offered first by the next
//! launch, and comes back where it left off.
//!
//! Everything in here is the app's own machinery: two real `evo-swarm serve`
//! processes and their lanes, started from the installed binaries into a temp
//! `EVO_HOME` whose `init.lisp` registers a stub provider, the real `Shell`,
//! window, tabs, composer, quit sequence and `app.json` — read back by a *second*
//! app instance on the same data root (`TestAppContext::new_app`). Each swarm gets
//! one turn: a swarm writes its journal with its first reply, and only a tab with a
//! journal is something the next launch can offer.
//!
//! The milestone's two claims are what the assertions are about: the first launch
//! leaves three tabs and marks exactly the two swarms open at quit, and the second
//! launch offers those two first — badged and clickable — and resuming one boots a
//! swarm with `--resume` whose transcript still holds the turn from before.
//!
//! `cargo test -p evo-desktop --test m2_relaunch -- --nocapture` prints the step
//! timings.

mod common;

use std::path::PathBuf;
use std::time::Instant;

use common::{
    command_line, drain, launch, open, quit, running, same_path, send, set_evo_home, tab_state,
    transcript, wait_booted, wait_for, wait_for_text, wait_nothing_left, NOTE,
};
use evo_desktop::{history_entries, push_launcher_data, AppLog, Shell};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, ElementId, TestAppContext};
use store::app_state::{AppState, Binaries, Recent};
use store::history::{self, ScanBudget};
use store::paths::Root as AppRoot;
use swarm_client::harness::{Fixture, HarnessConfig};
use workspace::TabState;

/// The first turn, and what the stub model answers it with (`ok: <the turn>`).
const PROMPT: &str = "M2 first turn";
const PROMPT_REPLY: &str = "ok: M2 first turn";
/// The turn B gets, so that both swarms have written a journal by the quit.
const B_PROMPT: &str = "M2 first turn in B";
const B_REPLY: &str = "ok: M2 first turn in B";
/// A turn sent after the resume, in the second launch.
const SECOND: &str = "M2 after the resume";
const SECOND_REPLY: &str = "ok: M2 after the resume";

/// The ids the history list gives its rows and their badge
/// (`workspace::empty_tab`'s own constants).
const HISTORY_ROW: &str = "history-row";
const OPEN_AT_QUIT: &str = "history-open-at-quit";

#[gpui_kit::test]
fn a_swarm_that_ran_once_is_offered_first_and_comes_back(cx: &mut TestAppContext) {
    let whole = Instant::now();
    cx.dispatcher.allow_parking();
    cx.update(gpui_kit::init);

    // A stub provider, a temp evo home (`init.lisp` and all), and the installed
    // binaries — the environment `tab_engine`'s own end-to-end tests use.
    let fixture = Fixture::new(HarnessConfig {
        workers: 1,
        ..Default::default()
    })
    .expect("the fixture: a stub provider, a temp evo home and the installed binaries");

    set_evo_home(&fixture);

    let root = AppRoot::at(fixture.home.join("desktop"));
    let a = fixture.dir.join("A");
    let b = fixture.dir.join("B");
    std::fs::create_dir_all(&a).expect("folder A");
    std::fs::create_dir_all(&b).expect("folder B");
    // `app.json` names the binaries a tab starts its swarms with; the window's own
    // config points at the same files, plus the environment that keeps it hermetic.
    let binaries = Binaries {
        evo_swarm: fixture.bins.swarm.clone(),
        evo_agent: fixture.bins.agent.clone(),
    };

    // --- the first launch: three tabs, two of them swarms --------------------
    let log = AppLog::open(&root);
    let first_launch = AppState {
        binaries: binaries.clone(),
        ..Default::default()
    };
    let (window, view) = open(cx, &fixture, &root, first_launch);

    // §14.6: a launch opens one empty tab, whatever `app.json` said.
    let a_tab = cx.update(|cx| view.read(cx).selected_tab().clone());
    assert!(
        matches!(tab_state(cx, &a_tab), TabState::Empty),
        "a launch starts on the empty tab"
    );

    launch(cx, &a_tab, &a, 1);
    let (a_session, a_pid) = wait_booted(cx, "A's swarm", &a_tab);

    // The user's turn in A, and its reply: the one thing the resume has to bring
    // back. (A swarm that has answered nothing has written no journal yet — the
    // file appears with the first assistant message — so this is also what puts
    // A's session on disk.)
    send(cx, window, &a_tab, PROMPT);
    wait_for_text(cx, "A's reply", &a_tab, PROMPT_REPLY);
    let a_turns = transcript(cx, &a_tab);
    assert!(
        a_turns.contains(PROMPT),
        "the prompt is in the transcript:\n{a_turns}"
    );
    assert!(
        a_turns.contains(PROMPT_REPLY),
        "and so is the reply:\n{a_turns}"
    );
    // The journal lands with the reply, a moment behind the text that reaches the
    // app: the session A is on is now a file the next launch can resume.
    wait_for(cx, "A's journal on disk", |_| a_session.is_file());
    println!("{NOTE} A's journal: {}", a_session.display());

    let b_tab = cx
        .update_window(window, |_, window, cx| {
            view.update(cx, |view, cx| view.open_empty_tab(window, cx))
        })
        .expect("a second tab");
    launch(cx, &b_tab, &b, 1);
    let (b_session, b_pid) = wait_booted(cx, "B's swarm", &b_tab);
    // B gets a turn too: a swarm writes its journal with its first reply, and a tab
    // with no journal is nothing the next launch can offer
    // (`quit::tests::a_tab_whose_journal_was_never_written_is_not_a_recent`).
    send(cx, window, &b_tab, B_PROMPT);
    wait_for_text(cx, "B's reply", &b_tab, B_REPLY);
    wait_for(cx, "B's journal on disk", |_| b_session.is_file());

    // A third swarm, launched in C and never spoken to: it names a session and
    // writes no journal (the file arrives with the first reply), so the next launch
    // has nothing to resume and must not offer it.
    let c = fixture.dir.join("C");
    std::fs::create_dir_all(&c).expect("folder C");
    let c_tab = cx
        .update_window(window, |_, window, cx| {
            view.update(cx, |view, cx| view.open_empty_tab(window, cx))
        })
        .expect("a third tab");
    launch(cx, &c_tab, &c, 1);
    let (c_session, c_pid) = wait_booted(cx, "C's swarm", &c_tab);
    assert!(
        !c_session.is_file(),
        "a swarm that has answered nothing has written no journal: {}",
        c_session.display()
    );

    let empty_tab = cx
        .update_window(window, |_, window, cx| {
            view.update(cx, |view, cx| view.open_empty_tab(window, cx))
        })
        .expect("a fourth tab");
    assert!(
        matches!(tab_state(cx, &empty_tab), TabState::Empty),
        "the last tab stays empty — nothing was launched in it"
    );
    let records = cx.update(|cx| view.read(cx).tab_records(cx));
    assert_eq!(
        records
            .iter()
            .map(|record| record.folder.clone())
            .collect::<Vec<_>>(),
        vec![Some(a.clone()), Some(b.clone()), Some(c.clone()), None],
        "four tabs, in the strip's order: A, B, C, and the empty one"
    );

    // --- the quit: the engines are handed over, `app.json` is written ---------
    quit(cx, &log);
    drain(cx);
    wait_nothing_left(cx, &fixture, "after the first quit");
    assert!(
        !running(a_pid) && !running(b_pid) && !running(c_pid),
        "every swarm is gone: A {a_pid}, B {b_pid}, C {c_pid}"
    );

    let state = AppState::load(&root);
    let tabs: Vec<&str> = state.tabs.iter().map(store::paths::TabId::as_str).collect();
    assert_eq!(tabs.len(), 4, "four tabs were stored: {tabs:?}");
    let flagged: Vec<&Recent> = state
        .recents
        .iter()
        .filter(|recent| recent.open_at_quit)
        .collect();
    assert_eq!(
        flagged.len(),
        2,
        "only the swarms that wrote a journal were open at the last quit: {:?}",
        state
            .recents
            .iter()
            .map(|recent| (recent.folder.clone(), recent.open_at_quit))
            .collect::<Vec<_>>()
    );
    assert!(
        flagged.iter().any(|recent| recent.folder == a),
        "A is one of them"
    );
    assert!(
        flagged.iter().any(|recent| recent.folder == b),
        "and B the other"
    );
    assert_eq!(
        flagged
            .iter()
            .find(|recent| recent.folder == a)
            .map(|recent| recent.session.clone()),
        Some(a_session.clone()),
        "the recent remembers the journal the resume needs"
    );
    assert!(
        state
            .recents
            .iter()
            .all(|recent| recent.folder == a || recent.folder == b),
        "the silent swarm and the empty tab are nobody's recent: {:?}",
        state
            .recents
            .iter()
            .map(|recent| (recent.folder.clone(), recent.open_at_quit))
            .collect::<Vec<_>>()
    );
    assert!(
        !state
            .recents
            .iter()
            .any(|recent| same_path(&recent.folder, &c) || same_path(&recent.session, &c_session)),
        "the tab that was launched and never answered is not offered: it has no journal"
    );

    // --- the second launch: offered first, and it comes back ------------------
    let mut second = cx.new_app();
    second.dispatcher.allow_parking();
    second.update(gpui_kit::init);
    let second_log = AppLog::open(&root);
    let (window2, view2) = open(&mut second, &fixture, &root, AppState::load(&root));

    // §14.6 again: one empty tab, not the stored strip.
    let empty = second.update(|cx| view2.read(cx).selected_tab().clone());
    assert!(
        matches!(tab_state(&mut second, &empty), TabState::Empty),
        "the second launch opens empty"
    );
    assert_eq!(
        second.update(|cx| view2.read(cx).tab_records(cx).len()),
        1,
        "…and only one tab: nothing restores a strip by itself"
    );

    // What the app's launch-time scan produces: the sessions on disk plus the
    // recents `app.json` holds, through the same `Shell.launcher` every empty tab
    // reads (§9.5).
    let entries = history::load_history(&root, &history::sessions_dir(), &ScanBudget::default());
    assert_eq!(
        entries.iter().filter(|entry| entry.open_at_quit).count(),
        2,
        "the scan and the recents together know two sessions were open at quit"
    );
    second.update(|cx| {
        cx.global_mut::<Shell>().launcher.history = history_entries(&entries);
        push_launcher_data(cx);
    });

    wait_for(&mut second, "the history rows", |cx| {
        cx.update(|cx| !empty.read(cx).history_rows(cx).is_empty())
    });
    let rows = second.update(|cx| empty.read(cx).history_rows(cx).to_vec());
    let listed: Vec<(PathBuf, bool)> = rows
        .iter()
        .map(|row| (row.folder.clone(), row.open_at_quit))
        .collect();
    // The two that were open are the first two rows — whichever of them the scan
    // or the recents alone knows about — and nothing after them claims the flag.
    assert!(
        [&rows[0].folder, &rows[1].folder]
            .iter()
            .any(|folder| same_path(folder, &a))
            && [&rows[0].folder, &rows[1].folder]
                .iter()
                .any(|folder| same_path(folder, &b)),
        "the two that were open are first: {listed:?}"
    );
    assert!(
        rows[0].open_at_quit && rows[1].open_at_quit,
        "both wear the flag"
    );
    assert!(
        rows.iter().skip(2).all(|row| !row.open_at_quit),
        "and nothing else claims to have been open"
    );
    assert!(
        !rows.iter().any(|row| same_path(&row.folder, &c)),
        "and the silent swarm is not listed at all: {:?}",
        rows.iter()
            .map(|row| row.folder.clone())
            .collect::<Vec<_>>()
    );
    let a_row = rows
        .iter()
        .position(|row| same_path(&row.folder, &a))
        .expect("A's own row");
    assert_eq!(
        a_row,
        usize::from(!same_path(&rows[0].folder, &a)),
        "A is one of the first two"
    );
    assert!(
        same_path(&rows[a_row].session_path, &a_session),
        "A's row resumes A's journal: {:?}",
        rows[a_row].session_path
    );

    // The flag is on the screen, not only in the model: the chartreuse pill the
    // list draws for a row that was open at the last quit.
    second
        .update_window(window2, |_, window, cx| {
            window.render_frame(cx);
            for row in 0..2 {
                assert!(
                    window
                        .find(ElementId::named_usize(OPEN_AT_QUIT, row))
                        .visible(),
                    "row {row} wears the open-at-last-quit badge"
                );
            }
        })
        .expect("a drawn frame");

    // The click a user makes on that row: the empty tab emits Resume, the window
    // boots the swarm in a tab of its own (§7.2).
    second
        .update_window(window2, |_, window, cx| {
            window.render_frame(cx);
            window.click(ElementId::named_usize(HISTORY_ROW, a_row), cx);
        })
        .expect("the row's click");
    let resumed = second.update(|cx| view2.read(cx).selected_tab().clone());
    let (session, pid) = wait_booted(&mut second, "the resumed swarm", &resumed);

    assert_eq!(session, a_session, "it came back on A's own journal");
    let argv = command_line(pid);
    assert!(
        argv.contains("--resume") && argv.contains(&a_session.display().to_string()),
        "the swarm was started with --resume: {argv}"
    );

    // The earlier turn is in the transcript it restored, and a new turn works.
    wait_for_text(&mut second, "the earlier turn", &resumed, PROMPT_REPLY);
    let restored = transcript(&mut second, &resumed);
    assert!(
        restored.contains(PROMPT),
        "the prompt from before the quit is there:\n{restored}"
    );
    send(&mut second, window2, &resumed, SECOND);
    wait_for_text(&mut second, "the new reply", &resumed, SECOND_REPLY);

    // --- nothing of this test is left running --------------------------------
    quit(&mut second, &second_log);
    drain(&mut second);
    wait_nothing_left(&mut second, &fixture, "after the second quit");
    assert!(!running(pid), "the resumed swarm is gone: {pid}");
    // Both apps are done with; nothing of either may be left parked on them.
    drain(cx);
    drain(&mut second);

    println!("{NOTE} the whole run: {:?}", whole.elapsed());
}

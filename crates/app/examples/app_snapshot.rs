//! app_snapshot — **the assembled app window**, captured headlessly, in light and dark.
//!
//! ```sh
//! cargo run -p evo-desktop --example app_snapshot -- --capture docs/screens
//! ```
//!
//! What is on screen is the production assembly: the app's [`Shell`] global (the
//! data root, the log, the launcher), a `WorkspaceView` window at the app's own
//! 1600×1000 with the app's title bar, the real `tab_engine` behind every tab,
//! and real `evo-swarm` processes running the harness's scripted model in a temp
//! `HOME`. Only two things are scripted, both named in `docs/screens.md`: the
//! catalog the empty tab shows (a `ModelCache` with realistic models, one of them
//! unavailable to lanes) and the model the swarms talk to (a stub this example
//! writes into its own temp directory, because the shipped stub emits fixed text
//! and the pictures need markdown, a todo list and a tool call).
//!
//! Captures use GPUI's headless renderer, so no window opens on screen — the
//! machine may be locked — and every state is rendered twice: light and dark.
//!
//! The run leaves nothing behind: the fixture, the scripted model and the
//! journals live under one temp directory, the tabs' engines are stopped the way
//! the quit sequence stops them, and the last thing the example prints is whether
//! any swarm process is still alive.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::component::{Theme, ThemeMode, TitleBar};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    point, px, size, AnyWindowHandle, AppContext as _, BorrowAppContext as _, Bounds, Entity,
    HeadlessAppContext, WindowBounds, WindowOptions,
};
use session::{AgentKey, RowKind};
use store::app_state::Binaries;
use store::history::{self, ScanBudget};
use store::model_cache::ModelCache;
use store::paths::Root;
use store::time;
use workspace::{Launch, SwarmConfig, TabState, WorkspaceView};

use evo_desktop::{AppLog, Shell, history_entries, push_launcher_data};

/// The size the app opens at (§7.1), so the pictures show the real proportions.
const WINDOW_SIZE: (f32, f32) = (1600., 1000.);

/// A transition samples the app clock; advance it before a capture.
const SETTLE: Duration = Duration::from_millis(400);

/// How long a capture waits for a swarm to answer (§3 gives boot 90 s).
const WAIT: Duration = Duration::from_secs(90);

/// The document the scripted model streams: a heading, a list, a table and a
/// code fence, so one assistant row exercises all of the transcript's markdown.
const MARKDOWN: &str = "\
## Rollout status

The coordinator delegates, the lanes work, and the transcript keeps the receipts.

- **catalog** - probed; nine models, two wire APIs
- **stream** - folding every `message-end` into the readout
- **captures** - both themes, from the same run

| step | owner | state |
| ---- | ----- | ----- |
| probe the catalog | lane 1 | done |
| stream a turn | lane 2 | in progress |
| capture the frames | lane 3 | pending |

```sh
cargo run -p evo-desktop --example app_snapshot -- --capture docs/screens
```

That is the whole plan; the rest is waiting for the report.
";

/// The scripted model, written into the temp directory and pointed at with
/// `EVO_STUB_MESSAGES`. It speaks the same minimal Anthropic SSE the shipped
/// `evo-agent/tests/stub-messages.py` does (that file is the contract; this one
/// adds the rules the pictures need) and answers from the last user turn:
///
/// * `CALL <tool> {json}` → one tool call with that JSON;
/// * `TODOS` → one `todo` call with [`TODO_ITEMS`];
/// * `SLOW` / `SHOW` → [`MARKDOWN`], 40 deltas or three;
/// * a tool result → a line, or the markdown when the turn asked for it;
/// * anything else → `ok: <the first 40 characters>`.
///
/// A streamed reply pauses before its `message_end`, so a capture can catch the
/// whole document while the row is still streaming.
const STUB_MODEL_PY: &str = r####"#!/usr/bin/env python3
"""The scripted model the app captures talk to (written by app_snapshot.rs)."""

import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

DOC = """## Rollout status

The coordinator delegates, the lanes work, and the transcript keeps the receipts.

- **catalog** - probed; nine models, two wire APIs
- **stream** - folding every `message-end` into the readout
- **captures** - both themes, from the same run

| step | owner | state |
| ---- | ----- | ----- |
| probe the catalog | lane 1 | done |
| stream a turn | lane 2 | in progress |
| capture the frames | lane 3 | pending |

```sh
cargo run -p evo-desktop --example app_snapshot -- --capture docs/screens
```

That is the whole plan; the rest is waiting for the report.
"""

TODO_ITEMS = [
    {"text": "Probe the catalog and pin the kernel API set", "status": "done"},
    {"text": "Stream a turn and fold it into the readout", "status": "in-progress"},
    {"text": "Capture both themes from the same run", "status": "pending"},
]

TAIL_PAUSE = 8.0


def block_text(block):
    if block.get("type") == "text":
        return block.get("text", "")
    if block.get("type") == "tool_result":
        content = block.get("content")
        if isinstance(content, list):
            return " ".join(block_text(b) for b in content)
        return str(content or "")
    return ""


def user_side(messages):
    """(all user text, whether the newest turn carries a tool result, the newest
    block's text).  A turn that follows a tool call carries only the result, so
    the whole request's user side is what remembers what was asked."""
    newest, has_result, newest_block = "", False, ""
    parts = []
    for message in messages:
        if message.get("role") != "user":
            continue
        content = message.get("content")
        if isinstance(content, str):
            texts = [content]
            result = False
        else:
            texts = [block_text(b) for b in content]
            result = any(b.get("type") == "tool_result" for b in content)
        parts.extend(texts)
        newest, has_result, newest_block = " ".join(texts), result, (texts[-1] if texts else "")
    return " ".join(parts), has_result, newest_block


def system_text(system):
    if isinstance(system, str):
        return system
    if isinstance(system, list):
        return " ".join(b.get("text", "") for b in system if isinstance(b, dict))
    return ""


class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        pass

    def sse(self, event, data):
        body = "event: %s\ndata: %s\n\n" % (event, json.dumps(data))
        self.wfile.write(body.encode())
        self.wfile.flush()

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        request = json.loads(self.rfile.read(length) or b"{}")
        all_text, has_result, newest = user_side(request.get("messages", []))
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self.end_headers()
        system = system_text(request.get("system"))
        role = "lane"
        if "## Swarm coordinator" in system:
            role = "coordinator"
        try:
            self.respond(request.get("model"), all_text, newest, has_result, role)
        except (BrokenPipeError, ConnectionResetError):
            pass
        except (BrokenPipeError, ConnectionResetError):
            pass

    def respond(self, model, text, newest, has_result, role):
        self.sse("message_start", {
            "type": "message_start",
            "message": {"id": "msg_stub", "type": "message", "role": "assistant",
                        "model": model, "content": [],
                        "usage": {"input_tokens": 10, "output_tokens": 0}}})
        stripped = newest.strip()
        tool = None
        reply = None
        chunks = 0
        if has_result and role == "coordinator":
            # The delegation is acknowledged in one line: the coordinator's
            # transcript stays short, so its delegate row is still on screen
            # when the picture is taken.
            reply = "Delegated; I will report back when the lane is done."
        elif "report]" in newest:
            reply = "Lane 1 reported back: the checklist and the transcript are above."
        elif has_result and ("SLOW" in text or "SHOW" in text):
            chunks = 40 if "SLOW" in text else 3
        elif has_result:
            reply = "The checklist is in the panel above."
        elif stripped.startswith("CALL "):
            name, _, rest = stripped[5:].partition(" ")
            tool = (name, json.loads(rest) if rest.strip() else {})
        elif "TODOS" in newest:
            tool = ("todo", {"items": TODO_ITEMS})
        elif "SLOW" in newest:
            chunks = 40
        elif "SHOW" in newest:
            chunks = 3
        else:
            reply = "ok: " + newest[:40]

        if tool:
            name, args = tool
            self.sse("content_block_start", {
                "type": "content_block_start", "index": 0,
                "content_block": {"type": "tool_use", "id": "toolu_stub",
                                  "name": name, "input": {}}})
            self.sse("content_block_delta", {
                "type": "content_block_delta", "index": 0,
                "delta": {"type": "input_json_delta", "partial_json": json.dumps(args)}})
            stop = "tool_use"
        else:
            self.sse("content_block_start", {
                "type": "content_block_start", "index": 0,
                "content_block": {"type": "text", "text": ""}})
            if chunks:
                size = max(1, len(DOC) // chunks)
                for at in range(0, len(DOC), size):
                    self.sse("content_block_delta", {
                        "type": "content_block_delta", "index": 0,
                        "delta": {"type": "text_delta", "text": DOC[at:at + size]}})
                    time.sleep(0.05)
                # A capture takes a frame mid-stream: stay streaming for a while.
                time.sleep(TAIL_PAUSE)
            else:
                self.sse("content_block_delta", {
                    "type": "content_block_delta", "index": 0,
                    "delta": {"type": "text_delta", "text": reply or ""}})
            stop = "end_turn"
        self.sse("content_block_stop", {"type": "content_block_stop", "index": 0})
        self.sse("message_delta", {"type": "message_delta",
                                   "delta": {"stop_reason": stop},
                                   "usage": {"output_tokens": 5}})
        self.sse("message_stop", {"type": "message_stop"})


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 0
    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    server.daemon_threads = True
    print("stub listening %d" % server.server_address[1], flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
"####;

/// A catalog as a probe answers it: the models this machine's evo really
/// registers, one of them speaking an API the kernel does not have, which is
/// what makes the lanes chooser mark it unavailable (§9.4).
const CATALOG: &str = r#"{
  "models": [
    {"id": "claude-opus-5-5", "provider": "anthropic-oauth", "api": "anthropic-oauth-messages",
     "context_window": 1000000, "max_output": 128000, "vision": true, "thinking_mode": "adaptive"},
    {"id": "ark-deepseek-v4.1-flash", "provider": "aiden", "api": "anthropic-messages",
     "context_window": 936000, "max_output": 64000, "vision": false},
    {"id": "ark-glm-5.2", "provider": "aiden", "api": "anthropic-messages",
     "context_window": 200000, "max_output": 32000, "vision": false},
    {"id": "gpt-5.6-sol", "provider": "aiden", "api": "anthropic-messages",
     "context_window": 400000, "max_output": 64000, "vision": true},
    {"id": "seed-evolving", "provider": "super-relay", "api": "anthropic-messages",
     "context_window": 256000, "max_output": 32000, "vision": false},
    {"id": "kimi-k2-0905", "provider": "kimi", "api": "anthropic-messages",
     "context_window": 256000, "max_output": 32000, "vision": false}
  ],
  "providers": [
    {"key": "anthropic-oauth", "base_url": "https://api.anthropic.com", "has_api_key": true},
    {"key": "aiden", "base_url": "https://aidp.bytedance.net", "has_api_key": true},
    {"key": "super-relay", "base_url": "https://relay.example", "has_api_key": true},
    {"key": "kimi", "base_url": "https://api.moonshot.cn", "has_api_key": false}
  ],
  "apis": ["anthropic-messages", "anthropic-oauth-messages"],
  "tools": [], "active_tools": [], "commands": [], "skills": [], "templates": [],
  "languages": [{"code": "en", "name": "English", "native": "English"}],
  "settings": {"swarm_workers": 6, "model": "claude-opus-5-5"}
}"#;

/// One resumable swarm for the history list: where it ran, how long ago, how
/// many lanes, and the model its journal names. Folders under `$HOME` are
/// written as `~/…` here and expanded against the real home, so the rows show
/// the same shortening the app does; the last one is outside it, which is the
/// row whose subtitle is a whole path.
const HISTORY: &[(&str, u64, u32, &str)] = &[
    ("~/coding/evo-gui", 11 * 60, 6, "claude-opus-5-5"),
    ("~/coding/evo-agent", 3 * 3600, 4, "ark-deepseek-v4.1-flash"),
    ("~/coding/evo-desktop", 7 * 3600, 2, "gpt-5.6-sol"),
    ("~/coding/dotfiles", 26 * 3600, 1, "claude-sonnet-5"),
    ("~/notes", 2 * 86_400, 8, "ark-glm-5.2"),
    ("/opt/checkouts/a-very-long-project-directory-name/nested", 6 * 86_400, 3, "seed-evolving"),
];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [flag, dir] if flag == "--capture" => {
            if let Err(error) = capture(Path::new(dir)) {
                eprintln!("capture failed: {error}");
                std::process::exit(1);
            }
        }
        _ => {
            eprintln!("usage: app_snapshot --capture <dir>");
            std::process::exit(2);
        }
    }
}

/// The window options the captures use: the production title bar, at the size
/// the app opens with, without touching the screen.
fn capture_window_options() -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(WINDOW_SIZE.0), px(WINDOW_SIZE.1)),
        })),
        focus: false,
        show: false,
        ..TitleBar::window_options()
    }
}

fn capture(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;
    let work = temp_dir()?;

    // The scripted model, and the temp `HOME` it is registered in.
    let stub = work.join("scripted-model.py");
    std::fs::write(&stub, STUB_MODEL_PY)?;
    let fixture = Fixture::new(&work, &stub)?;
    println!(
        "[fixture] home {} project {} scripted model on 127.0.0.1:{}",
        fixture.home.display(),
        fixture.project.display(),
        fixture.port
    );

    // The history the empty tab lists comes from a real scan of real journals,
    // written here with the shape evo writes them.
    let sessions = work.join("sessions");
    write_journals(&sessions)?;
    let scanned = history::scan(&sessions, &ScanBudget::default());
    println!("[history] {} journal(s) → {} row(s)", scanned.files_read, scanned.entries.len());

    let catalog = catalog();
    let config = Arc::new(SwarmConfig {
        swarm_bin: fixture.swarm_bin.clone(),
        agent_bin: fixture.agent_bin.clone(),
        root: Root::at(work.join("app")),
        env: fixture.env(),
        env_remove: fixture.env_remove(),
    });

    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(gpui_kit::assets::Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(gpui_kit::init);
    // The engines boot on their own threads and their updates arrive on their own
    // channel: this test scheduler has to be told that real I/O is expected.
    cx.allow_parking();

    // The app's own assembly: the Shell global it logs and launches through, the
    // launcher data the empty tabs are fed from, then the window.
    let root = Root::at(work.join("app"));
    cx.update(|cx| {
        let log = AppLog::open(&root);
        log.info("app_snapshot: building the app's shell");
        Shell::new(
            root.clone(),
            log,
            Binaries {
                evo_swarm: fixture.swarm_bin.clone(),
                evo_agent: fixture.agent_bin.clone(),
            },
            Vec::new(),
            catalog.clone(),
        )
        .install(cx);
    });
    let (window, view) = open_workspace(&mut cx, config.clone())?;
    cx.update(|cx| {
        cx.update_global::<Shell, _>(|shell, _| {
            shell.view = Some(view.downgrade());
            // What `launcher::set_catalog` + `set_history` do for the launch:
            // the app's own launcher state, pushed to the window's tabs.
            shell.launcher.cache = catalog.clone();
            shell.launcher.history = history_entries(&scanned.entries);
            shell.launcher.now = time::now_epoch() as i64;
            shell.launcher.scanning = false;
        });
        push_launcher_data(cx);
    });
    cx.run_until_parked();

    // 1. What the app opens with: one empty tab, the choosers filled from the
    //    catalog, and the sessions the scan found (§7.2).
    both_themes(&mut cx, window, dir, "01-launch")?;

    // 1b. The lanes chooser open: the models the catalog holds, with the one a
    //     quarantined lane cannot reach called out (§9.4).
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("lanes-model", cx);
    })?;
    cx.run_until_parked();
    both_themes(&mut cx, window, dir, "01b-lanes-chooser")?;
    cx.update_window(window, |_, window, cx| {
        window.press("escape", cx);
    })?;

    // 2. Three tabs: two swarms and an empty one, with the first mid-stream
    //    (§2.8, §7.1). The tabs are launched one at a time so each picture's
    //    state is the real one, not half-booted.
    let first = view.read_with(&cx, |view, _| view.tabs()[0].clone());
    launch(&mut cx, window, &first, &fixture.project, 2)?;
    wait_running(&mut cx, &first, "the first swarm")?;

    click(&mut cx, window, "tab-add")?;
    let second = view.read_with(&cx, |view, _| view.tabs()[1].clone());
    let second_folder = work.join("proj-two");
    std::fs::create_dir_all(&second_folder)?;
    launch(&mut cx, window, &second, &second_folder, 2)?;
    wait_running(&mut cx, &second, "the second swarm")?;

    click(&mut cx, window, "tab-add")?;
    view.update(&mut cx, |view, cx| view.select_tab(0, cx));
    prompt(&mut cx, window, &first, "SHOW the rollout plan")?;
    wait_until(&mut cx, |cx| {
        text_chars(cx, &first) * 10 >= MARKDOWN.chars().count() * 9
    })?;
    println!("[state] two swarms running, the first streaming markdown");
    both_themes(&mut cx, window, dir, "02-three-tabs")?;

    // 3. The delegation, opened onto its arguments and result (§2.8). The
    //    coordinator answers in one line, so the row it made is still on
    //    screen; the lane it started is the next picture.
    wait_until(&mut cx, |cx| !working(cx, &first))?;
    select_lane(&mut cx, window, &first, 1)?;
    prompt(
        &mut cx,
        window,
        &first,
        "CALL delegate {\"lane\":1,\"task\":\"TODOS then SLOW: write up what you changed\"}",
    )?;
    select_main(&mut cx, window, &first)?;
    wait_until(&mut cx, |cx| tool_row_with_result(cx, &first).is_some())?;
    expand_tool_row(&mut cx, window, &first)?;
    println!("[state] the delegate call expanded");
    both_themes(&mut cx, window, dir, "03-tool-expanded")?;

    // 4. Lane 1's own transcript and the checklist it set (§7.3, §9.3), once its
    //    run is over so the report it wrote is whole.
    select_lane(&mut cx, window, &first, 1)?;
    wait_until(&mut cx, |cx| {
        lane_todos(cx, &first, 1).is_some_and(|todos| todos >= 3)
            && !lane_working(cx, &first, 1)
            && text_chars(cx, &first) * 10 >= MARKDOWN.chars().count() * 9
    })?;
    println!("[state] lane 1 selected with its checklist");
    both_themes(&mut cx, window, dir, "04-lane-todos")?;

    // 5. A swarm that cannot start: a binary that exits during boot, so the tab
    //    shows the log tail, Retry and Close (§9.7). A second window, because a
    //    tab cannot be told to boot a different binary.
    let broken_bin = work.join("broken-swarm.sh");
    write_broken_binary(&broken_bin)?;
    let broken = Arc::new(SwarmConfig {
        swarm_bin: broken_bin.clone(),
        ..(*config).clone()
    });
    let (broken_window, broken_view) = open_workspace(&mut cx, broken)?;
    let broken_tab = broken_view.read_with(&cx, |view, _| view.selected_tab().clone());
    launch(&mut cx, broken_window, &broken_tab, &fixture.project, 1)?;
    wait_until(&mut cx, |cx| {
        matches!(broken_tab.read_with(cx, |tab, _| tab.state().clone()), TabState::Failed { .. })
    })?;
    println!("[state] boot failure with its log tail");
    both_themes(&mut cx, broken_window, dir, "05-boot-failure")?;

    // 6. The coordinator's stream gone quiet while its process is still there
    //    (§9.7): the lane list's reconnecting badge. The server is stopped, not
    //    killed — a killed one would fail the tab instead — and started again
    //    after the pictures.
    view.update(&mut cx, |view, cx| view.select_tab(0, cx));
    let stopped = stop_swarms_blocking(&config.root);
    println!("[state] stopped {} swarm process(es); waiting for the badge", stopped.len());
    if !stopped.is_empty() {
        if wait_until(&mut cx, |cx| reconnecting(cx, &first)).is_ok() {
            println!("[state] the coordinator's stream is reconnecting");
            both_themes(&mut cx, window, dir, "06-reconnecting")?;
        } else {
            println!("[skip] the reconnect badge did not appear within {:?}", WAIT);
        }
        resume_swarms(&stopped);
    }

    // Stop the tabs' swarms the way the quit sequence does, and say what is
    // left: the example's contract is that it leaves no process behind.
    let handles = view.update(&mut cx, |view, cx| view.take_engines(cx));
    let _bridge = workspace::stop_in_background(handles);
    std::thread::sleep(Duration::from_secs(6));
    let alive = swarms_under(&config.root);
    println!("[cleanup] swarm process(es) still alive: {}", alive.len());
    for pid in &alive {
        let _ = std::process::Command::new("kill").args(["-9", &pid.to_string()]).status();
    }
    drop(fixture);
    if std::env::var_os("EVO_DESKTOP_KEEP_SCREENS_TMP").is_none() {
        let _ = std::fs::remove_dir_all(&work);
    }
    let pictures = std::fs::read_dir(dir)?.count();
    println!("[done] {pictures} picture(s) in {}", dir.display());
    Ok(())
}

/// The catalog the empty tab shows: the `ModelCache` the app would have on disk
/// after a probe.
fn catalog() -> ModelCache {
    ModelCache {
        version: 1,
        fetched_at: time::now_rfc3339(),
        kernel_apis: vec!["anthropic-messages".to_string()],
        registry: serde_json::from_str(CATALOG).expect("the catalog is JSON"),
    }
}

/// Six resumable swarms, written the way evo writes a journal: a header, a
/// `:custom :key "swarm"` record, and an assistant message naming the model.
fn write_journals(sessions: &Path) -> std::io::Result<()> {
    let now = time::now_epoch();
    for (folder, ago, lanes, model) in HISTORY {
        let cwd = expand_home(folder);
        let dir = sessions.join(cwd.replace('/', "-"));
        std::fs::create_dir_all(&dir)?;
        let at = now.saturating_sub(*ago);
        let when = time::format_rfc3339(at);
        let name = cwd.rsplit('/').next().unwrap_or("session");
        let id = format!("{name}{lanes}");
        let swarm = format!("{}-{lanes}", when.replace(['-', ':'], ""));
        let lane_cwds: Vec<String> = (1..=*lanes)
            .map(|n| {
                format!(
                    "(:n {n} :cwd \"{cwd}/\" :worktree nil :branch nil :task nil :extra-forms #())"
                )
            })
            .collect();
        let journal = format!(
            "(:type :session :version 1 :id \"{id}\" :cwd \"{cwd}/\" :timestamp \"{when}\")\n\
             (:type :message :id \"m1\" :parent-id nil :timestamp \"{when}\" :message (:role :user :content ((:type :text :text \"go\"))))\n\
             (:type :custom :id \"c1\" :parent-id nil :timestamp \"{when}\" :key \"swarm\" :data (:id \"{swarm}\" :dir \"/Users/you/.evo/swarm/{swarm}/\" :workers {lanes} :lanes #({lanes_})))\n\
             (:type :message :id \"m2\" :parent-id nil :timestamp \"{when}\" :message (:role :assistant :api :anthropic-messages :provider :aiden :model \"{model}\" :stop-reason :end-turn :usage (:input 4 :output 118 :cache-read 0 :cache-write 168541) :content ((:type :text :text \"done\"))))\n",
            lanes_ = lane_cwds.join(" "),
        );
        // Named like a real journal: `<timestamp>_<id>.sexp`.
        let file = dir.join(format!("{}_{id}.sexp", when.replace(['-', ':'], "")));
        std::fs::write(&file, journal)?;
        // The scan orders by mtime, and the row's age comes from the header.
        let modified = std::time::UNIX_EPOCH + Duration::from_secs(at);
        std::fs::File::options().write(true).open(&file)?.set_modified(modified)?;
    }
    Ok(())
}

/// `~/…` against the real `HOME`, so the rows shorten the way the app's do.
fn expand_home(folder: &str) -> String {
    match folder.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", std::env::var("HOME").unwrap_or_default()),
        None => folder.to_string(),
    }
}

/// A binary that exists but fails to boot, with the log tail such a failure
/// leaves behind.
fn write_broken_binary(path: &Path) -> std::io::Result<()> {
    std::fs::write(
        path,
        "#!/bin/sh\n\
         echo \"evo-swarm 0.1.0 (build 9f31c2a)\" >&2\n\
         echo \"serve: cannot open --token-file: permission denied\" >&2\n\
         echo \"serve: the supervisor exited before the port was bound\" >&2\n\
         exit 1\n",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

fn open_workspace(
    cx: &mut HeadlessAppContext,
    config: Arc<SwarmConfig>,
) -> Result<(AnyWindowHandle, Entity<WorkspaceView>), Box<dyn std::error::Error>> {
    let (window, view) = cx.update(|cx| {
        gpui_kit::open_window(capture_window_options(), cx, |window, cx| {
            cx.new(|cx| WorkspaceView::with_config(config, window, cx))
        })
    })?;
    Ok((window.into(), view))
}

fn launch(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    tab: &Entity<workspace::TabContent>,
    folder: &Path,
    workers: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut plan = session::LaunchPlan::default();
    plan.workers = Some(workers);
    let launch = Launch::New { folder: folder.to_path_buf(), plan };
    cx.update_window(window, |_, window, cx| {
        tab.update(cx, |tab, cx| tab.launch(launch, window, cx));
    })?;
    Ok(())
}

/// Type a turn into the composer and press Enter — the app's own send path (§9.2).
fn prompt(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    tab: &Entity<workspace::TabContent>,
    text: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    cx.update_window(window, |_, window, cx| {
        tab.update(cx, |tab, cx| {
            let composer = tab.composer().clone();
            composer.update(cx, |composer, cx| composer.focus_input(window, cx));
        });
        window.input(text, cx);
        window.press("enter", cx);
    })?;
    Ok(())
}

fn click(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    id: &'static str,
) -> Result<(), Box<dyn std::error::Error>> {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(id, cx);
    })?;
    Ok(())
}

fn select_lane(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    tab: &Entity<workspace::TabContent>,
    n: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    cx.update_window(window, |_, _window, cx| {
        tab.update(cx, |tab, cx| tab.select_agent(AgentKey::Lane(n), cx));
    })?;
    Ok(())
}

fn select_main(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    tab: &Entity<workspace::TabContent>,
) -> Result<(), Box<dyn std::error::Error>> {
    cx.update_window(window, |_, _window, cx| {
        tab.update(cx, |tab, cx| tab.select_agent(AgentKey::Coordinator, cx));
    })?;
    Ok(())
}

/// Open the first tool row of the shown transcript, the way a click does.
fn expand_tool_row(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    tab: &Entity<workspace::TabContent>,
) -> Result<(), Box<dyn std::error::Error>> {
    let row = tab.read_with(cx, |tab, cx| {
        tab.transcript().and_then(|view| {
            view.read(cx)
                .rows(cx)
                .iter()
                .find(|row| matches!(row.kind, RowKind::Tool { .. }))
                .map(|row| row.id)
        })
    });
    if let Some(id) = row {
        cx.update_window(window, |_, _window, cx| {
            if let Some(view) = tab.read(cx).transcript().cloned() {
                view.update(cx, |view, cx| view.set_expanded(id, true, cx));
            }
        })?;
    }
    Ok(())
}

/// The id of the first tool row that already has its result, for a wait loop:
/// the picture is of a call that ran, not one still in flight.
fn tool_row_with_result(cx: &HeadlessAppContext, tab: &Entity<workspace::TabContent>) -> Option<u64> {
    tab.read_with(cx, |tab, cx| {
        tab.transcript().and_then(|view| {
            view.read(cx)
                .rows(cx)
                .iter()
                .find(|row| matches!(&row.kind, RowKind::Tool { result: Some(_), .. }))
                .map(|row| row.id)
        })
    })
}

/// Whether lane `n` is mid-run, from its own model.
fn lane_working(cx: &HeadlessAppContext, tab: &Entity<workspace::TabContent>, n: u32) -> bool {
    tab.read_with(cx, |tab, _| {
        tab.model()
            .and_then(|model| model.lane_model(n))
            .is_some_and(|lane| lane.activity() != session::Activity::Idle)
    })
}

/// Render the current state in the light theme and then the dark one.
fn both_themes(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    dir: &Path,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    cx.update(|cx| Theme::change(ThemeMode::Light, None, cx));
    shot(cx, window, dir, &format!("{name}-light.png"))?;
    cx.update(|cx| Theme::change(ThemeMode::Dark, None, cx));
    shot(cx, window, dir, &format!("{name}-dark.png"))?;
    Ok(())
}

/// Draw a frame and save its pixels.
fn shot(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    dir: &Path,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Something that animates (a spinner, a stream fade) settles on the app
    // clock, which the headless context only advances when asked.
    cx.advance_clock(SETTLE);
    std::thread::sleep(SETTLE);
    cx.update_window(window, |_, window, cx| window.render_frame(cx))?;
    let image = cx.capture_screenshot(window)?;
    let path = dir.join(name);
    image.save(&path)?;
    println!("[capture] {}x{} -> {}", image.width(), image.height(), path.display());
    Ok(())
}

/// Spin the UI thread until `done` holds or the wait runs out. A capture is a
/// batch job, so a bounded wait loop is what it uses instead of events.
fn wait_until(
    cx: &mut HeadlessAppContext,
    done: impl Fn(&HeadlessAppContext) -> bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + WAIT;
    loop {
        cx.run_until_parked();
        if done(cx) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("timed out waiting for the swarm".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_running(
    cx: &mut HeadlessAppContext,
    tab: &Entity<workspace::TabContent>,
    what: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    wait_until(cx, |cx| {
        matches!(tab.read_with(cx, |tab, _| tab.state().clone()), TabState::Running { .. })
    })
    .map_err(|error| format!("{what}: {error}").into())
}

/// How much text the shown agent's transcript carries, in bytes — enough to tell
/// "nothing yet" from "streaming" inside a wait loop.
fn text_chars(cx: &HeadlessAppContext, tab: &Entity<workspace::TabContent>) -> usize {
    tab.read_with(cx, |tab, cx| {
        tab.transcript()
            .map(|view| view.read(cx).rows(cx).iter().map(row_chars).sum::<usize>())
            .unwrap_or_default()
    })
}

fn row_chars(row: &session::Row) -> usize {
    match &row.kind {
        RowKind::User { text } => text.chars().count(),
        RowKind::Assistant { markdown, .. } => markdown.chars().count(),
        RowKind::Tool { name, .. } => name.chars().count(),
        RowKind::Report { done, .. } => done.chars().count(),
        RowKind::Dim { text, .. } => text.chars().count(),
        RowKind::RunOutcome { text, .. } => text.chars().count(),
    }
}

/// How many checklist items lane `n` currently has.
fn lane_todos(cx: &HeadlessAppContext, tab: &Entity<workspace::TabContent>, n: u32) -> Option<usize> {
    tab.read_with(cx, |tab, _| {
        tab.model().and_then(|model| model.lane_model(n)).map(|lane| lane.todos().len())
    })
}

/// Whether the coordinator is mid-run.
fn working(cx: &HeadlessAppContext, tab: &Entity<workspace::TabContent>) -> bool {
    tab.read_with(cx, |tab, _| {
        tab.model().is_some_and(|model| model.activity() != session::Activity::Idle)
    })
}

/// Whether the tab is showing the coordinator's stream as reconnecting (§9.7).
fn reconnecting(cx: &HeadlessAppContext, tab: &Entity<workspace::TabContent>) -> bool {
    tab.read_with(cx, |tab, _| tab.is_reconnecting())
}

// --- the swarm processes ------------------------------------------------------

/// The pids under `root` whose command line names one of its tab tokens.
fn swarms_under(root: &Root) -> Vec<u32> {
    let needle = format!("--token-file {}", root.tabs_dir().display());
    let output = match std::process::Command::new("ps").args(["-axo", "pid=,command="]).output() {
        Ok(output) => output,
        Err(_) => return Vec::new(),
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let (pid, command) = line.trim().split_once(' ')?;
            command.contains(&needle).then(|| pid.parse().ok())?
        })
        .collect()
}

/// Stop the swarms' processes without killing them: a killed server fails its
/// tab, a stopped one leaves the stream to time out — which is what the
/// reconnecting badge is (§9.7). `SIGSTOP` goes to the whole group.
fn stop_swarms_blocking(root: &Root) -> Vec<u32> {
    let pids = swarms_under(root);
    for pid in &pids {
        let _ = std::process::Command::new("kill").args(["-STOP", &pid.to_string()]).status();
    }
    pids
}

fn resume_swarms(pids: &[u32]) {
    for pid in pids {
        let _ = std::process::Command::new("kill").args(["-CONT", &pid.to_string()]).status();
    }
}

/// A directory under the system temp dir for this run's fixture, journals and
/// scripted model. `capture` removes it when it finishes.
fn temp_dir() -> std::io::Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("evo-desktop-screens-{}-{nanos:x}", std::process::id()));
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

/// The model the temp `init.lisp` registers.
const STUB_MODEL: &str = "stub-a";

/// What a swarm runs against: a temp `HOME` whose evo home registers the
/// scripted model, a project folder, the installed binaries and the model's own
/// process. The model is [`STUB_MODEL_PY`], started here — the same shape as
/// `swarm_client`'s test harness, without making the app depend on it.
struct Fixture {
    home: PathBuf,
    project: PathBuf,
    swarm_bin: PathBuf,
    agent_bin: PathBuf,
    /// The scripted model: killed when the fixture drops.
    stub: Child,
    /// Kept open so the model's stdout stays a pipe.
    _stdout: ChildStdout,
    port: u16,
}

impl Fixture {
    fn new(work: &Path, script: &Path) -> Result<Fixture, Box<dyn std::error::Error>> {
        let swarm_bin = bin("EVO_SWARM_BIN", "/usr/local/bin/evo-swarm");
        let agent_bin = bin("EVO_AGENT_BIN", "/usr/local/bin/evo-agent");
        if !swarm_bin.is_file() || !agent_bin.is_file() {
            return Err(format!(
                "no evo binaries to drive: {} / {}",
                swarm_bin.display(),
                agent_bin.display()
            )
            .into());
        }
        let home = work.join("home");
        let project = work.join("proj");
        std::fs::create_dir_all(home.join(".evo"))?;
        std::fs::create_dir_all(&project)?;

        let (stub, stdout, port) = start_stub(script)?;
        std::fs::write(home.join(".evo").join("init.lisp"), stub_init_lisp(port))?;
        Ok(Fixture { home, project, swarm_bin, agent_bin, stub, _stdout: stdout, port })
    }

    /// What the servers run with: the temp home, the evo home inside it — with
    /// the trailing separator evo's own path merging needs (docs/proofs-real.md
    /// R2) — the agent binary the lanes run, and no real provider key.
    fn env(&self) -> Vec<(String, String)> {
        vec![
            ("HOME".to_string(), self.home.to_string_lossy().into_owned()),
            ("EVO_HOME".to_string(), format!("{}/.evo/", self.home.display())),
            ("EVO_BINARY".to_string(), self.agent_bin.to_string_lossy().into_owned()),
            ("TERM".to_string(), "xterm-256color".to_string()),
        ]
    }

    fn env_remove(&self) -> Vec<String> {
        vec!["ANTHROPIC_API_KEY".to_string(), "OPENAI_API_KEY".to_string()]
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.stub.kill();
        let _ = self.stub.wait();
    }
}

/// Where the scripted model listens, and the `init.lisp` that points evo at it.
fn stub_init_lisp(port: u16) -> String {
    format!(
        "(evo:register-provider :stub :base-url \"http://127.0.0.1:{port}\" \
         :api-key \"app-snapshot-stub-secret\")\n\
         (evo:register-model \"{STUB_MODEL}\" :provider :stub :context-window 200000 \
         :max-output 8000 :effort t)\n\
         (evo:set-setting :model \"{STUB_MODEL}\")\n"
    )
}

/// Start the scripted model on a free port and wait for it to say it is up.
fn start_stub(script: &Path) -> Result<(Child, ChildStdout, u16), Box<dyn std::error::Error>> {
    let port = free_port()?;
    let mut child = Command::new("python3")
        .arg(script)
        .arg(port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| format!("cannot run python3 {}: {error}", script.display()))?;
    let stdout = child.stdout.take().expect("stdout was piped");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if !line.starts_with("stub listening") {
        let _ = child.kill();
        return Err(format!("the scripted model did not start: {line:?}").into());
    }
    Ok((child, reader.into_inner(), port))
}

/// An installed binary, unless the environment names another.
fn bin(variable: &str, fallback: &str) -> PathBuf {
    std::env::var_os(variable).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(fallback))
}

/// A port nothing is listening on: ask the OS for one, then let it go.
fn free_port() -> std::io::Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

//! What every milestone proof needs: a hermetic swarm, and the one thing the UI
//! does with a [`tab_engine`] — fold its [`Update`]s into a `session::TabModel`.
//!
//! The folding here is `crates/workspace/src/tab.rs`'s `Live::absorb`, line for
//! line: the same `on_transcript`/`on_state`/`on_lanes`/`on_event_at` calls, the
//! same monotonic state revision, the same arrival stamp for the step clock. That
//! is what lets a proof assert on the model and mean what the tab page shows.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_channel::Receiver;
use serde_json::Value;
use session::{AgentKey, TabModel};
use swarm_client::harness::{Bins, Fixture, HarnessConfig};
use tab_engine::{Agent, Command, EngineHandle, StreamStatus, TabEngine, TabSpec, Update};

/// The real process check — `kill(pid, 0)` — so a proof can say, with evidence,
/// that nothing it started is still running.
pub use swarm_client::process_alive;

/// One swarm at a time: a test run is not a load test, and every swarm is a
/// supervisor plus a process per lane. `cargo test` runs one binary at a time, so
/// this serializes the tests inside a binary; across binaries the run is serial by
/// construction.
pub fn one_swarm() -> std::sync::MutexGuard<'static, ()> {
    static SWARM: std::sync::Mutex<()> = std::sync::Mutex::new(());
    SWARM.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The hermetic environment: a temp `HOME` whose `init.lisp` registers the stub
/// provider, a temp project, and the installed binaries. Panics when either binary
/// is missing — a proof that cannot run is not a proof that passed.
pub fn fixture(workers: u16) -> Fixture {
    let bins = Bins::installed();
    assert!(
        bins.available(),
        "no binaries to test against: {} / {}",
        bins.swarm.display(),
        bins.agent.display()
    );
    Fixture::new(HarnessConfig { workers, ..Default::default() }).expect("the fixture should come up")
}

/// The tab spec a [`Fixture`] describes: its project, its tab directory, and the
/// hermetic environment it sets up.
pub fn spec(fixture: &Fixture, workers: u16) -> TabSpec {
    let mut spec = TabSpec::new(&fixture.bins.swarm, &fixture.project, fixture.tab_dir())
        .with_agent_bin(&fixture.bins.agent)
        .with_workers(workers);
    for (key, value) in fixture.env() {
        spec = spec.with_env(key, value);
    }
    for key in fixture.env_remove() {
        spec = spec.with_env_removed(key);
    }
    spec
}

/// Epoch milliseconds, as the tab page stamps an event when it arrives — the clock
/// the step clock counts from (§7.3).
pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or(0)
}

/// A live tab without a window: the engine's handle, every update it has sent (kept,
/// so a proof can assert on the wire too), and the model they are folded into.
pub struct Drive {
    handle: Option<EngineHandle>,
    rx: Receiver<Update>,
    log: Vec<Update>,
    cursor: usize,
    /// The view models the tab page holds (§9.1).
    pub model: TabModel,
    /// The newest `/state` revision applied: an answer from an older fetch changes
    /// nothing (the tab page keeps the same gate).
    state_revision: u64,
    /// The swarm process the engine started, and the coordinator it supervises.
    pub swarm_pid: Option<u32>,
    pub coordinator_pid: Option<u32>,
    /// Lane number → the lane process's pid, as `/lanes` and `lane-state` report it.
    pub lane_pids: BTreeMap<u64, u32>,
    /// The session the swarm is writing to, as `/state` reports it (`session`).
    pub session: Option<String>,
}

impl Drive {
    /// Start a swarm and take its handle.
    pub fn start(spec: TabSpec) -> Drive {
        let (handle, rx) = TabEngine::start(spec);
        Drive {
            handle: Some(handle),
            rx,
            log: Vec::new(),
            cursor: 0,
            model: TabModel::new(),
            state_revision: 0,
            swarm_pid: None,
            coordinator_pid: None,
            lane_pids: BTreeMap::new(),
            session: None,
        }
    }

    /// Take everything the engine has sent since the last call, folding each update
    /// into the model in the order it arrived.
    pub fn pump(&mut self) {
        while let Ok(update) = self.rx.try_recv() {
            self.absorb(&update);
            self.log.push(update);
        }
    }

    /// One update folded into the model, exactly as the tab page folds it.
    fn absorb(&mut self, update: &Update) {
        match update {
            Update::Ready { health, pid, .. } => {
                self.swarm_pid = Some(*pid);
                self.coordinator_pid = Some(health.pid);
            }
            Update::State { raw, .. } => {
                if let Some(session) = raw.get("session").and_then(Value::as_str) {
                    self.session = Some(session.to_string());
                }
            }
            Update::Lanes { raw } => {
                for lane in raw["lanes"].as_array().into_iter().flatten() {
                    if let (Some(n), Some(pid)) = (lane["n"].as_u64(), lane["pid"].as_u64()) {
                        self.lane_pids.insert(n, pid as u32);
                    }
                }
            }
            Update::Event { data, .. } => {
                if data.get("type").and_then(Value::as_str) == Some("lane-state") {
                    if let (Some(n), Some(pid)) = (data["lane"].as_u64(), data["pid"].as_u64()) {
                        self.lane_pids.insert(n, pid as u32);
                    }
                }
            }
            _ => {}
        }
        match update {
            Update::Transcript { agent, revision, raw } => {
                self.model.on_transcript(agent_key(*agent), *revision, raw);
            }
            Update::State { revision, raw } => {
                if *revision <= self.state_revision {
                    // An answer from an older fetch: newer state is already shown.
                    return;
                }
                self.state_revision = *revision;
                self.model.on_state(raw);
            }
            Update::Registry { raw } => {
                self.model.on_registry(raw);
            }
            Update::Lanes { raw } => {
                self.model.on_lanes(raw);
            }
            Update::Event { agent, id, kind, data } => {
                let id = id.unwrap_or_default().max(0) as u64;
                // The event is stamped with when the tab saw it, which is what the
                // step clock counts from (§7.3).
                self.model.on_event_at(agent_key(*agent), id, kind, data, now_millis());
            }
            Update::Stream { agent, status } => {
                self.model.on_stream(agent_key(*agent), stream_status(*status));
            }
            Update::CacheSeed { entry } => {
                self.model.on_cache_seed(entry.as_ref());
            }
            _ => {}
        }
    }

    /// The next update, from the cursor, that matches — consuming it. Panics with
    /// what it saw when the deadline passes first.
    pub fn next(&mut self, deadline: Instant, what: &str, pred: impl Fn(&Update) -> bool) -> Update {
        loop {
            self.pump();
            if let Some(offset) = self.log[self.cursor..].iter().position(&pred) {
                let at = self.cursor + offset;
                self.cursor = at + 1;
                return self.log[at].clone();
            }
            assert!(
                Instant::now() < deadline,
                "no {what} before the deadline; saw {:?}",
                self.log.iter().map(kind_of).collect::<Vec<_>>()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Wait for the model itself to say something — a lane that went down, a row
    /// that arrived — rather than for one update. `what` is the failure message.
    pub fn wait_model(
        &mut self,
        deadline: Instant,
        what: &str,
        pred: impl Fn(&TabModel) -> bool,
    ) {
        loop {
            self.pump();
            if pred(&self.model) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "never became {what}; lanes: {:?}",
                self.model.lanes().lanes.iter().map(|row| (row.n, row.state.clone())).collect::<Vec<_>>()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// The coordinator's stream up, which is the moment the view is being assembled
    /// (§9.1) and a command has somewhere to land.
    pub fn wait_connected(&mut self, agent: Agent, deadline: Instant) {
        self.next(deadline, "stream", |update| {
            matches!(update, Update::Stream { agent: a, status: StreamStatus::Connected } if *a == agent)
        });
    }

    /// Wait for one SSE event of AGENT's stream.
    pub fn wait_event(
        &mut self,
        deadline: Instant,
        agent: Agent,
        kind: &str,
        data: &Value,
    ) {
        self.next(deadline, kind, |update| {
            matches!(update, Update::Event { agent: a, kind: k, .. } if *a == agent && k == kind)
        });
        let _ = data;
    }

    /// Wait for an event of AGENT's stream whose payload passes `pred`.
    pub fn wait_event_where(
        &mut self,
        deadline: Instant,
        agent: Agent,
        kind: &str,
        what: &str,
        pred: impl Fn(&Value) -> bool,
    ) -> Value {
        let update = self.next(deadline, what, |update| {
            matches!(update, Update::Event { agent: a, kind: k, data, .. }
                if *a == agent && k == kind && pred(data))
        });
        match update {
            Update::Event { data, .. } => data,
            _ => unreachable!(),
        }
    }

    /// Every event of AGENT's stream with this kind, as it arrived.
    pub fn events(&self, agent: Agent, kind: &str) -> Vec<Value> {
        self.log
            .iter()
            .filter_map(|update| match update {
                Update::Event { agent: a, kind: k, data, .. } if *a == agent && k == kind => {
                    Some(data.clone())
                }
                _ => None,
            })
            .collect()
    }

    /// The events of AGENT's stream since `from` (an index into [`Drive::updates`]),
    /// for "must not happen" checks.
    pub fn events_since(&self, from: usize, agent: Agent, kind: &str) -> Vec<Value> {
        self.log[from.min(self.log.len())..]
            .iter()
            .filter_map(|update| match update {
                Update::Event { agent: a, kind: k, data, .. } if *a == agent && k == kind => {
                    Some(data.clone())
                }
                _ => None,
            })
            .collect()
    }

    /// A lane's latest state as the left column folded it, or `None` for a lane the
    /// view has not seen.
    pub fn lane_state(&self, n: u64) -> Option<String> {
        self.model.lanes().lane(n).map(|row| row.state.clone())
    }

    pub fn updates(&self) -> &[Update] {
        &self.log
    }

    /// Every process this proof knows about: the swarm the engine started, the
    /// coordinator under its supervisor, and one per lane.
    pub fn pids(&self) -> Vec<u32> {
        let mut pids: Vec<u32> = self
            .swarm_pid
            .into_iter()
            .chain(self.coordinator_pid)
            .chain(self.lane_pids.values().copied())
            .collect();
        pids.sort_unstable();
        pids.dedup();
        pids
    }

    // --- commands --------------------------------------------------------

    /// Send a command to the engine. Panics when the engine is gone: every proof
    /// command here is one the test means.
    pub fn send(&self, command: Command) {
        let handle = self.handle.as_ref().expect("the engine is still running");
        assert!(handle.send(command), "the engine took the command");
    }

    pub fn prompt(&self, req_id: u64, text: impl Into<String>) {
        let handle = self.handle.as_ref().expect("the engine is still running");
        assert!(handle.prompt(req_id, text), "the engine took the prompt");
    }

    /// Show an agent, as clicking its row does: select it in the model and watch
    /// only that lane's stream (§9.3, §14.4).
    pub fn select(&mut self, agent: AgentKey) {
        self.model.select(agent);
        let handle = self.handle.as_ref().expect("the engine is still running");
        assert!(handle.watch_lane(agent.lane()), "the engine watched the lane");
    }

    pub fn shutdown(&mut self) {
        if let Some(handle) = self.handle.as_mut() {
            assert!(handle.shutdown(), "the engine took the shutdown");
        }
    }

    /// Wait for the engine to stop, then prove every process it knew about is gone.
    pub fn join_and_assert_gone(&mut self, deadline: Instant) {
        if let Some(handle) = self.handle.take() {
            handle.join();
        }
        let pids = self.pids();
        assert!(!pids.is_empty(), "the proof never learned a pid to check");
        loop {
            let alive: Vec<u32> = pids.iter().copied().filter(|pid| process_alive(*pid)).collect();
            if alive.is_empty() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "processes started by this proof are still alive: {alive:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The updates seen so far, as a cursor — pass it to [`Drive::events_since`].
    pub fn cursor(&self) -> usize {
        self.log.len()
    }
}

impl Drop for Drive {
    fn drop(&mut self) {
        // `EngineHandle`'s own Drop runs §3's ladder and joins the thread, so a
        // panicking proof leaves no swarm behind either.
    }
}

/// The engine names an agent; the model names the same one its own way.
pub fn agent_key(agent: Agent) -> AgentKey {
    match agent {
        Agent::Coordinator => AgentKey::Coordinator,
        Agent::Lane(n) => AgentKey::Lane(n),
    }
}

/// The engine's stream status, as the model spells it for the badge (§9.7).
fn stream_status(status: StreamStatus) -> session::StreamStatus {
    match status {
        StreamStatus::Connected => session::StreamStatus::Connected,
        StreamStatus::Reconnecting { retry_in } => session::StreamStatus::Reconnecting { retry_in },
    }
}

/// The update's name, for failure messages.
pub fn kind_of(update: &Update) -> &'static str {
    match update {
        Update::Booting => "Booting",
        Update::Ready { .. } => "Ready",
        Update::BootFailed { .. } => "BootFailed",
        Update::Transcript { .. } => "Transcript",
        Update::State { .. } => "State",
        Update::Registry { .. } => "Registry",
        Update::Lanes { .. } => "Lanes",
        Update::Event { .. } => "Event",
        Update::Stream { .. } => "Stream",
        Update::CacheSeed { .. } => "CacheSeed",
        Update::PostResult { .. } => "PostResult",
        Update::ServerGone => "ServerGone",
        Update::Exited { .. } => "Exited",
    }
}

/// The rows an agent's model holds, written out for a failure message.
pub fn row_summary(model: &session::AgentModel) -> Vec<String> {
    model
        .rows()
        .iter()
        .map(|row| match &row.kind {
            session::RowKind::User { text } => format!("user:{}", clip(text, 60)),
            session::RowKind::Assistant { markdown, .. } => format!("assistant:{}", clip(markdown, 60)),
            session::RowKind::Tool { name, result, .. } => {
                format!("tool:{name}{}", if result.is_some() { "✓" } else { "" })
            }
            session::RowKind::Report { done, .. } => format!("report:{}", clip(done, 60)),
            session::RowKind::Dim { text, .. } => format!("dim:{}", clip(text, 60)),
            session::RowKind::RunOutcome { outcome, text } => {
                format!("run-outcome:{outcome}:{}", clip(text, 60))
            }
        })
        .collect()
}

pub fn clip(text: &str, chars: usize) -> String {
    let one_line: String = text.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
    if one_line.chars().count() <= chars {
        one_line
    } else {
        let mut out: String = one_line.chars().take(chars).collect();
        out.push('…');
        out
    }
}

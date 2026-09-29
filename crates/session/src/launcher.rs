//! The empty tab's view model (§7.2, §9.4, §9.5, §9.6, §14.2): the two model choosers,
//! the worker-count chooser, the launch plan they produce, and the history list — so the
//! gpui empty tab is a thin renderer over this.
//!
//! Inputs are plain data and raw JSON: a `/registry` body (from the model cache, or from
//! the `--no-userspace` probe that tells the lanes chooser which APIs exist in a lane), the
//! configured `:swarm-workers` value, and one plain entry per resumable session. Nothing
//! here does I/O and nothing here depends on another crate.
//!
//! # The three choosers
//!
//! Every chooser's first option is **Default** — the state the tab starts in, and the one
//! that passes nothing to the swarm (§7.2). The coordinator's chooser lists every model in
//! the registry; the lanes' chooser lists the same models but marks one **unavailable**
//! when a lane could not register it, which is the case exactly when its API is not in the
//! kernel's own set (§9.4 — a `--no-userspace` probe's `/registry.apis`). Until a probe
//! says what that set is, the lanes chooser carries [`Chooser::uncertain`] rather than
//! claiming a model works.
//!
//! # History (§9.5)
//!
//! [`history_rows`] turns the scan's entries and the app's own recents into display rows,
//! merged by session path, newest first, each with the folder name, the `~`-shortened path
//! and a meta line of what is known (`4 lanes · 2h ago · coordinator: gpt-…`).

use serde_json::Value;

use crate::k_tokens;

/// The key of every chooser's first option: the state that passes nothing to evo (§7.2).
pub const DEFAULT_KEY: &str = "default";

/// The largest worker count the chooser offers (§14.2 allows 1–64).
pub const WORKERS_MAX: u16 = 64;

/// Why a lane cannot use a model: the API it needs is not in the lane, and only the
/// project's `swarm.lisp` can put it there (`swarm/init.lisp`'s model check).
pub const NEEDS_EXTENSION_API: &str = "needs an extension API — set it in swarm.lisp";

/// The detail line under the coordinator chooser's Default option.
const COORDINATOR_DEFAULT: &str = "evo's own default";

/// The detail line under the lanes chooser's Default option: a lane runs the coordinator's
/// model unless the project's `swarm.lisp` says otherwise (§9.6).
const LANES_DEFAULT: &str = "follows the coordinator";

/// Which chooser a selection belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Choice {
    Coordinator,
    Lanes,
    Workers,
}

/// One option of a chooser, as the Select renders it.
///
/// `model_id`/`provider` are the pair the launch plan needs, and are `None` on the Default
/// option and on the worker-count options, which are not models.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChooserOption {
    /// Stable identity of the option: `default`, `id@provider` for a model, `1`…`64` for a
    /// worker count.
    pub key: String,
    /// The model id, or `id (provider)` when that id is registered under more than one
    /// provider — the §7.3 rule, so the two frontends name a model the same way.
    pub label: String,
    /// One compact line under the label: the context window, then what else is known.
    pub detail: String,
    /// Whether this option can be launched as it is. Every Default option, every
    /// coordinator option and every worker count is available; a lane's model is not when
    /// a lane cannot register it.
    pub available: bool,
    /// Why not, for the unavailable ones (§9.4).
    pub unavailable_reason: Option<String>,
    pub model_id: Option<String>,
    pub provider: Option<String>,
}

impl ChooserOption {
    /// The model this option names, as the launch plan wants it.
    pub fn model(&self) -> Option<(String, String)> {
        Some((self.model_id.clone()?, self.provider.clone()?))
    }
}

/// One chooser: the options, and whether the availability of the lane options is actually
/// known.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Chooser {
    /// The Default option first, then the rest — models sorted by provider then id,
    /// worker counts ascending.
    pub options: Vec<ChooserOption>,
    /// True for a lanes chooser built without knowing the kernel's API set: every option
    /// reads available, but that has not been verified against a `--no-userspace` probe
    /// (§9.4), so the UI should say so rather than trust it.
    pub uncertain: bool,
}

impl Chooser {
    /// The key of the Default option, for a UI starting fresh.
    pub fn default_key() -> &'static str {
        DEFAULT_KEY
    }

    /// The option with this key, if the chooser has it.
    pub fn option(&self, key: &str) -> Option<&ChooserOption> {
        self.options.iter().find(|option| option.key == key)
    }

    /// Where this key sits, for a Select that takes an index.
    pub fn index_of(&self, key: &str) -> Option<usize> {
        self.options.iter().position(|option| option.key == key)
    }

    /// The models this chooser offers, Default excluded.
    pub fn models(&self) -> impl Iterator<Item = &ChooserOption> {
        self.options.iter().filter(|option| option.key != DEFAULT_KEY)
    }
}

/// What the tab's three choosers add up to: `None` is Default everywhere — nothing is
/// passed to the swarm, and no `swarm.lisp` block is written (§7.2, §9.6).
///
/// This is a **new** swarm's plan. A swarm resumed from history takes the journal's own
/// record instead — its lane count and its models are the session's, not the empty tab's
/// (§14.2) — so the workspace passes none of this for a resume.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaunchPlan {
    /// The coordinator's `--model`, as `(id, provider)`.
    pub model: Option<(String, String)>,
    /// The lanes' model, written into `<folder>/.evo/swarm.lisp` — a lane binary takes no
    /// lanes-model flag (§9.6, §14.1).
    pub lanes_model: Option<(String, String)>,
    /// The `--workers` count; evo's own `:swarm-workers`, else 6, applies without it.
    pub workers: Option<u16>,
}

/// Where a history entry came from: the disk scan (§9.5) or the app's own recent list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistorySource {
    Scan,
    Recent,
}

/// A session's timestamp, in either of the two shapes evo hands out: seconds since the
/// Unix epoch, or an RFC3339 UTC instant as a journal header carries it
/// (`2026-09-29T09:25:44Z`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum When {
    Epoch(i64),
    Text(String),
}

impl When {
    /// The instant as epoch seconds, or `None` when the text is not an RFC3339 UTC
    /// timestamp — the row then says nothing about the time.
    pub fn epoch_seconds(&self) -> Option<i64> {
        match self {
            When::Epoch(seconds) => Some(*seconds),
            When::Text(text) => parse_rfc3339(text),
        }
    }
}

impl From<i64> for When {
    fn from(seconds: i64) -> When {
        When::Epoch(seconds)
    }
}

impl From<&str> for When {
    fn from(text: &str) -> When {
        When::Text(text.to_string())
    }
}

impl From<String> for When {
    fn from(text: String) -> When {
        When::Text(text)
    }
}

/// One resumable session, as the store hands it over: the journal's path and folder, when
/// it ran, what it ran with, and which list it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    pub session_path: String,
    /// Absolute path of the folder the swarm ran in.
    pub folder: String,
    pub when: When,
    /// The lane count the journal records.
    pub lanes: Option<u32>,
    pub coordinator_model: Option<String>,
    pub lanes_model: Option<String>,
    pub source: HistorySource,
    /// The app had this session open when it last quit. Only the app's own recents can say
    /// so — the scan sees the journal, and a swarm that never came down wrote no new one —
    /// so a row the scan alone found is always `false` (the store's `Recent.open_at_quit`).
    pub open_at_quit: bool,
}

/// One row of the history list (§7.2): the folder's name, its path shortened around the
/// home directory, a meta line of what is known about the session, and the whole of it for
/// the row's tooltip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryRow {
    pub title: String,
    pub subtitle: String,
    pub meta: String,
    /// The whole entry on one line, for the tooltip: the folder's full path, the session
    /// file, the instant with its zone spelled out, both models and the lane count.
    pub tooltip: String,
    /// The session to resume (`--resume`) and the folder to run in (cwd).
    pub session_path: String,
    pub folder: String,
    pub coordinator_model: Option<String>,
    pub lanes_model: Option<String>,
    pub source: HistorySource,
    /// The app had this session open when it last quit: the row wears a badge for it.
    pub open_at_quit: bool,
}

// --- choosers from /registry -------------------------------------------------------

/// The coordinator's model chooser: every model the registry knows, sorted by provider then
/// id, with the ambiguity rule of §7.3 for the label.
///
/// An id registered under more than one provider can only be reached as
/// `--model <id>`, and a bare id resolves **first-wins** (`find-model`,
/// `src/provider/registry.lisp`: "A bare id resolves to the FIRST registration of that id"),
/// so exactly one of those registrations is offered and the others are listed disabled,
/// saying which one the flag would pick. See [`Reach::ById`].
pub fn coordinator_chooser(registry: &Value) -> Chooser {
    Chooser {
        options: model_options(registry, None, COORDINATOR_DEFAULT, Reach::ById),
        uncertain: false,
    }
}

/// The lanes' model chooser: the same models, but a model is available only when a lane
/// could register it — its `api` has to be in the kernel's own set, which a `--no-userspace`
/// probe's `/registry.apis` reports (§9.4). `kernel_apis` is that array; `None` means no
/// probe has said yet, and the chooser is then [`Chooser::uncertain`].
pub fn lanes_chooser(registry: &Value, kernel_apis: Option<&Value>) -> Chooser {
    match kernel_apis.filter(|apis| apis.is_array()) {
        Some(apis) => Chooser {
            options: model_options(registry, Some(apis), LANES_DEFAULT, Reach::ByProvider),
            uncertain: false,
        },
        None => Chooser {
            options: model_options(registry, None, LANES_DEFAULT, Reach::ByProvider),
            uncertain: true,
        },
    }
}

/// How a chosen model reaches the swarm — which decides whether *every* registration of a
/// model id is a real choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reach {
    /// The coordinator's `--model <id>`: the model is named by its bare id, so only the
    /// registration a bare id resolves to can be reached. evo's registries keep
    /// registration order (`*models*` is documented "in registration order",
    /// `src/provider/registry.lisp`, and `/registry.models` is that list walked in order,
    /// `src/serve/routes.lisp`), and `find-model` takes the **first** entry for a bare id —
    /// so the first registration in the registry is the one `--model` runs.
    ById,
    /// A lane's model, written into the project's `swarm.lisp` with its provider: every
    /// registration of an id is its own choice, exactly as §9.6 writes it.
    ByProvider,
}

/// The worker-count chooser (§7.2 row 3): Default, then 1…64. `swarm_workers` is the
/// configured `:swarm-workers` value, when one is known — it is what Default means, so the
/// label says so.
pub fn workers_chooser(swarm_workers: Option<u16>) -> Chooser {
    // The list is 1…64 (§14.2); the default a project configured may sit outside it, and
    // the label still says so rather than pretending nothing is configured.
    let known = swarm_workers.filter(|n| *n > 0);
    let default = ChooserOption {
        label: match known {
            Some(n) => format!("Default ({})", n),
            None => "Default".to_string(),
        },
        detail: match known {
            Some(n) => format!(":swarm-workers {}", n),
            None => "evo's own default, else 6".to_string(),
        },
        ..default_option("")
    };
    let mut options = vec![default];
    options.extend((1..=WORKERS_MAX).map(|n| ChooserOption {
        key: n.to_string(),
        label: n.to_string(),
        detail: String::new(),
        available: true,
        unavailable_reason: None,
        model_id: None,
        provider: None,
    }));
    Chooser { options, uncertain: false }
}

/// The configured `:swarm-workers` from a `/registry` body's `settings` — evo's own default
/// lane count (`docs/swarm.md`), which the workers chooser's Default means when it is set.
/// A JSON key crosses the wire hyphen→underscore (`src/serve/json.lisp`), hence
/// `swarm_workers`. Absent, null, non-numbers and zero read as "not configured"; a value the
/// chooser's own 1–64 range does not hold is still reported, because it is what Default
/// means.
pub fn swarm_workers_setting(registry: &Value) -> Option<u16> {
    registry
        .get("settings")?
        .get("swarm_workers")?
        .as_u64()
        .and_then(|n| u16::try_from(n).ok())
        .filter(|n| *n > 0)
}

/// Every model of a `/registry` body as an option, Default first.
///
/// `kernel_apis` is `None` for "cannot tell": then nothing is marked unavailable.
fn model_options(
    registry: &Value,
    kernel_apis: Option<&Value>,
    default_detail: &str,
    reach: Reach,
) -> Vec<ChooserOption> {
    let models = registry.get("models").and_then(Value::as_array);
    let Some(models) = models else {
        return vec![default_option(default_detail)];
    };

    // An id under more than one provider cannot be named by its id alone (§7.3), and the
    // first registration of an id is the one a bare id means. Both are read in registry
    // order, which is registration order.
    let mut by_id: Vec<(String, Vec<String>)> = Vec::new();
    for model in models {
        let (Some(id), Some(provider)) = (string_field(model, "id"), string_field(model, "provider"))
        else {
            continue;
        };
        match by_id.iter_mut().find(|(known, _)| *known == id) {
            Some((_, providers)) => providers.push(provider),
            None => by_id.push((id, vec![provider])),
        }
    }
    let ambiguous = |id: &str| {
        by_id
            .iter()
            .any(|(known, providers)| known == id && providers.len() > 1)
    };
    let bare_id_means = |id: &str| -> Option<String> {
        by_id
            .iter()
            .find(|(known, _)| known == id)
            .and_then(|(_, providers)| providers.first().cloned())
    };

    let apis: Option<Vec<String>> = kernel_apis.map(|apis| {
        apis.as_array()
            .map(|apis| apis.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default()
    });

    let mut options: Vec<ChooserOption> = Vec::with_capacity(models.len());
    for model in models {
        let (Some(id), Some(provider)) = (string_field(model, "id"), string_field(model, "provider"))
        else {
            continue;
        };
        let provider = provider.to_lowercase();
        let label = if ambiguous(&id) {
            format!("{} ({})", id, provider)
        } else {
            id.clone()
        };
        // A bare id reaches only the registration it resolves to; every other registration
        // of an ambiguous id is visible but not choosable, and says why.
        let unreachable = match reach {
            Reach::ByProvider => None,
            Reach::ById if ambiguous(&id) => bare_id_means(&id)
                .filter(|winner| *winner != provider)
                .map(|winner| format!("evo-swarm --model resolves this id to {}", winner)),
            Reach::ById => None,
        };
        let (api_ok, api_reason) = match &apis {
            // No probe: nothing is claimed to be unavailable, and the chooser says so.
            None => (true, None),
            Some(apis) => {
                let api = string_field(model, "api");
                if api.as_ref().is_some_and(|api| apis.contains(api)) {
                    (true, None)
                } else {
                    (false, Some(NEEDS_EXTENSION_API.to_string()))
                }
            }
        };
        let unavailable_reason = unreachable.clone().or(api_reason);
        let available = api_ok && unreachable.is_none();
        options.push(ChooserOption {
            key: format!("{}@{}", id, provider),
            label,
            detail: model_detail(model),
            available,
            unavailable_reason,
            model_id: Some(id),
            provider: Some(provider),
        });
    }
    // Stable, so two models with the same provider and id keep the registry's order.
    options.sort_by(|a, b| {
        a.provider
            .cmp(&b.provider)
            .then_with(|| a.model_id.cmp(&b.model_id))
    });

    let mut all = vec![default_option(default_detail)];
    all.extend(options);
    all
}

/// One model's detail line: the context window, then what else the registry says —
/// `200k ctx · vision · effort low–max`.
fn model_detail(model: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(window) = model.get("context_window").and_then(Value::as_u64) {
        if window > 0 {
            parts.push(format!("{} ctx", window_size(window)));
        }
    }
    if model.get("vision").and_then(Value::as_bool) == Some(true) {
        parts.push("vision".to_string());
    }
    let effort: Vec<&str> = model
        .get("effort")
        .and_then(Value::as_array)
        .map(|levels| levels.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    match effort.as_slice() {
        [] => {}
        [only] => parts.push(format!("effort {}", only)),
        [first, .., last] => parts.push(format!("effort {}–{}", first, last)),
    }
    parts.join(" · ")
}

/// A context window as evo's own picker writes it — `format-context-window`
/// (`src/command/command.lisp:241`), the narrow description column of `/model`:
/// `200000` → `200k`, `1000000` → `1M`, `1500000` → `1.5M`, `1048576` → `1.0M`, and
/// `999999` → `1000k`, because below a million it is the same `(round n 1000)` the status
/// line uses. Only this chooser's detail follows the picker; the §7.3 readout keeps the
/// TUI's k rule.
fn window_size(window: u64) -> String {
    if window < 1_000_000 {
        return k_tokens(window);
    }
    // The picker divides by a million in double precision and prints that double, so this
    // does the same — and rounds it the way `~,1f` does: the *exact* value of the double to
    // one decimal, ties away from zero. (Formatting the f64 with Rust's `{:.1}` would round
    // exact ties to even instead, which reads `1.2M` where evo prints `1.3M`.)
    let millions = window as f64 / 1_000_000.0;
    if millions.floor() == millions {
        return format!("{}M", window / 1_000_000);
    }
    let tenths = round_tenths(millions);
    format!("{}.{}M", tenths / 10, tenths % 10)
}

/// `m` (≥ 1) times ten, rounded to the nearest integer with ties away from zero — the
/// number of tenths `~,1f` prints. The arithmetic is exact (`m` is decomposed into its
/// mantissa and exponent), because multiplying the double by ten first would lose the very
/// bits the rounding turns on: `(format nil "~,1f" 1.65d0)` is `1.6`, and `1.65 * 10` in
/// double precision is `16.5`, which would round to `17`.
fn round_tenths(m: f64) -> u64 {
    let bits = m.to_bits();
    // A finite double is `mantissa * 2^exponent` with a 53-bit mantissa.
    let mantissa = (bits & ((1u64 << 52) - 1)) | (1u64 << 52);
    let exponent = ((bits >> 52) & 0x7ff) as i64 - 1075;
    // `m * 10 = mantissa * 5 * 2^(exponent + 1)`, as a fraction numerator/denominator.
    let power = exponent + 1;
    let mut numerator = mantissa as i128 * 5;
    let mut denominator = 1i128;
    if power >= 0 {
        numerator <<= power;
    } else {
        denominator <<= -power;
    }
    // Nearest with ties away from zero is `floor(m * 10 + 1/2)`, and neither value is
    // negative here, so integer division truncating is that floor.
    ((2 * numerator + denominator) / (2 * denominator)) as u64
}

/// The first option of every chooser.
fn default_option(detail: &str) -> ChooserOption {
    ChooserOption {
        key: DEFAULT_KEY.to_string(),
        label: "Default".to_string(),
        detail: detail.to_string(),
        available: true,
        unavailable_reason: None,
        model_id: None,
        provider: None,
    }
}

// --- the launch path (§9.6) --------------------------------------------------------

/// `<folder>/.evo/swarm.lisp` — the file the lanes' model is written into, and the folder's
/// own configuration, shared by every tab in that folder (§9.6).
pub fn swarm_lisp_path(folder: &str) -> String {
    format!("{}/.evo/swarm.lisp", folder.trim_end_matches('/'))
}

/// The note shown next to the lanes chooser (§9.6), with the folder's path shortened around
/// the home directory: `Saved to ~/coding/foo/.evo/swarm.lisp — shared by every tab`. Short
/// enough that a folder under `~` keeps it on one line under the lanes select; a deeper path
/// wraps to the second line the caption reserves.
pub fn lanes_model_note(folder: &str, home: Option<&str>) -> String {
    format!(
        "Saved to {} — shared by every tab",
        home_short(&swarm_lisp_path(folder), home)
    )
}

/// A path with the home directory shortened to `~`, the way the history rows show it.
pub fn home_short(path: &str, home: Option<&str>) -> String {
    let Some(home) = home else {
        return path.to_string();
    };
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return path.to_string();
    }
    if path == home {
        return "~".to_string();
    }
    match path.strip_prefix(home) {
        Some(rest) if rest.starts_with('/') => format!("~{}", rest),
        _ => path.to_string(),
    }
}

// --- history (§9.5) -----------------------------------------------------------------

/// The history rows: merged by session path, newest first, with unknown parts of the meta
/// line left out.
///
/// `now` is the clock the relative times are read against (epoch seconds) and
/// `offset_seconds` the caller's local UTC offset, which the relative words use for their
/// day boundaries and the tooltip prints the instant in — a desktop shows a session's time
/// where the person is, not on the machine that wrote the journal. Instants stay epoch
/// internally, so nothing here depends on the offset being the same between two calls.
pub fn history_rows(
    entries: &[HistoryEntry],
    now: i64,
    offset_seconds: i32,
    home: Option<&str>,
) -> Vec<HistoryRow> {
    // Newest first; a session whose time is unknown sorts last, and entries with the same
    // time keep the order they came in.
    let mut ordered: Vec<&HistoryEntry> = entries.iter().collect();
    ordered.sort_by_key(|entry| std::cmp::Reverse(entry.when.epoch_seconds()));

    let mut merged: Vec<HistoryEntry> = Vec::with_capacity(ordered.len());
    for entry in ordered {
        match merged.iter_mut().find(|row| row.session_path == entry.session_path) {
            // The same session from both lists (the scan and the app's own recents): one
            // row, carrying what either of them knew. The newer one is the base, so its
            // time is the one shown.
            Some(base) => {
                base.lanes = base.lanes.or(entry.lanes);
                if base.coordinator_model.is_none() {
                    base.coordinator_model = entry.coordinator_model.clone();
                }
                if base.lanes_model.is_none() {
                    base.lanes_model = entry.lanes_model.clone();
                }
                // The app's own recents are the only side that can know this, so one true
                // copy makes the row true.
                base.open_at_quit |= entry.open_at_quit;
            }
            None => merged.push(entry.clone()),
        }
    }
    merged
        .iter()
        .map(|entry| history_row(entry, now, offset_seconds, home))
        .collect()
}

fn history_row(entry: &HistoryEntry, now: i64, offset_seconds: i32, home: Option<&str>) -> HistoryRow {
    HistoryRow {
        title: base_name(&entry.folder),
        subtitle: home_short(&entry.folder, home),
        meta: meta_line(
            &entry.lanes,
            entry.coordinator_model.as_deref(),
            entry.when.epoch_seconds(),
            now,
            offset_seconds,
        ),
        tooltip: tooltip_line(entry, offset_seconds),
        session_path: entry.session_path.clone(),
        folder: entry.folder.clone(),
        coordinator_model: entry.coordinator_model.clone(),
        lanes_model: entry.lanes_model.clone(),
        source: entry.source,
        open_at_quit: entry.open_at_quit,
    }
}

/// The row's tooltip: everything known about the session, in one line, with the instant in
/// the caller's own offset.
fn tooltip_line(entry: &HistoryEntry, offset_seconds: i32) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !entry.folder.is_empty() {
        // The full path, not the `~`-shortened one the row shows: the tooltip is where the
        // absolute answer belongs.
        parts.push(entry.folder.trim_end_matches('/').to_string());
    }
    let session = base_name(&entry.session_path);
    if !session.is_empty() {
        parts.push(session);
    }
    if let Some(when) = entry.when.epoch_seconds() {
        parts.push(absolute_time(when, offset_seconds));
    }
    // The app's own record, right after the instant: this is the session you were in when
    // the app went away, not merely the one written last.
    if entry.open_at_quit {
        parts.push("open at last quit".to_string());
    }
    if let Some(model) = entry.coordinator_model.as_deref().filter(|model| !model.is_empty()) {
        parts.push(format!("coordinator: {}", model));
    }
    if let Some(model) = entry.lanes_model.as_deref().filter(|model| !model.is_empty()) {
        parts.push(format!("lanes: {}", model));
    }
    if let Some(lanes) = entry.lanes {
        parts.push(format!("{} lane{}", lanes, if lanes == 1 { "" } else { "s" }));
    }
    parts.join(" · ")
}

/// An instant in full, in a caller-given offset, with that offset spelled out:
/// `2026-09-29 17:25:44 +08:00`.
///
/// The offset is always printed, even when it is `+00:00`: the instant is epoch seconds and
/// a reader has to be told which zone the clock face is in, never left to assume one.
fn absolute_time(when: i64, offset_seconds: i32) -> String {
    let local = when + offset_seconds as i64;
    let (year, month, day) = civil_from_epoch(local);
    let seconds_of_day = local.rem_euclid(86_400);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} {}",
        year,
        month,
        day,
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
        seconds_of_day % 60,
        offset_label(offset_seconds)
    )
}

/// A UTC offset as `+08:00`, `-05:00`, `+05:30`.
fn offset_label(offset_seconds: i32) -> String {
    let sign = if offset_seconds < 0 { '-' } else { '+' };
    let magnitude = offset_seconds.unsigned_abs();
    format!("{}{:02}:{:02}", sign, magnitude / 3600, (magnitude % 3600) / 60)
}

/// The meta line: what is known about the session, unknown parts left out.
fn meta_line(
    lanes: &Option<u32>,
    coordinator_model: Option<&str>,
    when: Option<i64>,
    now: i64,
    offset_seconds: i32,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(lanes) = lanes {
        parts.push(format!("{} lane{}", lanes, if *lanes == 1 { "" } else { "s" }));
    }
    if let Some(when) = when {
        parts.push(relative_time(when, now, offset_seconds));
    }
    if let Some(model) = coordinator_model.filter(|model| !model.is_empty()) {
        parts.push(format!("coordinator: {}", model));
    }
    parts.join(" · ")
}

/// The last component of a path, without a trailing separator: a folder's own name, or a
/// session's file name.
fn base_name(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rsplit('/').next() {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => trimmed.to_string(),
    }
}

/// How long ago, in the history list's words: `just now`, `5m ago`, `2h ago`, `yesterday`,
/// `3d ago`, then the date — `12 Sep`, or `12 Sep 2025` when it is another year.
///
/// `offset_seconds` is the local UTC offset: `yesterday` means the calendar day before the
/// caller's today (so an evening session is still yesterday the next morning, and one from
/// two evenings ago is not), and the dates are the caller's calendar dates. The elapsed-hour
/// words are plain elapsed time and need no zone.
///
/// A time in the future reads as `just now`: the clock the app compares against is the
/// same machine's.
pub fn relative_time(when: i64, now: i64, offset_seconds: i32) -> String {
    let elapsed = now - when;
    if elapsed < 60 {
        return "just now".to_string();
    }
    if elapsed < 3600 {
        return format!("{}m ago", elapsed / 60);
    }
    if elapsed < 86_400 {
        return format!("{}h ago", elapsed / 3600);
    }
    // Elapsed time is at least a day, so the local day is at least yesterday's: with a fixed
    // offset, `days` here is ≥ 1 by construction.
    let offset = offset_seconds as i64;
    let days = (now + offset).div_euclid(86_400) - (when + offset).div_euclid(86_400);
    if days == 1 {
        return "yesterday".to_string();
    }
    if days < 7 {
        return format!("{}d ago", days);
    }
    let (year, month, day) = civil_from_epoch(when + offset);
    let (this_year, ..) = civil_from_epoch(now + offset);
    if year == this_year {
        format!("{} {}", day, MONTHS[(month - 1) as usize])
    } else {
        format!("{} {} {}", day, MONTHS[(month - 1) as usize], year)
    }
}

const MONTHS: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// Seconds since the epoch as a UTC calendar date.
fn civil_from_epoch(seconds: i64) -> (i64, u32, u32) {
    civil_from_days(seconds.div_euclid(86_400))
}

/// Days since 1970-01-01 as `(year, month, day)`, UTC — Howard Hinnant's civil calendar
/// algorithm, exact for every input (no float, no table).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365; // [0, 399]
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100); // [0, 365]
    let month_index = (5 * day_of_year + 2) / 153; // [0, 11], March is 0
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32; // [1, 31]
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// The inverse: the day number of a civil date.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400; // [0, 399]
    let month_index = if month > 2 { month - 3 } else { month + 9 } as i64; // [0, 11]
    let day_of_year = (153 * month_index + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// An RFC3339 instant as epoch seconds: `2026-09-29T09:25:44Z`, with an optional fractional
/// part and an optional `±HH:MM` offset (a space is accepted where RFC3339 wants the `T`,
/// which is how a hand-written header can look). Anything else is not a timestamp.
///
/// The offset is **required**: a naive `2026-09-29T09:25:44` is refused rather than read as
/// UTC, because guessing the zone of a local clock is exactly the mistake that makes two
/// machines' timestamps disagree.
fn parse_rfc3339(text: &str) -> Option<i64> {
    if text.len() < 19 {
        return None;
    }
    let year: i64 = text.get(0..4)?.parse().ok()?;
    let month: u32 = text.get(5..7)?.parse().ok()?;
    let day: u32 = text.get(8..10)?.parse().ok()?;
    let hour: i64 = text.get(11..13)?.parse().ok()?;
    let minute: i64 = text.get(14..16)?.parse().ok()?;
    let second: i64 = text.get(17..19)?.parse().ok()?;
    let separators = text.get(4..5)? == "-"
        && text.get(7..8)? == "-"
        && matches!(text.get(10..11)?, "T" | "t" | " ")
        && text.get(13..14)? == ":"
        && text.get(16..17)? == ":";
    if !separators
        || !(1..=12).contains(&month)
        || day < 1
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let mut rest = text.get(19..).unwrap_or("");
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.find(|c: char| !c.is_ascii_digit()).unwrap_or(fraction.len());
        if digits == 0 {
            return None;
        }
        rest = &fraction[digits..];
    }
    let offset = match rest {
        "" => return None,
        "Z" | "z" => 0,
        _ => {
            let sign = match rest.get(0..1)? {
                "+" => 1,
                "-" => -1,
                _ => return None,
            };
            let hours: i64 = rest.get(1..3)?.parse().ok()?;
            let minutes: i64 = rest.get(4..6)?.parse().ok()?;
            if rest.get(3..4)? != ":" || hours > 23 || minutes > 59 {
                return None;
            }
            sign * (hours * 3600 + minutes * 60)
        }
    };
    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

// --- the empty tab -----------------------------------------------------------------

/// The empty tab (§7.2): the three choosers, what is chosen in each, and the history list.
#[derive(Clone, Debug)]
pub struct Launcher {
    /// The catalog the choosers were built from: the model cache, or a probe's reply.
    registry: Value,
    /// The kernel's API set from a `--no-userspace` probe, or `None` while none has said.
    kernel_apis: Option<Value>,
    /// The configured `:swarm-workers`, which is what the workers chooser's Default means.
    swarm_workers: Option<u16>,
    coordinator: Chooser,
    lanes: Chooser,
    workers: Chooser,
    coordinator_key: String,
    lanes_key: String,
    workers_key: String,
    history: Vec<HistoryRow>,
}

impl Default for Launcher {
    fn default() -> Self {
        Launcher::new()
    }
}

impl Launcher {
    /// An empty tab before any registry has arrived: the Default option in every chooser,
    /// the worker counts, and no history.
    pub fn new() -> Launcher {
        let mut launcher = Launcher {
            registry: Value::Null,
            kernel_apis: None,
            swarm_workers: None,
            coordinator: Chooser::default(),
            lanes: Chooser::default(),
            workers: Chooser::default(),
            coordinator_key: DEFAULT_KEY.to_string(),
            lanes_key: DEFAULT_KEY.to_string(),
            workers_key: DEFAULT_KEY.to_string(),
            history: Vec::new(),
        };
        launcher.rebuild_choosers();
        launcher
    }

    /// The model catalog: the coordinator's chooser gets every model, the lanes' chooser
    /// the same models — but **not** measured against this registry's `apis`. A live
    /// coordinator's registry carries the APIs its extensions added, while a lane only has
    /// what a `--no-userspace` probe would report, so pass that with
    /// [`Launcher::set_kernel_apis`] — or hand the probe's reply to
    /// [`Launcher::set_probe_registry`], which is both. The registry's own
    /// `:swarm-workers` setting, when it has one, becomes the workers default.
    ///
    /// Returns whether anything the UI renders changed.
    pub fn set_registry(&mut self, registry: &Value) -> bool {
        let before = self.clone();
        self.registry = registry.clone();
        if let Some(workers) = swarm_workers_setting(registry) {
            self.swarm_workers = Some(workers);
        }
        self.rebuild_choosers();
        self.differs(&before)
    }

    /// A `--no-userspace` probe's registry: its models *and* its `apis` — the kernel's own
    /// set, which is what the lanes chooser measures availability against (§9.4).
    pub fn set_probe_registry(&mut self, registry: &Value) -> bool {
        let before = self.clone();
        self.registry = registry.clone();
        self.kernel_apis = normalize_apis(registry.get("apis"));
        if let Some(workers) = swarm_workers_setting(registry) {
            self.swarm_workers = Some(workers);
        }
        self.rebuild_choosers();
        self.differs(&before)
    }

    /// The kernel's API set — a probe's `/registry.apis` array, or `None` for "no probe has
    /// said". Without it the lanes chooser marks nothing unavailable and reports
    /// [`Chooser::uncertain`] instead of claiming models work in a lane.
    pub fn set_kernel_apis(&mut self, apis: Option<&Value>) -> bool {
        let before = self.clone();
        self.kernel_apis = normalize_apis(apis);
        self.rebuild_choosers();
        self.differs(&before)
    }

    /// The configured `:swarm-workers`, which is what the workers chooser's Default means
    /// (`docs/swarm.md`: the default lane count, `--workers` wins). Passed separately from
    /// the registry because it can also come from the project's own `swarm.lisp`.
    pub fn set_swarm_workers(&mut self, workers: Option<u16>) -> bool {
        let before = self.clone();
        self.swarm_workers = workers.filter(|n| *n > 0);
        self.rebuild_choosers();
        self.differs(&before)
    }

    /// The resumable sessions, from the disk scan and the app's own recents (§9.5). `now`
    /// is the clock the relative times read against, `offset_seconds` the local UTC offset
    /// the rows are shown in, `home` the directory to shorten paths around — the tab passes
    /// the same home it shows elsewhere.
    pub fn set_history(
        &mut self,
        entries: &[HistoryEntry],
        now: i64,
        offset_seconds: i32,
        home: Option<&str>,
    ) -> bool {
        let rows = history_rows(entries, now, offset_seconds, home);
        if self.history == rows {
            return false;
        }
        self.history = rows;
        true
    }

    pub fn coordinator(&self) -> &Chooser {
        &self.coordinator
    }

    pub fn lanes(&self) -> &Chooser {
        &self.lanes
    }

    pub fn workers(&self) -> &Chooser {
        &self.workers
    }

    pub fn history(&self) -> &[HistoryRow] {
        &self.history
    }

    /// The chooser of one row of the empty tab.
    pub fn chooser(&self, which: Choice) -> &Chooser {
        match which {
            Choice::Coordinator => &self.coordinator,
            Choice::Lanes => &self.lanes,
            Choice::Workers => &self.workers,
        }
    }

    /// The chosen key of one row — [`DEFAULT_KEY`] until something is chosen.
    pub fn selected_key(&self, which: Choice) -> &str {
        match which {
            Choice::Coordinator => &self.coordinator_key,
            Choice::Lanes => &self.lanes_key,
            Choice::Workers => &self.workers_key,
        }
    }

    /// The chosen option — `None` only if the chooser was rebuilt without it, which
    /// [`Launcher::select`] prevents by falling back to Default.
    pub fn selected(&self, which: Choice) -> Option<&ChooserOption> {
        let key = self.selected_key(which);
        self.chooser(which).option(key)
    }

    /// Choose one option by key. An unknown key is ignored — the choosers are rebuilt from
    /// `GET /registry`, and a selection cannot outlive the option it named. Returns whether
    /// the choice changed.
    pub fn select(&mut self, which: Choice, key: &str) -> bool {
        if self.chooser(which).option(key).is_none() || self.selected_key(which) == key {
            return false;
        }
        match which {
            Choice::Coordinator => self.coordinator_key = key.to_string(),
            Choice::Lanes => self.lanes_key = key.to_string(),
            Choice::Workers => self.workers_key = key.to_string(),
        }
        true
    }

    /// What the three choosers add up to (§7.2, §9.6): the coordinator's `--model`, the
    /// lanes' model for `<folder>/.evo/swarm.lisp`, and `--workers`. `None` is Default
    /// everywhere — nothing is passed and no block is written.
    pub fn plan(&self) -> LaunchPlan {
        LaunchPlan {
            model: selected_model(&self.coordinator, &self.coordinator_key),
            lanes_model: selected_model(&self.lanes, &self.lanes_key),
            workers: self
                .workers_key
                .parse::<u16>()
                .ok()
                .filter(|n| (1..=WORKERS_MAX).contains(n)),
        }
    }

    /// The note shown next to the lanes chooser for a folder (§9.6).
    pub fn lanes_model_note(&self, folder: &str, home: Option<&str>) -> String {
        lanes_model_note(folder, home)
    }

    fn rebuild_choosers(&mut self) {
        self.coordinator = coordinator_chooser(&self.registry);
        self.lanes = lanes_chooser(&self.registry, self.kernel_apis.as_ref());
        self.workers = workers_chooser(self.swarm_workers);
        self.keep_selections_valid();
    }

    /// A chosen key that is no longer in its chooser falls back to Default: a model can
    /// leave the registry between one refresh and the next.
    fn keep_selections_valid(&mut self) {
        if self.coordinator.option(&self.coordinator_key).is_none() {
            self.coordinator_key = DEFAULT_KEY.to_string();
        }
        if self.lanes.option(&self.lanes_key).is_none() {
            self.lanes_key = DEFAULT_KEY.to_string();
        }
        if self.workers.option(&self.workers_key).is_none() {
            self.workers_key = DEFAULT_KEY.to_string();
        }
    }

    /// Everything the empty tab renders. The history is not in it: it is set by its own
    /// entry point, from data that arrives separately (§9.5's scan).
    fn differs(&self, before: &Launcher) -> bool {
        self.coordinator != before.coordinator
            || self.lanes != before.lanes
            || self.workers != before.workers
            || self.coordinator_key != before.coordinator_key
            || self.lanes_key != before.lanes_key
            || self.workers_key != before.workers_key
    }
}

fn selected_model(chooser: &Chooser, key: &str) -> Option<(String, String)> {
    if key == DEFAULT_KEY {
        return None;
    }
    chooser.option(key)?.model()
}

/// An `apis` array, or `None` for anything else — a null or a missing field means no probe
/// has reported the kernel's set, not that the set is empty.
fn normalize_apis(apis: Option<&Value>) -> Option<Value> {
    apis.filter(|apis| apis.is_array()).cloned()
}

/// A JSON field as a string; null, a missing key and a non-string all read as `None`.
fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

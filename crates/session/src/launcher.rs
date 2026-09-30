//! The empty tab's view model (§7.2, §9, §14.2): the model and thinking choosers, the
//! worker-count chooser, the launch plan they add up to, the history list, and the
//! problems `evo-swarm check --json` found with the launch they describe.
//!
//! The input is one JSON document: the `/catalog` body, exactly as
//! `evo-swarm catalog --json` prints it (§5.6, §9). Nothing here does I/O and nothing
//! here depends on another crate — the app hands the body in, and the window paints
//! what comes out.
//!
//! # The choosers
//!
//! Every chooser's first option is **Default** — the state the tab starts in, and the
//! one that passes no flag at all. Then:
//!
//! * the coordinator's `--model`, one option per registration;
//! * the lanes' `--lane-model`, the same registrations, greyed out where the catalog's
//!   own `lanes.models` says a lane cannot register one — it says why, and that reason
//!   is what the option shows;
//! * the lanes' `--lane-thinking`, the levels the catalog lists;
//! * the worker count (`--workers`).
//!
//! A model is chosen by its **`ID@PROVIDER` pair**: `--model id@provider` selects
//! exactly that registration (§1), so the ambiguity rule of the old bare-id `--model`
//! is gone — two registrations of one id are two options, each named with its provider.
//!
//! # History (§2)
//!
//! [`history_rows`] turns the session index's rows, merged with the app's own recents,
//! into display rows: the session's title (the first user text) or the folder's name,
//! the `~`-shortened path, and a meta line of what is known (`6 lanes · 2h ago ·
//! coordinator: …`).
//!

use serde_json::Value;

use crate::k_tokens;

/// The key of every chooser's first option: the state that passes nothing to evo.
pub const DEFAULT_KEY: &str = "default";

/// The largest worker count the chooser offers (§14.2 allows 1–64).
pub const WORKERS_MAX: u16 = 64;

/// The detail line under the coordinator chooser's Default option.
const COORDINATOR_DEFAULT: &str = "evo's own default";

/// The detail line under the lanes chooser's Default option: a lane follows the
/// coordinator unless the launch says otherwise.
const LANES_DEFAULT: &str = "follows the coordinator";

/// The detail line under the thinking chooser's Default option.
const THINKING_DEFAULT: &str = "as the model is configured";

/// The thinking levels of §5.6, for a catalog that printed none.
const THINKING_LEVELS: [&str; 5] = ["off", "low", "medium", "high", "xhigh"];

/// What a lanes chooser says when the catalog reports a model is not ready: evo's own
/// `reason` is used when it gives one.
const NOT_READY: &str = "not ready in this session";

/// Which chooser a selection belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Choice {
    Coordinator,
    Lanes,
    LaneThinking,
    Workers,
}

/// One option of a chooser, as the Select renders it.
///
/// `model_id`/`provider` are the pair the launch plan needs and are `None` on the
/// Default option and on the thinking and worker-count options, which are not models.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChooserOption {
    /// Stable identity of the option: `default`, `id@provider` for a model, the level
    /// for a thinking option, `1`…`64` for a worker count.
    pub key: String,
    /// What the option is called: the model's id, with its provider when evo knows two
    /// registrations of that id.
    pub label: String,
    /// One compact line under the label: the context window and what else is known.
    pub detail: String,
    /// Whether this option can be launched as it is. Default options and worker counts
    /// always can; a model cannot when evo says it is not ready, or when a lane cannot
    /// register it.
    pub available: bool,
    /// Why not, for the unavailable ones — evo's own words where it gave them (§5.6).
    pub unavailable_reason: Option<String>,
    pub model_id: Option<String>,
    pub provider: Option<String>,
}

impl ChooserOption {
    /// The model this option names, as the launch plan wants it.
    pub fn model(&self) -> Option<(String, String)> {
        Some((self.model_id.clone()?, self.provider.clone()?))
    }

    /// The thinking level this option names, when it is one.
    pub fn level(&self) -> Option<&str> {
        (self.key != DEFAULT_KEY && self.model_id.is_none()).then_some(self.key.as_str())
    }
}

/// One chooser: the options, Default first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Chooser {
    pub options: Vec<ChooserOption>,
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

    /// The options that name a model, Default excluded.
    pub fn models(&self) -> impl Iterator<Item = &ChooserOption> {
        self.options
            .iter()
            .filter(|option| option.key != DEFAULT_KEY && option.model_id.is_some())
    }
}

/// What the tab's choosers add up to (§7.2): the flags a launch passes. Every field is
/// `None` on Default, which is a launch with no flag at all.
///
/// This is a **new** swarm's plan. A swarm resumed from history takes the coordinator's
/// own record instead — its models are the session's, not the empty tab's — so the
/// workspace passes none of this for a [`HistoryRow`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaunchPlan {
    /// The coordinator's `--model`.
    pub model: Option<(String, String)>,
    /// The lanes' `--lane-model`.
    pub lanes_model: Option<(String, String)>,
    /// The lanes' `--lane-thinking`.
    pub lane_thinking: Option<String>,
    /// The `--workers` count; evo's own default applies without it.
    pub workers: Option<u16>,
}

impl LaunchPlan {
    /// Whether this plan passes nothing at all.
    pub fn is_default(&self) -> bool {
        self.model.is_none()
            && self.lanes_model.is_none()
            && self.lane_thinking.is_none()
            && self.workers.is_none()
    }

    /// The model pair as the `ID@PROVIDER` spec a `--model` / `--lane-model` flag takes.
    pub fn spec(model: &(String, String)) -> String {
        format!("{}@{}", model.0, model.1)
    }
}

// --- choosers from the catalog -----------------------------------------------------

/// The coordinator's model chooser: every registration the catalog lists, sorted by
/// provider then id. `--model id@provider` names exactly one of them (§1), so each
/// registration is its own choice.
pub fn coordinator_chooser(catalog: &Value) -> Chooser {
    model_chooser(catalog, COORDINATOR_DEFAULT, |_, _| None)
}

/// The lanes' model chooser: the same registrations, but one a lane cannot register is
/// greyed out and says why — the catalog's own `lanes.models[].ok`/`reason`, computed by
/// `evo-swarm` from what a lane actually has (§5.6, §9).
///
/// A catalog with no `lanes` (an `evo-agent` body) has no such judgement to offer, and
/// then nothing is greyed out.
pub fn lanes_chooser(catalog: &Value) -> Chooser {
    let lanes = catalog
        .get("lanes")
        .and_then(|l| l.get("models"))
        .and_then(Value::as_array);
    model_chooser(catalog, LANES_DEFAULT, |id, provider| match lanes {
        None => None,
        Some(lanes) => lanes
            .iter()
            .find(|lane| {
                string(lane, "id").as_deref() == Some(id)
                    && string(lane, "provider").as_deref().map(str::to_lowercase)
                        == Some(provider.to_owned())
            })
            .and_then(|lane| match lane.get("ok").and_then(Value::as_bool) {
                Some(true) => None,
                _ => Some(string(lane, "reason").unwrap_or_else(|| NOT_READY.to_string())),
            }),
    })
}

/// The thinking chooser: the levels the catalog lists (`thinking_levels`), or the five
/// of §5.6 when it lists none. Default passes no `--thinking` at all, which leaves the
/// model's own configuration in place.
pub fn thinking_chooser(catalog: &Value) -> Chooser {
    let levels: Vec<String> = catalog
        .get("thinking_levels")
        .and_then(Value::as_array)
        .map(|levels| {
            levels
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .filter(|levels: &Vec<String>| !levels.is_empty())
        .unwrap_or_else(|| THINKING_LEVELS.iter().map(|l| (*l).to_string()).collect());
    let mut options = vec![default_option(THINKING_DEFAULT)];
    options.extend(levels.into_iter().map(|level| ChooserOption {
        key: level.clone(),
        label: level,
        detail: String::new(),
        available: true,
        unavailable_reason: None,
        model_id: None,
        provider: None,
    }));
    Chooser { options }
}

/// The worker-count chooser: Default, then 1…64.
pub fn workers_chooser() -> Chooser {
    let mut options = vec![ChooserOption {
        label: "Default".to_string(),
        detail: "evo's own default, else 6".to_string(),
        ..default_option("")
    }];
    options.extend((1..=WORKERS_MAX).map(|n| ChooserOption {
        key: n.to_string(),
        label: n.to_string(),
        detail: String::new(),
        available: true,
        unavailable_reason: None,
        model_id: None,
        provider: None,
    }));
    Chooser { options }
}

/// Every model of a catalog body as an option, Default first.
///
/// `unavailable` answers why a *lane* cannot run one registration; `None` means it can,
/// or that this chooser is not the lanes'.
fn model_chooser(
    catalog: &Value,
    default_detail: &str,
    unavailable: impl Fn(&str, &str) -> Option<String>,
) -> Chooser {
    let Some(models) = catalog.get("models").and_then(Value::as_array) else {
        return Chooser {
            options: vec![default_option(default_detail)],
        };
    };

    // Two registrations of one id are two models at launch time (§1), but a row still
    // has to be readable: an id evo knows under more than one provider is labelled with
    // the provider this option runs.
    let duplicate = |id: &str| {
        models
            .iter()
            .filter(|model| string(model, "id").as_deref() == Some(id))
            .count()
            > 1
    };

    let mut options: Vec<ChooserOption> = Vec::with_capacity(models.len());
    for model in models {
        let (Some(id), Some(provider)) = (string(model, "id"), string(model, "provider")) else {
            continue;
        };
        let provider = provider.to_lowercase();
        let ready = model.get("ready").and_then(Value::as_bool).unwrap_or(false);
        let why = unavailable(&id, &provider).or_else(|| {
            (!ready).then(|| string(model, "reason").unwrap_or_else(|| NOT_READY.to_string()))
        });
        options.push(ChooserOption {
            key: format!("{id}@{provider}"),
            label: if duplicate(&id) {
                format!("{id} ({provider})")
            } else {
                id.clone()
            },
            detail: model_detail(model),
            available: why.is_none(),
            unavailable_reason: why,
            model_id: Some(id),
            provider: Some(provider),
        });
    }
    // Stable, so two registrations with the same provider and id keep the catalog's
    // order.
    options.sort_by(|a, b| {
        a.provider
            .cmp(&b.provider)
            .then_with(|| a.model_id.cmp(&b.model_id))
    });

    let mut all = vec![default_option(default_detail)];
    all.extend(options);
    Chooser { options: all }
}

/// One model's detail line: the context window, then what else the catalog says —
/// `200k ctx · vision · reasons`.
fn model_detail(model: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(window) = model.get("context_window").and_then(Value::as_u64) {
        if window > 0 {
            parts.push(format!("{} ctx", k_tokens(window)));
        }
    }
    if model.get("images").and_then(Value::as_bool) == Some(true) {
        parts.push("vision".to_string());
    }
    if model.get("reasoning").and_then(Value::as_bool) == Some(true) {
        parts.push("reasons".to_string());
    }
    parts.join(" · ")
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

/// A JSON field as a non-empty string.
fn string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

// --- the launch path ----------------------------------------------------------------

/// The folder's own path shortened around the home directory, the way the history rows
/// show it: `/Users/you/coding/foo` with home `/Users/you` → `~/coding/foo`.
pub fn home_short(path: &str, home: Option<&str>) -> String {
    let Some(home) = home else {
        return path.to_string();
    };
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return path.to_string();
    }
    if path.trim_end_matches('/') == home {
        return "~".to_string();
    }
    match path.strip_prefix(home) {
        Some(rest) if rest.starts_with('/') => format!("~{}", rest.trim_end_matches('/')),
        _ => path.to_string(),
    }
}

// --- history (§2) -------------------------------------------------------------------

/// Where a history entry came from: the session index, or the app's own recent list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistorySource {
    /// Listed by `evo-agent sessions --json`.
    Index,
    /// Remembered by the app in `app.json`.
    Recent,
}

/// One resumable session, as the store hands it over: the journal's path and folder,
/// when it ran, what it ran with, and which list it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    /// Absolute path of the journal — the `--resume` argument.
    pub session_path: String,
    /// Absolute path of the folder the swarm ran in.
    pub folder: String,
    /// The first user text the index kept, or empty when it had none.
    pub title: String,
    /// Last written, in epoch seconds.
    pub when: Option<i64>,
    /// The lane count the app remembers starting, when it remembers one.
    pub lanes: Option<u32>,
    pub coordinator_model: Option<String>,
    pub lanes_model: Option<String>,
    pub source: HistorySource,
    /// The app had this session open when it last quit. Only the app's own recents can
    /// say so — a swarm that never came down wrote no new journal — so a row the index
    /// alone found is always `false`.
    pub open_at_quit: bool,
}

/// One row of the history list (§7.2): the session's title (or the folder's name), the
/// folder's path shortened around the home directory, a meta line of what is known about
/// the session, and the whole of it for the row's tooltip.
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

/// The history rows, newest first, with unknown parts of the meta line left out.
///
/// `now` is the clock the relative times are read against (epoch seconds) and
/// `offset_seconds` the caller's local UTC offset, which the relative words use for their
/// day boundaries and the tooltip prints the instant in — a desktop shows a session's
/// time where the person is, not on the machine that wrote the journal.
pub fn history_rows(
    entries: &[HistoryEntry],
    now: i64,
    offset_seconds: i32,
    home: Option<&str>,
) -> Vec<HistoryRow> {
    let mut ordered: Vec<&HistoryEntry> = entries.iter().collect();
    // Newest first; a session whose time is unknown sorts last, and rows with the same
    // time keep the order they came in.
    ordered.sort_by_key(|entry| std::cmp::Reverse(entry.when));
    // One row per session: the index and the app's recents can both name it, and the app
    // is the only side that knows the models it ran with.
    let mut merged: Vec<HistoryEntry> = Vec::with_capacity(ordered.len());
    for entry in ordered {
        match merged
            .iter_mut()
            .find(|row| row.session_path == entry.session_path)
        {
            Some(base) => {
                base.lanes = base.lanes.or(entry.lanes);
                if base.coordinator_model.is_none() {
                    base.coordinator_model = entry.coordinator_model.clone();
                }
                if base.lanes_model.is_none() {
                    base.lanes_model = entry.lanes_model.clone();
                }
                if base.title.is_empty() {
                    base.title = entry.title.clone();
                }
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

fn history_row(
    entry: &HistoryEntry,
    now: i64,
    offset_seconds: i32,
    home: Option<&str>,
) -> HistoryRow {
    let folder_name = base_name(&entry.folder);
    HistoryRow {
        // The session's own title names it when the index kept one — the first thing the
        // person typed — and the folder is the fallback.
        title: if entry.title.trim().is_empty() {
            folder_name
        } else {
            entry.title.clone()
        },
        subtitle: home_short(&entry.folder, home),
        meta: meta_line(
            &entry.lanes,
            entry.coordinator_model.as_deref(),
            entry.when,
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
    if let Some(when) = entry.when {
        parts.push(absolute_time(when, offset_seconds));
    }
    // The app's own record, right after the instant: this is the session you were in when
    // the app went away, not merely the one written last.
    if entry.open_at_quit {
        parts.push("open at last quit".to_string());
    }
    for (label, model) in [
        ("coordinator", entry.coordinator_model.as_deref()),
        ("lanes", entry.lanes_model.as_deref()),
    ] {
        if let Some(model) = model.filter(|model| !model.is_empty()) {
            parts.push(format!("{label}: {model}"));
        }
    }
    if let Some(lanes) = entry.lanes {
        parts.push(format!(
            "{} lane{}",
            lanes,
            if lanes == 1 { "" } else { "s" }
        ));
    }
    parts.join(" · ")
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
        parts.push(format!(
            "{} lane{}",
            lanes,
            if *lanes == 1 { "" } else { "s" }
        ));
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

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

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
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
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
    format!(
        "{}{:02}:{:02}",
        sign,
        magnitude / 3600,
        (magnitude % 3600) / 60
    )
}

// --- the empty tab ------------------------------------------------------------------

/// The empty tab (§7.2): the four choosers, what is chosen in each, and the history list.
/// What `evo-swarm check --json` says about the launch they describe is the store's
/// (`store::catalog::CheckReport`) and the tab holds it beside this.
#[derive(Clone, Debug, Default)]
pub struct Launcher {
    /// The catalog the choosers were built from: the last `catalog --json` body.
    catalog: Value,
    coordinator: Chooser,
    lanes: Chooser,
    lane_thinking: Chooser,
    workers: Chooser,
    coordinator_key: String,
    lanes_key: String,
    lane_thinking_key: String,
    workers_key: String,
    history: Vec<HistoryRow>,
}

impl Launcher {
    /// An empty tab before any catalog has arrived: the Default option in every chooser,
    /// the worker counts, no history and no problems.
    pub fn new() -> Launcher {
        let mut launcher = Launcher {
            coordinator_key: DEFAULT_KEY.to_string(),
            lanes_key: DEFAULT_KEY.to_string(),
            lane_thinking_key: DEFAULT_KEY.to_string(),
            workers_key: DEFAULT_KEY.to_string(),
            ..Launcher::default()
        };
        launcher.rebuild_choosers();
        launcher
    }

    /// The model catalog: the `/catalog` body `evo-swarm catalog --json` prints (§5.6).
    /// Returns whether anything the UI renders changed.
    pub fn set_catalog(&mut self, catalog: &Value) -> bool {
        let before = self.clone();
        self.catalog = catalog.clone();
        self.rebuild_choosers();
        self.differs(&before)
    }

    /// The resumable sessions, from the session index and the app's own recents (§2).
    /// `now` is the clock the relative times read against, `offset_seconds` the local UTC
    /// offset the rows are shown in, `home` the directory to shorten paths around.
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

    pub fn history(&self) -> &[HistoryRow] {
        &self.history
    }

    /// The chooser of one row of the empty tab.
    pub fn chooser(&self, which: Choice) -> &Chooser {
        match which {
            Choice::Coordinator => &self.coordinator,
            Choice::Lanes => &self.lanes,
            Choice::LaneThinking => &self.lane_thinking,
            Choice::Workers => &self.workers,
        }
    }

    /// The chosen key of one row — [`DEFAULT_KEY`] until something is chosen.
    pub fn selected_key(&self, which: Choice) -> &str {
        match which {
            Choice::Coordinator => &self.coordinator_key,
            Choice::Lanes => &self.lanes_key,
            Choice::LaneThinking => &self.lane_thinking_key,
            Choice::Workers => &self.workers_key,
        }
    }

    /// The chosen option — `None` only if the chooser was rebuilt without it, which
    /// [`Launcher::select`] prevents by falling back to Default.
    pub fn selected(&self, which: Choice) -> Option<&ChooserOption> {
        self.chooser(which).option(self.selected_key(which))
    }

    /// Choose one option by key. An unknown key is ignored — the choosers are rebuilt from
    /// the catalog, and a selection cannot outlive the option it named. Returns whether the
    /// choice changed.
    pub fn select(&mut self, which: Choice, key: &str) -> bool {
        if self.chooser(which).option(key).is_none() || self.selected_key(which) == key {
            return false;
        }
        match which {
            Choice::Coordinator => self.coordinator_key = key.to_string(),
            Choice::Lanes => self.lanes_key = key.to_string(),
            Choice::LaneThinking => self.lane_thinking_key = key.to_string(),
            Choice::Workers => self.workers_key = key.to_string(),
        }
        true
    }

    /// What the choosers add up to (§7.2): the coordinator's `--model`, the lanes'
    /// `--lane-model` and `--lane-thinking`, and `--workers`. `None` everywhere is a launch
    /// with no flag at all.
    pub fn plan(&self) -> LaunchPlan {
        LaunchPlan {
            model: selected_model(&self.coordinator, &self.coordinator_key),
            lanes_model: selected_model(&self.lanes, &self.lanes_key),
            lane_thinking: selected_level(&self.lane_thinking, &self.lane_thinking_key),
            workers: self
                .workers_key
                .parse::<u16>()
                .ok()
                .filter(|n| (1..=WORKERS_MAX).contains(n)),
        }
    }

    fn rebuild_choosers(&mut self) {
        self.coordinator = coordinator_chooser(&self.catalog);
        self.lanes = lanes_chooser(&self.catalog);
        self.lane_thinking = thinking_chooser(&self.catalog);
        self.workers = workers_chooser();
        self.keep_selections_valid();
    }

    /// A chosen key that is no longer in its chooser falls back to Default: a model can
    /// leave the catalog between one refresh and the next.
    fn keep_selections_valid(&mut self) {
        let stale = [
            !self.coordinator.option(&self.coordinator_key).is_some(),
            !self.lanes.option(&self.lanes_key).is_some(),
            !self.lane_thinking.option(&self.lane_thinking_key).is_some(),
            !self.workers.option(&self.workers_key).is_some(),
        ];
        for (stale, key) in stale.into_iter().zip([
            &mut self.coordinator_key,
            &mut self.lanes_key,
            &mut self.lane_thinking_key,
            &mut self.workers_key,
        ]) {
            if stale {
                *key = DEFAULT_KEY.to_string();
            }
        }
    }

    /// Everything the empty tab renders into its select widgets. The history is not in it:
    /// it has its own entry point, from data that arrives separately (§2).
    fn differs(&self, before: &Launcher) -> bool {
        self.coordinator != before.coordinator
            || self.lanes != before.lanes
            || self.lane_thinking != before.lane_thinking
            || self.workers != before.workers
            || self.coordinator_key != before.coordinator_key
            || self.lanes_key != before.lanes_key
            || self.lane_thinking_key != before.lane_thinking_key
            || self.workers_key != before.workers_key
    }
}

fn selected_model(chooser: &Chooser, key: &str) -> Option<(String, String)> {
    if key == DEFAULT_KEY {
        return None;
    }
    chooser.option(key)?.model()
}

fn selected_level(chooser: &Chooser, key: &str) -> Option<String> {
    if key == DEFAULT_KEY {
        return None;
    }
    chooser.option(key)?.level().map(str::to_owned)
}

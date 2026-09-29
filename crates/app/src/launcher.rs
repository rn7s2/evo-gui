//! What every empty tab is shown (§9.4, §9.5), and what a tab created later is
//! handed the moment it exists.
//!
//! The app learns three things at launch — the cached catalog, the catalog a
//! probe found, the sessions the scan found — and every empty tab has to show
//! them. That includes tabs that do not exist yet: the `+` in the strip, or a
//! tab the user opens while the scan is still walking, must come up with the
//! catalog and the history already in place. So the latest of everything lives in
//! [`LauncherData`] on the [`Shell`], and two paths apply it: a push after each
//! arrival, and the `observe_new` hook that catches a tab as it is created.

use gpui_kit::{App, Context, Entity, Window};
use serde_json::Value;

use session::{HistoryEntry, HistorySource, When};
use store::history::HistorySource as StoreSource;
use store::model_cache::ModelCache;
use store::time;
use swarm_client::{Payload, Registry};
use workspace::{TabContent, TabState};

use crate::Shell;

/// Everything the launch has learned, in the shapes the empty tab's choosers
/// want. Kept on the [`Shell`] so it can be applied to a tab that does not exist
/// yet as well as to the ones that do.
#[derive(Clone, Debug, Default)]
pub struct LauncherData {
    /// The catalog: what the disk cache held, or what the probe found.
    pub cache: ModelCache,
    /// A live server's `/registry`, once one has been seen (§9.4). `None` until a
    /// tab's own swarm has answered.
    pub registry: Option<Payload<Registry>>,
    /// The scan's rows, as `session` wants them.
    pub history: Vec<HistoryEntry>,
    /// The clock the rows' relative times are measured against.
    pub now: i64,
    /// The system's UTC offset in seconds, so the rows show local times.
    pub offset_seconds: i32,
    /// The `~` the history paths are shortened around.
    pub home: Option<String>,
    /// The scan is still walking `~/.evo/sessions`.
    pub scanning: bool,
    /// Why there is no catalog, when there is none.
    pub catalog_error: Option<String>,
    /// Why the history is empty, when the scan could not read it at all.
    pub history_error: Option<String>,
}

impl LauncherData {
    /// The state a launch starts in: the cache from disk, nothing scanned yet.
    pub fn new(cache: ModelCache) -> LauncherData {
        LauncherData {
            cache,
            registry: None,
            history: Vec::new(),
            now: time::now_epoch() as i64,
            offset_seconds: utc_offset_seconds(),
            home: home_string(),
            scanning: false,
            catalog_error: None,
            history_error: None,
        }
    }
}

/// The `~` the history paths are shortened around.
pub fn home_string() -> Option<String> {
    let home = store::paths::home_dir();
    (!home.as_os_str().is_empty()).then(|| home.to_string_lossy().into_owned())
}

/// The system's current UTC offset, in seconds east of UTC (`UTC+08:00` → 28800).
///
/// Only for display: every timestamp the app *writes* stays UTC (`store::time`).
/// The offset comes from `localtime_r`, so it follows the zone the machine is
/// actually in, including a daylight-saving change since launch.
pub fn utc_offset_seconds() -> i32 {
    #[cfg(unix)]
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut broken_down: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut broken_down).is_null() {
            return 0;
        }
        broken_down.tm_gmtoff as i32
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// The store's history entries as `session` wants them (§7.2). The two crates
/// deliberately do not know about each other, so the app is where they meet.
pub fn history_entries(entries: &[store::history::HistoryEntry]) -> Vec<HistoryEntry> {
    entries
        .iter()
        .map(|entry| HistoryEntry {
            session_path: entry.session.to_string_lossy().into_owned(),
            folder: entry.folder.to_string_lossy().into_owned(),
            when: match entry.when_epoch {
                Some(epoch) => When::Epoch(epoch as i64),
                None => When::Text(entry.when.clone()),
            },
            // The store records 0 for a journal that does not say; the list's
            // `Option` means "unknown", and a row with no count is better than a
            // row claiming zero lanes.
            lanes: (entry.lanes > 0).then_some(entry.lanes),
            coordinator_model: entry.models.coordinator.clone(),
            lanes_model: entry.models.lanes.clone(),
            source: match entry.source {
                StoreSource::Scanned => HistorySource::Scan,
                StoreSource::Recent => HistorySource::Recent,
            },
        })
        .collect()
}

/// A `/registry` body as the empty tab's setter wants it.
pub fn registry_payload(raw: Value) -> Option<Payload<Registry>> {
    let typed = serde_json::from_value::<Registry>(raw.clone()).ok()?;
    Some(Payload { typed, raw })
}

// --- pushing ---------------------------------------------------------------

/// Give one tab everything the launch has learned. Called on the observer that
/// catches a newly created tab, and for each tab when something arrives.
pub fn apply_to(tab: &mut TabContent, window: &mut Window, cx: &mut Context<TabContent>) {
    let data = cx.global::<Shell>().launcher.clone();
    if let Some(registry) = &data.registry {
        tab.set_registry(registry, window, cx);
    }
    if !data.cache.is_empty() {
        tab.set_model_cache(&data.cache, window, cx);
    }
    tab.set_catalog_error(data.catalog_error.clone(), cx);
    tab.set_scanning(data.scanning, cx);
    tab.set_history_entries(&data.history, data.now, data.offset_seconds, data.home.as_deref(), cx);
    tab.set_history_error(data.history_error.clone(), cx);
}

/// Give it to every tab that is still empty.
pub fn apply_all(cx: &mut App) {
    for_each_empty_tab(cx, |tab, window, cx| apply_to(tab, window, cx));
}

/// Run `f` for every tab that has not started a swarm, with the window they live
/// in. A tab that is already running a swarm has no empty state left to fill.
fn for_each_empty_tab(cx: &mut App, f: impl Fn(&mut TabContent, &mut Window, &mut Context<TabContent>)) {
    let Some(view) = crate::quit::view(cx) else {
        return;
    };
    let Some(window) = window_of(cx) else {
        return;
    };
    let tabs: Vec<Entity<TabContent>> = view.read(cx).tabs().to_vec();
    for tab in tabs {
        if !is_empty(&tab, cx) {
            continue;
        }
        let _ = window.update(cx, |_view, window, cx| {
            tab.update(cx, |tab, cx| f(tab, window, cx));
        });
    }
}

/// Whether a tab still has nothing chosen.
fn is_empty(tab: &Entity<TabContent>, cx: &App) -> bool {
    matches!(tab.read(cx).state(), TabState::Empty)
}

/// The window the tabs are in. The app has exactly one (§7.1).
pub fn window_of(cx: &App) -> Option<gpui_kit::AnyWindowHandle> {
    cx.windows().into_iter().next()
}

/// The window's tabs.
fn tabs_of(cx: &App) -> Vec<Entity<TabContent>> {
    match crate::quit::view(cx) {
        Some(view) => view.read(cx).tabs().to_vec(),
        None => Vec::new(),
    }
}

/// The empty tabs, for a test or a diagnostic.
pub fn empty_tab_count(cx: &App) -> usize {
    tabs_of(cx).iter().filter(|tab| is_empty(tab, cx)).count()
}

// --- what the background work reports --------------------------------------

/// The catalog is known: the disk cache at launch, or a probe's answer.
pub fn set_catalog(cx: &mut App, cache: ModelCache, error: Option<String>) {
    cx.global_mut::<Shell>().launcher.cache = cache;
    cx.global_mut::<Shell>().launcher.catalog_error = error;
    apply_all(cx);
}

/// A live server answered `/registry` (§9.4): keep it for the empty tabs and for
/// the next launch.
///
/// **Waiting on lane 1.** Nothing calls this yet: a tab's engine is the
/// workspace's, and the hook that would report its `Update::Registry` is not
/// there. Wiring it is one call.
pub fn on_live_registry(cx: &mut App, raw: Value) {
    let Some(payload) = registry_payload(raw.clone()) else {
        return;
    };
    let (root, log) = {
        let shell = cx.global::<Shell>();
        (shell.root.clone(), shell.log.clone())
    };
    // The catalog moves forward; the kernel api set only a probe can tell us is
    // kept (§9.4).
    let cache = {
        let shell = cx.global::<Shell>();
        shell.launcher.cache.with_live_registry(raw)
    };
    if let Err(error) = cache.save(&root) {
        log.error(format!("could not save {}: {error}", root.model_cache().display()));
    }
    {
        let launcher = &mut cx.global_mut::<Shell>().launcher;
        launcher.cache = cache;
        launcher.registry = Some(payload);
        launcher.catalog_error = None;
    }
    apply_all(cx);
}

/// The scan found what it found. `error` is set when the sessions directory
/// could not be read at all.
pub fn set_history(cx: &mut App, entries: Vec<store::history::HistoryEntry>, error: Option<String>) {
    let entries = history_entries(&entries);
    {
        let launcher = &mut cx.global_mut::<Shell>().launcher;
        launcher.history = entries;
        launcher.now = time::now_epoch() as i64;
        launcher.offset_seconds = utc_offset_seconds();
        launcher.home = home_string();
        launcher.scanning = false;
        launcher.history_error = error;
    }
    apply_all(cx);
}

/// The scan started (`true`) or finished (`false`).
pub fn set_scanning(cx: &mut App, scanning: bool) {
    cx.global_mut::<Shell>().launcher.scanning = scanning;
    apply_all(cx);
}

/// The catalog could not be learned: the tabs say so where the loading hint was.
pub fn set_catalog_error(cx: &mut App, message: Option<String>) {
    cx.global_mut::<Shell>().launcher.catalog_error = message;
    apply_all(cx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::history::HistoryEntry as StoreEntry;
    use store::tab::TabModels;
    use std::path::PathBuf;

    fn store_entry(source: StoreSource, when_epoch: Option<u64>, lanes: u32) -> StoreEntry {
        StoreEntry {
            session: PathBuf::from("/sessions/a.sexp"),
            folder: PathBuf::from("/coding/foo"),
            when: "2026-09-29T09:25:44Z".to_owned(),
            when_epoch,
            mtime: 1,
            session_id: "a".to_owned(),
            swarm_id: "s".to_owned(),
            workers: lanes,
            lanes,
            lane_cwds: Vec::new(),
            models: TabModels {
                coordinator: Some("coord-1".to_owned()),
                lanes: Some("lane-1".to_owned()),
            },
            source,
        }
    }

    #[test]
    fn the_store_entry_becomes_a_session_entry() {
        let entries = history_entries(&[store_entry(StoreSource::Scanned, Some(1_700_000_000), 4)]);
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.session_path, "/sessions/a.sexp");
        assert_eq!(entry.folder, "/coding/foo");
        assert_eq!(entry.when, When::Epoch(1_700_000_000));
        assert_eq!(entry.lanes, Some(4));
        assert_eq!(entry.coordinator_model.as_deref(), Some("coord-1"));
        assert_eq!(entry.lanes_model.as_deref(), Some("lane-1"));
        assert_eq!(entry.source, HistorySource::Scan);
    }

    #[test]
    fn a_recent_entry_keeps_its_source_and_its_text_when_there_is_no_epoch() {
        let entries = history_entries(&[store_entry(StoreSource::Recent, None, 2)]);
        assert_eq!(entries[0].source, HistorySource::Recent);
        assert_eq!(entries[0].when, When::Text("2026-09-29T09:25:44Z".to_owned()));
    }

    #[test]
    fn a_journal_that_does_not_say_the_lane_count_says_nothing() {
        let entries = history_entries(&[store_entry(StoreSource::Scanned, Some(1), 0)]);
        assert_eq!(entries[0].lanes, None, "0 means the journal did not record a count");
    }

    #[test]
    fn the_local_offset_is_a_plausible_zone_offset() {
        let offset = utc_offset_seconds();
        // Whole minutes, within ±14 h — and the same answer twice.
        assert_eq!(offset % 60, 0, "{offset} is not a whole minute");
        assert!(offset.abs() <= 14 * 3600, "{offset} is not a real zone offset");
        assert_eq!(offset, utc_offset_seconds());
    }

    #[test]
    fn a_registry_body_becomes_the_payload_the_tab_wants() {
        let raw = serde_json::json!({ "models": [{ "id": "m" }], "apis": ["anthropic-messages"] });
        let payload = registry_payload(raw.clone()).expect("a registry");
        assert_eq!(payload.raw, raw);
        assert_eq!(payload.typed.models.len(), 1);
        assert!(payload.typed.apis.contains(&"anthropic-messages".to_owned()));
        assert!(registry_payload(serde_json::json!("not a registry")).is_none());
    }
}

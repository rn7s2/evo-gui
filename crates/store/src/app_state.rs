//! `app.json` — everything the app remembers across launches (§6, §2 rule 1).
//!
//! Versioned, tolerant, and free of secrets: loading a missing, truncated or
//! hand-broken file yields defaults (the broken file is kept as `app.json.bak`)
//! and never stops the app from opening.

use serde::{Deserialize, Deserializer, Serialize};
use std::io;
use std::path::{Path, PathBuf};

use crate::paths::{self, Root, TabId};
use crate::tab::TabModels;

/// Bumped when the shape of `app.json` changes incompatibly.
pub const SCHEMA_VERSION: u32 = 1;

/// How many recent sessions `app.json` remembers.
pub const MAX_RECENTS: usize = 50;
/// How many tabs a stored tab set may hold (a hand-edited file cannot make the
/// strip unbounded).
pub const MAX_TABS: usize = 64;

/// Window size the app opens with, before clamping to the display work area.
pub const DEFAULT_SIZE: (f32, f32) = (1600.0, 1000.0);
/// Smallest window the app allows.
pub const MIN_SIZE: (f32, f32) = (900.0, 700.0);

/// Where the window was last seen.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(default)]
pub struct WindowBounds {
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub width: f32,
    pub height: f32,
}

impl Default for WindowBounds {
    fn default() -> Self {
        WindowBounds {
            x: None,
            y: None,
            width: DEFAULT_SIZE.0,
            height: DEFAULT_SIZE.1,
        }
    }
}

impl WindowBounds {
    /// Persisted bounds, clamped to something a window can actually be.
    fn sanitized(self) -> WindowBounds {
        let w = if self.width.is_finite() && self.width >= MIN_SIZE.0 {
            self.width
        } else {
            DEFAULT_SIZE.0
        };
        let h = if self.height.is_finite() && self.height >= MIN_SIZE.1 {
            self.height
        } else {
            DEFAULT_SIZE.1
        };
        let finite = |v: Option<f32>| v.filter(|x| x.is_finite());
        WindowBounds {
            x: finite(self.x),
            y: finite(self.y),
            width: w,
            height: h,
        }
    }
}

/// The evo binaries this app spawns.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct Binaries {
    pub evo_swarm: PathBuf,
    pub evo_agent: PathBuf,
}

/// The tab page's column widths, in points (§7.3).
///
/// One set for the app, not one per tab: every tab's page is the same columns —
/// the agent list, the conversation beside it, and the terminal when it is open —
/// and a browser's sidebar is not per-tab either. Dragging the split between them
/// changes this, and this is what `app.json` remembers — so the numbers, and how
/// far they may be dragged, live with the schema rather than with the view that
/// draws them.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(default)]
pub struct Panes {
    /// The agent list on the left.
    pub left: f32,
}

/// Where the left column opens, and how far it may be dragged (§7.3).
pub const LEFT_DEFAULT: f32 = 260.0;
pub const LEFT_MIN: f32 = 180.0;
pub const LEFT_MAX: f32 = 480.0;

/// The terminal pane on the right of a tab's page: the width it opens at the
/// first time, and how far its divider may be dragged. The width is each tab's
/// own — a terminal belongs to the tab whose folder it runs in.
pub const TERMINAL_DEFAULT: f32 = 500.0;
pub const TERMINAL_MIN: f32 = 200.0;
pub const TERMINAL_MAX: f32 = 900.0;

/// The terminal's type size: what it opens at, and what Settings accepts.
pub const TERMINAL_FONT_SIZE_MIN: f32 = 8.0;
pub const TERMINAL_FONT_SIZE_MAX: f32 = 32.0;

/// The conversation column never goes below this: the transcript is what the page
/// is for, and a page that is mostly chrome is not a page (§7.3). Lowered from the
/// design's 420 so a narrow window — or one with a terminal pane beside the
/// conversation — still has room for both columns.
pub const CENTER_MIN: f32 = 360.0;

/// The scale the transcript's text is drawn at, and the range the View menu's
/// Zoom In / Zoom Out work in (§7.2).
///
/// One app-wide number, like the pane widths: every open transcript in every tab
/// follows it, and `1.0` is the design's own size. The bounds live with the
/// schema because a stored number is only read if it is inside them.
pub const ZOOM_DEFAULT: f32 = 1.0;
pub const ZOOM_MIN: f32 = 0.75;
pub const ZOOM_MAX: f32 = 2.0;

impl Default for Panes {
    fn default() -> Panes {
        Panes { left: LEFT_DEFAULT }
    }
}

impl Panes {
    pub fn new(left: f32) -> Panes {
        Panes { left }.sanitized()
    }

    /// The same widths, dragged back into their ranges — and into something that
    /// can be drawn: a hand-edited file may say anything.
    pub fn sanitized(self) -> Panes {
        Panes {
            left: ranged(self.left, LEFT_MIN, LEFT_MAX, LEFT_DEFAULT),
        }
    }
}

/// A number from a file: finite, and inside `min..=max`, or the default.
fn ranged(value: f32, min: f32, max: f32, default: f32) -> f32 {
    if value.is_finite() && value >= min && value <= max {
        value
    } else {
        default
    }
}

impl Default for Binaries {
    fn default() -> Self {
        Binaries {
            evo_swarm: PathBuf::from("/usr/local/bin/evo-swarm"),
            evo_agent: PathBuf::from("/usr/local/bin/evo-agent"),
        }
    }
}

impl Binaries {
    fn sanitized(mut self) -> Binaries {
        if self.evo_swarm.as_os_str().is_empty() {
            self.evo_swarm = Binaries::default().evo_swarm;
        }
        if self.evo_agent.as_os_str().is_empty() {
            self.evo_agent = Binaries::default().evo_agent;
        }
        self
    }
}

/// One entry of the app's own recent list (§9.5: merged with the on-disk scan).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Recent {
    /// Coordinator session path — the `--resume` argument.
    pub session: PathBuf,
    /// The folder the swarm ran in.
    pub folder: PathBuf,
    /// RFC 3339 UTC.
    pub when: String,
    pub models: TabModels,
    /// Lane count, as the swarm was started.
    pub lanes: u32,
    /// Whether the session was a swarm's or one agent's (§7.2). Absent in a file
    /// written before a tab could start one agent: a swarm, which is what this app
    /// started then.
    pub swarm: bool,
    /// The tab was still open when the app last quit (§6, §9.5).
    ///
    /// The session scan cannot know this: a swarm that is still up has not
    /// written its journal again. The app sets it for every tab that had a
    /// session at quit, and clears it on the others, so the history list can put
    /// "open at last quit" first.
    pub open_at_quit: bool,
}

impl Default for Recent {
    fn default() -> Self {
        Recent {
            session: PathBuf::new(),
            folder: PathBuf::new(),
            when: crate::time::now_rfc3339(),
            models: TabModels::default(),
            lanes: 0,
            swarm: true,
            open_at_quit: false,
        }
    }
}

impl Recent {
    pub fn new(session: impl Into<PathBuf>, folder: impl Into<PathBuf>, lanes: u32) -> Recent {
        Recent {
            session: session.into(),
            folder: folder.into(),
            when: crate::time::now_rfc3339(),
            models: TabModels::default(),
            lanes,
            swarm: true,
            open_at_quit: false,
        }
    }

    /// The same recent, marked as open when the app last quit (§9.5).
    pub fn open_at_quit(mut self) -> Recent {
        self.open_at_quit = true;
        self
    }

    /// The folder's last path component, for a history row.
    pub fn folder_name(&self) -> String {
        crate::tab::folder_label(&self.folder)
    }
}

/// Light/dark/system.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    pub fn as_str(self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }

    pub fn parse(s: &str) -> Option<Theme> {
        match s.trim().to_ascii_lowercase().as_str() {
            "system" | "auto" => Some(Theme::System),
            "light" => Some(Theme::Light),
            "dark" => Some(Theme::Dark),
            _ => None,
        }
    }
}

fn de_theme<'de, D: Deserializer<'de>>(d: D) -> Result<Theme, D::Error> {
    // An unknown theme is not a reason to throw the file away.
    let s = String::deserialize(d)?;
    Ok(Theme::parse(&s).unwrap_or_default())
}

/// `app.json`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct AppState {
    pub version: u32,
    pub window: WindowBounds,
    /// The open tabs, in strip order.
    pub tabs: Vec<TabId>,
    /// The selected tab, always one of `tabs` (or `None` for an empty strip).
    pub selected: Option<TabId>,
    pub binaries: Binaries,
    pub recents: Vec<Recent>,
    #[serde(deserialize_with = "de_theme")]
    pub theme: Theme,
    /// The tab page's side columns, app-wide (§7.3). Absent in a file written
    /// before there were any: the defaults are what the page opens with.
    pub panes: Panes,
    /// The transcript's font zoom (§7.2): what Zoom In / Zoom Out / Actual Size
    /// leave behind. Absent in a file written before the View menu existed, which
    /// reads as the design's own size.
    pub zoom: f32,
    /// The terminal pane's font family. Absent in older files, which read as the
    /// design's own monospace (`"Menlo"`).
    #[serde(default = "default_terminal_font")]
    pub terminal_font: String,
    /// The terminal pane's type size, in points. Absent in older files, which
    /// read as the design's monospace size.
    pub terminal_font_size: f32,
}

/// The font a terminal pane draws with when `app.json` names none (§13): the
/// design's own monospace.
fn default_terminal_font() -> String {
    crate::design::MONO_FONT.to_string()
}

impl Default for AppState {
    fn default() -> Self {
        AppState {
            version: SCHEMA_VERSION,
            window: WindowBounds::default(),
            tabs: Vec::new(),
            selected: None,
            binaries: Binaries::default(),
            recents: Vec::new(),
            theme: Theme::System,
            panes: Panes::default(),
            zoom: ZOOM_DEFAULT,
            terminal_font: default_terminal_font(),
            terminal_font_size: crate::design::FONT_MONO,
        }
    }
}

impl AppState {
    /// Load `app.json`, tolerating anything: a missing file, a truncated one, a
    /// hand-edited one with an unknown field or a tab id shaped like a path.
    pub fn load(root: &Root) -> AppState {
        let path = root.app_json();
        match paths::read_json::<AppState>(&path) {
            Ok(state) => state.sanitized(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => AppState::default(),
            Err(_) => {
                // Keep what we could not read beside it rather than deleting it.
                let _ = paths::quarantine(&path);
                AppState::default()
            }
        }
    }

    /// Persist, keeping the previous contents as `app.json.bak`.
    pub fn save(&self, root: &Root) -> io::Result<()> {
        root.ensure()?;
        let path = root.app_json();
        paths::backup(&path)?;
        paths::write_json(&path, self)
    }

    /// Drop everything that would be unsafe or nonsensical to act on.
    fn sanitized(mut self) -> AppState {
        self.version = SCHEMA_VERSION;
        self.window = self.window.sanitized();
        self.binaries = self.binaries.sanitized();
        self.panes = self.panes.sanitized();
        self.zoom = ranged(self.zoom, ZOOM_MIN, ZOOM_MAX, ZOOM_DEFAULT);
        self.terminal_font_size = ranged(
            self.terminal_font_size,
            TERMINAL_FONT_SIZE_MIN,
            TERMINAL_FONT_SIZE_MAX,
            crate::design::FONT_MONO,
        );

        // A stored id is only kept if it is still safe as a single path segment.
        let mut seen = std::collections::HashSet::new();
        self.tabs = self
            .tabs
            .drain(..)
            .filter_map(|id| TabId::parse(id.as_str()))
            .filter(|id| seen.insert(id.clone()))
            .collect();
        self.tabs.truncate(MAX_TABS);
        if self
            .selected
            .as_ref()
            .is_some_and(|s| !self.tabs.contains(s))
        {
            self.selected = None;
        }

        let mut seen = std::collections::HashSet::new();
        self.recents = self
            .recents
            .drain(..)
            .filter(|r| !r.session.as_os_str().is_empty() && seen.insert(r.session.clone()))
            .collect();
        self.recents.truncate(MAX_RECENTS);
        self
    }

    /// Append a tab at the end of the strip and select it.
    pub fn add_tab(&mut self, id: TabId) {
        if self.tabs.len() < MAX_TABS && !self.tabs.contains(&id) {
            self.tabs.push(id.clone());
        }
        self.selected = Some(id);
    }

    /// Remove a tab. The selection moves to the neighbour that takes its place
    /// (the next tab, else the previous one). Returns the new selection.
    pub fn close_tab(&mut self, id: &TabId) -> Option<TabId> {
        let Some(pos) = self.tabs.iter().position(|t| t == id) else {
            return self.selected.clone();
        };
        let was_selected = self.selected.as_ref() == Some(id);
        self.tabs.remove(pos);
        if was_selected {
            self.selected = self
                .tabs
                .get(pos)
                .or_else(|| self.tabs.get(pos.saturating_sub(1)))
                .cloned();
        }
        if self
            .selected
            .as_ref()
            .is_some_and(|s| !self.tabs.contains(s))
        {
            self.selected = None;
        }
        self.selected.clone()
    }

    pub fn select(&mut self, id: &TabId) {
        if self.tabs.contains(id) {
            self.selected = Some(id.clone());
        }
    }

    /// Remember a session as most recent: deduped by session path, newest first.
    pub fn touch_recent(&mut self, recent: Recent) {
        if recent.session.as_os_str().is_empty() {
            return;
        }
        self.recents.retain(|r| r.session != recent.session);
        self.recents.insert(0, recent);
        self.recents.truncate(MAX_RECENTS);
    }

    pub fn recent_for(&self, session: &Path) -> Option<&Recent> {
        self.recents.iter().find(|r| r.session == session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(name: &str) -> Root {
        let dir = std::env::temp_dir().join(format!("store-app-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        Root::at(dir)
    }

    fn sample() -> AppState {
        let mut s = AppState::default();
        s.add_tab(TabId::new());
        s.add_tab(TabId::new());
        s.window = WindowBounds {
            x: Some(10.0),
            y: Some(20.0),
            width: 1440.0,
            height: 900.0,
        };
        s.theme = Theme::Dark;
        s.touch_recent(Recent::new("/s/1.sexp", "/Users/x/foo", 4));
        s
    }

    #[test]
    fn round_trips_through_disk() {
        let root = temp_root("roundtrip");
        let state = sample();
        state.save(&root).unwrap();
        assert_eq!(AppState::load(&root), state);
        // And a second save keeps a backup of the first.
        let mut next = state.clone();
        next.theme = Theme::Light;
        next.save(&root).unwrap();
        let bak: AppState = paths::read_json(&paths::backup_path(&root.app_json())).unwrap();
        assert_eq!(bak, state);
        fs::remove_dir_all(root.path()).unwrap();
    }

    #[test]
    fn missing_file_is_defaults() {
        let root = temp_root("missing");
        assert_eq!(AppState::load(&root), AppState::default());
        assert_eq!(AppState::default().window.width, 1600.0);
        assert_eq!(
            AppState::default().binaries.evo_swarm,
            PathBuf::from("/usr/local/bin/evo-swarm")
        );
        assert_eq!(
            AppState::default().binaries.evo_agent,
            PathBuf::from("/usr/local/bin/evo-agent")
        );
        let _ = fs::remove_dir_all(root.path());
    }

    #[test]
    fn corrupt_file_is_kept_as_bak_and_defaulted() {
        let root = temp_root("corrupt");
        root.ensure().unwrap();
        fs::write(root.app_json(), "{\"version\": 1, \"tabs\": [").unwrap();
        assert_eq!(AppState::load(&root), AppState::default());
        assert!(paths::backup_path(&root.app_json()).exists());
        fs::remove_dir_all(root.path()).unwrap();
    }

    #[test]
    fn partial_and_unknown_fields_are_tolerated() {
        let root = temp_root("partial");
        root.ensure().unwrap();
        fs::write(
            root.app_json(),
            r#"{"version":1,"tabs":["a-b_c"],"selected":"a-b_c","theme":"neon","nonsense":42,
                "recents":[{"session":"/s/1.sexp","folder":"/f","when":"2026-01-01T00:00:00Z","lanes":2}]}"#,
        )
        .unwrap();
        let state = AppState::load(&root);
        assert_eq!(state.tabs, vec![TabId::parse("a-b_c").unwrap()]);
        assert_eq!(state.selected, Some(TabId::parse("a-b_c").unwrap()));
        assert_eq!(state.theme, Theme::System); // unknown theme → default
        assert_eq!(state.recents.len(), 1);
        assert_eq!(state.recents[0].lanes, 2);
        assert_eq!(state.binaries, Binaries::default()); // absent → defaults
        assert_eq!(state.window.width, 1600.0);
        fs::remove_dir_all(root.path()).unwrap();
    }

    #[test]
    fn hostile_tab_ids_and_bad_bounds_are_dropped() {
        let root = temp_root("hostile");
        root.ensure().unwrap();
        fs::write(
            root.app_json(),
            r#"{"tabs":["../../etc","good-id","good-id"],"selected":"../../etc",
                "window":{"x":null,"y":null,"width":-5,"height":0}}"#,
        )
        .unwrap();
        let state = AppState::load(&root);
        assert_eq!(state.tabs, vec![TabId::parse("good-id").unwrap()]);
        assert_eq!(state.selected, None); // selection that is not a tab
        assert_eq!(state.window.width, DEFAULT_SIZE.0);
        assert_eq!(state.window.height, DEFAULT_SIZE.1);
        fs::remove_dir_all(root.path()).unwrap();
    }

    /// §7.2: the New Swarm page's own switch is **not** this file's — it is one
    /// page's, for one launch — and a file that still carries one from when it was
    /// written here opens fine: the field is not read, and the file's own facts are.
    #[test]
    fn a_file_that_still_carries_the_swarm_switch_opens() {
        let root = temp_root("use-swarm");
        root.ensure().unwrap();
        fs::write(
            root.app_json(),
            r#"{"version":1,"theme":"dark","use_swarm":false,"zoom":1.25}"#,
        )
        .unwrap();
        let state = AppState::load(&root);
        assert_eq!(state.theme, Theme::Dark, "the file's own facts are read");
        assert_eq!(state.zoom, 1.25);
        let before = fs::read_to_string(root.app_json()).unwrap();
        assert!(
            before.contains("use_swarm"),
            "the field is still in the file the test wrote"
        );

        // And saving over it drops it: what the app writes is what the app reads.
        state.save(&root).unwrap();
        let after = fs::read_to_string(root.app_json()).unwrap();
        assert!(
            !after.contains("use_swarm"),
            "a switch that is nobody's is not written down again: {after}"
        );
        assert_eq!(AppState::load(&root), state);
        fs::remove_dir_all(root.path()).unwrap();
    }

    /// §7.3: the pane widths are remembered, and the file is allowed to be wrong
    /// about them: a width outside its range, or one that is not a number at all,
    /// opens at the default instead.
    #[test]
    fn the_pane_width_is_remembered_and_dragged_back_into_range() {
        let root = temp_root("panes");
        let mut state = sample();
        state.panes = Panes::new(300.0);
        state.save(&root).unwrap();
        assert_eq!(AppState::load(&root).panes, Panes { left: 300.0 });

        // A file from before there was one, a hand-edited one, and one an earlier
        // build wrote with a terminal width in it — that width is simply not read:
        // a terminal's width is its tab's own.
        root.ensure().unwrap();
        fs::write(root.app_json(), r#"{"version":1}"#).unwrap();
        assert_eq!(AppState::load(&root).panes, Panes::default());
        fs::write(root.app_json(), r#"{"version":1,"panes":{"left":9999}}"#).unwrap();
        assert_eq!(AppState::load(&root).panes, Panes::default());
        fs::write(
            root.app_json(),
            r#"{"version":1,"panes":{"left":200,"right":640.5}}"#,
        )
        .unwrap();
        assert_eq!(AppState::load(&root).panes, Panes { left: 200.0 });
        fs::remove_dir_all(root.path()).unwrap();
    }

    /// §7.2: the transcript's zoom is remembered, and a file this app did not write
    /// opens at the design's own size.
    #[test]
    fn the_transcript_zoom_is_remembered_and_read_back_into_range() {
        let root = temp_root("zoom");
        let mut state = sample();
        state.zoom = 1.4;
        state.save(&root).unwrap();
        assert_eq!(AppState::load(&root).zoom, 1.4);

        // A file from before the View menu existed, one whose number is outside the
        // range, and one that is not a number at all: each opens at the default.
        for body in [
            r#"{"version":1}"#,
            r#"{"version":1,"zoom":6.0}"#,
            r#"{"version":1,"zoom":0.1}"#,
            r#"{"version":1,"zoom":"big"}"#,
        ] {
            root.ensure().unwrap();
            fs::write(root.app_json(), body).unwrap();
            assert_eq!(
                AppState::load(&root).zoom,
                ZOOM_DEFAULT,
                "{body} opens at the design's own size"
            );
        }
        assert_eq!(ZOOM_DEFAULT, 1.0);
        const { assert!(ZOOM_MIN < ZOOM_DEFAULT && ZOOM_DEFAULT < ZOOM_MAX) };
        fs::remove_dir_all(root.path()).unwrap();
    }

    /// §13: the terminal's font is remembered, and a file that does not name one —
    /// from before the setting existed, or broken in the way that makes the app fall
    /// back to its defaults — opens on the design's own monospace.
    #[test]
    fn the_terminal_font_is_remembered_and_reads_as_the_designs_monospace() {
        let root = temp_root("terminal-font");
        let mut state = sample();
        state.terminal_font = "JetBrains Mono".to_owned();
        state.save(&root).unwrap();
        assert_eq!(AppState::load(&root).terminal_font, "JetBrains Mono");

        for body in [r#"{"version":1}"#, r#"{"version":1,"terminal_font":42}"#] {
            root.ensure().unwrap();
            fs::write(root.app_json(), body).unwrap();
            assert_eq!(
                AppState::load(&root).terminal_font,
                crate::design::MONO_FONT,
                "{body} opens on the design's own monospace"
            );
        }
        assert_eq!(AppState::default().terminal_font, "Menlo");

        // The size: kept when it is one Settings would accept, the design's own
        // otherwise.
        let mut state = sample();
        state.terminal_font_size = 15.0;
        state.save(&root).unwrap();
        assert_eq!(AppState::load(&root).terminal_font_size, 15.0);
        for body in [
            r#"{"version":1}"#,
            r#"{"version":1,"terminal_font_size":2}"#,
            r#"{"version":1,"terminal_font_size":400}"#,
        ] {
            fs::write(root.app_json(), body).unwrap();
            assert_eq!(
                AppState::load(&root).terminal_font_size,
                crate::design::FONT_MONO,
                "{body}"
            );
        }
        fs::remove_dir_all(root.path()).unwrap();
    }

    /// The page as geometry: every range is a range, dragging has room to move,
    /// and the *widest* page — the column at its maximum, and the conversation at
    /// its minimum — still fits the smallest window the app opens (§7.1, §7.3).
    ///
    /// Assertions over constants, so they are made where a build can make them:
    /// a range that stopped holding would be a compile error rather than a test
    /// run nobody was watching.
    #[test]
    fn the_pane_ranges_hold_together() {
        const _: () = assert!(LEFT_MIN < LEFT_DEFAULT && LEFT_DEFAULT < LEFT_MAX);
        const _: () = assert!(
            TERMINAL_MIN < TERMINAL_DEFAULT && TERMINAL_DEFAULT < TERMINAL_MAX,
            "the terminal opens inside its own range"
        );
        const _: () = assert!(
            TERMINAL_FONT_SIZE_MIN < crate::design::FONT_MONO
                && crate::design::FONT_MONO < TERMINAL_FONT_SIZE_MAX
        );
        const _: () = assert!(
            LEFT_MAX + CENTER_MIN <= MIN_SIZE.0,
            "the widest page fits the smallest window the app opens: the terminal is collapsed, \
             so it is not part of the smallest page"
        );
    }

    #[test]
    fn tab_bookkeeping() {
        let a = TabId::new();
        let b = TabId::new();
        let c = TabId::new();
        let mut s = AppState::default();
        s.add_tab(a.clone());
        s.add_tab(b.clone());
        s.add_tab(c.clone());
        assert_eq!(s.selected, Some(c.clone()));
        assert_eq!(s.close_tab(&c), Some(b.clone()));
        assert_eq!(s.close_tab(&b), Some(a.clone()));
        s.select(&b); // not a tab: ignored
        assert_eq!(s.selected, Some(a.clone()));
        assert_eq!(s.close_tab(&a), None);
        assert!(s.tabs.is_empty());
    }

    #[test]
    fn recents_are_deduped_newest_first_and_capped() {
        let mut s = AppState::default();
        s.touch_recent(Recent::new("/s/1.sexp", "/f1", 2));
        s.touch_recent(Recent::new("/s/2.sexp", "/f2", 4));
        s.touch_recent(Recent::new("/s/1.sexp", "/f1", 6));
        assert_eq!(s.recents.len(), 2);
        assert_eq!(s.recents[0].session, PathBuf::from("/s/1.sexp"));
        assert_eq!(s.recents[0].lanes, 6);
        assert_eq!(s.recent_for(Path::new("/s/2.sexp")).unwrap().lanes, 4);
        for i in 0..MAX_RECENTS + 10 {
            s.touch_recent(Recent::new(format!("/s/{i}.sexp"), "/f", 1));
        }
        assert_eq!(s.recents.len(), MAX_RECENTS);
        // An empty session path is not a recent.
        let before = s.recents.len();
        s.touch_recent(Recent::new("", "/f", 1));
        assert_eq!(s.recents.len(), before);
    }

    /// §2 rule 4: `app.json` never carries a secret. No field is named like
    /// one, and no value looks like a bearer token.
    #[test]
    fn app_json_carries_no_secrets() {
        let json = serde_json::to_string_pretty(&sample()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        fn walk(v: &serde_json::Value, path: &str) {
            match v {
                serde_json::Value::Object(map) => {
                    for (k, val) in map {
                        let lower = k.to_ascii_lowercase();
                        for word in [
                            "token",
                            "secret",
                            "password",
                            "api_key",
                            "apikey",
                            "bearer",
                            "credential",
                        ] {
                            assert!(!lower.contains(word), "secret-looking key {path}.{k}");
                        }
                        walk(val, &format!("{path}.{k}"));
                    }
                }
                serde_json::Value::Array(items) => {
                    for (i, item) in items.iter().enumerate() {
                        walk(item, &format!("{path}[{i}]"));
                    }
                }
                serde_json::Value::String(s) => {
                    assert!(!path.to_ascii_lowercase().contains("token"), "{path} = {s}");
                }
                _ => {}
            }
        }
        walk(&value, "$");
    }
}

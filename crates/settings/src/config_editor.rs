//! The raw-config file editor: one page over evo's own four config documents.
//!
//! The app's second Settings surface (§13). [`crate::SettingsPanel`] edits the two
//! binaries and the theme; this page shows the user's own evo configuration as text,
//! for a global scope (`~/.evo/`) or a project's (`<folder>/.evo/`): `init.lisp`,
//! `swarm.lisp`, `memory.sexp`, `lore.sexp`.
//!
//! The four documents are file tabs, each keeping its own [`EditorState`], so a draft
//! survives switching tabs and the page being hidden. Everything that touches a file
//! — read, SBCL check, write — runs on the background executor.
//!
//! Two counters keep answers honest, and they mean different things:
//!
//! * **`op`** — one operation over a buffer: a load or a save. It is bumped when an
//!   operation starts, and an answer carrying an older `op` is dropped whole, so a
//!   superseded read never rewrites facts a newer load or save has landed.
//! * **`revision`** — one text. It is bumped by every edit, so a read that lands
//!   after the person has typed keeps the file's facts (baseline, base) but does not
//!   replace the draft.
//!
//! A save is exclusive: while one is in flight the page is read-only, Reload and
//! Discard are refused, the status says `Saving…`, and [`ConfigEditor::is_saving`]
//! tells the host to wait rather than offer a discard the pending write would undo.
//!
//! ```text
//! read(path)             → Some(text) | None (not there) | Err (cannot be edited)
//! save(path, text, base) → SBCL check + conflict check + atomic write
//! ```

use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, InputEvent};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, Disableable as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, px, relative, App, Context, Entity, Focusable as _, Hsla, IntoElement, KeyDownEvent,
    Render, Rgba, SharedString, Subscription, TestSupportExt as _, WeakEntity, Window,
};
use store::config_file::{self, ConfigError};
use store::design;
use store::{ConfigFile, ConfigScope};

/// The page's own element id, and the ids of everything in it, so the app's tests
/// can find a control rather than a position.
pub const CONFIG_EDITOR_ID: &str = "config-editor";
pub const CONFIG_TABS_ID: &str = "config-file-tabs";
pub const CONFIG_PATH_ID: &str = "config-path";
pub const CONFIG_STATUS_ID: &str = "config-status";
pub const CONFIG_REMINDER_ID: &str = "config-reminder";
pub const CONFIG_SAVE_ID: &str = "config-save";
pub const CONFIG_RELOAD_ID: &str = "config-reload";
pub const CONFIG_DISCARD_ID: &str = "config-discard";
/// The surface the editor is drawn on.
pub const CONFIG_SURFACE_ID: &str = "config-surface";

/// Always on the page: a save here is picked up by the next launch, not by the
/// session running now.
pub const REMINDER_TEXT: &str = "Changes take effect after restarting Evo Desktop.";

const SMALL_TEXT: gpui_kit::Pixels = px(12.);
const TAB_PAD: gpui_kit::Pixels = px(8.);
const PATH_CHARS: usize = 72;

/// The slot a document's state lives in, in [`ConfigFile::ALL`] order.
fn slot(file: ConfigFile) -> usize {
    match file {
        ConfigFile::Init => 0,
        ConfigFile::Swarm => 1,
        ConfigFile::Memory => 2,
        ConfigFile::Lore => 3,
    }
}

/// One document's editor state inside the page.
struct Buffer {
    /// The editor the person types in; retained, so its draft is kept while another
    /// tab is shown.
    editor: Entity<EditorState>,
    /// The resolved path, fixed for the life of the page.
    path: PathBuf,
    /// What [`config_file::read`] found: `Some(text)` when the file was there,
    /// `None` when it was not. This is the `base` a save passes.
    base: Option<String>,
    /// The text the editor is measured against: what was last read or written.
    baseline: String,
    /// Why the file could not be read (a symlink, a directory, bad bytes). While
    /// set, Save stays refused.
    read_error: Option<String>,
    /// The generation of the load or save that owns this buffer (see the module
    /// docs).
    op: u64,
    /// The generation of the text.
    revision: u64,
    status: Status,
}

impl Buffer {
    fn is_dirty(&self, cx: &App) -> bool {
        self.editor.read(cx).value() != self.baseline
    }

    /// A file that could not be read is never written over.
    fn writable(&self) -> bool {
        self.read_error.is_none()
    }
}

/// What a document is doing, as the status line shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Status {
    /// The first read is in flight.
    Loading,
    /// Read (or edited back to nothing to say), and nothing is wrong.
    Ready,
    /// A check and a write are in flight.
    Saving,
    /// The file was written: the confirmation stays until the next edit, select or
    /// reload.
    Saved,
    /// The file cannot be edited at all — a symlink, a directory, bytes that are
    /// not UTF-8. Read errors, shown as `Cannot be edited: …`.
    Unreadable(String),
    /// A save was refused — it did not read, there is no SBCL, or the file changed
    /// on disk. Shown as `Not saved: …`.
    Refused(String),
}

/// The one page that edits evo's raw config files (§13).
pub struct ConfigEditor {
    buffers: [Buffer; 4],
    selected: ConfigFile,
    /// The document whose save is in flight, while one is.
    saving: Option<ConfigFile>,
    /// Kept for as long as the page lives: dropping them unsubscribes.
    _subscriptions: Vec<Subscription>,
}

impl ConfigEditor {
    /// A page over `scope`'s four documents, each already being read.
    pub fn new(scope: ConfigScope, window: &mut Window, cx: &mut Context<Self>) -> ConfigEditor {
        let mut subscriptions = Vec::new();
        let mut buffers: Vec<Buffer> = Vec::with_capacity(ConfigFile::ALL.len());
        for file in ConfigFile::ALL {
            let editor = cx.new(|cx| {
                EditorState::new(window, cx)
                    // No line-number gutter: the kit paints it with the *syntax*
                    // theme's background, which evo does not set, so under the dark
                    // theme it is a white strip down the editor's left edge.
                    .line_number(false)
                    .placeholder(SharedString::from("write it here"))
            });
            subscriptions.push(cx.subscribe(
                &editor,
                move |this: &mut ConfigEditor, _state, event: &InputEvent, cx| {
                    // `set_value` does not emit this, so a programmatic load does
                    // not count as an edit; only the person's typing does.
                    if matches!(event, InputEvent::Change) {
                        this.on_edit(file, cx);
                    }
                },
            ));
            buffers.push(Buffer {
                editor,
                path: config_file::path(&scope, file),
                base: None,
                baseline: String::new(),
                read_error: None,
                op: 0,
                revision: 0,
                status: Status::Loading,
            });
        }
        let buffers: [Buffer; 4] = match buffers.try_into() {
            Ok(buffers) => buffers,
            Err(_) => unreachable!("one buffer per document"),
        };
        let mut page = ConfigEditor {
            buffers,
            selected: ConfigFile::Init,
            saving: None,
            _subscriptions: subscriptions,
        };
        for file in ConfigFile::ALL {
            page.load(file, window, cx);
        }
        page
    }

    /// Which document is being shown.
    pub fn selected(&self) -> ConfigFile {
        self.selected
    }

    /// Whether a save is in flight. A host must not close or discard while this is
    /// true: the write would land after the answer it gave.
    pub fn is_saving(&self) -> bool {
        self.saving.is_some()
    }

    /// The document whose save is in flight, while one is: what a host shows as
    /// "saving" so it names the file being written rather than the one on screen.
    pub fn saving_file(&self) -> Option<ConfigFile> {
        self.saving
    }

    /// Show a document, and put the keyboard in its editor. Switching is safe at
    /// any time: no draft is touched. It does dismiss a `Saved.` confirmation,
    /// which belongs to the moment it was written.
    pub fn select(&mut self, file: ConfigFile, window: &mut Window, cx: &mut Context<Self>) {
        for buffer in &mut self.buffers {
            if matches!(buffer.status, Status::Saved) {
                buffer.status = Status::Ready;
            }
        }
        if self.selected != file {
            self.selected = file;
            cx.notify();
        }
        self.focus_into(window, cx);
    }

    /// Where the keyboard goes when the page is shown or a tab is switched.
    pub fn focus_into(&self, window: &mut Window, cx: &mut App) {
        let handle = self.buffers[slot(self.selected)]
            .editor
            .read(cx)
            .focus_handle(cx);
        window.focus(&handle, cx);
    }

    /// Whether any document has unsaved edits.
    pub fn is_dirty(&self, cx: &App) -> bool {
        self.buffers.iter().any(|buffer| buffer.is_dirty(cx))
    }

    /// The documents with unsaved edits, in tab order.
    pub fn dirty_files(&self, cx: &App) -> Vec<ConfigFile> {
        ConfigFile::ALL
            .into_iter()
            .filter(|file| self.buffers[slot(*file)].is_dirty(cx))
            .collect()
    }

    /// Whether Save would do anything for the document being shown.
    pub fn can_save(&self, cx: &App) -> bool {
        let buffer = &self.buffers[slot(self.selected)];
        self.saving.is_none()
            && !matches!(buffer.status, Status::Loading | Status::Saving)
            && buffer.writable()
            && buffer.is_dirty(cx)
    }

    /// Throw every draft away: each editor goes back to its baseline. Refused while
    /// a save is in flight, because the pending write would land after this.
    ///
    /// A file that could not be read is reverted too — its draft is thrown away
    /// like any other — but the reason it cannot be edited stays on the status
    /// line.
    pub fn discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving.is_some() {
            return;
        }
        for ix in 0..self.buffers.len() {
            // A buffer still being read has no draft to throw away, and the read
            // owns its facts.
            if matches!(self.buffers[ix].status, Status::Loading) {
                continue;
            }
            self.buffers[ix].op += 1;
            self.buffers[ix].revision += 1;
            if self.buffers[ix].writable() {
                self.buffers[ix].status = Status::Ready;
            }
            let text = self.buffers[ix].baseline.clone();
            let editor = self.buffers[ix].editor.clone();
            editor.update(cx, |state, cx| state.set_value(text, window, cx));
        }
        self.after_change(cx);
    }

    /// Check and write the document being shown, on the background executor.
    ///
    /// Nothing is written when the text does not read, when there is no SBCL to
    /// check it with, or when the file changed on disk.
    pub fn save_selected(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let file = self.selected;
        let ix = slot(file);
        if self.saving.is_some() || !self.can_save(cx) {
            return;
        }
        let text = self.buffers[ix].editor.read(cx).value().to_string();
        let base = self.buffers[ix].base.clone();
        let path = self.buffers[ix].path.clone();
        self.buffers[ix].op += 1;
        let op = self.buffers[ix].op;
        self.buffers[ix].status = Status::Saving;
        self.saving = Some(file);
        cx.notify();

        cx.spawn(async move |this: WeakEntity<Self>, cx| {
            // The write takes its own copy: `text` is moved on into the result,
            // where it becomes the new baseline.
            let writing = text.clone();
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    config_file::save(&path, &writing, base.as_deref())
                        .map_err(|error| error.to_string())
                })
                .await;
            let _ = this.update(cx, |page, cx| page.apply_save(file, op, text, outcome, cx));
        })
        .detach();
    }

    /// Read the document being shown again from disk. Refused while a save is in
    /// flight, and refused while the document has a draft: either would throw an
    /// edit away without asking.
    pub fn reload_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving.is_some() {
            return;
        }
        let file = self.selected;
        if self.buffers[slot(file)].is_dirty(cx) {
            return;
        }
        self.load(file, window, cx);
    }

    // --- the background work ---------------------------------------------

    /// Start a read of `file`. The result lands through [`Self::apply_read`].
    fn load(&mut self, file: ConfigFile, window: &mut Window, cx: &mut Context<Self>) {
        let ix = slot(file);
        self.buffers[ix].op += 1;
        self.buffers[ix].revision += 1;
        let op = self.buffers[ix].op;
        let revision = self.buffers[ix].revision;
        self.buffers[ix].status = Status::Loading;
        cx.notify();

        let path = self.buffers[ix].path.clone();
        cx.spawn_in(window, async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { config_file::read(&path) })
                .await;
            let _ = this.update_in(cx, |page, window, cx| {
                page.apply_read(file, op, revision, outcome, window, cx)
            });
        })
        .detach();
    }

    /// A read came back.
    ///
    /// An older operation's answer is dropped whole. Within the operation, the
    /// file's facts are taken — its text is what the draft is measured against,
    /// and it is the `base` a save needs — but the editor's text is replaced only
    /// when nothing was typed since the read started.
    fn apply_read(
        &mut self,
        file: ConfigFile,
        op: u64,
        revision: u64,
        outcome: Result<Option<String>, ConfigError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ix = slot(file);
        if self.buffers[ix].op != op {
            return;
        }
        let replace_text = self.buffers[ix].revision == revision;
        let text = match outcome {
            Ok(Some(text)) => {
                self.buffers[ix].base = Some(text.clone());
                self.buffers[ix].baseline = text.clone();
                self.buffers[ix].read_error = None;
                self.buffers[ix].status = Status::Ready;
                text
            }
            Ok(None) => {
                self.buffers[ix].base = None;
                self.buffers[ix].baseline = String::new();
                self.buffers[ix].read_error = None;
                self.buffers[ix].status = Status::Ready;
                String::new()
            }
            Err(error) => {
                self.buffers[ix].base = None;
                self.buffers[ix].baseline = String::new();
                self.buffers[ix].read_error = Some(error.to_string());
                self.buffers[ix].status = Status::Unreadable(error.to_string());
                String::new()
            }
        };
        if replace_text {
            let editor = self.buffers[ix].editor.clone();
            editor.update(cx, |state, cx| state.set_value(text, window, cx));
        }
        self.after_change(cx);
    }

    /// A save came back. The file on disk is `text` now, so that is what the draft
    /// measures against and the `base` the next save passes.
    fn apply_save(
        &mut self,
        file: ConfigFile,
        op: u64,
        text: String,
        outcome: Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        if self.saving == Some(file) {
            self.saving = None;
        }
        let ix = slot(file);
        if self.buffers[ix].op != op {
            self.after_change(cx);
            return;
        }
        match outcome {
            Ok(()) => {
                let buffer = &mut self.buffers[ix];
                buffer.base = Some(text.clone());
                buffer.baseline = text;
                buffer.read_error = None;
                // The confirmation a person gets that the write landed.
                buffer.status = Status::Saved;
            }
            Err(message) => self.buffers[ix].status = Status::Refused(message),
        }
        self.after_change(cx);
    }

    /// A keystroke in one of the editors.
    fn on_edit(&mut self, file: ConfigFile, cx: &mut Context<Self>) {
        let ix = slot(file);
        self.buffers[ix].revision += 1;
        // A refusal describes text that is gone now, and a save confirmation
        // belongs to text that is gone too — unless the file could not be read,
        // which typing cannot fix.
        if self.buffers[ix].writable()
            && matches!(self.buffers[ix].status, Status::Refused(_) | Status::Saved)
        {
            self.buffers[ix].status = Status::Ready;
        }
        self.after_change(cx);
    }

    /// Repaint.
    fn after_change(&mut self, cx: &mut Context<Self>) {
        cx.notify();
    }

    fn file_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut row = h_flex()
            .id(CONFIG_TABS_ID)
            .test_support()
            .flex_none()
            .items_center()
            .gap(px(2.));
        for file in ConfigFile::ALL {
            let selected = file == self.selected;
            let label = tab_label(file, self.buffers[slot(file)].is_dirty(cx));
            let a11y = label.clone();
            row = row.child(
                div()
                    .id(tab_id(file))
                    .test_support()
                    .px(TAB_PAD)
                    .py(px(3.))
                    .rounded(cx.theme().radius)
                    .text_size(px(12.))
                    .cursor_pointer()
                    .aria_label(a11y)
                    .when(selected, |tab| {
                        tab.bg(cx.theme().muted).text_color(cx.theme().foreground)
                    })
                    .when(!selected, |tab| {
                        tab.text_color(cx.theme().muted_foreground)
                            .hover(|tab| tab.text_color(cx.theme().foreground))
                    })
                    .on_click(cx.listener(move |page, _, window, cx| page.select(file, window, cx)))
                    .child(label),
            );
        }
        row
    }

    fn path_line(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let path = self.buffers[slot(self.selected)].path.display().to_string();
        let shown = shorten(&path, PATH_CHARS);
        div()
            .id(CONFIG_PATH_ID)
            .test_support()
            .flex_none()
            .min_w_0()
            .truncate()
            .text_size(SMALL_TEXT)
            .text_color(cx.theme().muted_foreground)
            .aria_label(path)
            .child(SharedString::from(shown))
    }

    fn status_line(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let buffer = &self.buffers[slot(self.selected)];
        let (text, danger) = match &buffer.status {
            Status::Loading => ("Reading…".to_owned(), false),
            Status::Saving => ("Saving…".to_owned(), false),
            Status::Saved => ("Saved.".to_owned(), false),
            Status::Ready => (String::new(), false),
            Status::Unreadable(message) => (format!("Cannot be edited: {message}"), true),
            Status::Refused(message) => (format!("Not saved: {message}"), true),
        };
        let color = if danger {
            cx.theme().danger
        } else {
            cx.theme().muted_foreground
        };
        div()
            .id(CONFIG_STATUS_ID)
            .test_support()
            .flex_none()
            .min_w_0()
            .truncate()
            .text_size(SMALL_TEXT)
            .text_color(color)
            .aria_label(text.clone())
            .child(SharedString::from(text))
    }

    /// The keyboard's one extra key: ⌘S saves the document being shown.
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.key.eq_ignore_ascii_case("s") && keystroke.modifiers.platform {
            cx.stop_propagation();
            self.save_selected(window, cx);
        }
    }
}

impl Render for ConfigEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let editor = self.buffers[slot(self.selected)].editor.clone();
        let saving = self.saving.is_some();
        let dirty = self.is_dirty(cx);
        let palette = design::palette(cx.theme().mode.is_dark());
        // Neither of these may throw a draft away without being asked: Reload is
        // the one that would, so it waits for a clean buffer, and Discard is the
        // explicit answer that does it.
        let reload_enabled = !saving && !self.buffers[slot(self.selected)].is_dirty(cx);
        let discard_enabled = !saving && dirty;
        let save_enabled = self.can_save(cx);
        v_flex()
            .id(CONFIG_EDITOR_ID)
            .test_support()
            .size_full()
            .gap_2()
            // The page's own inset (§7.3): the tabs, the path, the editor and the
            // foot keep the design's margin instead of sitting on the host's edge,
            // where the trailing Save would be clipped by the window.
            .p(px(design::INSET))
            .text_color(cx.theme().foreground)
            .on_key_down(cx.listener(Self::key_down))
            .child(self.file_tabs(cx))
            .child(self.path_line(cx))
            .child(
                div()
                    .id(CONFIG_SURFACE_ID)
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(ink(palette.border))
                    // The design's `--input`, the surface a person types on. The kit's
                    // code editor otherwise paints the *syntax* theme's background, and
                    // the kit's own highlight theme is light: under the dark theme that
                    // is a white surface with pale ink on it. `appearance(false)` below
                    // silences the kit's fill, and this is what it is drawn on instead.
                    .bg(ink(palette.input))
                    // Read-only while a save is in flight, so the text cannot move
                    // under the write.
                    .child(
                        Editor::new(&editor)
                            .appearance(false)
                            .bordered(false)
                            .readonly(saving)
                            .h(relative(1.)),
                    ),
            )
            .child(self.status_line(cx))
            .child(
                div()
                    .id(CONFIG_REMINDER_ID)
                    .test_support()
                    .flex_none()
                    .text_size(SMALL_TEXT)
                    .text_color(cx.theme().muted_foreground)
                    .aria_label(REMINDER_TEXT)
                    .child(REMINDER_TEXT),
            )
            .child(
                h_flex()
                    .flex_none()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new(CONFIG_DISCARD_ID)
                                    .label("Discard")
                                    .ghost()
                                    .disabled(!discard_enabled)
                                    .on_click(
                                        cx.listener(|page, _, window, cx| page.discard(window, cx)),
                                    ),
                            )
                            .child(
                                Button::new(CONFIG_RELOAD_ID)
                                    .label("Reload")
                                    .ghost()
                                    .disabled(!reload_enabled)
                                    .on_click(cx.listener(|page, _, window, cx| {
                                        page.reload_selected(window, cx)
                                    })),
                            ),
                    )
                    .child(
                        Button::new(CONFIG_SAVE_ID)
                            .label("Save")
                            .primary()
                            .disabled(!save_enabled)
                            .on_click(
                                cx.listener(|page, _, window, cx| page.save_selected(window, cx)),
                            ),
                    ),
            )
    }
}

/// A document's tab, with a dot while it has unsaved edits.
fn tab_label(file: ConfigFile, dirty: bool) -> SharedString {
    if dirty {
        SharedString::from(format!("{} ·", file.file_name()))
    } else {
        SharedString::from(file.file_name())
    }
}

/// The element id of a document's tab.
fn tab_id(file: ConfigFile) -> &'static str {
    match file {
        ConfigFile::Init => "config-file-tab-init",
        ConfigFile::Swarm => "config-file-tab-swarm",
        ConfigFile::Memory => "config-file-tab-memory",
        ConfigFile::Lore => "config-file-tab-lore",
    }
}

/// A palette token as a colour gpui draws with. The one line `widgets::paint`
/// uses; this crate does not depend on `widgets`.
fn ink(c: design::Rgb) -> Hsla {
    Rgba {
        r: f32::from(c.r) / 255.,
        g: f32::from(c.g) / 255.,
        b: f32::from(c.b) / 255.,
        a: 1.,
    }
    .into()
}

/// A path elided in the middle, keeping both ends (the tail names the file).
fn shorten(path: &str, limit: usize) -> String {
    let count = path.chars().count();
    if count <= limit || limit < 8 {
        return path.to_string();
    }
    let head = limit / 2 - 1;
    let tail = limit - 1 - head;
    let start: String = path.chars().take(head).collect();
    let end: String = path.chars().skip(count - tail).collect();
    format!("{start}…{end}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::component::highlighter::HighlightTheme;
    use gpui_kit::component::{Theme as ComponentTheme, ThemeMode};
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        point, size, AnyWindowHandle, Bounds, TestAppContext, WindowBounds, WindowOptions,
    };
    use std::rc::Rc;

    /// A directory of this test's own, removed on the way out. A project scope
    /// keeps every test away from the real `$HOME`.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Rc<Scratch> {
            let dir =
                std::env::temp_dir().join(format!("settings-config-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("a scratch directory");
            Rc::new(Scratch(dir))
        }

        fn scope(&self) -> ConfigScope {
            ConfigScope::Project(self.0.clone())
        }

        fn file(&self, file: ConfigFile) -> PathBuf {
            config_file::path(&self.scope(), file)
        }

        fn write(&self, file: ConfigFile, text: &str) {
            let path = self.file(file);
            std::fs::create_dir_all(path.parent().unwrap()).expect("the .evo directory");
            std::fs::write(&path, text).expect("a config file");
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A save goes through the real SBCL (`store::config_file::save` validates
    /// with it), so a machine without a *usable* one cannot exercise the save path
    /// — `Checker::discover` answers for a configured binary that does not run,
    /// which is why this asks the checker to read something. The tests that need
    /// it say so rather than accept a fake; the missing-checker path has its own
    /// test (`a_missing_checker_blocks_the_save`), which always runs.
    fn require_sbcl() -> bool {
        let usable = store::Checker::discover()
            .map(|checker| checker.check("(a)\n").is_ok())
            .unwrap_or(false);
        if !usable {
            eprintln!("skipping: no usable SBCL to check config text with");
        }
        usable
    }

    /// The checker is found through the environment (`EVO_SBCL_BIN`), and one test
    /// points it at nothing to prove a save is blocked without one. Environment
    /// variables are process-wide, so every test that triggers a save holds this
    /// for its whole run: none of them can see another's setting.
    fn sbcl_env() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Point the checker at a binary that is not there, for as long as this lives:
    /// the machine-without-SBCL case, made deterministic. It puts the environment
    /// back on the way out, even if the test panics.
    struct NoChecker;

    impl NoChecker {
        fn install() -> NoChecker {
            std::env::set_var(
                store::lispcheck::SBCL_BIN_ENV,
                "/nonexistent/sbcl-for-this-test",
            );
            NoChecker
        }
    }

    impl Drop for NoChecker {
        fn drop(&mut self) {
            std::env::remove_var(store::lispcheck::SBCL_BIN_ENV);
        }
    }

    struct Fixture {
        window: AnyWindowHandle,
        page: Entity<ConfigEditor>,
        scratch: Rc<Scratch>,
        _subscriptions: Vec<Subscription>,
    }

    impl Fixture {
        fn act<R>(&self, cx: &mut TestAppContext, f: impl FnOnce(&mut Window, &mut App) -> R) -> R {
            cx.update_window(self.window, |_, window, cx| f(window, cx))
                .expect("config editor window")
        }

        fn update<R>(
            &self,
            cx: &mut TestAppContext,
            f: impl FnOnce(&mut ConfigEditor, &mut Context<ConfigEditor>) -> R,
        ) -> R {
            cx.update(|cx| self.page.update(cx, f))
        }

        fn with_window<R>(
            &self,
            cx: &mut TestAppContext,
            f: impl FnOnce(&mut ConfigEditor, &mut Window, &mut Context<ConfigEditor>) -> R,
        ) -> R {
            self.act(cx, |window, cx| {
                self.page.update(cx, |page, cx| f(page, window, cx))
            })
        }

        fn state(&self, cx: &mut TestAppContext, file: ConfigFile) -> Entity<EditorState> {
            cx.update(|cx| self.page.read(cx).buffers[slot(file)].editor.clone())
        }

        fn select(&self, cx: &mut TestAppContext, file: ConfigFile) {
            self.with_window(cx, |page, window, cx| page.select(file, window, cx));
        }

        fn type_into(&self, cx: &mut TestAppContext, file: ConfigFile, text: &str) {
            self.write_into(cx, file, text, false)
        }

        /// Replace a document's whole text: select all, then type over it.
        fn replace_into(&self, cx: &mut TestAppContext, file: ConfigFile, text: &str) {
            self.write_into(cx, file, text, true)
        }

        fn write_into(&self, cx: &mut TestAppContext, file: ConfigFile, text: &str, replace: bool) {
            self.select(cx, file);
            let state = self.state(cx, file);
            self.act(cx, |window, cx| {
                let handle = state.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
                if replace {
                    window.press("cmd-a", cx);
                }
                window.input(text, cx);
                window.render_frame(cx);
            });
            cx.run_until_parked();
        }

        fn click(&self, cx: &mut TestAppContext, id: &'static str) {
            self.act(cx, |window, cx| {
                window.click(id, cx);
                window.render_frame(cx);
            });
            cx.run_until_parked();
        }

        fn press(&self, cx: &mut TestAppContext, key: &str) {
            self.act(cx, |window, cx| {
                window.press(key, cx);
                window.render_frame(cx);
            });
            cx.run_until_parked();
        }

        fn label(&self, cx: &mut TestAppContext, id: &'static str) -> String {
            self.act(cx, |window, cx| {
                window.render_frame(cx);
                window.find(id).label().unwrap_or_default().to_string()
            })
        }

        fn text(&self, cx: &mut TestAppContext, file: ConfigFile) -> String {
            let state = self.state(cx, file);
            cx.update(|cx| state.read(cx).value().to_string())
        }

        fn is_dirty(&self, cx: &mut TestAppContext) -> bool {
            cx.update(|cx| self.page.read(cx).is_dirty(cx))
        }

        fn is_saving(&self, cx: &mut TestAppContext) -> bool {
            cx.update(|cx| self.page.read(cx).is_saving())
        }

        fn saving_file(&self, cx: &mut TestAppContext) -> Option<ConfigFile> {
            cx.update(|cx| self.page.read(cx).saving_file())
        }

        fn can_save(&self, cx: &mut TestAppContext) -> bool {
            cx.update(|cx| self.page.read(cx).can_save(cx))
        }

        fn disk(&self, file: ConfigFile) -> Option<String> {
            std::fs::read_to_string(self.scratch.file(file)).ok()
        }
    }

    fn open(cx: &mut TestAppContext, scratch: Rc<Scratch>) -> Fixture {
        cx.update(gpui_kit::init);
        let scope = scratch.scope();
        let (window, page) = cx.update(|cx| {
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.), px(0.)),
                    size: size(px(760.), px(520.)),
                })),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| ConfigEditor::new(scope.clone(), window, cx))
            })
            .expect("config editor window")
        });
        cx.run_until_parked();
        Fixture {
            window,
            page,
            scratch,
            _subscriptions: Vec::new(),
        }
    }

    fn page(cx: &mut TestAppContext, name: &str) -> Fixture {
        open(cx, Scratch::new(name))
    }

    /// The four documents are offered as tabs, and the page opens on `init.lisp`.
    #[gpui_kit::test]
    fn the_four_documents_are_offered(cx: &mut TestAppContext) {
        let f = page(cx, "tabs");
        f.act(cx, |window, cx| {
            window.render_frame(cx);
            assert!(window.find(CONFIG_TABS_ID).visible());
        });
        for file in ConfigFile::ALL {
            assert_eq!(f.label(cx, tab_id(file)), file.file_name());
        }
        cx.update(|cx| assert_eq!(f.page.read(cx).selected(), ConfigFile::Init));
    }

    /// A file that is not there and has not been touched is not offered by Save:
    /// opening one creates nothing, and an empty draft is not a reason to write.
    #[gpui_kit::test]
    fn an_unchanged_missing_file_is_not_offered_to_save(cx: &mut TestAppContext) {
        let f = page(cx, "missing-untouched");
        assert!(!f.can_save(cx));
        assert!(!f.is_dirty(cx));
        // The button is offered disabled (the state predicate is what draws it).
        f.act(cx, |window, cx| window.render_frame(cx));
        assert!(f.disk(ConfigFile::Init).is_none());
    }

    /// A missing file opens empty, is not created by opening it, and is written
    /// only once it has been typed into.
    #[gpui_kit::test]
    fn a_missing_file_opens_empty_and_is_not_created_until_saved(cx: &mut TestAppContext) {
        let _guard = sbcl_env();
        if !require_sbcl() {
            return;
        }
        let f = page(cx, "missing");
        assert_eq!(f.text(cx, ConfigFile::Init), "");
        assert!(!f.is_dirty(cx));
        assert!(!f.can_save(cx), "nothing to save");
        assert!(!f.scratch.0.join(".evo").exists(), "reading created it");

        f.type_into(cx, ConfigFile::Init, "(set-setting :language \"en\")\n");
        assert!(f.is_dirty(cx));
        assert!(f.can_save(cx));
        assert!(
            f.disk(ConfigFile::Init).is_none(),
            "not written before Save"
        );

        f.click(cx, CONFIG_SAVE_ID);
        assert_eq!(
            f.disk(ConfigFile::Init).as_deref(),
            Some("(set-setting :language \"en\")\n")
        );
        assert!(!f.is_dirty(cx));
        assert_eq!(f.label(cx, CONFIG_STATUS_ID), "Saved.");
    }

    /// The save confirmation is shown until the next step: an edit, a select or a
    /// reload dismisses it.
    #[gpui_kit::test]
    fn a_save_confirmation_is_dismissed_by_the_next_step(cx: &mut TestAppContext) {
        let _guard = sbcl_env();
        if !require_sbcl() {
            return;
        }
        let f = page(cx, "saved");
        f.type_into(cx, ConfigFile::Init, "(a)\n");
        f.click(cx, CONFIG_SAVE_ID);
        assert_eq!(f.label(cx, CONFIG_STATUS_ID), "Saved.");

        // An edit, without going through a select.
        let state = f.state(cx, ConfigFile::Init);
        f.act(cx, |window, cx| {
            let handle = state.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
            window.input("x", cx);
            window.render_frame(cx);
        });
        assert_eq!(f.label(cx, CONFIG_STATUS_ID), "", "an edit dismisses it");

        // A select: written again, then the tab is switched away and back.
        f.click(cx, CONFIG_SAVE_ID);
        assert_eq!(f.label(cx, CONFIG_STATUS_ID), "Saved.");
        f.select(cx, ConfigFile::Lore);
        f.select(cx, ConfigFile::Init);
        assert_eq!(f.label(cx, CONFIG_STATUS_ID), "", "a select dismisses it");
    }

    /// The real reader is the gate: a text that asks the reader to evaluate at read
    /// time (`#.`) is refused, so it can never be written.
    #[gpui_kit::test]
    fn the_real_reader_refuses_reader_eval(cx: &mut TestAppContext) {
        let _guard = sbcl_env();
        if !require_sbcl() {
            return;
        }
        let f = page(cx, "reader-eval");
        f.type_into(cx, ConfigFile::Init, "#.(+ 1 2)\n");
        f.click(cx, CONFIG_SAVE_ID);
        assert!(f.disk(ConfigFile::Init).is_none(), "nothing was written");
        assert!(f.is_dirty(cx));
        assert!(f.label(cx, CONFIG_STATUS_ID).starts_with("Not saved:"));
    }

    /// A text that does not read is never written: the failure is shown and the
    /// draft is kept.
    #[gpui_kit::test]
    fn an_invalid_document_is_not_written(cx: &mut TestAppContext) {
        let _guard = sbcl_env();
        if !require_sbcl() {
            return;
        }
        let f = page(cx, "syntax");
        // An unmatched close parenthesis: the editor's auto-closing pairs make an
        // unmatched opener balance itself, but a stray close is still a reader
        // error.
        f.type_into(cx, ConfigFile::Init, ")\n");
        f.click(cx, CONFIG_SAVE_ID);
        assert!(
            f.disk(ConfigFile::Init).is_none(),
            "a refused save wrote nothing"
        );
        assert!(f.is_dirty(cx), "the draft is kept");
        assert!(f.label(cx, CONFIG_STATUS_ID).starts_with("Not saved:"));
    }

    /// A machine with no SBCL still gets a working page: the save is refused with
    /// the checker's own words, nothing is written, and the draft is kept. This
    /// runs whatever the machine has — it points the checker at a binary that is
    /// not there — so the blocked path is never a silent skip.
    #[gpui_kit::test]
    fn a_missing_checker_blocks_the_save(cx: &mut TestAppContext) {
        let _guard = sbcl_env();
        let _missing = NoChecker::install();
        let f = page(cx, "no-checker");
        f.type_into(cx, ConfigFile::Init, "(a)\n");
        assert!(f.can_save(cx), "the text is a save away");
        f.click(cx, CONFIG_SAVE_ID);
        assert!(
            f.disk(ConfigFile::Init).is_none(),
            "nothing may be written without a checker"
        );
        assert!(f.is_dirty(cx), "the draft is kept");
        assert!(f.label(cx, CONFIG_STATUS_ID).starts_with("Not saved:"));
    }

    /// A file that exists but cannot be read is not edited over: Save is refused
    /// until it can be read.
    #[gpui_kit::test]
    fn a_file_that_cannot_be_read_is_not_written(cx: &mut TestAppContext) {
        let scratch = Scratch::new("unreadable");
        std::fs::create_dir_all(scratch.file(ConfigFile::Init))
            .expect("a directory where the file is");
        let f = open(cx, scratch);
        assert!(
            !f.can_save(cx),
            "no readable file, so nothing may be written"
        );
        assert_eq!(f.text(cx, ConfigFile::Init), "");
        assert_eq!(
            f.label(cx, CONFIG_STATUS_ID),
            format!(
                "Cannot be edited: {} is not a regular file",
                f.scratch.file(ConfigFile::Init).display()
            ),
            "a read error says the file cannot be edited"
        );
        f.type_into(cx, ConfigFile::Init, "(a)\n");
        assert!(!f.can_save(cx), "still refused");
        f.click(cx, CONFIG_SAVE_ID);
        assert!(
            f.scratch.file(ConfigFile::Init).is_dir(),
            "the directory is untouched"
        );
    }

    /// Discard throws a draft away even for a file that could not be read — and the
    /// reason it cannot be edited stays on the status line.
    #[gpui_kit::test]
    fn discard_reverts_an_unreadable_file_and_keeps_the_reason(cx: &mut TestAppContext) {
        let scratch = Scratch::new("discard-unreadable");
        std::fs::create_dir_all(scratch.file(ConfigFile::Init))
            .expect("a directory where the file is");
        let f = open(cx, scratch);
        f.type_into(cx, ConfigFile::Init, "(draft)\n");
        assert!(
            f.is_dirty(cx),
            "the draft is real even though it cannot be saved"
        );

        f.click(cx, CONFIG_DISCARD_ID);
        assert_eq!(f.text(cx, ConfigFile::Init), "", "the draft is gone");
        assert!(!f.is_dirty(cx));
        assert_eq!(
            f.label(cx, CONFIG_STATUS_ID),
            format!(
                "Cannot be edited: {} is not a regular file",
                f.scratch.file(ConfigFile::Init).display()
            ),
            "the reason it cannot be edited stays"
        );
    }

    /// A conflict — the file changed under the editor — refuses the write and says
    /// so, keeping the draft.
    #[gpui_kit::test]
    fn a_file_changed_on_disk_refuses_the_write(cx: &mut TestAppContext) {
        let _guard = sbcl_env();
        if !require_sbcl() {
            return;
        }
        let scratch = Scratch::new("conflict");
        scratch.write(ConfigFile::Swarm, "(a)\n");
        let f = open(cx, scratch);
        // A running session writes the file while the page has it open.
        f.scratch.write(ConfigFile::Swarm, "(b)\n");
        f.type_into(cx, ConfigFile::Swarm, "(c)\n");
        f.click(cx, CONFIG_SAVE_ID);
        assert_eq!(
            f.disk(ConfigFile::Swarm).as_deref(),
            Some("(b)\n"),
            "the other writer keeps their bytes"
        );
        assert!(f.is_dirty(cx));
        assert_eq!(
            f.label(cx, CONFIG_STATUS_ID),
            format!(
                "Not saved: {} changed on disk since it was opened; your edit was not written",
                f.scratch.file(ConfigFile::Swarm).display()
            ),
            "a refused save says the file was not saved"
        );
    }

    /// Switching documents keeps each draft.
    #[gpui_kit::test]
    fn switching_documents_keeps_drafts(cx: &mut TestAppContext) {
        let f = page(cx, "drafts");
        f.type_into(cx, ConfigFile::Init, "(init)\n");
        f.type_into(cx, ConfigFile::Swarm, "(swarm)\n");
        f.select(cx, ConfigFile::Init);
        assert_eq!(f.text(cx, ConfigFile::Init), "(init)\n");
        f.select(cx, ConfigFile::Swarm);
        assert_eq!(f.text(cx, ConfigFile::Swarm), "(swarm)\n");
        assert!(f.is_dirty(cx));
        f.select(cx, ConfigFile::Init);
        assert_eq!(
            f.label(cx, tab_id(ConfigFile::Init)),
            "init.lisp ·",
            "the tab marks the unsaved draft"
        );
    }

    /// Discard puts every draft back to what was last read or written.
    #[gpui_kit::test]
    fn discard_restores_the_loaded_baseline(cx: &mut TestAppContext) {
        let scratch = Scratch::new("discard");
        scratch.write(ConfigFile::Init, "(loaded)\n");
        let f = open(cx, scratch);
        assert_eq!(f.text(cx, ConfigFile::Init), "(loaded)\n");
        f.type_into(cx, ConfigFile::Init, "(draft)\n");
        assert!(f.is_dirty(cx));

        f.click(cx, CONFIG_DISCARD_ID);
        assert_eq!(f.text(cx, ConfigFile::Init), "(loaded)\n");
        assert!(!f.is_dirty(cx));
    }

    /// Reload never throws a draft away: it is refused while the document is dirty.
    #[gpui_kit::test]
    fn reload_refuses_a_dirty_draft(cx: &mut TestAppContext) {
        let scratch = Scratch::new("reload-dirty");
        scratch.write(ConfigFile::Init, "(on disk)\n");
        let f = open(cx, scratch);
        f.type_into(cx, ConfigFile::Init, "(draft)\n");

        // The button is offered disabled (the state predicate is what draws it),
        // and the ask itself is refused.
        f.act(cx, |window, cx| window.render_frame(cx));
        f.click(cx, CONFIG_RELOAD_ID);
        f.with_window(cx, |page, window, cx| page.reload_selected(window, cx));
        cx.run_until_parked();
        assert_eq!(
            f.text(cx, ConfigFile::Init),
            "(draft)\n(on disk)\n",
            "the draft is untouched"
        );
        assert!(f.is_dirty(cx));
    }

    /// Reload on a clean document reads the file again.
    #[gpui_kit::test]
    fn reload_reads_again(cx: &mut TestAppContext) {
        let scratch = Scratch::new("reload");
        scratch.write(ConfigFile::Init, "(one)\n");
        let f = open(cx, scratch);
        assert_eq!(f.text(cx, ConfigFile::Init), "(one)\n");
        f.scratch.write(ConfigFile::Init, "(two)\n");
        f.click(cx, CONFIG_RELOAD_ID);
        assert_eq!(f.text(cx, ConfigFile::Init), "(two)\n");
        assert!(!f.is_dirty(cx));
    }

    /// An answer from a superseded load is dropped whole: the newer load's facts
    /// stand.
    #[gpui_kit::test]
    fn a_stale_read_does_not_overwrite_a_newer_load(cx: &mut TestAppContext) {
        let scratch = Scratch::new("stale-op");
        scratch.write(ConfigFile::Init, "(on disk)\n");
        let f = open(cx, scratch);
        assert_eq!(f.text(cx, ConfigFile::Init), "(on disk)\n");

        // A read from before the last load, claiming different facts.
        let stale_op = f.update(cx, |page, _| page.buffers[slot(ConfigFile::Init)].op) - 1;
        let revision = f.update(cx, |page, _| page.buffers[slot(ConfigFile::Init)].revision);
        f.with_window(cx, |page, window, cx| {
            page.apply_read(
                ConfigFile::Init,
                stale_op,
                revision,
                Ok(Some("(stale)\n".to_owned())),
                window,
                cx,
            )
        });
        assert_eq!(f.text(cx, ConfigFile::Init), "(on disk)\n");
        assert!(
            !f.is_dirty(cx),
            "the stale read did not become the baseline"
        );
    }

    /// A read from the current operation does not replace a draft typed since it
    /// started; the file's facts are still taken.
    #[gpui_kit::test]
    fn a_read_does_not_clobber_a_newer_edit(cx: &mut TestAppContext) {
        let scratch = Scratch::new("stale-read");
        scratch.write(ConfigFile::Init, "(on disk)\n");
        let f = open(cx, scratch);
        f.replace_into(cx, ConfigFile::Init, "(typed)\n");
        assert_eq!(f.text(cx, ConfigFile::Init), "(typed)\n");

        // The operation that is running, but a revision from before the edit.
        let op = f.update(cx, |page, _| page.buffers[slot(ConfigFile::Init)].op);
        let oldest = f.update(cx, |page, _| page.buffers[slot(ConfigFile::Init)].revision) - 1;
        f.with_window(cx, |page, window, cx| {
            page.apply_read(
                ConfigFile::Init,
                op,
                oldest,
                Ok(Some("(reloaded)\n".to_owned())),
                window,
                cx,
            )
        });
        assert_eq!(
            f.text(cx, ConfigFile::Init),
            "(typed)\n",
            "a read must not clobber a newer edit"
        );
        assert!(f.is_dirty(cx), "the reloaded text is the baseline");
    }

    /// While a save is in flight, Discard and Reload are refused and Save is not
    /// offered: nothing may promise a discard the pending write would undo.
    #[gpui_kit::test]
    fn discard_and_reload_are_refused_while_saving(cx: &mut TestAppContext) {
        let _guard = sbcl_env();
        if !require_sbcl() {
            return;
        }
        let f = page(cx, "saving");
        f.type_into(cx, ConfigFile::Init, "(a)\n");
        // Start the save and do not park: it is in flight.
        f.act(cx, |window, cx| {
            window.click(CONFIG_SAVE_ID, cx);
            window.render_frame(cx);
        });
        assert!(f.is_saving(cx), "the save is in flight");
        assert!(!f.can_save(cx));

        f.with_window(cx, |page, window, cx| page.discard(window, cx));
        f.with_window(cx, |page, window, cx| page.reload_selected(window, cx));
        assert_eq!(f.text(cx, ConfigFile::Init), "(a)\n", "the draft stands");
        assert!(f.is_dirty(cx));
        assert!(f.is_saving(cx), "and the save is still the one in flight");

        cx.run_until_parked();
        assert_eq!(f.disk(ConfigFile::Init).as_deref(), Some("(a)\n"));
        assert!(!f.is_saving(cx));
        assert!(!f.is_dirty(cx));
    }

    /// ⌘S saves, as the button does.
    #[gpui_kit::test]
    fn cmd_s_saves(cx: &mut TestAppContext) {
        let _guard = sbcl_env();
        if !require_sbcl() {
            return;
        }
        let f = page(cx, "cmds");
        f.type_into(cx, ConfigFile::Init, "(a)\n");
        f.press(cx, "cmd-s");
        assert_eq!(f.disk(ConfigFile::Init).as_deref(), Some("(a)\n"));
        assert!(!f.is_dirty(cx));
    }

    /// The restart reminder is on the page, in exactly these words.
    #[gpui_kit::test]
    fn the_restart_reminder_is_always_shown(cx: &mut TestAppContext) {
        let f = page(cx, "reminder");
        assert_eq!(f.label(cx, CONFIG_REMINDER_ID), REMINDER_TEXT);
        assert_eq!(
            REMINDER_TEXT,
            "Changes take effect after restarting Evo Desktop."
        );
    }

    /// The resolved path is shown, so a person knows which of the two files it is.
    #[gpui_kit::test]
    fn the_resolved_path_is_shown(cx: &mut TestAppContext) {
        let f = page(cx, "path");
        assert_eq!(
            f.label(cx, CONFIG_PATH_ID),
            f.scratch.file(ConfigFile::Init).display().to_string()
        );
        f.click(cx, tab_id(ConfigFile::Lore));
        assert_eq!(
            f.label(cx, CONFIG_PATH_ID),
            f.scratch.file(ConfigFile::Lore).display().to_string()
        );
    }

    /// Nothing is written on the thread that draws: a click starts work that only
    /// finishes once the executor is run.
    #[gpui_kit::test]
    fn a_save_is_not_written_synchronously(cx: &mut TestAppContext) {
        let _guard = sbcl_env();
        if !require_sbcl() {
            return;
        }
        let f = page(cx, "async");
        f.type_into(cx, ConfigFile::Init, "(a)\n");
        f.act(cx, |window, cx| {
            window.click(CONFIG_SAVE_ID, cx);
            window.render_frame(cx);
        });
        assert!(
            f.disk(ConfigFile::Init).is_none(),
            "the UI thread wrote the file"
        );
        cx.run_until_parked();
        assert_eq!(f.disk(ConfigFile::Init).as_deref(), Some("(a)\n"));
    }

    /// The opaque solid colours painted at several points across the editor
    /// surface — its corners' inset, its gutter, its middle, its right edge — read
    /// off the frame's own quads (glyphs are not quads, so what is read is the
    /// surface). Not a guess.
    fn surface_colours(f: &Fixture, cx: &mut TestAppContext) -> Vec<Hsla> {
        let (points, scale, quads) = f.act(cx, |window, cx| {
            window.render_frame(cx);
            let b = window.find(CONFIG_SURFACE_ID).bounds();
            let mid_y = b.origin.y + b.size.height / 2.;
            let points = vec![
                b.origin + point(px(4.), px(4.)),
                point(b.origin.x + px(10.), mid_y),
                b.center(),
                point(b.origin.x + b.size.width - px(6.), mid_y),
            ];
            (points, window.scale_factor(), window.painted_quads())
        });
        let mut colours = Vec::new();
        for at in points {
            let at = at.scale(scale);
            for quad in quads.iter().filter(|quad| quad.bounds.contains(&at)) {
                if let Some(colour) = quad.background.as_solid() {
                    if colour.a > 0.5 {
                        colours.push(colour);
                    }
                }
            }
        }
        colours
    }

    /// Two colours the same, within a channel step: the design's hex and gpui's
    /// conversion meet in the middle.
    fn same_colour(a: Hsla, b: Hsla) -> bool {
        let (a, b) = (Rgba::from(a), Rgba::from(b));
        (a.r - b.r).abs() < 0.01 && (a.g - b.g).abs() < 0.01 && (a.b - b.b).abs() < 0.01
    }

    /// The editor is drawn on the design's `--input`, in both themes — and never
    /// on the kit's own highlight background, which is light and would leave pale
    /// code on a white surface under the dark theme.
    #[gpui_kit::test]
    fn the_editor_surface_is_the_designs_input_in_both_themes(cx: &mut TestAppContext) {
        let f = page(cx, "surface");
        for (mode, dark) in [(ThemeMode::Dark, true), (ThemeMode::Light, false)] {
            f.act(cx, |window, cx| {
                ComponentTheme::change(mode, Some(window), cx);
                // Evo's own theme leaves the kit's highlight theme alone, and the
                // kit's default for one is light: that is the state the app is
                // really in, and the one that made the code editor white under the
                // dark theme. Reproduce it here, not a kinder one.
                ComponentTheme::global_mut(cx).highlight_theme = HighlightTheme::default_light();
                window.render_frame(cx);
            });
            let painted = surface_colours(&f, cx);
            assert!(
                painted
                    .iter()
                    .any(|colour| same_colour(*colour, ink(design::palette(dark).input))),
                "{mode:?}: the editor is not drawn on the design's --input: {painted:?}"
            );
            // The reported defect: a light surface (the kit's own highlight
            // background) under the dark theme — and its mirror in the light one.
            if dark {
                assert!(
                    !painted.iter().any(|colour| colour.l > 0.5),
                    "{mode:?}: a light surface under the dark theme: {painted:?}"
                );
            } else {
                assert!(
                    !painted.iter().any(|colour| colour.l < 0.2),
                    "{mode:?}: a dark surface under the light theme: {painted:?}"
                );
            }
        }
    }

    /// The host is told which document is being written, not just that one is.
    #[gpui_kit::test]
    fn saving_file_names_the_document_being_written(cx: &mut TestAppContext) {
        let _guard = sbcl_env();
        if !require_sbcl() {
            return;
        }
        let f = page(cx, "saving-file");
        assert_eq!(f.saving_file(cx), None);
        f.type_into(cx, ConfigFile::Memory, "(a)\n");
        f.act(cx, |window, cx| {
            window.click(CONFIG_SAVE_ID, cx);
            window.render_frame(cx);
        });
        assert_eq!(f.saving_file(cx), Some(ConfigFile::Memory));
        cx.run_until_parked();
        assert_eq!(f.saving_file(cx), None);
    }
}

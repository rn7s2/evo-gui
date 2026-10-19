//! What this window cannot do for itself, because the machine it draws on is not
//! the machine the work happens on.
//!
//! Three things every desktop window does by reaching out of the process: it puts
//! a system **dialog** on the screen, it reads a **file**, and it **runs a
//! binary** to get a document out of it. A browser can do none of them — there is
//! no panel to open, its filesystem is the server's rather than the page's, and
//! there is no child process at all — so each is behind a seam here, answered by
//! target.
//!
//! ```text
//! desktop  dialog: rfd's panel     read: std::fs         run: store::cli, a real child
//! browser  dialog: Picker          read: /api/config     run: /api/cli, the server's own
//! ```
//!
//! ## The browser side today
//!
//! A browser reaches the machine that served the page, over the routes
//! `docs/web-fix-interfaces.md` fixes:
//!
//! * **a folder** — [`Picker`], a panel that walks the serving machine's
//!   directories through `GET /api/fs` and answers with one of them. A page has no
//!   file dialog to open, so this is what "choose a folder" is here; the field the
//!   empty tab draws beside it still takes a typed path, and a typed path is still
//!   a path on the *serving* machine.
//! * **a file, for an attachment** — the same panel, listing files as well as
//!   directories. What it answers is a path on the serving machine, which is
//!   exactly what an attachment is: a path evo reads when the turn arrives.
//! * **the config files** — `settings::config_http`, which is `store::config_file`'s
//!   two calls on a host and `GET`/`POST /api/config` on a browser. The editor does
//!   not branch: both answer the same shapes. (It lives in `settings` because the
//!   editor that calls it does; this file's seam is the one the *window* needs.)
//! * **the binary probe** — [`run_json`]/[`run_json_reporting`] ask the server to
//!   run one of its own two binaries (`GET /api/cli`) and hand back the JSON
//!   document the desktop's own run would have produced. The empty tab's cards read
//!   the same either way, so a model list and a launch check are drawn in a browser
//!   exactly as they are in the app. A server that cannot be reached answers
//!   [`not_hosted`] — "we could not establish it" — which is what these cards drew
//!   before this route existed, so a transport failure degrades to that rather than
//!   to a blank card.

use std::path::{Path, PathBuf};

use store::cli::CliError;

// The two wasm-bindgen traits a `fetch` reply arrives through: `dyn_into` to see
// the promise's value as a `Response`, and `as_string` to read a `JsValue` that is
// one. `JsValue` itself is the NULL a promise's resolver is called with.
#[cfg(target_family = "wasm")]
use wasm_bindgen::{JsCast as _, JsValue};

/// Ask the person for a folder, starting in `start` when the caller knows a good
/// one. `None` is a dialog that was cancelled.
///
/// The desktop answer is `rfd`'s async dialog: the caller neither blocks on it
/// nor waits for it. A browser has no such dialog — see [`ENTERS_FOLDER`].
#[cfg(not(target_family = "wasm"))]
pub async fn pick_folder(title: String, start: Option<PathBuf>) -> Option<PathBuf> {
    let mut dialog = rfd::AsyncFileDialog::new().set_title(title);
    if let Some(folder) = start {
        dialog = dialog.set_directory(folder);
    }
    let picked = dialog.pick_folder().await?;
    Some(picked.path().to_path_buf())
}

/// Ask the person for a folder, on the serving machine.
///
/// The desktop's answer is `rfd`'s async dialog. A browser has no dialog — and the
/// folder a session runs in is not a folder *here* — so the answer is a panel that
/// walks the serving machine's directories through `GET /api/fs`
/// ([`Picker::folder`]). `None` is the same as a cancelled dialog.
#[cfg(target_family = "wasm")]
pub async fn pick_folder(title: String, start: Option<PathBuf>) -> Option<PathBuf> {
    Picker::folder(title, start).await
}

/// Ask the person for **images**, through the browser's own file dialog — what the
/// composer's `+` does in a page.
///
/// This is the one ask here that is not the server's. A browser's file dialog hands
/// back a `File`: its bytes, and no path — and that is exactly what an image
/// attachment needs, because an image rides the turn *in the op itself*
/// (`input.send`'s `images`, `{name, media_type, data}` with the data base64), so the
/// bytes travel with the message and no server route is involved at all.
///
/// It is offered **images only**, and deliberately: a file that is not an image has
/// no shape in the protocol that a page can fill — the only shape a file has is an
/// absolute path the session's machine can open, and a page's file is on the
/// *reader's* machine. A dialog that let one be chosen would be a dialog that let a
/// reader attach something that could not work.
///
/// The dialog is the element, made and clicked: `showOpenFilePicker` is not used
/// because it is not in every browser, and a `File`'s non-standard `path` property is
/// never read — it would name the page's machine, which is the wrong machine.
#[cfg(target_family = "wasm")]
pub async fn pick_files() -> Option<Vec<composer::ChosenFile>> {
    let input = file_input().await?;
    let files = input.files()?;
    let mut chosen = Vec::new();
    for index in 0..files.length() {
        let Some(file) = files.get(index) else {
            continue;
        };
        let name = file.name();
        let media_type = file.type_();
        let Some(bytes) = read_bytes(&file).await else {
            continue;
        };
        chosen.push(composer::ChosenFile {
            name,
            media_type,
            bytes,
        });
    }
    (!chosen.is_empty()).then_some(chosen)
}

/// `<input type="file">`, clicked, answering with the element once a choice is made.
///
/// The promise is the browser's own: the element announces a choice with `change`,
/// and a page cannot wait for an event without one. `Promise::new` hands the executor
/// the two settling functions, which is the only way to make one from here — so the
/// resolve is kept in a `Rc<RefCell<…>>` and called from the listener.
#[cfg(target_family = "wasm")]
async fn file_input() -> Option<web_sys::HtmlInputElement> {
    use std::cell::RefCell;
    use std::rc::Rc;
    use wasm_bindgen::closure::Closure;

    let document = web_sys::window()?.document()?;
    let input = document
        .create_element("input")
        .ok()?
        .dyn_into::<web_sys::HtmlInputElement>()
        .ok()?;
    input.set_type("file");
    // Images only, and any number of them: what this build can attach (see
    // `pick_files`). The attribute is the dialog's own filter, so a reader is shown
    // what they can choose rather than everything and a refusal.
    input.set_accept("image/*");
    input.set_multiple(true);

    let settle: Rc<RefCell<Option<js_sys::Function>>> = Rc::new(RefCell::new(None));
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        *settle.borrow_mut() = Some(resolve);
    });
    let input_for_event = input.clone();
    let settle_for_event = settle.clone();
    let listener = Closure::<dyn FnMut()>::new(move || {
        let _ = input_for_event;
        if let Some(resolve) = settle_for_event.borrow().as_ref() {
            let _ = resolve.call0(&JsValue::NULL);
        }
    });
    input
        .add_event_listener_with_callback("change", listener.as_ref().unchecked_ref())
        .ok()?;
    input.click();
    wasm_bindgen_futures::JsFuture::from(promise).await.ok()?;
    // The listener lives as long as the element, which is until this function
    // returns: the element is not in the document, so nothing else holds it.
    listener.forget();
    Some(input)
}

/// One file's bytes, read with `FileReader` — the page's own reader, because a
/// `Blob`'s bytes arrive through an event and not a call.
#[cfg(target_family = "wasm")]
async fn read_bytes(file: &web_sys::File) -> Option<Vec<u8>> {
    use std::cell::RefCell;
    use std::rc::Rc;
    use wasm_bindgen::closure::Closure;

    let reader = web_sys::FileReader::new().ok()?;
    let settle: Rc<RefCell<Option<js_sys::Function>>> = Rc::new(RefCell::new(None));
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        *settle.borrow_mut() = Some(resolve);
    });
    let settle_for_event = settle.clone();
    let listener = Closure::<dyn FnMut()>::new(move || {
        if let Some(resolve) = settle_for_event.borrow().as_ref() {
            let _ = resolve.call0(&JsValue::NULL);
        }
    });
    reader
        .add_event_listener_with_callback("load", listener.as_ref().unchecked_ref())
        .ok()?;
    reader.read_as_array_buffer(file).ok()?;
    wasm_bindgen_futures::JsFuture::from(promise).await.ok()?;
    listener.forget();

    let buffer = reader.result().ok()?;
    let array = js_sys::Uint8Array::new(&buffer);
    let mut bytes = vec![0u8; array.length() as usize];
    array.copy_to(&mut bytes);
    Some(bytes)
}

/// The picker's own answer, and the one place a page's two asks share their/// The picker's own answer, and the one place a page's two asks share their
/// implementation.
///
/// A native dialog answers with a path from *this* machine; a page cannot, so this
/// walks the serving machine's directories instead. That makes it the same shape of
/// answer — a `PathBuf` — which is what keeps every call site from branching.
///
/// The browser's panel is the page's own: `window.prompt` cannot list a directory,
/// so the only honest way to choose one is to ask the server what is in it. That is
/// a small sequence of dialogs — a directory, then its subdirectories, then the
/// choice — which is deliberately the *least* machinery that works, because the
/// real answer (an in-app picker drawn over the empty tab, with the listing in a
/// list) is a UI change to `empty_tab.rs` and this is the seam it would call.
#[cfg(target_family = "wasm")]
struct Picker;

#[cfg(target_family = "wasm")]
impl Picker {
    /// Choose a directory.
    async fn folder(title: String, start: Option<PathBuf>) -> Option<PathBuf> {
        // The route refuses a path that is not a directory, so what comes back is
        // one — and the caller's own check is the tab's, not this seam's.
        ask(title, start).await
    }
}

/// Walk the serving machine's directories and ask which one — the whole of the
/// browser's picker.
///
/// `GET /api/fs` lists directories and nothing else, so every step is a directory:
/// the answer is the one we are in, or a child of it.
///
/// The walk ends when the person stops descending: a directory with nothing in it,
/// or a `prompt` left empty, is "this one". Every round trip is `GET /api/fs`, and
/// a refusal (a path that is not a directory, a server that is not there) ends the
/// walk with `None` — the same answer a cancelled dialog gives.
#[cfg(target_family = "wasm")]
async fn ask(title: String, start: Option<PathBuf>) -> Option<PathBuf> {
    let mut here = start;
    loop {
        let listing = fs_list(here.as_deref()).await?;
        if listing.is_empty() {
            // Nothing below this one: it is the answer, and asking again would be a
            // dialog with one button. The listing's own `path` is the server's, which
            // is what a caller gets back — a directory it reported, not one this page
            // guessed at.
            return Some(listing.path.clone());
        }
        let choice = choose(&title, &listing.path, &listing)?;
        match choice {
            Choice::Here => return Some(listing.path.clone()),
            Choice::Into(name) => here = Some(listing.join(&name)),
        }
    }
}

/// What the person answered at one step of the walk.
///
/// Pure, and compiled on both targets: the walk is the same decision whether the
/// question was put up by a page's `prompt` or by a test. On a host only the tests
/// reach it — the browser is where the walk actually runs — so a native build is
/// told that is deliberate.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_family = "wasm"), allow(dead_code))]
enum Choice {
    /// Stop here: this directory is the answer.
    Here,
    /// Go into this child directory.
    Into(String),
}

/// The question one step of the walk puts up: where we are, what is under it, and
/// the ways out.
///
/// The numbers are the walk's own: `0` is "this folder", and the rest index the
/// listing in the order it is shown.
///
/// A host reaches this only from its tests; the browser is where it runs.
#[cfg_attr(not(target_family = "wasm"), allow(dead_code))]
fn question(title: &str, here: &Path, listing: &Listing) -> String {
    let mut lines = vec![format!("{title}\n\nIn {}", here.display()), String::new()];
    lines.push("  0  — this folder".to_owned());
    for (n, dir) in listing.dirs.iter().enumerate() {
        lines.push(format!("  {}  — {}", n + 1, dir.name));
    }
    lines.push(String::new());
    lines.push("Type a number, or leave it empty to cancel.".to_owned());
    lines.join("\n")
}

/// What the answer means. A cancelled or unparseable answer is `None` — a dialog
/// that was put away, or one whose text was not a number this listing has.
///
/// A host reaches this only from its tests; the browser is where it runs.
#[cfg_attr(not(target_family = "wasm"), allow(dead_code))]
fn parse(answer: &str, listing: &Listing) -> Option<Choice> {
    let answer = answer.trim();
    if answer.is_empty() {
        return None;
    }
    let n: usize = answer.parse().ok()?;
    if n == 0 {
        return Some(Choice::Here);
    }
    listing
        .dirs
        .get(n - 1)
        .map(|dir| Choice::Into(dir.name.clone()))
}

/// One step's question, put up, and what the answer means.
#[cfg(target_family = "wasm")]
fn choose(title: &str, here: &Path, listing: &Listing) -> Option<Choice> {
    let answer = prompt(&question(title, here, listing))?;
    parse(&answer, listing)
}

/// One entry of a directory listing: its name, and the whole path it names.
///
/// The browser's `GET /api/fs` reply; a host builds it only in its tests.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(target_family = "wasm", derive(serde::Deserialize))]
#[cfg_attr(not(target_family = "wasm"), allow(dead_code))]
pub struct Entry {
    name: String,
    path: PathBuf,
}

/// `GET /api/fs?path=` — what is in one directory of the serving machine.
///
/// The browser's `GET /api/fs` reply; a host builds it only in its tests.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(target_family = "wasm", derive(serde::Deserialize))]
#[cfg_attr(not(target_family = "wasm"), allow(dead_code))]
pub struct Listing {
    path: PathBuf,
    #[cfg_attr(target_family = "wasm", serde(default))]
    dirs: Vec<Entry>,
}

/// The browser's `GET /api/fs` reply; a host builds it only in its tests.
#[cfg_attr(not(target_family = "wasm"), allow(dead_code))]
impl Listing {
    /// One child of this listing, by name.
    fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    /// Whether there is nothing below this directory: it is the answer, and asking
    /// again would be a dialog with one button.
    ///
    /// Directories are the whole of it: `GET /api/fs` lists directories and nothing
    /// else, so "empty" is one question and not two.
    fn is_empty(&self) -> bool {
        self.dirs.is_empty()
    }
}

/// The listing, or `None` when the server would not give one.
///
/// `None` asks for **no path**, which the route reads as its own home directory —
/// where a picker starts, and the one place a page need not be told about. Every
/// step after the first names a directory the server itself just reported, so the
/// walk never guesses at a path.
#[cfg(target_family = "wasm")]
async fn fs_list(path: Option<&Path>) -> Option<Listing> {
    let route = match path {
        Some(path) => format!("/api/fs?path={}", encode(&path.display().to_string())),
        None => "/api/fs".to_owned(),
    };
    let body = get_json(&route).await.ok()?;
    serde_json::from_value(body).ok()
}

/// One `GET` of a same-origin route, its body as JSON.
///
/// `fetch` resolves a relative URL against the page's own origin, which is the
/// `evo-web` server that served this page — so the routes above need no host and
/// nothing to configure. `settings::config_http` has its own copy of this for the
/// same reason: it is four lines of `web-sys`, and two crates that each need it are
/// not a crate to share.
#[cfg(target_family = "wasm")]
async fn get_json(path: &str) -> Result<serde_json::Value, String> {
    let window = web_sys::window().ok_or("this page has no window")?;
    let response = wasm_bindgen_futures::JsFuture::from(window.fetch_with_str(path))
        .await
        .map_err(|_| format!("the server did not answer {path}"))?;
    let response: web_sys::Response = response
        .dyn_into()
        .map_err(|_| "the answer was not a response".to_owned())?;
    let text = wasm_bindgen_futures::JsFuture::from(
        response
            .text()
            .map_err(|_| "cannot read the answer".to_owned())?,
    )
    .await
    .map_err(|_| "cannot read the answer".to_owned())?
    .as_string()
    .unwrap_or_default();
    if !response.ok() {
        return Err(format!("the server refused {path}: {text}"));
    }
    serde_json::from_str(&text).map_err(|error| format!("{path} did not answer JSON: {error}"))
}

/// Everything outside the unreserved set becomes `%XX`, so a path cannot break out
/// of the query it is in.
///
/// Both targets, with [`cli_route`]: it is the one that uses this to build a query,
/// and a host compiles it only so that function can be tested.
#[cfg_attr(not(target_family = "wasm"), allow(dead_code))]
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Put a yes/no question to the person, and answer whether they agreed.
///
/// The desktop answer is `cx.prompt_for_paths`'s sibling — a system dialog — which
/// the desktop shell opens for itself where it needs one; nothing native calls this,
/// which is why there is no native arm. A browser has exactly one modal it can put
/// up without drawing anything, and that is `window.confirm`.
#[cfg(target_family = "wasm")]
pub async fn confirm(question: &str) -> bool {
    web_sys::window()
        .and_then(|window| window.confirm_with_message(question).ok())
        .unwrap_or(false)
}

/// The page's own dialog: `window.prompt`, which is the one input a browser can put
/// up without drawing anything. `None` when it was put away.
#[cfg(target_family = "wasm")]
fn prompt(message: &str) -> Option<String> {
    web_sys::window()?
        .prompt_with_message(message)
        .ok()
        .flatten()
}

/// Whether this build shows a folder **text field** beside the card, so a path can
/// be typed as well as picked.
///
/// True in a browser, and it is not a preference: the folder a browser session runs
/// in is not a folder *here* — it is on the machine that served the page, which is
/// the machine `POST /api/tabs` opens the tab on — so a person has to be able to
/// name one. Since [`pick_folder`] is now a real picker in a browser
/// ([`Picker`] walks the serving machine's directories), the field is what it
/// should have been all along: the fallback for a path that is easier to type than
/// to walk to, not the only way in.
pub const ENTERS_FOLDER: bool = cfg!(target_family = "wasm");

/// The note under the folder field in a browser: whose machine the path names.
///
/// A person who types `/Users/me/coding/foo` into a page is naming a folder on the
/// serving machine, and one that is not there is refused by that machine rather
/// than by this one — which is worth saying before they try, not after.
///
/// Drawn by the browser's folder card (`empty_tab.rs`).
#[cfg_attr(not(target_family = "wasm"), allow(dead_code))]
pub const FOLDER_HINT: &str = "An absolute path on the machine that served this page.";

/// The placeholder a browser's folder field shows: a path, so what is expected of
/// it is visible before anything is typed.
pub const FOLDER_PLACEHOLDER: &str = "/Users/you/coding/foo";

/// What a file holds, or why it could not be read.
///
/// The desktop answer is this machine's own filesystem ([`std::fs`]). The browser
/// has no filesystem to read: a path is the *serving* machine's, and the file
/// behind it arrives over the server. Until that is wired, every read is an error
/// — which is the answer a file that is not there gives, and the one callers here
/// already handle.
#[cfg(not(target_family = "wasm"))]
pub fn read_to_string(path: &std::path::Path) -> std::io::Result<String> {
    std::fs::read_to_string(path)
}

/// [`read_to_string`] where there is no filesystem to read: a browser.
#[cfg(target_family = "wasm")]
pub fn read_to_string(path: &std::path::Path) -> std::io::Result<String> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!(
            "{} is on the serving machine, not in this page",
            path.display()
        ),
    ))
}

/// The wall clock, in epoch milliseconds — the step clock's own reading, and the
/// one place a target difference is allowed to show.
///
/// `std::time` is unimplemented on `wasm32-unknown-unknown`: `SystemTime::now()`
/// **panics** there rather than returning an error (measured — see the panic in
/// `docs/` and the note in `launcher_data.rs`). A page has a clock, though: the
/// browser's own, which is what `Date.now()` reads. So the reading is taken per
/// target, and callers get the same number in the same unit either way.
#[cfg(not(target_family = "wasm"))]
pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or_default()
}

/// [`now_millis`] from the page's own clock: `Date.now()` is already milliseconds
/// since the epoch, which is the unit this answers in.
#[cfg(target_family = "wasm")]
pub fn now_millis() -> u64 {
    js_sys::Date::now() as u64
}

/// The same path, as the machine spells it: symlinks followed, `.` and `..`
/// resolved, a trailing slash dropped.
///
/// The desktop answer is [`std::fs::canonicalize`]. A browser has no filesystem
/// to resolve against, and the paths it holds are the *serving* machine's — so
/// this returns the path as written, which is what the desktop's own fallback
/// does for a path that cannot be resolved. Two spellings of one path then
/// compare unequal on wasm; the comparison they feed is a display's, not a
/// decision's.
#[cfg(not(target_family = "wasm"))]
pub fn canonicalize(path: &std::path::Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(path)
}

/// [`canonicalize`] where there is no filesystem: a browser.
#[cfg(target_family = "wasm")]
pub fn canonicalize(path: &std::path::Path) -> std::io::Result<PathBuf> {
    Ok(PathBuf::from(path.to_string_lossy().trim_end_matches('/')))
}

/// Run `bin args…` and read stdout as one JSON document (§9).
///
/// The desktop answer is [`store::cli::run_json`]'s bounded run: a real child, in
/// its own process group, stopped if it never answers (§9.1). A browser has no
/// child to run, so it asks the machine that has one — see [`cli_read`].
#[cfg(not(target_family = "wasm"))]
pub fn run_json(bin: &std::path::Path, args: &[String]) -> Result<serde_json::Value, CliError> {
    store::cli::run_json(bin, args)
}

/// [`run_json`] where the run is the serving machine's: `GET /api/cli`.
#[cfg(target_family = "wasm")]
pub async fn run_json(
    bin: &std::path::Path,
    args: &[String],
) -> Result<serde_json::Value, CliError> {
    cli_read(bin, args).await
}

/// [`run_json`], for the one read that is read on a non-zero exit too
/// (`evo-swarm check --json` prints its problems and exits 1).
#[cfg(not(target_family = "wasm"))]
pub fn run_json_reporting(
    bin: &std::path::Path,
    args: &[String],
) -> Result<serde_json::Value, CliError> {
    store::cli::run_json_reporting(bin, args)
}

/// [`run_json_reporting`] where the run is the serving machine's.
///
/// The non-zero exit is the server's to honour: `/api/cli` reads `check` with
/// `run_json_reporting`, so the document arrives as a **200** and the problems in it
/// are drawn. Nothing here has to know that `check` exits 1.
#[cfg(target_family = "wasm")]
pub async fn run_json_reporting(
    bin: &std::path::Path,
    args: &[String],
) -> Result<serde_json::Value, CliError> {
    cli_read(bin, args).await
}

/// One offline read, asked of the server that has the binaries (`GET /api/cli`).
///
/// The two functions above are one read apiece and both end here: the route already
/// knows which of the two reads is which, so the only difference between them is
/// the argv they were handed.
///
/// **Every way this can fail is [`not_hosted`]**, and deliberately. A page that
/// cannot reach its server, or that asks for a read the route will not run, has
/// established nothing about the binary — which is exactly what `not_hosted` says
/// and exactly what these cards draw today. So an unreachable server degrades to the
/// present behaviour rather than to a blank card or a hang, and a read that *is*
/// answered comes back as the same `serde_json::Value` the desktop's own run would
/// have produced.
#[cfg(target_family = "wasm")]
async fn cli_read(bin: &std::path::Path, args: &[String]) -> Result<serde_json::Value, CliError> {
    let Some(route) = cli_route(bin, args) else {
        // Not a read this route can express: the binary is not one of the two the
        // server runs, or the argv is not one of the two reads it knows.
        return Err(not_hosted(bin));
    };
    match get_json(&route).await {
        Ok(body) => Ok(body),
        Err(reason) => Err(not_hosted_because(bin, reason)),
    }
}

/// The `/api/cli` route for one read, or `None` when this is not a read the route
/// can express.
///
/// Pure, and compiled on both targets so it can be tested without a browser: this is
/// the whole of the translation between what a caller had (a binary path and the
/// argv `store::launch` built) and what the route takes (a binary *name* and a
/// *read* with named flags). Doing it here rather than in the route is what keeps
/// the server from having to parse an argv — and it means the closed sets are
/// enforced in two places, the caller's argv shape and the route's own table.
///
/// `None` for anything else: a binary that is not `evo-swarm`/`evo-agent` (the
/// server runs its own two, so a path here names nothing it will run), a subcommand
/// that is not `catalog`/`check`, or a token that is not one of the flags those two
/// take.
///
/// Compiled on **both** targets, and only called on one: it is pure, so a host can
/// test it. That is the point — the translation between an argv and this route is
/// the whole contract between the two halves of this seam, and a browser is a bad
/// place to discover it is wrong.
#[cfg_attr(not(target_family = "wasm"), allow(dead_code))]
fn cli_route(bin: &std::path::Path, args: &[String]) -> Option<String> {
    let name = match bin.file_name()?.to_str()? {
        "evo-swarm" => "evo-swarm",
        "evo-agent" => "evo-agent",
        _ => return None,
    };
    let mut query = vec![format!("name={name}")];
    match args.first().map(String::as_str)? {
        "catalog" => query.push("read=catalog".to_owned()),
        "check" => {
            query.push("read=check".to_owned());
            let mut rest = args.iter().skip(1);
            while let Some(token) = rest.next() {
                match token.as_str() {
                    // The document, not a flag: `check_argv` always writes it.
                    "--json" => {}
                    "--workers" | "--model" | "--lane-model" => {
                        let value = rest.next()?;
                        query.push(format!(
                            "{}={}",
                            token.trim_start_matches("--"),
                            encode(value)
                        ));
                    }
                    _ => return None,
                }
            }
        }
        _ => return None,
    }
    Some(format!("/api/cli?{}", query.join("&")))
}

/// The answer a read gives where there is nothing to run it on.
///
/// [`CliError::NotFound`] is the desktop's own "this binary could not be
/// started", which is exactly what happened: it is on the serving machine and no
/// process here can reach it. The `source` is what the hover shows, so it says
/// the whole of it.
#[cfg(target_family = "wasm")]
fn not_hosted(bin: &std::path::Path) -> CliError {
    not_hosted_because(
        bin,
        "a browser has no process to run it in: the binary is on the serving \
         machine, and the page reaches it through the server"
            .to_owned(),
    )
}

/// [`not_hosted`] with the server's own words as the reason — a refusal, or a
/// server that did not answer.
#[cfg(target_family = "wasm")]
fn not_hosted_because(bin: &std::path::Path, reason: String) -> CliError {
    CliError::NotFound {
        bin: bin.to_path_buf(),
        source: std::io::Error::new(std::io::ErrorKind::Unsupported, reason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The route one read becomes, driven from the argv `store::launch` builds —
    /// so what a browser asks for is exactly what a host would have run.
    ///
    /// `cli_route` is pure, so this runs on a host: the argv shapes it asserts are
    /// the ones the desktop app produces for the same two reads.
    #[test]
    fn a_read_becomes_the_route_that_names_it() {
        use std::path::Path;

        // The agent catalog the empty tab asks for when the switch is off.
        assert_eq!(
            cli_route(
                Path::new("/usr/local/bin/evo-agent"),
                &["catalog".to_owned(), "--json".to_owned()]
            )
            .as_deref(),
            Some("/api/cli?name=evo-agent&read=catalog")
        );

        // A bare check, and one carrying every flag the route accepts.
        assert_eq!(
            cli_route(
                Path::new("/usr/local/bin/evo-swarm"),
                &["check".to_owned(), "--json".to_owned()]
            )
            .as_deref(),
            Some("/api/cli?name=evo-swarm&read=check")
        );
        assert_eq!(
            cli_route(
                Path::new("/usr/local/bin/evo-swarm"),
                &[
                    "check".to_owned(),
                    "--json".to_owned(),
                    "--workers".to_owned(),
                    "4".to_owned(),
                    "--model".to_owned(),
                    "m@acme".to_owned(),
                    "--lane-model".to_owned(),
                    "l@acme".to_owned(),
                ]
            )
            .as_deref(),
            Some("/api/cli?name=evo-swarm&read=check&workers=4&model=m%40acme&lane-model=l%40acme")
        );

        // A *name* picks which of the server's two binaries runs, not a path — so a
        // host configured with its own copy of evo-swarm still gets the server's
        // swarm read. The directory is ignored on purpose: the page is saying "the
        // swarm binary", and which file that is on the serving machine is the
        // server's business (its own `swarm_bin`), not the page's.
        assert_eq!(
            cli_route(
                Path::new("/opt/evo-swarm"),
                &["check".to_owned(), "--json".to_owned()]
            )
            .as_deref(),
            Some("/api/cli?name=evo-swarm&read=check")
        );

        // Anything the route cannot express is `None`, which the caller answers with
        // `not_hosted` rather than by asking for something it will not get: a binary
        // the server does not run, a subcommand it does not have, a flag it does not
        // take, and a flag with no value.
        for (bin, argv) in [
            ("/bin/sh", vec!["catalog", "--json"]),
            ("/opt/other", vec!["check", "--json"]),
            ("", vec!["catalog", "--json"]),
            ("/usr/local/bin/evo-swarm", vec!["serve"]),
            ("/usr/local/bin/evo-swarm", vec!["sessions", "--json"]),
            (
                "/usr/local/bin/evo-swarm",
                vec!["check", "--json", "--port", "0"],
            ),
            (
                "/usr/local/bin/evo-swarm",
                vec!["check", "--json", "--workers"],
            ),
            ("/usr/local/bin/evo-swarm", vec![]),
        ] {
            let argv: Vec<String> = argv.into_iter().map(str::to_owned).collect();
            assert_eq!(
                cli_route(Path::new(bin), &argv),
                None,
                "{bin} {argv:?} is not a read the route can express"
            );
        }
    }

    fn entry(name: &str) -> Entry {
        Entry {
            name: name.to_owned(),
            path: PathBuf::from("/home/me").join(name),
        }
    }

    fn listing(dirs: &[&str]) -> Listing {
        Listing {
            path: PathBuf::from("/home/me"),
            dirs: dirs.iter().map(|name| entry(name)).collect(),
        }
    }

    /// The walk stops on a directory with nothing below it rather than asking a
    /// question with one button.
    #[test]
    fn an_empty_directory_is_the_answer() {
        assert!(listing(&[]).is_empty());
        assert!(!listing(&["src"]).is_empty());
    }

    /// The numbers are the walk's own: 0 is this folder, then the directories.
    #[test]
    fn the_question_numbers_this_folder_then_the_listing() {
        let shown = question("Pick", Path::new("/home/me"), &listing(&["src", "docs"]));
        assert!(shown.contains("In /home/me"), "{shown}");
        assert!(shown.contains("0  — this folder"), "{shown}");
        assert!(shown.contains("1  — src"), "{shown}");
        assert!(shown.contains("2  — docs"), "{shown}");
    }

    /// The answer is a number the listing has, and nothing else is a choice.
    #[test]
    fn an_answer_is_read_as_the_thing_it_names() {
        let listed = listing(&["src", "docs"]);
        assert_eq!(parse("0", &listed), Some(Choice::Here));
        assert_eq!(parse(" 1 ", &listed), Some(Choice::Into("src".to_owned())));
        assert_eq!(parse("2", &listed), Some(Choice::Into("docs".to_owned())));

        // Out of range, not a number, and cancelled: nothing chosen.
        assert_eq!(parse("3", &listed), None);
        assert_eq!(parse("99", &listed), None);
        assert_eq!(parse("src", &listed), None);
        assert_eq!(parse("-1", &listed), None);
        assert_eq!(parse("", &listed), None);
        assert_eq!(parse("   ", &listed), None);
    }

    /// The route's answer is directories only, so the listing must read that shape
    /// — and must not require a `files` key the route does not send.
    #[test]
    fn a_listing_is_directories_and_the_route_sends_nothing_else() {
        let body: serde_json::Value = serde_json::from_str(
            r#"{"path":"/private/tmp","parent":"/private",
                "dirs":[{"name":"assets","path":"/private/tmp/assets"}]}"#,
        )
        .unwrap();
        let dirs = body["dirs"].as_array().unwrap();
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0]["name"], "assets");
        assert!(body.get("files").is_none(), "the route sends no files");
    }
}

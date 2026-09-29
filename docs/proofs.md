# Milestone proofs

One section per milestone in `docs/PROMPT.md` §12, as it is proven. The M0 record
is its own, longer document: `docs/proofs-real.md`.

Append your milestone's section; do not rewrite another lane's.

## M4 — bundle

The hardening milestone's Finder half (§12: "`cargo build --release` and a bundle
you can launch from Finder", single instance + activation, quit shutting
everything down). Run on macOS 27.0, arm64, `2026-09-29`.

### The bundle

```
$ scripts/bundle.sh
   Compiling evo-desktop v0.1.0 (/Users/bytedance/coding/evo-gui/crates/app)
/Users/bytedance/coding/evo-gui/dist/evo-desktop.app: replacing existing signature
bundle: /Users/bytedance/coding/evo-gui/dist/evo-desktop.app        # exit 0

$ codesign -dv dist/evo-desktop.app
Identifier=com.evo.desktop
Format=app bundle with Mach-O thin (arm64)
Signature=adhoc
```

`Contents/`: `MacOS/evo-desktop`, `Resources/AppIcon.icns`, `Info.plist` (linted
by `plutil` inside the script), `_CodeSignature`.

### Launched the Finder way

`HOME` is the real one — `open` cannot hand the app another, and §9.1 puts the
app's state in `~/.evo/desktop` anyway. No swarm was started (nothing to resume
was launched), so nothing else in `~/.evo/desktop` was touched: the run left
`app.log`, `app.json`, `lock`, `activate.sock` and `probe/`.

```
$ open dist/evo-desktop.app
$ tail ~/.evo/desktop/app.log
2026-09-29T12:44:00Z info  evo-desktop 0.1.0 starting (pid 44806, root /Users/bytedance/.evo/desktop)
2026-09-29T12:44:00Z info  app.json: window 1600x1000 at None,None, 0 recent(s), binaries /usr/local/bin/evo-swarm / /usr/local/bin/evo-agent
2026-09-29T12:44:00Z info  window open
2026-09-29T12:44:00Z info  startup: cached catalog has 0 model(s), fetched never
2026-09-29T12:44:00Z info  startup: 1 tab(s) will show it
2026-09-29T12:44:00Z info  catalog: probing with /usr/local/bin/evo-agent (cache older than 86400s or missing)
```

```
$ lsappinfo list | grep -A 6 'com.evo.desktop'
106) "Evo Desktop" ASN:0x0-0x1682681: (in front)
    bundleID="com.evo.desktop"
    bundle path="/Users/bytedance/coding/evo-gui/dist/evo-desktop.app"
    executable path="…/evo-desktop.app/Contents/MacOS/evo-desktop"
    pid = 44806 … type="Foreground" … Version="0.1.0" Arch=ARM64
```

`type="Foreground"` is the point: LaunchServices sees an app with a UI, not a
background process. The name it shows is the bundle's, `Evo Desktop`.

### The name, the icon, and the menu bar

The menu bar of the running app read ` Evo Desktop | File | Edit | Window` —
the app menu from the bundle's name, and the four menus this round added.

The Dock: an icon of the app's own is present while it runs and gone after it
quits. Screen captures taken for the check (`/tmp/evo-app-check/dock.png` before,
`dock_after.png` after) show the same Dock with one icon fewer: the app's, the
one between the Docker whale and the Downloads folder. A close-up of the surface
appears in the window too — the empty tab rendered the **real** history: sixteen
resumable sessions with folders, lane counts and coordinator models
(`~/coding/evo-agent/ · 12 lanes · coordinator: claude-opus-5-5`, and so on),
which is §9.5 against real data. It also showed `GET /registry` failing with the
500 `docs/proofs-real.md` finding R1 records, as the catalog's own error line.

### A second launch activates instead of duplicating

```
$ open dist/evo-desktop.app        # again, while it runs
$ pgrep -f 'evo-desktop.app/Contents/MacOS/evo-desktop' | wc -l
1
$ lsappinfo list | grep -c com.evo.desktop
1
# and nothing new in app.log: LaunchServices brought the running app forward
```

Finder's own second `open` therefore activates rather than duplicates. For the
stronger case — a second *process*, which is what the app's own lock and
activation socket are for — the bundle's binary was run directly:

```
$ ./dist/evo-desktop.app/Contents/MacOS/evo-desktop
2026-09-29T12:44:26Z info  evo-desktop 0.1.0 starting (pid 45302, root /Users/bytedance/.evo/desktop)
2026-09-29T12:44:26Z info  another instance is running (activation acknowledged: true); exiting
$ echo $?
0
# in the *first* process's log, the knock arrived:
2026-09-29T12:44:26Z info  activated by another launch (pid 45302)
$ pgrep … | wc -l
1
```

The second process left with success (the user asked for the app and it is up),
the first raised its window, and there is still one instance.

### ⌘Q

```
$ osascript -e 'tell application "System Events" to keystroke "q" using command down'
$ tail ~/.evo/desktop/app.log
2026-09-29T12:48:15Z info  quitting: stopping every tab
2026-09-29T12:48:15Z info  saved /Users/bytedance/.evo/desktop/app.json: 1 tab(s), 0 of them resumable
2026-09-29T12:48:15Z info  shutdown: all 0 tab(s) exited
2026-09-29T12:48:15Z info  stopped; exiting
$ pgrep -f 'Contents/MacOS/evo-desktop' | wc -l
0
```

`app.json` after that run (§6's tab set, and §9.5's recents):

```json
{"version": 1, "tabs": ["tab-0"], "selected": "tab-0"}
```

One tab, which is what an untouched launch has; no recents, because no tab ever
had a session.

### What this proves, and what it does not

Proven: the release build bundles into an app Finder opens; it comes up as a
foreground app named `Evo Desktop` with its own Dock icon; one instance, with the
second launch activating the first instead of duplicating it; ⌘Q runs §9.8's
ladder, writes `app.json` and leaves no process behind.

Not proven here: a tab whose *swarm* runs inside a bundle (that is M1/M2's proof,
and it needs a folder picked by hand — the failure half of §9.7 is proven
headless in `crates/app/tests/boot_failure.rs`); the icon's design beyond "the
bundle's own icon is what the Dock shows"; and a relaunch reading the tab set
back (nothing restores a tab set yet — the app opens one empty tab by design,
§14.6).

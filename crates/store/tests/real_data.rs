//! The real thing: `~/.evo/sessions` and `~/.evo/swarm.lisp` as evo writes them.
//!
//! Both are **strictly read-only** — the source files are copied into a temp
//! directory before anything is written to them (see `swarm_config`'s test),
//! and the session scan only ever opens files for reading.
//!
//! Skipped, loudly, when this machine has no evo data yet.

use std::fs;
use std::time::{Duration, Instant};

use store::history::{scan, ScanBudget};
use store::paths::evo_dir;
use store::swarm_config::{self, LanesModel, WriteOutcome};

fn skip(reason: &str) {
    eprintln!("skipping: {reason}");
}

/// `~/.evo/sessions`, the real one — never the process's `EVO_SESSIONS_DIR`
/// override, which points at a single agent's own directory.
fn real_sessions_dir() -> std::path::PathBuf {
    evo_dir().join("sessions")
}

#[test]
fn real_journals_parse_into_resumable_swarms() {
    let dir = real_sessions_dir();
    if !dir.is_dir() {
        skip(&format!("{} does not exist", dir.display()));
        return;
    }
    // A bounded scan of the real tree: the same budget the app uses, but
    // smaller, so this stays a test and not a chore.
    let budget = ScanBudget {
        max_files: 500,
        max_duration: Duration::from_secs(20),
        ..ScanBudget::default()
    };
    let started = Instant::now();
    let outcome = scan(&dir, &budget);
    let elapsed = started.elapsed();
    assert!(
        elapsed <= Duration::from_secs(25),
        "the scan must respect its budget: {elapsed:?}"
    );

    if outcome.entries.is_empty() {
        skip(&format!(
            "no resumable swarm among {} journals",
            outcome.files_seen
        ));
        return;
    }
    eprintln!(
        "scanned {} of {} journals in {:?} → {} resumable swarms",
        outcome.files_read,
        outcome.files_seen,
        elapsed,
        outcome.entries.len()
    );

    for entry in &outcome.entries {
        // Every field we report came out of that file, not out of thin air.
        assert!(entry.session.is_absolute(), "{:?}", entry.session);
        assert!(entry.session.is_file(), "{:?}", entry.session);
        assert!(entry.session.extension().is_some_and(|e| e == "sexp"));
        assert!(!entry.swarm_id.is_empty(), "{:?}", entry.session);
        assert!(
            entry.folder.is_absolute(),
            "{:?} → {:?}",
            entry.session,
            entry.folder
        );
        assert!(
            entry.lanes >= 1,
            "{:?} has {} lanes",
            entry.session,
            entry.lanes
        );
        assert!(
            entry.when_epoch.is_some(),
            "unparsable timestamp {:?}",
            entry.when
        );
        assert!(
            entry.when.ends_with('Z'),
            "timestamps are UTC: {:?}",
            entry.when
        );
        assert!(!entry.folder_name().is_empty());
        assert_eq!(entry.resume_args().1, entry.session);
        assert!(entry.mtime > 0);
    }

    // Re-reading the journals is the expensive part: do it for a handful and
    // confirm the scan read them the way we would by hand.
    for entry in outcome.entries.iter().take(10) {
        let text = fs::read_to_string(&entry.session).expect("read journal");
        let first_line = text.lines().next().unwrap_or_default();
        assert!(
            first_line.starts_with("(:type :session"),
            "{} does not start with a session header: {first_line}",
            entry.session.display()
        );
        assert!(
            first_line.contains(&format!("\"{}\"", entry.session_id)),
            "header id ≠ scan id in {}: {first_line}",
            entry.session.display()
        );
        assert!(
            first_line.contains(":cwd"),
            "no :cwd in the header of {}: {first_line}",
            entry.session.display()
        );
        assert!(
            text.lines()
                .any(|l| l.starts_with("(:type :custom ") && l.contains(":key \"swarm\"")),
            "a resumable swarm must have a swarm record: {}",
            entry.session.display()
        );
        // Anything that ran a turn names its model in an assistant message, so
        // a journal with an assistant message must give us a model.
        if text.lines().any(|l| l.contains(":role :assistant")) {
            assert!(
                entry.models.coordinator.is_some(),
                "no model read from {} despite assistant messages",
                entry.session.display()
            );
        }
    }

    // The whole set: at least one row carries a model, and none invents one.
    let with_model = outcome
        .entries
        .iter()
        .filter(|e| e.models.coordinator.is_some())
        .count();
    eprintln!(
        "{} of {} rows know their coordinator model",
        with_model,
        outcome.entries.len()
    );
    assert!(with_model > 0, "no row learned its model from the journals");
    for entry in &outcome.entries {
        // The only models we can report are ids the journal actually wrote.
        if let Some(model) = &entry.models.coordinator {
            let text = fs::read_to_string(&entry.session).unwrap();
            assert!(
                text.contains(&format!(":model \"{model}\"")),
                "{} reports a model it does not contain: {model}",
                entry.session.display()
            );
        }
    }

    // Newest first, and no duplicates.
    let mut seen = std::collections::HashSet::new();
    for pair in outcome.entries.windows(2) {
        assert!(
            pair[0].mtime >= pair[1].mtime,
            "entries are not newest-first"
        );
    }
    for entry in &outcome.entries {
        assert!(
            seen.insert(entry.session.clone()),
            "duplicate {:?}",
            entry.session
        );
    }
}

#[test]
fn the_real_swarm_lisp_is_only_ever_touched_by_copy() {
    let real = store::paths::evo_dir().join("swarm.lisp");
    if !real.is_file() {
        skip(&format!("{} does not exist", real.display()));
        return;
    }
    let original = fs::read_to_string(&real).expect("read ~/.evo/swarm.lisp");
    let before = fs::metadata(&real).unwrap().modified().unwrap();

    // Work on a copy in a temp folder: the real file is never opened for
    // writing, which is the rule for everything outside ~/.evo/desktop/.
    let folder = std::env::temp_dir().join(format!("store-real-swarm-{}", std::process::id()));
    let _ = fs::remove_dir_all(&folder);
    fs::create_dir_all(folder.join(".evo")).unwrap();
    let copy = swarm_config::swarm_lisp_path(&folder);
    fs::write(&copy, &original).unwrap();

    // A file without a trailing newline would come back with one; that is the
    // only edit the round trip may make to the user's text.
    let expected = if original.ends_with('\n') {
        original.clone()
    } else {
        format!("{original}\n")
    };

    let model = LanesModel::new("ark-deepseek-v4.1-flash", "aiden");
    assert_eq!(
        swarm_config::set_lanes_model(&folder, Some(&model)).unwrap(),
        WriteOutcome::Written
    );
    let written = fs::read_to_string(&copy).unwrap();
    // The block is at the very top, and the user's own file is intact below it.
    assert!(written.starts_with(";;; evo-desktop:begin"), "{written}");
    assert!(written.contains("(evo.swarm:in-lanes ()\n"));
    assert!(written.contains("(evo:set-setting :model \"ark-deepseek-v4.1-flash\")"));
    assert!(written.contains("(evo:set-setting :model-provider :aiden)"));
    assert!(
        written.ends_with(&expected),
        "the file below our block must not move"
    );
    assert_eq!(written.matches(";;; evo-desktop:begin").count(), 1);
    assert_eq!(
        swarm_config::set_lanes_model(&folder, Some(&model)).unwrap(),
        WriteOutcome::Unchanged
    );

    // Default puts the file back, byte for byte.
    assert_eq!(
        swarm_config::set_lanes_model(&folder, None).unwrap(),
        WriteOutcome::Removed
    );
    assert_eq!(fs::read_to_string(&copy).unwrap(), expected);

    fs::remove_dir_all(&folder).unwrap();
    assert_eq!(
        fs::metadata(&real).unwrap().modified().unwrap(),
        before,
        "the real file is untouched"
    );
}

/// The shape of a real swarm record, as `swarm/lanes.lisp` writes it — pinned
/// here so a change in evo shows up as a failing test rather than as an empty
/// history list.
#[test]
fn the_swarm_record_fields_the_scan_depends_on() {
    let dir = real_sessions_dir();
    if !dir.is_dir() {
        skip(&format!("{} does not exist", dir.display()));
        return;
    }
    let budget = ScanBudget {
        max_files: 500,
        max_duration: Duration::from_secs(20),
        ..ScanBudget::default()
    };
    let outcome = scan(&dir, &budget);
    let Some(entry) = outcome.entries.first() else {
        skip("no resumable swarm on this machine");
        return;
    };
    let text = fs::read_to_string(&entry.session).unwrap();
    let line = text
        .lines()
        .rfind(|l| l.starts_with("(:type :custom ") && l.contains(":key \"swarm\""))
        .expect("a swarm record");
    for field in [":id", ":workers", ":lanes", ":cwd"] {
        assert!(
            line.contains(field),
            "the swarm record no longer has {field}: {line}"
        );
    }
    // …and the record we parsed is the last one in the file.
    assert!(line.contains(&format!("\"{}\"", entry.swarm_id)));
    for lane in &entry.lane_cwds {
        assert!(lane.is_absolute(), "{lane:?}");
    }
}

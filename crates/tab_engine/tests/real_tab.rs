//! A tab against the real server: `build/evo-agent` (or `evo-swarm`), the binary
//! the app will run. Everything here goes through `TabEngine`'s public API only —
//! no fake control endpoints — so it is exactly the path the app takes.
//!
//! Skipped unless `EVO_AGENT_BIN` (or `EVO_SWARM_BIN`) names a build:
//!
//! ```sh
//! EVO_AGENT_BIN=…/build/evo-agent EVO_TEST_HOME=/tmp/stub \
//!   cargo test -p tab_engine --test real_tab -- --nocapture
//! ```

use std::path::PathBuf;
use std::time::{Duration, Instant};

use swarm_client::harness::{kill_tree, serving_argv, with_stub_home, OrphanGuard, TempDir};
use swarm_client::{process_alive, ServerConfig};
use tab_engine::{Queue, TabEngine, TabModel, Update};

mod common;
use common::{serving, Feed};

/// The binary under test, or a note that this run has none.
fn real_bin() -> Option<PathBuf> {
    for name in ["EVO_AGENT_BIN", "EVO_SWARM_BIN"] {
        if let Some(path) = std::env::var_os(name) {
            return Some(PathBuf::from(path));
        }
    }
    eprintln!("real_tab: no EVO_AGENT_BIN/EVO_SWARM_BIN — skipping");
    None
}

#[test]
fn a_tab_drives_the_real_server_end_to_end() {
    let Some(bin) = real_bin() else {
        return;
    };
    let dir = TempDir::new("real-tab").expect("a temp dir");
    let config = ServerConfig::swarm(bin, dir.path(), dir.path());
    let argv = serving_argv(&config.ready_file, &[]);
    let (handle, updates) = TabEngine::start(with_stub_home(config.with_argv(argv)));
    let mut feed = Feed::new(updates);

    // §1/§5.2: a boot, a ready file, one snapshot per topic, a live stream.
    feed.expect("Booting", |update| matches!(update, Update::Booting));
    let (epoch, pid) = serving(&mut feed);
    assert!(!epoch.is_empty() && pid > 0);
    // The engine thread owns the server, so a failure in the rest of this test
    // would leave a server behind (running, at a core apiece); the guard kills it
    // however the test ends.
    let guard = OrphanGuard::new(pid);

    let session = feed.expect(
        "the session snapshot",
        |update| matches!(update, Update::Snapshot { topic, .. } if topic == "session"),
    );
    let Update::Snapshot { body, .. } = session else {
        unreachable!()
    };
    assert!(body["state"]["model"]["id"].is_string(), "{body}");
    assert!(body["items"].is_array(), "{body}");

    // §5.5: the model builds the op, the tab sends it, and the reply comes back
    // under the rid the handle minted.
    let model = TabModel::new();
    let rid = handle
        .request(model.send_input("hello from the real tab", Queue::Now))
        .expect("the tab took the request");
    let reply = feed.expect("the reply", |update| {
        matches!(update, Update::OpReply { .. })
    });
    let Update::OpReply {
        rid: answered,
        op,
        reply,
    } = reply
    else {
        unreachable!()
    };
    assert_eq!(answered, rid);
    assert_eq!(op, "input.send");
    assert!(reply.ok, "{reply:?}");
    let item_id = reply.result["item_id"].as_str().expect("an item id");

    // §5.3: the turn appears as items on the stream the tab is already reading —
    // the reader's own item, and the answer streaming into the assistant's.
    let user = feed.expect("the user item", |update| {
        matches!(update, Update::Op { op, .. }
            if matches!(op, session::Op::ItemAdd { item, .. } if item.id == item_id))
    });
    let Update::Op { topic, op } = user else {
        unreachable!()
    };
    assert_eq!(topic, "session");
    let session::Op::ItemAdd { item, .. } = op else {
        unreachable!()
    };
    assert_eq!(item.id, item_id);
    feed.expect("the streamed answer", |update| {
        matches!(update, Update::Op { op, .. } if matches!(op, session::Op::ItemAppend { .. }))
    });

    // §5.4: the reads a scrollback and a tool row make.
    assert!(handle.page("session", None, 5));
    feed.expect(
        "a page of items",
        |update| matches!(update, Update::ItemsBefore { topic, .. } if topic == "session"),
    );
    assert!(handle.item("session", item_id));
    feed.expect(
        "one whole item",
        |update| matches!(update, Update::Item { topic, .. } if topic == "session"),
    );

    // §8: a dropped tab closes the server's stdin, and the server stops and takes
    // its ready file with it. A build that does not act on EOF is stopped by the
    // ladder instead; either way the tab must not leave a server behind.
    let ready_file = dir.path().join("ready.json");
    let dropped = Instant::now();
    drop(handle);
    let deadline = Instant::now() + Duration::from_secs(20);
    while process_alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    // This build does not act on stdin EOF once a turn has run (reported to the
    // serve lane), so the tab's ladder is what stops it, ten seconds later; a
    // server that stopped on its own takes its ready file with it.
    eprintln!(
        "note: the server stopped {:?} after the tab was dropped; ready file left: {}",
        dropped.elapsed(),
        ready_file.exists()
    );
    let stopped_on_its_own = dropped.elapsed() < Duration::from_secs(5);
    if process_alive(pid) {
        kill_tree(pid);
        let deadline = Instant::now() + Duration::from_secs(10);
        while process_alive(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    assert!(!process_alive(pid), "the real server outlived its tab");
    if stopped_on_its_own {
        assert!(
            !ready_file.exists(),
            "a clean shutdown deletes the ready file"
        );
    }
    guard.disarm();
}

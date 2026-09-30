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

use async_channel::Receiver;
use swarm_client::harness::{serving_argv, with_stub_home, TempDir};
use swarm_client::{process_alive, ServerConfig};
use tab_engine::{Queue, TabEngine, TabModel, Update};

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

/// The next update of a kind, or a panic naming what was being waited for.
fn expect(updates: &Receiver<Update>, what: &str, wanted: impl Fn(&Update) -> bool) -> Update {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(update) = updates.try_recv() {
            if wanted(&update) {
                return update;
            }
            continue;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
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

    // §1/§5.2: a boot, a ready file, one snapshot per topic, a live stream.
    expect(&updates, "Booting", |update| {
        matches!(update, Update::Booting)
    });
    let ready = expect(&updates, "Ready", |update| {
        matches!(update, Update::Ready { .. })
    });
    let Update::Ready {
        epoch,
        pid,
        port,
        session,
    } = ready
    else {
        unreachable!()
    };
    assert!(!epoch.is_empty() && pid > 0 && port > 0);
    assert!(session.path.contains(".evo/sessions/"), "{session:?}");

    let session = expect(
        &updates,
        "the session snapshot",
        |update| matches!(update, Update::Snapshot { topic, .. } if topic == "session"),
    );
    let Update::Snapshot { body, .. } = session else {
        unreachable!()
    };
    assert!(body["state"]["model"]["id"].is_string(), "{body}");
    assert!(body["items"].is_array(), "{body}");
    expect(
        &updates,
        "the live stream",
        |update| matches!(update, Update::Stream { status } if !status.is_reconnecting()),
    );

    // §5.5: the model builds the op, the tab sends it, and the reply comes back
    // under the rid the handle minted.
    let model = TabModel::new();
    let rid = handle
        .request(model.send_input("hello from the real tab", Queue::Now))
        .expect("the tab took the request");
    let reply = expect(&updates, "the reply", |update| {
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

    // §5.3: the turn appears as items on the stream the tab is already reading.
    let user = expect(&updates, "the user item", |update| {
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
    expect(
        &updates,
        "the streamed answer",
        |update| matches!(update, Update::Op { op, .. } if matches!(op, session::Op::ItemAppend { .. })),
    );

    // §5.4: the reads the scrollback and a tool row make.
    assert!(handle.page("session", None, 5));
    expect(
        &updates,
        "a page of items",
        |update| matches!(update, Update::ItemsBefore { topic, .. } if topic == "session"),
    );
    assert!(handle.item("session", item_id));
    expect(
        &updates,
        "one whole item",
        |update| matches!(update, Update::Item { topic, .. } if topic == "session"),
    );

    // §8: a dropped tab stops the server — EOF on its stdin, and the ready file
    // goes with it.
    drop(handle);
    let deadline = Instant::now() + Duration::from_secs(30);
    while process_alive(pid) {
        assert!(
            Instant::now() < deadline,
            "the real server outlived its tab"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

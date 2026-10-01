//! t09 — a turn queued while the coordinator works is drawn as sent once it is
//! steered in: the stream says so, and the app's own model (`session::TabModel`)
//! folds it to `sent`. Both queues are proven: `now` (what the composer sends
//! while a run is in flight) is drained at the run's next step, `after_run` when
//! the run ends.
//!
//! ```sh
//! CARGO_TARGET_DIR=target/proofs cargo test -p proofs --test t09_queued_turn_is_sent -- --nocapture
//! ```

use proofs::fixture::{Fixture, NOTE, WAIT};
use proofs::watch::{deadline_after, op, snapshot, Watcher};
use serde_json::json;
use session::{AgentKey, ItemKind, Op, TabModel, UserStatus};
use store::launch::Program;

#[test]
fn t09_queued_turn_is_sent() {
    let deadline = deadline_after(WAIT);
    let fixture = Fixture::new("t09");
    let server = fixture.spawn(&fixture.spec(Program::Swarm, 1));
    let client = server.client().clone();

    let seeded = snapshot(&client, &["session"], 200);
    let cursor = swarm_client::Cursor::new(
        server.ready().epoch.clone(),
        seeded["seq"].as_u64().unwrap(),
    );
    let mut watcher = Watcher::start(&client, &["session"], Some(cursor));

    op(
        &client,
        "input.send",
        json!({"text": "SLOW t09", "queue": "now"}),
    );
    watcher.wait_mirror(deadline, "the run to start", |mirror| {
        mirror.state("session")["status"].as_str() == Some("running")
    });
    let now = op(
        &client,
        "input.send",
        json!({"text": "steer now", "queue": "now"}),
    );
    let later = op(
        &client,
        "input.send",
        json!({"text": "after the run", "queue": "after_run"}),
    );
    let ids: Vec<String> = [&now, &later]
        .iter()
        .map(|reply| reply["item_id"].as_str().expect("an item id").to_owned())
        .collect();
    println!("{NOTE} queued {ids:?}: {now} {later}");

    watcher.wait_mirror(deadline, "both turns sent and the run over", |mirror| {
        ids.iter().all(|id| {
            mirror
                .item("session", id)
                .and_then(|item| item["status"].as_str())
                == Some("sent")
        }) && mirror.state("session")["status"].as_str() == Some("idle")
    });

    // The app's own fold of the same frames.
    let mut model = TabModel::new();
    model.on_snapshot("session", &seeded["topics"]["session"]);
    for frame in &watcher.frames {
        let topic = frame.topic().unwrap_or_default().to_owned();
        if let Some(op) = Op::from_json(&frame.op, &frame.data) {
            model.on_op(&topic, &op);
        }
        if frame.data.to_string().contains(&ids[0]) || frame.data.to_string().contains(&ids[1]) {
            println!(
                "{NOTE} {} {}",
                frame.op,
                frame.data.to_string().chars().take(200).collect::<String>()
            );
        }
    }
    for id in &ids {
        let item = model
            .items(AgentKey::Coordinator)
            .iter()
            .find(|item| &item.id == id)
            .unwrap_or_else(|| panic!("the model holds {id}"));
        match &item.kind {
            ItemKind::User(user) => assert_eq!(user.status, UserStatus::Sent, "{id}: {user:?}"),
            other => panic!("{id} is a user turn: {other:?}"),
        }
    }
}

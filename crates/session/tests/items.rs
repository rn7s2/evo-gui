//! Items and the topic mirror: parsing, ordering, append/patch/remove, paging
//! (CONTRACT §4.1, §5.3, §5.4).
//!
//! Everything here is the *view model*'s job: an item's fields, what one line of it says,
//! and where an op puts it. The UI's job — drawing it — is `transcript`'s.

mod common;

use common::{fixture, topic_body};
use serde_json::json;
use session::{
    AssistantStatus, Item, ItemKind, LaneEventKind, NoticeSeverity, NoticeSource, Op,
    QueuePosition, ToolStatus, Topic, UserStatus,
};

fn session_topic() -> Topic {
    let mut topic = Topic::new("session");
    topic.apply_snapshot(&topic_body(&fixture("snapshot-session.json"), "session"));
    topic
}

#[test]
fn every_kind_of_item_parses_into_its_fields() {
    let topic = session_topic();
    let kinds: Vec<&str> = topic.items().iter().map(|item| item.kind.label()).collect();
    assert_eq!(
        kinds,
        [
            "user",
            "context",
            "assistant",
            "tool",
            "tool",
            "goal",
            "notice",
            "lane_report",
            "lane_event",
            "command_note",
            "human_action",
            "compaction",
            "assistant",
            "run_outcome",
            "recovery",
            "provider_retry",
            "user",
            "assistant",
        ]
    );

    // A user turn, with what its images and its queue position say.
    let ItemKind::User(user) = &topic.item("e_1").unwrap().kind else {
        panic!("e_1 is a user item")
    };
    assert_eq!(user.text, "port the view model");
    assert_eq!(user.status, UserStatus::Sent);
    assert!(user.queue.is_none() && !user.is_queued());

    // The queued one, which is what a row draws as held back and cancels.
    let ItemKind::User(queued) = &topic.item("e_queued_1").unwrap().kind else {
        panic!("e_queued_1 is a user item")
    };
    assert!(queued.is_queued());
    assert_eq!(queued.queue, Some(QueuePosition::AfterRun));

    // An assistant message: text, thinking, model, usage.
    let ItemKind::Assistant(assistant) = &topic.item("e_3").unwrap().kind else {
        panic!("e_3 is an assistant item")
    };
    assert_eq!(assistant.status, AssistantStatus::Final);
    assert_eq!(assistant.model.as_deref(), Some("stub-a"));
    assert_eq!(assistant.usage.unwrap().total(), 41500);
    let ItemKind::Assistant(streaming) = &topic.item("e_15").unwrap().kind else {
        panic!("e_15 is an assistant item")
    };
    assert!(streaming.is_streaming());

    // A tool call: its args as an object, its status, its truncated result.
    let ItemKind::Tool(tool) = &topic.item("t_call_2").unwrap().kind else {
        panic!("t_call_2 is a tool item")
    };
    assert_eq!(tool.name, "read");
    assert_eq!(tool.args["path"], "crates/session/src/model.rs");
    assert_eq!(tool.status, ToolStatus::Ok);
    assert_eq!(tool.parent.as_deref(), Some("e_3"));
    let result = tool.result.as_ref().unwrap();
    assert!(result.truncated && result.chars == 9000);
    let ItemKind::Tool(running) = &topic.item("t_call_1").unwrap().kind else {
        panic!("t_call_1 is a tool item")
    };
    assert!(running.status.is_running() && running.result.is_none());

    // A structured lane report, every field of it.
    let ItemKind::LaneReport(report) = &topic.item("e_6").unwrap().kind else {
        panic!("e_6 is a lane report")
    };
    assert_eq!(report.lane, 2);
    assert_eq!(report.done, "moved the reducer");
    assert_eq!(report.evidence, "cargo test -p session");
    assert_eq!(report.goal.as_deref(), Some("active"));

    // A lane transition, already typed and already weighted.
    let ItemKind::LaneEvent(event) = &topic.item("e_7").unwrap().kind else {
        panic!("e_7 is a lane event")
    };
    assert_eq!(event.event, LaneEventKind::RunEnded);
    assert_eq!(event.outcome.as_deref(), Some("stop"));
    assert_eq!(event.severity, NoticeSeverity::Info);

    // A notice carries its own severity and source.
    let ItemKind::Notice(notice) = &topic.item("e_5").unwrap().kind else {
        panic!("e_5 is a notice")
    };
    assert_eq!(notice.severity, NoticeSeverity::Warn);
    assert_eq!(notice.source, NoticeSource::Swarm);
    assert!(notice.durable);

    // A compaction, which is what the scrollback pages across.
    assert!(topic.item("e_10").unwrap().is_compaction());
    let ItemKind::Compaction(compaction) = &topic.item("e_10").unwrap().kind else {
        panic!("e_10 is a compaction")
    };
    assert_eq!(compaction.tokens_before, 210000);
    assert!(!compaction.manual);

    // The human action names the lanes it touched.
    let ItemKind::HumanAction(action) = &topic.item("e_9").unwrap().kind else {
        panic!("e_9 is a human action")
    };
    assert_eq!(action.action, "interrupt");
    assert_eq!(action.lanes_label().as_deref(), Some("lane 2"));

    // A run's outcome is said in the run's own words.
    let ItemKind::RunOutcome(outcome) = &topic.item("e_12").unwrap().kind else {
        panic!("e_12 is a run outcome")
    };
    assert_eq!(outcome.text(), "Run aborted");
}

#[test]
fn an_item_the_build_does_not_know_is_kept_and_summarized() {
    let item = Item::from_json(&json!({
        "id": "e_new", "ts": 1, "kind": "sparkle", "text": "a later build's item"
    }))
    .expect("an unknown kind still has an id");
    assert_eq!(item.kind.label(), "sparkle");
    assert_eq!(item.summary(), "a later build's item");

    // Without an id an item cannot be keyed, patched or removed: it is dropped.
    assert!(Item::from_json(&json!({ "kind": "user", "text": "?" })).is_none());
}

#[test]
fn one_line_of_an_item_says_what_it_is() {
    let topic = session_topic();
    let summary = |id: &str| topic.item(id).unwrap().summary();
    assert_eq!(summary("e_1"), "port the view model");
    assert_eq!(summary("t_call_1"), "bash · running");
    assert_eq!(summary("t_call_2"), "read · ok");
    assert_eq!(summary("e_6"), "report: moved the reducer");
    assert_eq!(summary("e_7"), "run ended — task: port the view model");
    assert_eq!(summary("e_4"), "goal created");
    assert_eq!(summary("e_2"), "context · global-memory");
    assert_eq!(summary("e_8"), "command /lore");
    assert_eq!(summary("e_9"), "interrupt lane 2");
    assert_eq!(summary("e_5"), "lane 3 is down");
    assert_eq!(
        summary("e_10"),
        "The redesign was planned; the session crate is next."
    );
    assert_eq!(summary("e_12"), "Run aborted");
    assert_eq!(summary("e_13"), "recovered · the child died");
    assert_eq!(summary("e_14"), "retrying provider (2/5)");
    // A message that has said nothing yet is writing, not empty.
    assert_eq!(summary("e_15"), "Working on ");
}

#[test]
fn a_snapshot_replaces_the_topic_and_says_so() {
    let mut topic = Topic::new("session");
    assert!(topic.items().is_empty());
    let changes = topic.apply_snapshot(&topic_body(&fixture("snapshot-session.json"), "session"));
    assert!(changes.reset && changes.state && changes.items().is_empty());
    assert_eq!(topic.items().len(), 18);
    assert!(
        topic.has_older(),
        "the snapshot says there is more behind it"
    );
    assert_eq!(
        topic.state().model.as_ref().unwrap().label(),
        "stub-a (openai)"
    );
    assert_eq!(topic.state().status.label(), "running");
    assert_eq!(topic.state().queue, vec!["e_queued_1"]);
}

#[test]
fn an_op_lands_where_it_says_it_does() {
    let mut topic = session_topic();
    let last = topic.items().len();

    // An append with no item to append to is dropped: the next snapshot carries it whole.
    let changes = topic.apply_op(&Op::ItemAppend {
        id: "nope".into(),
        field: session::AppendField::Text,
        text: "…".into(),
    });
    assert!(changes.is_empty());

    // An op the build does not know is nothing at all.
    assert!(Op::from_json("sparkle.add", &json!({})).is_none());
    assert!(Op::from_json("item.patch", &json!({ "id": "e_1" })).is_some());

    for (op, topic_name, data) in common::ops("ops.json") {
        if topic_name.as_deref() != Some("session") {
            continue;
        }
        let Some(op) = Op::from_json(&op, &data) else {
            continue;
        };
        let changes = topic.apply_op(&op);
        let _ = changes;
    }

    // The appended item grew in place, and its patch closed it.
    let ItemKind::Assistant(assistant) = &topic.item("e_17").unwrap().kind else {
        panic!("e_17 is an assistant item")
    };
    assert_eq!(assistant.text, "Trimming the workspace.");
    assert_eq!(assistant.status, AssistantStatus::Final);
    assert_eq!(assistant.usage.unwrap().output, 40);

    // The tool call that was inserted after `e_3` was removed again by the next op.
    assert!(topic.item("t_call_9").is_none());
    assert_eq!(topic.items().len(), last + 2);
}

#[test]
fn an_id_already_held_is_patched_not_duplicated() {
    let mut topic = session_topic();
    let before = topic.items().len();
    let changes = topic.apply_op(&Op::ItemAdd {
        item: Box::new(topic.item("e_1").unwrap().clone()),
        after: None,
    });
    assert_eq!(topic.items().len(), before, "ids are stable: no second row");
    assert!(matches!(
        changes.items(),
        [session::ItemChange::Upsert { index, .. }] if *index == 0
    ));
}

#[test]
fn older_items_page_in_at_the_front() {
    let mut topic = session_topic();
    let newest = topic.items().first().unwrap().id.clone();
    let changes = topic.prepend_items(&fixture("items-before.json"));
    assert_eq!(changes.prepended, 2);
    assert!(!topic.has_older(), "the page says that was the last of it");
    assert_eq!(topic.items().first().unwrap().id, "e_0a");
    assert_eq!(topic.items()[2].id, newest);

    // Paging the same page twice adds nothing.
    let again = topic.prepend_items(&fixture("items-before.json"));
    assert!(again.is_empty());
    assert_eq!(topic.items().len(), 20);
}

#[test]
fn removing_an_item_reindexes_what_follows_it() {
    let mut topic = session_topic();
    let second = topic.items()[1].id.clone();
    let changes = topic.apply_op(&Op::ItemRemove {
        id: topic.items()[0].id.clone(),
    });
    assert_eq!(
        changes.items(),
        [session::ItemChange::Remove { id: "e_1".into() }]
    );
    assert_eq!(topic.items()[0].id, second);
    assert_eq!(topic.index_of(&second), Some(0));
    assert_eq!(topic.index_of("e_1"), None);
}

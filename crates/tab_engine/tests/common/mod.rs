//! What the tab's two integration tests share: reading its updates without
//! losing the ones a test was not looking for at that moment.
//!
//! This module is compiled into *each* test binary, and no two of them use all of
//! it — what one does not is not dead code.
#![allow(dead_code)]

//!
//! A tab's updates are a *stream*, and a caller that waits for one kind must not
//! throw the others away: the order they arrive in is the server's, not the
//! test's (an `item.add` lands before the `OpReply` that asked for it, topics come
//! back in the server's map order). So an expectation searches what has already
//! arrived first, and anything else waits in `seen`.

use std::time::{Duration, Instant};

use async_channel::Receiver;
use tab_engine::{TabModel, Update};

/// The updates a tab has sent, kept rather than skipped.
pub struct Feed {
    updates: Receiver<Update>,
    seen: Vec<Update>,
}

impl Feed {
    pub fn new(updates: Receiver<Update>) -> Feed {
        Feed {
            updates,
            seen: Vec::new(),
        }
    }

    /// The next update of a kind, from what has arrived or from what comes next.
    pub fn expect(&mut self, what: &str, wanted: impl Fn(&Update) -> bool) -> Update {
        if let Some(index) = self.seen.iter().position(&wanted) {
            return self.seen.remove(index);
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            match self.updates.try_recv() {
                Ok(update) => {
                    if wanted(&update) {
                        return update;
                    }
                    self.seen.push(update);
                }
                Err(_) => {
                    assert!(
                        Instant::now() < deadline,
                        "timed out waiting for {what}, saw {:?}",
                        self.seen
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }

    /// Forget what has already arrived: what a test does when it cares only about
    /// what happens *next* (a restart emits the same updates as a boot).
    pub fn forget(&mut self) {
        self.seen.clear();
    }

    /// Feed every update into the model until it is in the state the caller
    /// describes — the whole path a workspace takes from a tab to its view.
    pub fn feed_model(&mut self, model: &mut TabModel, done: impl Fn(&TabModel) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !done(model) {
            assert!(Instant::now() < deadline, "the model never caught up");
            if let Ok(update) = self.updates.recv_blocking() {
                apply(model, update);
            }
        }
    }
}

/// Feed one update into the model, the way the workspace does.
pub fn apply(model: &mut TabModel, update: Update) {
    match update {
        Update::Snapshot { topic, body } => {
            model.on_snapshot(&topic, &body);
        }
        Update::Op { topic, op } => {
            model.on_op(&topic, &op);
        }
        Update::Stream { status } => {
            model.on_stream("session", status);
        }
        _ => {}
    }
}

/// Start a tab and wait until it is serving *and* streaming: the epoch and
/// process id it announced. A test that drives the server must not do it before
/// the tab is listening to it.
pub fn serving(feed: &mut Feed) -> (String, u32) {
    let update = feed.expect("Ready", |update| matches!(update, Update::Ready { .. }));
    let Update::Ready { epoch, pid, .. } = update else {
        unreachable!()
    };
    feed.expect(
        "the live stream",
        |update| matches!(update, Update::Stream { status } if !status.is_reconnecting()),
    );
    (epoch, pid)
}

/// Whether an update is a snapshot of this topic.
pub fn snapshot_of(topic: &str) -> impl Fn(&Update) -> bool + '_ {
    move |update| matches!(update, Update::Snapshot { topic: name, .. } if name == topic)
}

//! The threading bridge between worker threads and the UI.
//!
//! Every I/O path in this app (HTTP calls, SSE streams, the history scan) runs
//! off the UI thread and reports back through one of these channels: a plain
//! `std::thread` produces values, a GPUI foreground task (`cx::spawn`) awaits
//! them and applies them to a `WeakEntity` on the UI thread. Nothing else
//! touches GPUI state (§2.6).
//!
//! Each value carries the [`Revision`] of the work that produced it. The bridge
//! drops a value whose revision is no longer the one its owner considers live,
//! so an answer that arrives after a request was replaced, a tab was resynced
//! or a stream was re-subscribed can never be applied to newer state (§9.1).
//!
//! ```text
//! let (bridge, worker) = Bridge::spawn(Revision::new(1), |tx| {
//!     let Ok(body) = blocking_get("/state") else { return };
//!     let _ = tx.send(body);          // fails once the UI side is gone
//! });
//! let task = bridge.drive_into(cx, |view| view.revision, |view, body, cx| {
//!     view.state = body;
//!     cx.notify();
//! });
//! ```

use std::thread::{self, JoinHandle};

use async_channel::{Receiver, RecvError, SendError, Sender};
use gpui_kit::{Context, Task};

/// Counter that identifies one generation of work on a target.
///
/// A view keeps the revision of the work it currently wants; a worker tags
/// everything it produces with the revision it was started under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Revision(u64);

impl Revision {
    pub fn new(value: u64) -> Self {
        Revision(value)
    }

    /// Start of the next generation, the way a resync or a reconnect bumps it.
    pub fn next(self) -> Self {
        Revision(self.0 + 1)
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

/// A value on its way from a worker thread to the UI, tagged with the revision
/// of the work that produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Tagged<T> {
    pub revision: Revision,
    pub payload: T,
}

impl<T> Tagged<T> {
    pub fn new(revision: Revision, payload: T) -> Self {
        Tagged { revision, payload }
    }
}

/// The worker's end of a [`Bridge`]. Cheap to clone.
pub struct BridgeSender<T> {
    sender: Sender<Tagged<T>>,
    revision: Revision,
}

impl<T> BridgeSender<T> {
    /// The revision this sender tags its values with.
    pub fn revision(&self) -> Revision {
        self.revision
    }

    /// Tag `payload` with this sender's revision and hand it to the UI.
    ///
    /// Returns `Err` when the UI side is gone — the receiver was dropped or its
    /// task ended — which is the worker's signal to stop.
    pub fn send(&self, payload: T) -> Result<(), SendError<Tagged<T>>> {
        self.sender
            .send_blocking(Tagged::new(self.revision, payload))
    }

    /// True once the UI side is gone, without sending anything.
    pub fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }

    /// The same channel, tagging later values with a new revision.
    ///
    /// A worker that outlives one generation of work — a reconnecting SSE
    /// stream, say — uses this so values from the old generation still get
    /// dropped while its own keep flowing.
    pub fn rebind(&self, revision: Revision) -> Self {
        BridgeSender {
            sender: self.sender.clone(),
            revision,
        }
    }
}

impl<T> Clone for BridgeSender<T> {
    fn clone(&self) -> Self {
        BridgeSender {
            sender: self.sender.clone(),
            revision: self.revision,
        }
    }
}

/// The UI's end of a [`Bridge`]: what a worker thread sends arrives here.
pub struct Bridge<T> {
    receiver: Receiver<Tagged<T>>,
}

impl<T: Send + 'static> Bridge<T> {
    /// Starts `worker` on a thread of its own and returns the channel its
    /// values arrive on.
    ///
    /// The worker runs on a plain OS thread, so it may block on sockets, files
    /// and subprocesses; the UI thread never waits for it. Ignore the
    /// [`Worker`] handle to let the thread finish on its own, or keep it to
    /// join it (tests do).
    pub fn spawn(
        revision: Revision,
        worker: impl FnOnce(BridgeSender<T>) + Send + 'static,
    ) -> (Bridge<T>, Worker) {
        let (sender, receiver) = async_channel::unbounded();
        let handle = thread::spawn(move || worker(BridgeSender { sender, revision }));
        (Bridge { receiver }, Worker(handle))
    }

    /// The next value from the worker; `Err` means every sender is gone and
    /// nothing more will arrive.
    ///
    /// The channel is runtime-agnostic: awaiting it parks a GPUI task without
    /// occupying the UI thread, and the worker's `send` wakes it.
    pub async fn recv(&self) -> Result<Tagged<T>, RecvError> {
        self.receiver.recv().await
    }

    /// Applies every value the worker sends to `owner` on the UI thread, in
    /// order, until the worker is done or `owner` is released.
    ///
    /// `live` reports the revision `owner` currently wants: a value from any
    /// other revision is dropped instead of applied. `apply` runs inside the
    /// update and is responsible for `cx.notify()`.
    pub fn drive_into<E>(
        self,
        cx: &mut Context<E>,
        live: impl Fn(&E) -> Revision + 'static,
        apply: impl Fn(&mut E, T, &mut Context<E>) + 'static,
    ) -> Task<()>
    where
        E: 'static,
    {
        let receiver = self.receiver;
        cx.spawn(async move |owner, cx| {
            while let Ok(Tagged { revision, payload }) = receiver.recv().await {
                let updated = owner.update(cx, |owner, cx| {
                    if live(owner) != revision {
                        return;
                    }
                    apply(owner, payload, cx);
                });
                if updated.is_err() {
                    // The owner is released; nothing is left to apply to.
                    return;
                }
            }
        })
    }
}

/// Handle to a bridge's worker thread.
pub struct Worker(JoinHandle<()>);

impl Worker {
    /// Blocks until the worker is done. Only for callers that own the thread's
    /// lifetime (tests); production code drops this handle instead.
    pub fn join(self) {
        let _ = self.0.join();
    }

    pub fn is_finished(&self) -> bool {
        self.0.is_finished()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::thread::ThreadId;

    use gpui_kit::{AppContext as _, Entity, TestAppContext};

    /// A stand-in for the views the app drives from worker results.
    struct Sink {
        revision: Revision,
        applied: Vec<i32>,
    }

    /// Starts `bridge` driving `sink`, and reports which thread each value was
    /// applied on. The task is dropped at the end of the test.
    fn drive(
        sink: &Entity<Sink>,
        bridge: Bridge<i32>,
        cx: &mut TestAppContext,
        applied_on: Arc<Mutex<Option<ThreadId>>>,
    ) -> gpui_kit::Task<()> {
        sink.update(cx, move |_sink, cx| {
            bridge.drive_into(
                cx,
                |sink: &Sink| sink.revision,
                move |sink, value, cx| {
                    *applied_on.lock().unwrap() = Some(thread::current().id());
                    sink.applied.push(value);
                    cx.notify();
                },
            )
        })
    }

    /// Values cross a real thread boundary and land on the UI thread with their
    /// revision intact, in order.
    #[gpui_kit::test]
    fn worker_values_reach_the_ui_thread(cx: &mut TestAppContext) {
        let worker_thread = Arc::new(Mutex::new(None));
        let seen = worker_thread.clone();
        let (bridge, worker) = Bridge::spawn(Revision::new(7), move |tx| {
            *seen.lock().unwrap() = Some(thread::current().id());
            for value in 0..5 {
                assert!(tx.send(value).is_ok());
            }
        });
        let sink = cx.update(|cx| {
            cx.new(|_| Sink {
                revision: Revision::new(7),
                applied: Vec::new(),
            })
        });
        let applied_thread: Arc<Mutex<Option<ThreadId>>> = Arc::new(Mutex::new(None));
        let task = drive(&sink, bridge, cx, applied_thread.clone());

        worker.join();
        cx.run_until_parked();

        cx.update(|cx| assert_eq!(sink.read(cx).applied, vec![0, 1, 2, 3, 4]));
        assert!(task.is_ready());
        assert_ne!(
            *worker_thread.lock().unwrap(),
            *applied_thread.lock().unwrap(),
            "the values must have crossed a thread boundary"
        );
    }

    /// A value from a superseded revision is consumed and dropped, so a slow
    /// answer cannot overwrite newer state.
    #[gpui_kit::test]
    fn stale_values_are_dropped(cx: &mut TestAppContext) {
        let (bridge, worker) = Bridge::spawn(Revision::new(1), |tx| {
            for value in 0..4 {
                if tx.send(value).is_err() {
                    return;
                }
            }
        });
        // The view has moved on to the next generation of work.
        let sink = cx.update(|cx| {
            cx.new(|_| Sink {
                revision: Revision::new(2),
                applied: Vec::new(),
            })
        });
        let task = drive(&sink, bridge, cx, Arc::new(Mutex::new(None)));

        worker.join();
        cx.run_until_parked();

        cx.update(|cx| {
            assert!(
                sink.read(cx).applied.is_empty(),
                "stale values are dropped instead of applied"
            )
        });
        // The task ends only after the receiver saw every value and the worker
        // hung up, so nothing was left sitting in the channel.
        assert!(task.is_ready());
    }

    /// Awaiting the bridge parks the GPUI task instead of occupying the UI
    /// thread: the frame loop keeps draining while the worker is still working,
    /// and the value lands only once the worker sends it from its own thread.
    #[gpui_kit::test]
    fn awaiting_the_bridge_parks_the_ui_task(cx: &mut TestAppContext) {
        // A parked GPUI task woken by a real thread is exactly what the
        // deterministic test scheduler rejects, so let this test's scheduler
        // accept external activity — the supported switch for a test that talks
        // to real I/O.
        cx.dispatcher.allow_parking();

        // "The worker is still working" is a condition this test owns rather than a
        // race against a sleep: the worker blocks on this channel until the test has
        // read everything it wants to read while the worker is mid-flight. A sleep
        // here would be a coin toss on a loaded machine — it can elapse before the
        // assertions below run, and then the worker is finished after all.
        let (release, released) = mpsc::channel::<()>();
        let (bridge, worker) = Bridge::spawn(Revision::new(1), move |tx| {
            released.recv().unwrap();
            assert!(tx.send(1).is_ok());
        });
        let sink = cx.update(|cx| {
            cx.new(|_| Sink {
                revision: Revision::new(1),
                applied: Vec::new(),
            })
        });
        let task = drive(&sink, bridge, cx, Arc::new(Mutex::new(None)));

        // The UI side drains every ready task and returns while the worker is
        // still held: awaiting the bridge blocked nobody.
        cx.run_until_parked();
        assert!(!worker.is_finished(), "the worker is still working");
        assert!(!task.is_ready());
        cx.update(|cx| assert!(sink.read(cx).applied.is_empty()));

        // Let the worker send, and only then is the task runnable again.
        release.send(()).unwrap();
        worker.join();
        cx.run_until_parked();

        cx.update(|cx| assert_eq!(sink.read(cx).applied, vec![1]));
    }

    /// `send` reports a closed bridge, and `rebind` retags a worker that
    /// outlives one generation of work.
    #[test]
    fn send_reports_a_closed_bridge_and_rebind_retags() {
        let (release, released) = mpsc::channel::<()>();
        let (bridge, worker) = Bridge::spawn(Revision::new(1), move |tx| {
            released.recv().unwrap();
            let retagged = tx.rebind(Revision::new(2));
            assert_eq!(retagged.revision(), Revision::new(2));
            assert_eq!(retagged.revision(), tx.revision().next());
            assert!(retagged.is_closed());
            assert!(retagged.send(1).is_err());
        });

        drop(bridge);
        release.send(()).unwrap();
        worker.join();
    }
}

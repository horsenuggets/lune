use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use async_channel::{Receiver, Sender, TrySendError, bounded};
use notify::Event;

use super::event::{WatchEvent, WatchTarget};

/**
    The maximum number of events that may wait for delivery.

    Events are only drained while no callback is running, so without a limit,
    a slow or suspended callback on a busy directory would grow memory forever.
*/
pub const QUEUE_CAPACITY: usize = 1024;

/**
    Creates a bounded queue of watch events.

    The forwarding half runs on a thread owned by `notify`, and never blocks it.
    When the queue is full, new events are dropped, and the receiving half
    emits a single rescan event so that callbacks know changes were missed.
*/
pub fn event_queue(target: WatchTarget, capacity: usize) -> (EventForwarder, EventQueue) {
    let (sender, receiver) = bounded(capacity);
    let overflowed = Arc::new(AtomicBool::new(false));
    let forwarder = EventForwarder {
        overflowed: Arc::clone(&overflowed),
        sender,
        target,
    };
    let queue = EventQueue {
        overflowed,
        receiver,
    };
    (forwarder, queue)
}

/**
    The sending half of an event queue, which translates and filters raw
    `notify` events before they are queued, so that ignored events (access
    events, siblings of a watched file) never take up space in the queue.
*/
#[derive(Debug)]
pub struct EventForwarder {
    overflowed: Arc<AtomicBool>,
    sender: Sender<WatchEvent>,
    target: WatchTarget,
}

impl EventForwarder {
    pub fn forward(&self, res: notify::Result<Event>) {
        // TODO: Propagate watcher errors somehow
        let Ok(event) = res else {
            return;
        };
        // NOTE: Backends flag events that mean changes were missed, such as
        // FSEvents recovery events, and those may come with a path that would
        // otherwise be filtered out or delivered as an ordinary event
        let event = if event.need_rescan() {
            WatchEvent::rescan()
        } else if let Some(event) = self.target.translate(event.kind, &event.paths) {
            event
        } else {
            return;
        };
        if let Err(TrySendError::Full(_)) = self.sender.try_send(event) {
            self.overflowed.store(true, Ordering::SeqCst);
        }
    }
}

/**
    The receiving half of an event queue.
*/
#[derive(Debug)]
pub struct EventQueue {
    overflowed: Arc<AtomicBool>,
    receiver: Receiver<WatchEvent>,
}

impl EventQueue {
    /**
        Returns a rescan event if any events were dropped since the last call.

        Events that are still queued at that point are discarded, since they
        are older than the rescan, and delivering them afterwards could undo
        whatever state the callback recovered through rescanning.
    */
    pub fn take_overflow(&self) -> Option<WatchEvent> {
        if !self.overflowed.swap(false, Ordering::SeqCst) {
            return None;
        }
        while self.receiver.try_recv().is_ok() {}
        Some(WatchEvent::rescan())
    }

    /**
        Waits for the next queued event.

        Returns `None` once the forwarding half has been dropped.
    */
    pub async fn recv(&self) -> Option<WatchEvent> {
        self.receiver.recv().await.ok()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use futures_lite::future::block_on;
    use notify::{
        EventKind,
        event::{AccessKind, CreateKind, Flag},
    };

    use super::*;

    fn root() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from("C:\\dir")
        } else {
            PathBuf::from("/dir")
        }
    }

    fn create(name: &str) -> notify::Result<Event> {
        Ok(Event::new(EventKind::Create(CreateKind::File)).add_path(root().join(name)))
    }

    #[test]
    fn forwards_translated_events_in_order() {
        let (forwarder, queue) = event_queue(WatchTarget::new(root(), None, None), 8);
        forwarder.forward(create("a"));
        forwarder.forward(create("b"));
        let first = block_on(queue.recv()).unwrap();
        let second = block_on(queue.recv()).unwrap();
        assert_eq!(first.paths, vec![root().join("a")]);
        assert_eq!(second.paths, vec![root().join("b")]);
        assert!(queue.take_overflow().is_none());
    }

    #[test]
    fn ignored_events_never_take_up_space() {
        let target = WatchTarget::new(root(), None, Some(root().join("watched")));
        let (forwarder, queue) = event_queue(target, 1);
        let access =
            Event::new(EventKind::Access(AccessKind::Any)).add_path(root().join("watched"));
        forwarder.forward(Ok(access));
        forwarder.forward(create("sibling"));
        forwarder.forward(Err(notify::Error::generic("ignored")));
        forwarder.forward(create("watched"));
        assert!(queue.take_overflow().is_none());
        let event = block_on(queue.recv()).unwrap();
        assert_eq!(event.paths, vec![root().join("watched")]);
    }

    #[test]
    fn overflow_emits_one_rescan_and_discards_stale_events() {
        let (forwarder, queue) = event_queue(WatchTarget::new(root(), None, None), 2);
        for name in ["a", "b", "c", "d"] {
            forwarder.forward(create(name));
        }
        assert_eq!(queue.take_overflow(), Some(WatchEvent::rescan()));
        assert!(queue.take_overflow().is_none());

        // Events queued before the rescan are stale, but newer ones still arrive
        forwarder.forward(create("e"));
        assert_eq!(
            block_on(queue.recv()).unwrap().paths,
            vec![root().join("e")]
        );
    }

    #[test]
    fn flagged_events_become_rescans_for_every_target() {
        let flagged = || {
            Event::new(EventKind::Other)
                .add_path(root().join("sub"))
                .set_flag(Flag::Rescan)
        };
        let targets = [
            WatchTarget::new(root(), None, None),
            WatchTarget::new(root(), None, Some(root().join("watched"))),
        ];
        for target in targets {
            let (forwarder, queue) = event_queue(target, 4);
            forwarder.forward(Ok(flagged()));
            assert_eq!(block_on(queue.recv()), Some(WatchEvent::rescan()));
        }
    }

    #[test]
    fn recv_ends_once_forwarder_is_dropped() {
        let (forwarder, queue) = event_queue(WatchTarget::new(root(), None, None), 2);
        drop(forwarder);
        assert!(block_on(queue.recv()).is_none());
    }
}

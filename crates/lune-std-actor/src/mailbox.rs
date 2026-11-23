use std::{
    collections::HashMap,
    sync::{Mutex, PoisonError},
};

use async_channel::{Receiver, Sender};

use crate::transfer::Transfer;

#[derive(Debug, Default)]
struct MailboxState {
    closed: bool,
    topics: HashMap<String, (Sender<Transfer>, Receiver<Transfer>)>,
}

/**
    One direction of messages between an actor and its parent, grouped by topic.

    Each topic keeps its messages in order. Closing the mailbox stops new messages, while
    messages that were already sent can still be received.
*/
#[derive(Debug, Default)]
pub(crate) struct Mailbox {
    state: Mutex<MailboxState>,
}

impl Mailbox {
    fn with_topic<T>(
        &self,
        topic: &str,
        f: impl FnOnce(&(Sender<Transfer>, Receiver<Transfer>), bool) -> T,
    ) -> T {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let closed = state.closed;
        let channel = state.topics.entry(topic.to_string()).or_insert_with(|| {
            let channel = async_channel::unbounded();
            if closed {
                channel.0.close();
            }
            channel
        });
        f(channel, closed)
    }

    /// Queues a message, returning `false` when the mailbox no longer accepts messages.
    pub(crate) fn send(&self, topic: &str, value: Transfer) -> bool {
        self.with_topic(topic, |(sender, _), closed| {
            !closed && sender.try_send(value).is_ok()
        })
    }

    /// Returns the receiving end for a topic, which errors once it is closed and empty.
    pub(crate) fn receiver(&self, topic: &str) -> Receiver<Transfer> {
        self.with_topic(topic, |(_, receiver), _| receiver.clone())
    }

    /// Stops accepting messages for every current and future topic.
    pub(crate) fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.closed = true;
        for (sender, _) in state.topics.values() {
            sender.close();
        }
    }

    /// Whether the mailbox has stopped accepting messages.
    #[cfg(test)]
    pub(crate) fn is_closed(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .closed
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // Messages hold exact integers, so exact comparisons are intended
mod tests {
    use async_io::block_on;

    use super::*;

    fn number(value: &Transfer) -> f64 {
        match value {
            Transfer::Number(n) => *n,
            other => panic!("expected a number, got {other:?}"),
        }
    }

    #[test]
    fn keeps_topics_separate_and_ordered() {
        let mailbox = Mailbox::default();
        assert!(mailbox.send("A", Transfer::Number(1.0)));
        assert!(mailbox.send("B", Transfer::Number(2.0)));
        assert!(mailbox.send("A", Transfer::Number(3.0)));
        let a = mailbox.receiver("A");
        assert_eq!(number(&block_on(a.recv()).unwrap()), 1.0);
        assert_eq!(number(&block_on(a.recv()).unwrap()), 3.0);
        assert_eq!(
            number(&block_on(mailbox.receiver("B").recv()).unwrap()),
            2.0
        );
    }

    #[test]
    fn closing_keeps_queued_messages_and_rejects_new_ones() {
        let mailbox = Mailbox::default();
        assert!(mailbox.send("A", Transfer::Number(1.0)));
        mailbox.close();
        assert!(mailbox.is_closed());
        assert!(!mailbox.send("A", Transfer::Number(2.0)));
        assert!(!mailbox.send("New", Transfer::Number(3.0)));
        let a = mailbox.receiver("A");
        assert_eq!(number(&block_on(a.recv()).unwrap()), 1.0);
        assert!(block_on(a.recv()).is_err());
        assert!(block_on(mailbox.receiver("New").recv()).is_err());
    }
}

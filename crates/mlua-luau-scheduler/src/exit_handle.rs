use std::sync::{Arc, Mutex, PoisonError};

use async_channel::{Receiver, Sender};

#[derive(Debug)]
struct ExitHandleInner {
    code: Mutex<Option<u8>>,
    signal_rx: Receiver<()>,
    signal_tx: Sender<()>,
}

/**
    A thread-safe handle that stops a [`crate::Scheduler`] from any OS thread.

    The scheduler's own exit state is bound to the thread that runs it, so this handle
    lets other threads (such as the owner of an actor) ask it to stop. The first exit
    code that was requested wins, and requesting an exit wakes an idle scheduler.
*/
#[derive(Debug, Clone)]
pub struct ExitHandle {
    inner: Arc<ExitHandleInner>,
}

impl ExitHandle {
    pub(crate) fn new() -> Self {
        let (signal_tx, signal_rx) = async_channel::bounded(1);
        Self {
            inner: Arc::new(ExitHandleInner {
                code: Mutex::new(None),
                signal_rx,
                signal_tx,
            }),
        }
    }

    /**
        Requests that the scheduler stops with the given exit code.

        Only the first requested code is kept. Later requests still wake the scheduler.
    */
    pub fn exit(&self, code: u8) {
        {
            let mut current = self
                .inner
                .code
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if current.is_none() {
                *current = Some(code);
            }
        }
        self.inner.signal_tx.close();
    }

    /**
        Returns the exit code requested through this handle, if any.
    */
    #[must_use]
    pub fn code(&self) -> Option<u8> {
        *self
            .inner
            .code
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) async fn listen(&self) {
        // The channel never carries messages. Closing it is the one-time exit signal.
        let _ = self.inner.signal_rx.recv().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_first_exit_code() {
        let handle = ExitHandle::new();
        assert_eq!(handle.code(), None);
        handle.exit(3);
        handle.exit(4);
        assert_eq!(handle.code(), Some(3));
    }

    #[test]
    fn wakes_listeners_from_another_thread() {
        let handle = ExitHandle::new();
        let remote = handle.clone();
        let thread = std::thread::spawn(move || remote.exit(7));
        async_io::block_on(handle.listen());
        thread.join().expect("exit thread panicked");
        assert_eq!(handle.code(), Some(7));
    }
}

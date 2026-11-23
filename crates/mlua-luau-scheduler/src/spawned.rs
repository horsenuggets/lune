use std::{
    future::Future,
    panic::{AssertUnwindSafe, resume_unwind},
    pin::Pin,
    sync::{Arc, Mutex, PoisonError},
    task::{Context, Poll},
};

use async_channel::{Receiver, Sender};
use async_executor::{Executor, Task};
use futures_lite::FutureExt;

use crate::util::panic_message;

/**
    Carries panics of tasks that nobody will ever await back to the scheduler, which
    reports them through its error callback. Tasks may run on any thread, so the sink
    only ever sends plain messages.
*/
#[derive(Clone)]
pub(crate) struct PanicSink {
    sender: Sender<String>,
}

impl PanicSink {
    pub fn new() -> (Self, Receiver<String>) {
        let (sender, receiver) = async_channel::unbounded();
        (Self { sender }, receiver)
    }

    pub fn report(&self, message: String) {
        // The receiver only closes once the scheduler stopped running, and a panic
        // after that has no run left to fail, so a failed send is safe to ignore.
        let _ = self.sender.try_send(message);
    }
}

/// Formats the error a scheduler reports for a panic that no thread observed.
pub(crate) fn background_panic_message(payload: &str) -> String {
    format!("A background task panicked with {payload}.")
}

#[derive(Default)]
struct PanicState {
    detached: bool,
    observed: bool,
    pending: Option<String>,
}

/**
    A handle to a task spawned through [`crate::LuaSpawnExt`].

    Awaiting it resumes a panic of the task in the awaiting code, exactly like the
    underlying executor task, so the awaiting Lua thread fails with that panic. A panic
    that no code will observe, because the handle was detached or dropped first, is
    instead reported through the scheduler's error callback, so it can never be lost.
*/
pub struct SpawnedTask<T> {
    task: Option<Task<T>>,
    state: Arc<Mutex<PanicState>>,
    sink: PanicSink,
}

impl<T: Send + 'static> SpawnedTask<T> {
    pub(crate) fn spawn(
        executor: &Executor<'static>,
        future: impl Future<Output = T> + Send + 'static,
        sink: PanicSink,
    ) -> Self {
        let state = Arc::new(Mutex::new(PanicState::default()));
        let task_state = Arc::clone(&state);
        let task_sink = sink.clone();
        let task = executor.spawn(async move {
            match AssertUnwindSafe(future).catch_unwind().await {
                Ok(value) => value,
                Err(payload) => {
                    let message = background_panic_message(panic_message(payload.as_ref()));
                    let mut state = task_state.lock().unwrap_or_else(PoisonError::into_inner);
                    if state.detached {
                        task_sink.report(message);
                    } else {
                        state.pending = Some(message);
                    }
                    drop(state);
                    // The executor stores the panic for whoever awaits the handle.
                    resume_unwind(payload)
                }
            }
        });
        Self {
            task: Some(task),
            state,
            sink,
        }
    }
}

impl<T> SpawnedTask<T> {
    /**
        Detaches the task, letting it run to completion in the background.

        A panic of a detached task is reported through the scheduler's error callback,
        including a panic that happened before the task was detached.
    */
    pub fn detach(mut self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.detached = true;
        if let Some(message) = state.pending.take() {
            self.sink.report(message);
        }
        drop(state);
        if let Some(task) = self.task.take() {
            task.detach();
        }
    }
}

impl<T> Future for SpawnedTask<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        let task = self
            .task
            .as_mut()
            .expect("a spawned task is polled after completion");
        match std::panic::catch_unwind(AssertUnwindSafe(|| Pin::new(task).poll(cx))) {
            Ok(poll) => poll,
            Err(payload) => {
                // The awaiting code receives the panic, so it is not reported again.
                self.state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .observed = true;
                resume_unwind(payload)
            }
        }
    }
}

impl<T> Drop for SpawnedTask<T> {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.observed
            && let Some(message) = state.pending.take()
        {
            // The handle was dropped after the task panicked without anyone awaiting it.
            self.sink.report(message);
        }
    }
}

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
    ownerless: bool,
    observed: bool,
    pending: Option<String>,
}

/**
    A handle to a task spawned through [`crate::LuaSpawnExt`].

    Awaiting it resumes a panic of the task in the awaiting code, exactly like the
    underlying executor task, so the awaiting Lua thread fails with that panic. A panic
    of a detached task, or of a task whose handle is dropped while it runs, is instead
    reported through the scheduler's error callback.

    One case is still not reported. Dropping the handle cancels the task, but a closure
    given to `spawn_blocking` keeps running on its blocking thread, and a panic it raises
    after the cancellation is lost. The scheduler cannot observe that panic, so the run
    may still finish successfully despite the blocking operation's failure.
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
                    if state.ownerless {
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
        state.ownerless = true;
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
        // A poll already running on another thread can still panic after cancellation.
        state.ownerless = true;
        if !state.observed
            && let Some(message) = state.pending.take()
        {
            // The handle was dropped after the task panicked without anyone awaiting it.
            self.sink.report(message);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, thread, time::Duration};

    use super::*;

    fn assert_reported_once(reports: &Receiver<String>, message: &str) {
        assert_eq!(
            reports.try_recv().unwrap(),
            background_panic_message(message)
        );
        assert!(
            reports.try_recv().is_err(),
            "a panic must only be reported once"
        );
    }

    #[test]
    fn dropping_a_handle_during_poll_reports_a_later_panic_once() {
        let executor = Arc::new(Executor::new());
        let (sink, reports) = PanicSink::new();
        let (started, polling) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let task: SpawnedTask<()> = SpawnedTask::spawn(
            &executor,
            async move {
                started.send(()).unwrap();
                released.recv().unwrap();
                panic!("panic after the handle was dropped");
            },
            sink,
        );
        let runner = thread::spawn(move || executor.try_tick());
        polling.recv_timeout(Duration::from_secs(10)).unwrap();
        drop(task);
        release.send(()).unwrap();
        assert!(runner.join().unwrap());
        assert_reported_once(&reports, "panic after the handle was dropped");
    }

    #[test]
    fn awaited_panics_do_not_also_become_background_errors() {
        let executor = Executor::new();
        let (sink, reports) = PanicSink::new();
        let task: SpawnedTask<()> =
            SpawnedTask::spawn(&executor, async { panic!("awaited panic") }, sink);
        let result =
            futures_lite::future::block_on(executor.run(AssertUnwindSafe(task).catch_unwind()));
        assert_eq!(panic_message(result.unwrap_err().as_ref()), "awaited panic");
        assert!(reports.try_recv().is_err());
    }

    #[test]
    fn detached_panics_are_reported_once_before_or_after_completion() {
        for detach_after_panic in [false, true] {
            let executor = Executor::new();
            let (sink, reports) = PanicSink::new();
            let task: SpawnedTask<()> =
                SpawnedTask::spawn(&executor, async { panic!("detached panic") }, sink);
            if detach_after_panic {
                assert!(executor.try_tick());
                assert!(reports.try_recv().is_err());
                task.detach();
            } else {
                task.detach();
                assert!(executor.try_tick());
            }
            assert_reported_once(&reports, "detached panic");
        }
    }

    #[test]
    fn dropping_a_handle_after_a_panic_reports_it_once() {
        let executor = Executor::new();
        let (sink, reports) = PanicSink::new();
        let task: SpawnedTask<()> =
            SpawnedTask::spawn(&executor, async { panic!("unobserved panic") }, sink);
        assert!(executor.try_tick());
        assert!(reports.try_recv().is_err());
        drop(task);
        assert_reported_once(&reports, "unobserved panic");
    }

    #[test]
    fn dropping_a_handle_cancels_a_pending_future_without_background_errors() {
        let executor = Executor::new();
        let (sink, reports) = PanicSink::new();
        let (sender, receiver) = async_channel::bounded::<()>(1);
        let task = SpawnedTask::spawn(
            &executor,
            async move {
                futures_lite::future::pending::<()>().await;
                sender.send(()).await.unwrap();
            },
            sink,
        );
        assert!(executor.try_tick());
        assert!(!receiver.is_closed());
        drop(task);
        while executor.try_tick() {}
        assert!(
            receiver.is_closed(),
            "cancellation must drop the pending future"
        );
        assert!(reports.try_recv().is_err());
    }

    #[test]
    fn successful_tasks_return_values_without_background_errors() {
        let executor = Executor::new();
        let (sink, reports) = PanicSink::new();
        let task = SpawnedTask::spawn(&executor, async { 42 }, sink);
        assert_eq!(futures_lite::future::block_on(executor.run(task)), 42);
        assert!(reports.try_recv().is_err());
    }
}

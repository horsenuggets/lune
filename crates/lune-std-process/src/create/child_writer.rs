use std::{
    io::{self, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

use async_lock::Mutex as AsyncMutex;
use async_process::ChildStdin as AsyncChildStdin;
use futures_lite::prelude::*;

use bstr::BString;
use mlua::prelude::*;

#[derive(Debug)]
enum WriteRequest {
    Write(Vec<u8>, async_channel::Sender<io::Result<()>>),
    Close(async_channel::Sender<io::Result<()>>),
}

#[derive(Debug)]
struct OwnedWriter<T> {
    stream: Option<T>,
    sender: async_channel::Sender<WriteRequest>,
    receiver: Option<async_channel::Receiver<WriteRequest>>,
}

impl<T: Write + Send + 'static> OwnedWriter<T> {
    fn new(stream: T) -> Self {
        let (sender, receiver) = async_channel::bounded(1);
        Self {
            stream: Some(stream),
            sender,
            receiver: Some(receiver),
        }
    }

    fn start(&mut self) -> io::Result<()> {
        if let Some(mut stream) = self.stream.take() {
            let receiver = self.receiver.take().expect("unstarted writer");
            // Do not queue native pipe writes behind blocking readers in a shared pool.
            thread::Builder::new()
                .name("lune-owned-writer".into())
                .spawn(move || {
                    while let Ok(request) = receiver.recv_blocking() {
                        match request {
                            WriteRequest::Write(bytes, result) => {
                                let written =
                                    stream.write_all(&bytes).and_then(|()| stream.flush());
                                let _ = result.try_send(written);
                            }
                            WriteRequest::Close(result) => {
                                drop(stream);
                                let _ = result.try_send(Ok(()));
                                return;
                            }
                        }
                    }
                })?;
        }
        Ok(())
    }

    async fn write(&mut self, bytes: Vec<u8>) -> io::Result<()> {
        self.start()?;
        let (sender, receiver) = async_channel::bounded(1);
        self.sender
            .send(WriteRequest::Write(bytes, sender))
            .await
            .map_err(io::Error::other)?;
        receiver.recv().await.map_err(io::Error::other)?
    }

    async fn close(&mut self) -> io::Result<()> {
        if self.stream.take().is_some() {
            return Ok(());
        }
        let (sender, receiver) = async_channel::bounded(1);
        self.sender
            .send(WriteRequest::Close(sender))
            .await
            .map_err(io::Error::other)?;
        receiver.recv().await.map_err(io::Error::other)?
    }
}

impl<T> Drop for OwnedWriter<T> {
    fn drop(&mut self) {
        self.sender.close();
    }
}

// Inner (plumbing) implementation

#[derive(Debug)]
struct ChildWriterInner(Option<Writer>);

#[derive(Debug)]
enum Writer {
    Ordinary(AsyncChildStdin),
    Owned(OwnedWriter<std::process::ChildStdin>),
}

impl ChildWriterInner {
    async fn write(&mut self, data: Vec<u8>) -> Result<(), std::io::Error> {
        match self.0.as_mut() {
            Some(Writer::Ordinary(stdin)) => stdin.write_all(&data).await?,
            Some(Writer::Owned(stdin)) => stdin.write(data).await?,
            None => {}
        }
        Ok(())
    }

    async fn close(&mut self) -> Result<(), std::io::Error> {
        match self.0.take() {
            Some(Writer::Ordinary(mut stdin)) => stdin.flush().await?,
            Some(Writer::Owned(mut stdin)) => stdin.close().await?,
            None => {}
        }
        Ok(())
    }
}

impl From<AsyncChildStdin> for ChildWriterInner {
    fn from(stdin: AsyncChildStdin) -> Self {
        ChildWriterInner(Some(Writer::Ordinary(stdin)))
    }
}

impl From<Option<AsyncChildStdin>> for ChildWriterInner {
    fn from(stdin: Option<AsyncChildStdin>) -> Self {
        ChildWriterInner(stdin.map(Writer::Ordinary))
    }
}

// Outer (lua-accessible, clonable) implementation

#[derive(Debug, Clone)]
pub struct ChildWriter {
    inner: Arc<AsyncMutex<ChildWriterInner>>,
    canceled: Arc<AtomicBool>,
    cancel_sender: Option<async_channel::Sender<WriteRequest>>,
}

impl ChildWriter {
    pub(super) fn owned(stdin: std::process::ChildStdin) -> Self {
        let writer = OwnedWriter::new(stdin);
        let cancel_sender = writer.sender.clone();
        let mut this = Self::from(ChildWriterInner(Some(Writer::Owned(writer))));
        this.cancel_sender = Some(cancel_sender);
        this
    }

    pub(super) fn cancel(&self) {
        self.canceled.store(true, Ordering::SeqCst);
        // Closing the worker queue cannot depend on a write future resuming or releasing
        // its mutex. Once native termination unblocks that write, the worker must exit.
        if let Some(sender) = &self.cancel_sender {
            sender.close();
        }
        if let Some(mut inner) = self.inner.try_lock() {
            inner.0.take();
        }
    }
}

impl LuaUserData for ChildWriter {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_async_method("write", |_, this, data: BString| {
            let inner = this.inner.clone();
            let data = data.to_vec();
            let canceled = Arc::clone(&this.canceled);
            async move {
                let mut inner = inner.lock().await;
                if canceled.load(Ordering::SeqCst) {
                    inner.0.take();
                    return Err(LuaError::external(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "Owned process stdin was closed",
                    )));
                }
                let result = inner.write(data).await.into_lua_err();
                if canceled.load(Ordering::SeqCst) {
                    inner.0.take();
                }
                result
            }
        });
        methods.add_async_method("close", |_, this, (): ()| {
            let inner = this.inner.clone();
            async move {
                let mut inner = inner.lock().await;
                inner.close().await.into_lua_err()
            }
        });
    }
}

impl<T: Into<ChildWriterInner>> From<T> for ChildWriter {
    fn from(inner: T) -> Self {
        Self {
            inner: Arc::new(AsyncMutex::new(inner.into())),
            canceled: Arc::new(AtomicBool::new(false)),
            cancel_sender: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, time::Duration};

    use super::*;

    struct BlockedWriter {
        started: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        dropped: mpsc::Sender<()>,
    }

    impl Write for BlockedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.started.send(()).unwrap();
            self.release.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Drop for BlockedWriter {
        fn drop(&mut self) {
            let _ = self.dropped.send(());
        }
    }

    #[test]
    fn canceled_locked_write_closes_worker_without_resuming_coroutine() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (dropped_tx, dropped_rx) = mpsc::channel();
        let mut writer = OwnedWriter::new(BlockedWriter {
            started: started_tx,
            release: release_rx,
            dropped: dropped_tx,
        });
        let child = ChildWriter {
            inner: Arc::new(AsyncMutex::new(ChildWriterInner(None))),
            canceled: Arc::new(AtomicBool::new(false)),
            cancel_sender: Some(writer.sender.clone()),
        };
        let mut write = Box::pin(async {
            let _lock = child.inner.lock().await;
            writer.write(vec![1]).await
        });
        assert!(
            futures_lite::future::block_on(futures_lite::future::poll_once(&mut write)).is_none()
        );
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(child.inner.try_lock().is_none());
        child.cancel();
        drop(write);
        // Native termination releases an in-flight pipe write. The canceled coroutine
        // never resumes, and both the child handle and writer sender remain retained.
        release_tx.send(()).unwrap();
        dropped_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(writer.sender.is_closed());
        assert!(child.canceled.load(Ordering::SeqCst));
    }

    #[test]
    fn canceled_backpressured_close_drops_worker_with_retained_cancel_sender() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (dropped_tx, dropped_rx) = mpsc::channel();
        let mut writer = OwnedWriter::new(BlockedWriter {
            started: started_tx,
            release: release_rx,
            dropped: dropped_tx,
        });
        let retained = writer.sender.clone();
        let mut first = Box::pin(writer.write(vec![1]));
        assert!(
            futures_lite::future::block_on(futures_lite::future::poll_once(&mut first)).is_none()
        );
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(first);
        let mut second = Box::pin(writer.write(vec![2]));
        assert!(
            futures_lite::future::block_on(futures_lite::future::poll_once(&mut second)).is_none()
        );
        drop(second);
        assert!(retained.is_full());
        let mut close = Box::pin(async move { writer.close().await });
        assert!(
            futures_lite::future::block_on(futures_lite::future::poll_once(&mut close)).is_none()
        );
        drop(close);
        assert!(retained.is_closed());
        release_tx.send(()).unwrap();
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        release_tx.send(()).unwrap();
        dropped_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    }
}

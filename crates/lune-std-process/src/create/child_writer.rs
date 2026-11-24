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
struct OwnedWriter {
    stream: Option<std::process::ChildStdin>,
    sender: async_channel::Sender<WriteRequest>,
    receiver: Option<async_channel::Receiver<WriteRequest>>,
}

impl OwnedWriter {
    fn new(stream: std::process::ChildStdin) -> Self {
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

// Inner (plumbing) implementation

#[derive(Debug)]
struct ChildWriterInner(Option<Writer>);

#[derive(Debug)]
enum Writer {
    Ordinary(AsyncChildStdin),
    Owned(OwnedWriter),
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
}

impl ChildWriter {
    pub(super) fn owned(stdin: std::process::ChildStdin) -> Self {
        Self::from(ChildWriterInner(Some(Writer::Owned(OwnedWriter::new(
            stdin,
        )))))
    }

    pub(super) fn cancel(&self) {
        self.canceled.store(true, Ordering::SeqCst);
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
        }
    }
}

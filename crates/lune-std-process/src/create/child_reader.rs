use std::{
    io::Read,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    thread,
};

use async_lock::Mutex as AsyncMutex;
use async_process::{ChildStderr as AsyncChildStderr, ChildStdout as AsyncChildStdout};
use futures_lite::{io, prelude::*};

use mlua::prelude::*;

const DEFAULT_BUFFER_SIZE: usize = 1024;

// Each owned pipe gets a dedicated, lazily started reader. Native blocking reads must
// not occupy the shared blocking executor that other I/O needs to make progress.
#[derive(Debug)]
struct OwnedReader<T> {
    stream: Option<T>,
    sender: Option<async_channel::Sender<std::io::Result<Vec<u8>>>>,
    receiver: Pin<Box<async_channel::Receiver<std::io::Result<Vec<u8>>>>>,
    buffer: Vec<u8>,
    offset: usize,
}

impl<T> OwnedReader<T> {
    fn new(stream: T) -> Self {
        let (sender, receiver) = async_channel::bounded(1);
        Self {
            stream: Some(stream),
            sender: Some(sender),
            receiver: Box::pin(receiver),
            buffer: Vec::new(),
            offset: 0,
        }
    }
}

impl<T: Read + Send + Unpin + 'static> AsyncRead for OwnedReader<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        if buffer.is_empty() {
            return Poll::Ready(Ok(0));
        }
        if let Some(mut stream) = this.stream.take() {
            let sender = this.sender.take().expect("unstarted reader");
            if let Err(error) = thread::Builder::new()
                .name("lune-owned-reader".into())
                .spawn(move || {
                    loop {
                        let mut chunk = vec![0_u8; 8192];
                        match stream.read(&mut chunk) {
                            Ok(0) => break,
                            Ok(size) => {
                                chunk.truncate(size);
                                if sender.send_blocking(Ok(chunk)).is_err() {
                                    break;
                                }
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                            Err(error) => {
                                let _ = sender.send_blocking(Err(error));
                                break;
                            }
                        }
                    }
                })
            {
                return Poll::Ready(Err(error));
            }
        }
        if this.offset == this.buffer.len() {
            match this.receiver.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => return Poll::Ready(Ok(0)),
                Poll::Ready(Some(Err(error))) => return Poll::Ready(Err(error)),
                Poll::Ready(Some(Ok(chunk))) => {
                    this.buffer = chunk;
                    this.offset = 0;
                }
            }
        }
        let size = buffer.len().min(this.buffer.len() - this.offset);
        buffer[..size].copy_from_slice(&this.buffer[this.offset..this.offset + size]);
        this.offset += size;
        Poll::Ready(Ok(size))
    }
}

// Inner (plumbing) implementation

#[derive(Debug)]
enum ChildReaderInner {
    None,
    Stdout(AsyncChildStdout),
    Stderr(AsyncChildStderr),
    OwnedStdout(OwnedReader<std::process::ChildStdout>),
    OwnedStderr(OwnedReader<std::process::ChildStderr>),
}

impl ChildReaderInner {
    async fn read(&mut self, size: usize) -> Result<Vec<u8>, std::io::Error> {
        if matches!(self, ChildReaderInner::None) {
            return Ok(Vec::new());
        }

        let mut buf = vec![0; size];

        let read = match self {
            ChildReaderInner::None => unreachable!(),
            ChildReaderInner::Stdout(stdout) => stdout.read(&mut buf).await?,
            ChildReaderInner::Stderr(stderr) => stderr.read(&mut buf).await?,
            ChildReaderInner::OwnedStdout(stdout) => stdout.read(&mut buf).await?,
            ChildReaderInner::OwnedStderr(stderr) => stderr.read(&mut buf).await?,
        };

        buf.truncate(read);

        Ok(buf)
    }

    async fn read_to_end(&mut self) -> Result<Vec<u8>, std::io::Error> {
        let mut buf = Vec::new();

        match self {
            ChildReaderInner::None => {}
            // `copy` is much faster compared to `read_to_end` when reading a large amount of data.
            ChildReaderInner::Stdout(stdout) => {
                io::copy(stdout, &mut buf).await?;
            }
            ChildReaderInner::Stderr(stderr) => {
                io::copy(stderr, &mut buf).await?;
            }
            ChildReaderInner::OwnedStdout(stdout) => {
                io::copy(stdout, &mut buf).await?;
            }
            ChildReaderInner::OwnedStderr(stderr) => {
                io::copy(stderr, &mut buf).await?;
            }
        }

        Ok(buf)
    }
}

impl From<AsyncChildStdout> for ChildReaderInner {
    fn from(stdout: AsyncChildStdout) -> Self {
        Self::Stdout(stdout)
    }
}

impl From<AsyncChildStderr> for ChildReaderInner {
    fn from(stderr: AsyncChildStderr) -> Self {
        Self::Stderr(stderr)
    }
}

impl From<Option<AsyncChildStdout>> for ChildReaderInner {
    fn from(stdout: Option<AsyncChildStdout>) -> Self {
        stdout.map_or(Self::None, Into::into)
    }
}

impl From<Option<AsyncChildStderr>> for ChildReaderInner {
    fn from(stderr: Option<AsyncChildStderr>) -> Self {
        stderr.map_or(Self::None, Into::into)
    }
}

impl From<std::process::ChildStdout> for ChildReaderInner {
    fn from(stdout: std::process::ChildStdout) -> Self {
        Self::OwnedStdout(OwnedReader::new(stdout))
    }
}

impl From<std::process::ChildStderr> for ChildReaderInner {
    fn from(stderr: std::process::ChildStderr) -> Self {
        Self::OwnedStderr(OwnedReader::new(stderr))
    }
}

// Outer (lua-accessible, clonable) implementation

#[derive(Debug, Clone)]
pub struct ChildReader {
    inner: Arc<AsyncMutex<ChildReaderInner>>,
}

impl LuaUserData for ChildReader {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_async_method("read", |lua, this, size: Option<usize>| {
            let inner = this.inner.clone();
            let size = size.unwrap_or(DEFAULT_BUFFER_SIZE);
            async move {
                let mut inner = inner.lock().await;
                let bytes = inner.read(size).await.into_lua_err()?;
                if bytes.is_empty() {
                    Ok(LuaValue::Nil)
                } else {
                    Ok(LuaValue::String(lua.create_string(bytes)?))
                }
            }
        });
        methods.add_async_method("readToEnd", |lua, this, (): ()| {
            let inner = this.inner.clone();
            async move {
                let mut inner = inner.lock().await;
                let bytes = inner.read_to_end().await.into_lua_err()?;
                Ok(lua.create_string(bytes))
            }
        });
    }
}

impl<T: Into<ChildReaderInner>> From<T> for ChildReader {
    fn from(inner: T) -> Self {
        Self {
            inner: Arc::new(AsyncMutex::new(inner.into())),
        }
    }
}

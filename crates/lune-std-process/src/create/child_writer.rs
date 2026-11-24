use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use async_lock::Mutex as AsyncMutex;
use async_process::ChildStdin as AsyncChildStdin;
use futures_lite::prelude::*;

use bstr::BString;
use mlua::prelude::*;

// Inner (plumbing) implementation

#[derive(Debug)]
struct ChildWriterInner(Option<Writer>);

#[derive(Debug)]
enum Writer {
    Ordinary(AsyncChildStdin),
    Owned(blocking::Unblock<std::process::ChildStdin>),
}

impl ChildWriterInner {
    async fn write(&mut self, data: Vec<u8>) -> Result<(), std::io::Error> {
        match self.0.as_mut() {
            Some(Writer::Ordinary(stdin)) => stdin.write_all(&data).await?,
            Some(Writer::Owned(stdin)) => {
                stdin.write_all(&data).await?;
                // Unblock buffers writes. Preserve write's completion/error semantics
                // by waiting until the bytes have actually reached the native pipe.
                stdin.flush().await?;
            }
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
        Self::from(ChildWriterInner(Some(Writer::Owned(
            blocking::Unblock::with_capacity(8192, stdin),
        ))))
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

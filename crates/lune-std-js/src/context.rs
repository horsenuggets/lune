use std::{
    cell::{Cell, RefCell},
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    rc::{Rc, Weak},
    sync::{Arc, OnceLock, mpsc},
    task::{Context as TaskContext, Poll, Waker},
    thread,
    time::{Duration, Instant},
};

use lune_utils::path::LuauModulePath;
use mlua::prelude::*;
use mlua_luau_scheduler::LuaSpawnExt;
use rquickjs::{
    AsyncContext, AsyncRuntime, CaughtError, Ctx, Error, Function, Module, Object, Persistent,
    Promise, Value,
    context::EvalOptions,
    function::{Rest, This},
    promise::{MaybePromise, PromiseState},
};

use crate::{
    convert::{js_to_lua_many, lua_returns_to_js, lua_to_js},
    error::{JsError, catch, format_caught_error, throw},
    loader::SharedModuleCache,
    module::ModuleId,
    options::{JsContextOptions, JsEvaluateOptions},
    source::{BundleSourceProvider, HostSourceProvider, ScriptPathProvider, SourceProvider},
    store::ModuleGraph,
};

struct QuickJsState {
    context: AsyncContext,
    runtime: AsyncRuntime,
}

#[derive(Clone, Copy)]
enum AbortReason {
    Closed,
    ExecutionLimit,
    OperationTimeout,
}

impl AbortReason {
    fn into_error(self) -> JsError {
        match self {
            Self::Closed => JsError::closed(),
            Self::ExecutionLimit => JsError::execution_limit(),
            Self::OperationTimeout => JsError::operation_timeout(),
        }
    }
}

struct ActiveOperation {
    abort: Option<AbortReason>,
    execution_remaining: Option<Duration>,
    operation_deadline: Option<Instant>,
    running_since: Option<Instant>,
}

impl ActiveOperation {
    fn new(options: &JsContextOptions) -> Self {
        Self {
            abort: None,
            execution_remaining: options.execution_limit,
            operation_deadline: options
                .operation_timeout
                .and_then(|timeout| Instant::now().checked_add(timeout)),
            running_since: None,
        }
    }
}

struct Inner {
    // Callback promises must be dropped before the QuickJS runtime they belong to.
    callbacks: RefCell<Vec<Rc<RefCell<AsyncCallback>>>>,
    state: QuickJsState,
    options: JsContextOptions,
    module_cache: SharedModuleCache,
    closed: Cell<bool>,
    active: RefCell<Option<ActiveOperation>>,
    next_waiter: Cell<u64>,
    waiters: RefCell<Vec<(u64, Waker)>>,
}

impl Inner {
    fn abort(&self, reason: AbortReason) {
        if let Some(active) = self.active.borrow_mut().as_mut() {
            active.abort.get_or_insert(reason);
        }
        self.wake_all();
    }

    fn wake_all(&self) {
        for (_, waker) in self.waiters.borrow_mut().drain(..) {
            waker.wake();
        }
    }

    fn check_active(&self) -> Result<(), JsError> {
        if self.closed.get() {
            return Err(JsError::closed());
        }
        let mut active = self.active.borrow_mut();
        let Some(active) = active.as_mut() else {
            return Ok(());
        };
        if let Some(reason) = active.abort {
            return Err(reason.into_error());
        }
        if active
            .operation_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            active.abort = Some(AbortReason::OperationTimeout);
            return Err(JsError::operation_timeout());
        }
        if active
            .execution_remaining
            .is_some_and(|remaining| remaining.is_zero())
        {
            active.abort = Some(AbortReason::ExecutionLimit);
            return Err(JsError::execution_limit());
        }
        Ok(())
    }
}

struct OperationGuard {
    inner: Rc<Inner>,
}

impl OperationGuard {
    fn begin(inner: &Rc<Inner>) -> Result<Self, JsError> {
        if inner.closed.get() {
            return Err(JsError::closed());
        }
        if inner.active.borrow().is_some() {
            return Err(JsError::reentrant());
        }
        inner
            .active
            .borrow_mut()
            .replace(ActiveOperation::new(&inner.options));
        Ok(Self {
            inner: inner.clone(),
        })
    }
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        self.inner.active.borrow_mut().take();
    }
}

/**
    An isolated `QuickJS` runtime and context.

    `QuickJS` objects are not thread-safe without `rquickjs`'s `parallel` feature. The state is
    therefore kept in reference-counted, thread-local storage and is only accessed from Lune's
    local scheduler through async Luau methods.
*/
#[derive(Clone)]
pub struct JsContext {
    inner: Rc<Inner>,
}

impl JsContext {
    pub async fn new(
        lua: Lua,
        runtime: AsyncRuntime,
        context: AsyncContext,
        options: JsContextOptions,
    ) -> Result<Self, JsError> {
        let module_cache = SharedModuleCache::new();
        runtime
            .set_loader(module_cache.clone(), module_cache.clone())
            .await;
        let inner = Rc::new(Inner {
            state: QuickJsState { context, runtime },
            options,
            module_cache,
            closed: Cell::new(false),
            active: RefCell::new(None),
            callbacks: RefCell::new(Vec::new()),
            next_waiter: Cell::new(0),
            waiters: RefCell::new(Vec::new()),
        });
        let weak = Rc::downgrade(&inner);
        inner
            .state
            .runtime
            .set_interrupt_handler(Some(Box::new(move || should_interrupt(&weak))))
            .await;
        let this = Self { inner };
        this.inner
            .state
            .context
            .with(|ctx| {
                let function = create_dynamic_callback(&ctx, &lua, &this.inner)?;
                catch(&ctx, ctx.globals().set("__luneDynamicImport", function))
            })
            .await?;
        Ok(this)
    }

    fn check_open(&self) -> Result<(), JsError> {
        if self.inner.closed.get() {
            Err(JsError::closed())
        } else {
            Ok(())
        }
    }

    async fn evaluate(
        &self,
        lua: Lua,
        source: Vec<u8>,
        options: JsEvaluateOptions,
    ) -> Result<LuaValue, JsError> {
        let _operation = OperationGuard::begin(&self.inner)?;
        let promise = MeasuredFuture::new(
            self.inner.clone(),
            self.inner.state.context.with(move |ctx| {
                let mut eval_options = EvalOptions::default();
                eval_options.global = !options.module;
                eval_options.promise = options.module;
                eval_options.filename = options.filename;
                let value = catch(
                    &ctx,
                    ctx.eval_with_options::<Value, _>(source, eval_options),
                )?;
                normalize_thenable(&ctx, value)
            }),
        )
        .await;
        let promise = self.prefer_abort(promise)?;
        let value = self.settle(promise).await?;
        self.convert_value(lua, value).await
    }

    async fn import(&self, lua: Lua, specifier: String) -> Result<LuaValue, JsError> {
        let _operation = OperationGuard::begin(&self.inner)?;
        let request = self.prepare_import(&lua, &specifier)?;
        let graph = lua
            .spawn_blocking(move || request.load())
            .await
            .map_err(|error| JsError::new(error.to_string()))?;
        self.inner.check_active()?;
        let entry = graph.entry().to_string();
        self.inner.module_cache.insert_graph(graph);

        let promise = MeasuredFuture::new(
            self.inner.clone(),
            self.inner.state.context.with(move |ctx| {
                let promise = catch(&ctx, Module::import(&ctx, entry))?;
                Ok(Persistent::save(&ctx, promise))
            }),
        )
        .await;
        let promise = self.prefer_abort(promise)?;
        let value = self.settle(promise).await?;
        self.convert_value(lua, value).await
    }

    fn prepare_import(&self, lua: &Lua, specifier: &str) -> Result<ModuleImportRequest, JsError> {
        let script_path = current_script_path(lua)?;
        let bundled = lua
            .app_data_ref::<Option<Arc<BundleSourceProvider>>>()
            .and_then(|provider| provider.clone());
        if let Some(provider) = bundled {
            let base = module_base_for_path(Path::new(&script_path), Path::new("/"), false)?;
            return Ok(ModuleImportRequest {
                provider,
                base,
                specifier: specifier.to_string(),
            });
        }

        let script_path = PathBuf::from(script_path);
        let script_directory = script_path.parent().ok_or_else(|| {
            JsError::new("Cannot resolve a JavaScript import without a script directory")
        })?;
        let root = self.inner.options.module_root.clone().map_or_else(
            || script_directory.to_path_buf(),
            |root| {
                if root.is_absolute() {
                    root
                } else {
                    script_directory.join(root)
                }
            },
        );
        let provider =
            HostSourceProvider::new(&root).map_err(|error| JsError::new(error.to_string()))?;
        let base = module_base_for_path(&script_path, provider.root(), true)?;
        Ok(ModuleImportRequest {
            provider: Arc::new(provider),
            base,
            specifier: specifier.to_string(),
        })
    }

    pub(crate) async fn call_function(
        &self,
        lua: Lua,
        function: Persistent<Function<'static>>,
        arguments: LuaMultiValue,
    ) -> LuaResult<LuaValue> {
        self.call_function_inner(lua, function, arguments)
            .await
            .map_err(LuaError::from)
    }

    async fn call_function_inner(
        &self,
        lua: Lua,
        function: Persistent<Function<'static>>,
        arguments: LuaMultiValue,
    ) -> Result<LuaValue, JsError> {
        let _operation = OperationGuard::begin(&self.inner)?;
        let promise = MeasuredFuture::new(
            self.inner.clone(),
            self.inner.state.context.with(move |ctx| {
                let arguments = lua_to_js(&ctx, arguments)?;
                let function = catch(&ctx, function.restore(&ctx))?;
                let result = catch(&ctx, function.call::<_, MaybePromise>((Rest(arguments),)))?;
                normalize_thenable(&ctx, result.into_value())
            }),
        )
        .await;
        let promise = self.prefer_abort(promise)?;
        let value = self.settle(promise).await?;
        self.convert_value(lua, value).await
    }

    async fn convert_value(
        &self,
        lua: Lua,
        value: Persistent<Value<'static>>,
    ) -> Result<LuaValue, JsError> {
        let result = MeasuredFuture::new(
            self.inner.clone(),
            self.inner.state.context.with(|ctx| {
                let value = catch(&ctx, value.restore(&ctx))?;
                crate::convert::js_to_lua(&ctx, &lua, self, value)
            }),
        )
        .await;
        self.prefer_abort(result)
    }

    async fn drive_callbacks(&self) -> Result<bool, JsError> {
        let callbacks = self.inner.callbacks.borrow().clone();
        if callbacks.is_empty() {
            return Ok(false);
        }

        std::future::poll_fn(|cx| {
            for callback in &callbacks {
                callback.borrow_mut().poll(cx);
            }
            Poll::Ready(())
        })
        .await;

        let pending = callbacks
            .iter()
            .any(|callback| callback.borrow().result.is_none());
        let ready = callbacks
            .into_iter()
            .filter(|callback| callback.borrow().result.is_some())
            .collect::<Vec<_>>();
        if ready.is_empty() {
            return Ok(pending);
        }

        let result = MeasuredFuture::new(
            self.inner.clone(),
            self.inner.state.context.with(move |ctx| {
                for callback in ready {
                    finish_async_callback(&ctx, &callback)?;
                }
                Ok(())
            }),
        )
        .await;
        self.inner
            .callbacks
            .borrow_mut()
            .retain(|callback| !callback.borrow().settled);
        self.prefer_abort(result).map(|()| pending)
    }

    async fn settle(
        &self,
        promise: Persistent<Promise<'static>>,
    ) -> Result<Persistent<Value<'static>>, JsError> {
        let mut jobs_since_yield = 0usize;
        loop {
            self.inner.check_active()?;
            let callbacks_pending = self.drive_callbacks().await?;
            match self.inspect_promise(&promise).await? {
                Settlement::Pending => {}
                Settlement::Fulfilled(value) => return Ok(value),
            }

            if jobs_since_yield >= self.inner.options.job_budget {
                YieldNow::default().await;
                jobs_since_yield = 0;
                continue;
            }

            let job = MeasuredFuture::new(
                self.inner.clone(),
                self.inner.state.runtime.execute_pending_job(),
            )
            .await;
            match job {
                Ok(true) => jobs_since_yield += 1,
                Ok(false) if callbacks_pending => YieldNow::default().await,
                Ok(false) if self.inner.state.runtime.is_job_pending().await => {
                    YieldNow::default().await;
                }
                Ok(false) => IdleWait::new(self.inner.clone()).await,
                Err(error) => {
                    let error = MeasuredFuture::new(
                        self.inner.clone(),
                        error.0.with(|ctx| {
                            format_caught_error(CaughtError::from_error(
                                &ctx,
                                rquickjs::Error::Exception,
                            ))
                        }),
                    )
                    .await;
                    return self.prefer_abort(Err(error));
                }
            }
        }
    }

    async fn inspect_promise(
        &self,
        promise: &Persistent<Promise<'static>>,
    ) -> Result<Settlement, JsError> {
        let result = MeasuredFuture::new(
            self.inner.clone(),
            self.inner.state.context.with(|ctx| {
                let promise = catch(&ctx, promise.clone().restore(&ctx))?;
                match promise.state() {
                    PromiseState::Pending => Ok(Settlement::Pending),
                    PromiseState::Resolved => {
                        let value =
                            catch(&ctx, promise.result::<Value>().expect("resolved promise"))?;
                        Ok(Settlement::Fulfilled(Persistent::save(&ctx, value)))
                    }
                    PromiseState::Rejected => {
                        let error = promise
                            .result::<Value>()
                            .expect("rejected promise")
                            .expect_err("rejected promises always contain an error");
                        Err(format_caught_error(CaughtError::from_error(&ctx, error)))
                    }
                }
            }),
        )
        .await;
        self.prefer_abort(result)
    }

    fn prefer_abort<T>(&self, result: Result<T, JsError>) -> Result<T, JsError> {
        match self.inner.check_active() {
            Err(error) => Err(error),
            Ok(()) => result,
        }
    }

    async fn expose(
        &self,
        lua: Lua,
        name: String,
        callback: LuaFunction,
        asynchronous: bool,
    ) -> Result<(), JsError> {
        self.check_open()?;
        let inner = self.inner.clone();
        self.inner
            .state
            .context
            .with(move |ctx| {
                let function = if asynchronous {
                    create_async_callback(&ctx, &lua, &inner, callback)?
                } else {
                    create_sync_callback(&ctx, &lua, &inner, callback)?
                };
                catch(&ctx, ctx.globals().set(name, function))
            })
            .await
    }

    fn close(&self) {
        if self.inner.closed.replace(true) {
            return;
        }
        self.inner.abort(AbortReason::Closed);
    }
}

impl Drop for JsContext {
    fn drop(&mut self) {
        if Rc::strong_count(&self.inner) == 1 {
            self.close();
        }
    }
}

impl LuaUserData for JsContext {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_async_method(
            "Evaluate",
            |lua, this, (source, options): (LuaString, Option<JsEvaluateOptions>)| {
                let source = source.as_bytes().to_vec();
                async move {
                    this.evaluate(lua, source, options.unwrap_or_default())
                        .await
                        .map_err(LuaError::from)
                }
            },
        );
        methods.add_async_method("Import", |lua, this, specifier: String| async move {
            this.import(lua, specifier).await.map_err(LuaError::from)
        });
        methods.add_async_method(
            "Expose",
            |lua, this, (name, callback): (String, LuaFunction)| async move {
                this.expose(lua, name, callback, false)
                    .await
                    .map_err(LuaError::from)
            },
        );
        methods.add_async_method(
            "ExposeAsync",
            |lua, this, (name, callback): (String, LuaFunction)| async move {
                this.expose(lua, name, callback, true)
                    .await
                    .map_err(LuaError::from)
            },
        );
        methods.add_method("Close", |_, this, (): ()| {
            this.close();
            Ok(())
        });
    }
}

struct ModuleImportRequest {
    provider: Arc<dyn SourceProvider + Send + Sync>,
    base: ModuleId,
    specifier: String,
}

impl ModuleImportRequest {
    fn load(self) -> Result<ModuleGraph, crate::module::ModuleError> {
        let result = if self.specifier.starts_with('/') {
            ModuleGraph::load_entry(self.provider.as_ref(), &self.specifier)
        } else {
            ModuleGraph::load_specifier(self.provider.as_ref(), &self.base, &self.specifier)
        };
        match result {
            Err(crate::module::ModuleError::NotFound(path)) if self.provider.is_bundle() => {
                Err(crate::module::ModuleError::NotPreloaded(format!(
                    "\"{path}\" was not included by lune build"
                )))
            }
            other => other,
        }
    }
}

fn current_script_path(lua: &Lua) -> Result<String, JsError> {
    let provider = lua
        .app_data_ref::<ScriptPathProvider>()
        .map(|provider| provider.0)
        .ok_or_else(|| JsError::new("JavaScript imports require a Luau script path provider"))?;
    provider(lua)
        .map_err(|error| JsError::new(error.to_string()))?
        .ok_or_else(|| JsError::new("JavaScript imports must be resolved from a Luau source file"))
}

fn module_base_for_path(
    path: &Path,
    root: &Path,
    canonical_host: bool,
) -> Result<ModuleId, JsError> {
    let relative: PathBuf = if canonical_host {
        let resolved = LuauModulePath::resolve(path)
            .map_err(|error| JsError::new(format!("Cannot resolve Luau script path. {error:?}")))?;
        let script_file = resolved.target().as_file().ok_or_else(|| {
            JsError::new("JavaScript imports require a Luau script file, not a directory")
        })?;
        let canonical = script_file.canonicalize().map_err(|error| {
            JsError::new(format!("Cannot canonicalize Luau script path. {error}"))
        })?;
        canonical
            .strip_prefix(root)
            .map_err(|_| {
                JsError::new(format!(
                    "Luau script \"{}\" is outside JavaScript ModuleRoot \"{}\"",
                    canonical.display(),
                    root.display()
                ))
            })?
            .to_path_buf()
    } else {
        path.strip_prefix(root)
            .map_err(|_| JsError::new("Luau script is outside the standalone module root"))?
            .to_path_buf()
    };
    let parent = relative.parent().unwrap_or(Path::new(""));
    let base = format!("/{}/__Luau.ts", parent.display()).replace('\\', "/");
    ModuleId::parse(base).map_err(|error| JsError::new(error.to_string()))
}

enum Settlement {
    Pending,
    Fulfilled(Persistent<Value<'static>>),
}

fn create_dynamic_callback<'js>(
    ctx: &Ctx<'js>,
    lua: &Lua,
    inner: &Rc<Inner>,
) -> Result<Function<'js>, JsError> {
    let lua = lua.clone();
    let inner = Rc::downgrade(inner);
    catch(
        ctx,
        Function::new(
            ctx.clone(),
            move |callback_ctx: Ctx<'js>, arguments: Rest<Value<'js>>| {
                let Some(inner) = inner.upgrade() else {
                    return Err(throw(&callback_ctx, JsError::closed()));
                };
                if inner.closed.get() {
                    return Err(throw(&callback_ctx, JsError::closed()));
                }
                let base = arguments.0.first().ok_or_else(|| {
                    throw(&callback_ctx, "Dynamic import requires an importer URL")
                })?;
                let base = base
                    .as_string()
                    .ok_or_else(|| throw(&callback_ctx, "Dynamic importer URLs must be strings"))?;
                let base = catch(&callback_ctx, base.to_string())
                    .map_err(|error| throw(&callback_ctx, error))?;
                let value = arguments.0.get(1).ok_or_else(|| {
                    throw(&callback_ctx, "Dynamic import requires a module specifier")
                })?;
                let string = value.as_string().ok_or_else(|| {
                    throw(&callback_ctx, "Dynamic import specifiers must be strings")
                })?;
                let specifier = catch(&callback_ctx, string.to_string())
                    .map_err(|error| throw(&callback_ctx, error))?;
                let mut request = JsContext {
                    inner: inner.clone(),
                }
                .prepare_import(&lua, &specifier)
                .map_err(|error| throw(&callback_ctx, error))?;
                request.base = ModuleId::parse(&base)
                    .map_err(|error| throw(&callback_ctx, error.to_string()))?;
                let operation_name = format!("{} from {base}", request.specifier);
                let callback_lua = lua.clone();

                let load_promise = Promise::wrap_future(&callback_ctx, async move {
                    let graph = callback_lua
                        .spawn_blocking(move || request.load())
                        .await
                        .map_err(|error| {
                            Error::new_loading_message(&operation_name, error.to_string())
                        })?;
                    inner.check_active().map_err(|error| {
                        Error::new_loading_message(&operation_name, error.to_string())
                    })?;
                    let entry = graph.entry().to_string();
                    inner.module_cache.insert_graph(graph);
                    Ok::<String, Error>(entry)
                })?;
                let import_function = Function::new(callback_ctx.clone(), |ctx, entry: String| {
                    Module::import(&ctx, entry)
                })?;
                let then = load_promise.then()?;
                then.call::<_, Value>((This(load_promise), import_function))
            },
        ),
    )
}

fn normalize_thenable<'js>(
    ctx: &Ctx<'js>,
    value: Value<'js>,
) -> Result<Persistent<Promise<'static>>, JsError> {
    let constructor = catch(ctx, ctx.globals().get::<_, Object>("Promise"))?;
    let resolve = catch(ctx, constructor.get::<_, Function>("resolve"))?;
    let promise = catch(ctx, resolve.call::<_, Promise>((This(constructor), value)))?;
    Ok(Persistent::save(ctx, promise))
}

fn create_sync_callback<'js>(
    ctx: &Ctx<'js>,
    lua: &Lua,
    inner: &Rc<Inner>,
    callback: LuaFunction,
) -> Result<Function<'js>, JsError> {
    let lua = lua.clone();
    let inner = Rc::downgrade(inner);
    catch(
        ctx,
        Function::new(
            ctx.clone(),
            move |callback_ctx: Ctx<'js>, arguments: Rest<Value<'js>>| {
                let Some(inner) = inner.upgrade() else {
                    return Err(throw(&callback_ctx, JsError::closed()));
                };
                if inner.closed.get() {
                    return Err(throw(&callback_ctx, JsError::closed()));
                }
                let context = JsContext { inner };
                let arguments = js_to_lua_many(&callback_ctx, &lua, &context, arguments.0)
                    .map_err(|error| throw(&callback_ctx, error))?;
                let returns = callback.call::<LuaMultiValue>(arguments).map_err(|error| {
                    throw(&callback_ctx, format!("Luau callback failed: {error}"))
                })?;
                lua_returns_to_js(&callback_ctx, returns)
                    .map_err(|error| throw(&callback_ctx, error))
            },
        ),
    )
}

fn create_async_callback<'js>(
    ctx: &Ctx<'js>,
    lua: &Lua,
    inner: &Rc<Inner>,
    callback: LuaFunction,
) -> Result<Function<'js>, JsError> {
    let lua = lua.clone();
    let inner = Rc::downgrade(inner);
    catch(
        ctx,
        Function::new(
            ctx.clone(),
            move |callback_ctx: Ctx<'js>, arguments: Rest<Value<'js>>| {
                let Some(inner) = inner.upgrade() else {
                    return Err(throw(&callback_ctx, JsError::closed()));
                };
                if inner.closed.get() {
                    return Err(throw(&callback_ctx, JsError::closed()));
                }
                let context = JsContext {
                    inner: inner.clone(),
                };
                let arguments = js_to_lua_many(&callback_ctx, &lua, &context, arguments.0)
                    .map_err(|error| throw(&callback_ctx, error))?;
                let (promise, resolve, reject) = catch(&callback_ctx, callback_ctx.promise())
                    .map_err(|error| throw(&callback_ctx, error))?;
                let future = CancelableWait {
                    inner: Rc::downgrade(&inner),
                    future: Box::pin(callback.call_async(arguments)),
                };
                inner
                    .callbacks
                    .borrow_mut()
                    .push(Rc::new(RefCell::new(AsyncCallback {
                        future: Some(Box::pin(future)),
                        resolve: Persistent::save(&callback_ctx, resolve),
                        reject: Persistent::save(&callback_ctx, reject),
                        result: None,
                        settled: false,
                    })));
                Ok(promise.into_value())
            },
        ),
    )
}

type AsyncCallbackFuture = Pin<Box<dyn Future<Output = Result<LuaMultiValue, JsError>>>>;

struct AsyncCallback {
    future: Option<AsyncCallbackFuture>,
    resolve: Persistent<Function<'static>>,
    reject: Persistent<Function<'static>>,
    result: Option<Result<LuaMultiValue, JsError>>,
    settled: bool,
}

impl AsyncCallback {
    fn poll(&mut self, cx: &mut TaskContext<'_>) {
        if self.result.is_some() {
            return;
        }
        let Some(future) = self.future.as_mut() else {
            return;
        };
        if let Poll::Ready(result) = future.as_mut().poll(cx) {
            self.result = Some(result);
            self.future = None;
        }
    }
}

fn finish_async_callback(
    ctx: &Ctx<'_>,
    callback: &Rc<RefCell<AsyncCallback>>,
) -> Result<(), JsError> {
    let (result, resolve, reject) = {
        let mut callback = callback.borrow_mut();
        let result = callback
            .result
            .take()
            .expect("finished callbacks should have a result");
        callback.settled = true;
        (result, callback.resolve.clone(), callback.reject.clone())
    };

    let value = match result {
        Ok(returns) => lua_returns_to_js(ctx, returns),
        Err(error) => Err(error),
    };
    match value {
        Ok(value) => {
            let resolve = catch(ctx, resolve.restore(ctx))?;
            catch(ctx, resolve.call::<_, ()>((value,)))?;
        }
        Err(error) => {
            let reject = catch(ctx, reject.restore(ctx))?;
            throw(ctx, error);
            let value = ctx.catch();
            catch(ctx, reject.call::<_, ()>((value,)))?;
        }
    }
    Ok(())
}

fn should_interrupt(weak: &Weak<Inner>) -> bool {
    let Some(inner) = weak.upgrade() else {
        return true;
    };
    if inner.closed.get() {
        return true;
    }
    let mut active = inner.active.borrow_mut();
    let Some(active) = active.as_mut() else {
        return false;
    };
    if active.abort.is_some() {
        return true;
    }
    if active
        .operation_deadline
        .is_some_and(|deadline| Instant::now() >= deadline)
    {
        active.abort = Some(AbortReason::OperationTimeout);
        return true;
    }
    if let (Some(remaining), Some(started)) = (active.execution_remaining, active.running_since)
        && started.elapsed() >= remaining
    {
        active.abort = Some(AbortReason::ExecutionLimit);
        return true;
    }
    false
}

struct MeasuredFuture<F> {
    inner: Rc<Inner>,
    future: Pin<Box<F>>,
}

impl<F> MeasuredFuture<F> {
    fn new(inner: Rc<Inner>, future: F) -> Self {
        Self {
            inner,
            future: Box::pin(future),
        }
    }
}

impl<F: Future> Future for MeasuredFuture<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        {
            let mut active = this.inner.active.borrow_mut();
            if let Some(active) = active.as_mut() {
                active.running_since = Some(Instant::now());
            }
        }
        let result = this.future.as_mut().poll(cx);
        let mut active = this.inner.active.borrow_mut();
        if let Some(active) = active.as_mut()
            && let Some(started) = active.running_since.take()
            && let Some(remaining) = active.execution_remaining.as_mut()
        {
            let elapsed = started.elapsed();
            if elapsed >= *remaining {
                *remaining = Duration::ZERO;
                active.abort = Some(AbortReason::ExecutionLimit);
            } else {
                *remaining -= elapsed;
            }
        }
        result
    }
}

struct WaitRegistration {
    inner: Rc<Inner>,
    id: u64,
}

impl WaitRegistration {
    fn new(inner: Rc<Inner>) -> Self {
        let id = inner.next_waiter.get();
        inner.next_waiter.set(
            id.checked_add(1)
                .expect("JavaScript context waiter IDs should not overflow"),
        );
        Self { inner, id }
    }

    fn register(&mut self, waker: &Waker) {
        let mut waiters = self.inner.waiters.borrow_mut();
        if let Some((_, registered)) = waiters.iter_mut().find(|(id, _)| *id == self.id) {
            if !registered.will_wake(waker) {
                registered.clone_from(waker);
            }
        } else {
            waiters.push((self.id, waker.clone()));
        }
    }
}

impl Drop for WaitRegistration {
    fn drop(&mut self) {
        self.inner
            .waiters
            .borrow_mut()
            .retain(|(id, _)| *id != self.id);
    }
}

struct IdleWait {
    inner: Rc<Inner>,
    timer: Option<ThreadTimer>,
    registration: WaitRegistration,
}

impl IdleWait {
    fn new(inner: Rc<Inner>) -> Self {
        let timer = inner
            .active
            .borrow()
            .as_ref()
            .and_then(|active| active.operation_deadline)
            .map(ThreadTimer::new);
        let registration = WaitRegistration::new(inner.clone());
        Self {
            inner,
            timer,
            registration,
        }
    }
}

impl Future for IdleWait {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if this.inner.check_active().is_err() {
            return Poll::Ready(());
        }
        if let Some(timer) = this.timer.as_mut()
            && Pin::new(timer).poll(cx).is_ready()
        {
            this.inner.abort(AbortReason::OperationTimeout);
            return Poll::Ready(());
        }
        this.registration.register(cx.waker());
        Poll::Pending
    }
}

struct ThreadTimer {
    deadline: Instant,
    started: bool,
}

impl ThreadTimer {
    fn new(deadline: Instant) -> Self {
        Self {
            deadline,
            started: false,
        }
    }
}

impl Future for ThreadTimer {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let duration = self.deadline.saturating_duration_since(Instant::now());
        if duration.is_zero() {
            return Poll::Ready(());
        }
        if !self.started {
            self.started = true;
            let waker = cx.waker().clone();
            drop(thread::spawn(move || {
                thread::sleep(duration);
                waker.wake();
            }));
        }
        Poll::Pending
    }
}

struct CancelableWait<F> {
    inner: Weak<Inner>,
    future: Pin<Box<F>>,
}

impl<F> Future for CancelableWait<F>
where
    F: Future<Output = LuaResult<LuaMultiValue>>,
{
    type Output = Result<LuaMultiValue, JsError>;

    fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let Some(inner) = this.inner.upgrade() else {
            return Poll::Ready(Err(JsError::closed()));
        };
        if let Err(error) = inner.check_active() {
            return Poll::Ready(Err(error));
        }
        match this.future.as_mut().poll(cx) {
            Poll::Ready(result) => Poll::Ready(
                result.map_err(|error| JsError::new(format!("Luau callback failed: {error}"))),
            ),
            Poll::Pending => Poll::Pending,
        }
    }
}

#[derive(Default)]
struct YieldNow(bool);

impl YieldNow {
    fn schedule(waker: Waker) {
        // Use an external queue so the executor can run other tasks before polling this
        // operation again.
        static SENDER: OnceLock<mpsc::Sender<Waker>> = OnceLock::new();
        let sender = SENDER.get_or_init(|| {
            let (sender, receiver) = mpsc::channel::<Waker>();
            drop(thread::spawn(move || {
                while let Ok(waker) = receiver.recv() {
                    thread::yield_now();
                    waker.wake();
                }
            }));
            sender
        });
        if let Err(error) = sender.send(waker) {
            error.0.wake();
        }
    }
}

impl Future for YieldNow {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        if self.0 {
            Poll::Ready(())
        } else {
            self.0 = true;
            Self::schedule(cx.waker().clone());
            Poll::Pending
        }
    }
}

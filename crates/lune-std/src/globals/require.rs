use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use async_channel::{Receiver, Sender};
use async_fs::read as read_file;
use mlua::prelude::*;
use mlua_luau_scheduler::LuaSchedulerExt;
use serde::Deserialize;

use crate::globals::script::ScriptReference;
use crate::require::RequireResolver;
use lune_utils::path::{
    LuauModulePath, clean_path_and_make_absolute,
    constants::{FILE_CHUNK_PREFIX, FILE_NAME_CONFIG},
    relative_path_normalize,
};

type RequireResult = LuaResult<LuaMultiValue>;
type RequireResultSender = Sender<RequireResult>;
type RequireResultReceiver = Receiver<RequireResult>;

/// Registry key for the built-in require function
const BUILTIN_REQUIRE_KEY: &str = "__lune_builtin_require";

/// Registry key for the module cache
const MODULE_CACHE_KEY: &str = "__lune_require_cache";

/// Shared state for tracking pending requires to avoid duplicate loading.
#[derive(Debug, Clone)]
struct RequireState {
    tx: Rc<RefCell<HashMap<PathBuf, RequireResultSender>>>,
    rx: Rc<RefCell<HashMap<PathBuf, RequireResultReceiver>>>,
}

impl RequireState {
    fn new() -> Self {
        Self {
            tx: Rc::new(RefCell::new(HashMap::new())),
            rx: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    fn get_pending(&self, path: &Path) -> Option<RequireResultReceiver> {
        self.rx.borrow().get(path).cloned()
    }

    fn create_pending(&self, path: &Path) -> RequireResultSender {
        let (tx, rx) = async_channel::bounded(1);
        self.tx.borrow_mut().insert(path.to_path_buf(), tx.clone());
        self.rx.borrow_mut().insert(path.to_path_buf(), rx);
        tx
    }

    fn remove_pending(&self, path: &Path) {
        self.tx.borrow_mut().remove(path);
        self.rx.borrow_mut().remove(path);
    }
}

/// Get or create the module cache table
fn get_module_cache(lua: &Lua) -> LuaResult<LuaTable> {
    match lua.named_registry_value::<LuaTable>(MODULE_CACHE_KEY) {
        Ok(cache) => Ok(cache),
        Err(_) => {
            let cache = lua.create_table()?;
            lua.set_named_registry_value(MODULE_CACHE_KEY, cache.clone())?;
            Ok(cache)
        }
    }
}

/// Get the calling script's path by inspecting the Lua stack.
fn get_caller_path(lua: &Lua) -> Option<PathBuf> {
    for level in 0..100 {
        let result: Option<Option<PathBuf>> = lua.inspect_stack(level, |debug| {
            let source_info = debug.source();
            if let Some(source) = source_info.source {
                // Skip C functions, internal code, and our wrapper
                if source == "[C]"
                    || source == "=[C]"
                    || source == "@[C]"
                    || source == "=require_wrapper"
                    || source.starts_with("__mlua")
                {
                    return None;
                }
                // Handle @-prefixed paths (standard Lua chunk naming)
                if let Some(path) = source.strip_prefix('@') {
                    // Skip internal chunk names
                    if path.starts_with("__mlua") || path == "[C]" {
                        return None;
                    }
                    return Some(PathBuf::from(path));
                }
                // Handle =-prefixed paths (but only real file paths, not internal names)
                if let Some(path) = source.strip_prefix('=') {
                    // Skip internal chunk names and things that look like internal identifiers
                    if path.starts_with("__mlua")
                        || path == "[C]"
                        || path == "require_wrapper"
                        || !path.contains('/')
                    {
                        return None;
                    }
                    return Some(PathBuf::from(path));
                }
            }
            None
        });

        match result {
            None => break,              // No more stack frames
            Some(Some(path)) => return Some(path), // Found a valid source
            Some(None) => continue,     // Skip this frame
        }
    }
    None
}

/// Convert an absolute target path to a relative path from the current script.
fn make_relative_path(current_script: &Path, target_path: &Path) -> PathBuf {
    let current_dir = current_script.parent().unwrap_or(current_script);

    // Try to make target relative to current_dir
    if let Ok(relative) = target_path.strip_prefix(current_dir) {
        return PathBuf::from(format!("./{}", relative.display()));
    }

    // Need to go up directories - find common ancestor
    let current_parts: Vec<_> = current_dir.components().collect();
    let target_parts: Vec<_> = target_path.components().collect();

    let common_len = current_parts
        .iter()
        .zip(target_parts.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let ups_needed = current_parts.len() - common_len;

    let mut result = String::new();
    if ups_needed == 0 {
        result.push_str("./");
    } else {
        for _ in 0..ups_needed {
            result.push_str("../");
        }
    }

    for (i, part) in target_parts.iter().skip(common_len).enumerate() {
        if i > 0 {
            result.push('/');
        }
        result.push_str(&part.as_os_str().to_string_lossy());
    }

    PathBuf::from(result)
}

/// Result of resolving a require argument.
enum ResolveResult {
    /// A file path to load (relative, absolute)
    FilePath(PathBuf, PathBuf),
    /// An alias path that needs to be resolved
    Alias(String),
}

/// Structure representing a .luaurc configuration file
#[derive(Debug, Deserialize, Default)]
struct LuauConfig {
    #[serde(default)]
    aliases: HashMap<String, String>,
}

/// Read and parse a .luaurc file
fn read_luaurc(dir: &Path) -> Option<LuauConfig> {
    let config_path = dir.join(FILE_NAME_CONFIG);
    if !config_path.exists() {
        return None;
    }

    match std::fs::read_to_string(&config_path) {
        Ok(content) => match serde_json::from_str::<LuauConfig>(&content) {
            Ok(config) => Some(config),
            Err(_) => None,
        },
        Err(_) => None,
    }
}

/// Resolve an alias path to an absolute path by searching for .luaurc files
fn resolve_alias(alias: &str, caller_dir: &Path) -> Option<PathBuf> {
    // Alias format: @alias/path/to/module or @alias
    // Strip the leading @
    let alias_path = alias.strip_prefix('@')?;

    // Split into alias name and rest of path
    let (alias_name, rest) = match alias_path.find('/') {
        Some(idx) => (&alias_path[..idx], Some(&alias_path[idx + 1..])),
        None => (alias_path, None),
    };

    // Special case: @lune/* is handled by registered modules, not .luaurc
    if alias_name == "lune" {
        return None;
    }

    // Special case: @self refers to the current module's directory
    // It's used to reference modules relative to the current script
    if alias_name == "self" {
        let mut resolved = caller_dir.to_path_buf();
        if let Some(rest_path) = rest {
            resolved = resolved.join(rest_path);
        }
        return Some(clean_path_and_make_absolute(&resolved));
    }

    // Search for .luaurc files starting from caller directory going up
    let mut search_dir = caller_dir.to_path_buf();
    loop {
        if let Some(config) = read_luaurc(&search_dir) {
            if let Some(alias_value) = config.aliases.get(alias_name) {
                // The alias value is a path relative to the .luaurc file's directory
                let mut resolved = search_dir.join(alias_value);

                // If there's additional path after the alias, append it
                if let Some(rest_path) = rest {
                    resolved = resolved.join(rest_path);
                }

                return Some(clean_path_and_make_absolute(&resolved));
            }
        }

        // Go up to parent directory
        if !search_dir.pop() {
            break;
        }
    }

    None
}

/// Resolve a require argument to paths or an alias.
fn resolve_require_arg(
    arg: &LuaValue,
    caller_path: Option<&Path>,
) -> LuaResult<ResolveResult> {
    match arg {
        LuaValue::String(s) => {
            let path_str: String = s.to_str()?.to_string();

            if path_str.starts_with('/') {
                // Absolute path
                let abs = clean_path_and_make_absolute(Path::new(&path_str));
                let rel = caller_path
                    .map(|cp| make_relative_path(cp, &abs))
                    .unwrap_or_else(|| PathBuf::from(format!(".{}", path_str)));
                Ok(ResolveResult::FilePath(rel, abs))
            } else if path_str.starts_with("./") || path_str.starts_with("../") {
                // Relative path
                let rel = relative_path_normalize(Path::new(&path_str));
                let abs = if let Some(caller) = caller_path {
                    let caller_dir = caller.parent().unwrap_or(caller);
                    clean_path_and_make_absolute(&caller_dir.join(&rel))
                } else {
                    clean_path_and_make_absolute(&rel)
                };
                Ok(ResolveResult::FilePath(rel, abs))
            } else if path_str.starts_with('@') {
                // Alias - delegate to built-in require
                Ok(ResolveResult::Alias(path_str))
            } else {
                Err(LuaError::runtime(format!(
                    "require path must start with './', '../', '/', or '@': got '{}'",
                    path_str
                )))
            }
        }
        LuaValue::UserData(ud) => {
            if let Ok(script_ref) = ud.borrow::<ScriptReference>() {
                let abs = PathBuf::from(script_ref.path_string());
                let rel = caller_path
                    .map(|cp| make_relative_path(cp, &abs))
                    .unwrap_or_else(|| PathBuf::from(format!("./{}", abs.display())));
                Ok(ResolveResult::FilePath(rel, abs))
            } else {
                Err(LuaError::runtime(
                    "require expects a string or ScriptReference",
                ))
            }
        }
        _ => Err(LuaError::runtime(
            "require expects a string or ScriptReference",
        )),
    }
}

/// Registry key for storing the caller path temporarily
const CALLER_PATH_KEY: &str = "__lune_require_caller_path";

pub fn create(lua: Lua) -> LuaResult<LuaValue> {
    // Create the built-in require function for alias paths
    let builtin_require = lua.create_require_function(RequireResolver::new())?;
    lua.set_named_registry_value(BUILTIN_REQUIRE_KEY, builtin_require)?;

    let state = RequireState::new();

    let require_fn = lua.create_async_function(move |lua, arg: LuaValue| {
        let state = state.clone();

        async move {
            // Get caller path from registry (set by sync wrapper) or fallback to stack inspection
            let caller_path: Option<PathBuf> = lua
                .named_registry_value::<Option<String>>(CALLER_PATH_KEY)
                .ok()
                .flatten()
                .map(PathBuf::from)
                .or_else(|| get_caller_path(&lua));

            // Clear the stored caller path
            let _: Option<()> = lua
                .set_named_registry_value(CALLER_PATH_KEY, LuaValue::Nil)
                .ok();

            // Resolve the argument to paths
            match resolve_require_arg(&arg, caller_path.as_deref())? {
                ResolveResult::Alias(alias) => {
                    // Handle @lune/* built-in modules
                    if alias.starts_with("@lune/") {
                        let module_name = alias.strip_prefix("@lune/").unwrap();
                        let registered_modules: LuaTable =
                            lua.named_registry_value("_REGISTEREDMODULES")?;
                        let module_key = format!("@lune/{}", module_name);
                        match registered_modules.get::<LuaValue>(module_key.as_str()) {
                            Ok(value) if !value.is_nil() => {
                                return Ok(LuaMultiValue::from_vec(vec![value]));
                            }
                            _ => {
                                return Err(LuaError::runtime(format!(
                                    "cannot find built-in module '{}'",
                                    alias
                                )));
                            }
                        }
                    }

                    // Handle custom aliases via .luaurc files
                    let caller_dir = caller_path
                        .as_ref()
                        .and_then(|p| p.parent())
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

                    let absolute_path = resolve_alias(&alias, &caller_dir).ok_or_else(|| {
                        LuaError::runtime(format!("cannot find alias '{}'", alias))
                    })?;

                    // Resolve to actual filesystem path (handling .luau/.lua extensions)
                    let resolved = LuauModulePath::resolve(&absolute_path).map_err(|e| {
                        LuaError::runtime(format!(
                            "cannot find module '{}': {:?}",
                            absolute_path.display(),
                            e
                        ))
                    })?;

                    let resolved_path = resolved.target().as_file().ok_or_else(|| {
                        LuaError::runtime(format!(
                            "cannot require directory '{}'",
                            absolute_path.display()
                        ))
                    })?;

                    let cache_key = resolved_path.to_string_lossy().to_string();

                    // Check cache first
                    let cache = get_module_cache(&lua)?;
                    if let Ok(cached) = cache.get::<LuaValue>(cache_key.as_str()) {
                        if !cached.is_nil() {
                            return Ok(LuaMultiValue::from_vec(vec![cached]));
                        }
                    }

                    // Check if already being loaded (concurrent require)
                    if let Some(rx) = state.get_pending(resolved_path) {
                        return rx
                            .recv()
                            .await
                            .into_lua_err()
                            .context("require interrupted")?;
                    }

                    let tx = state.create_pending(resolved_path);

                    // Load and execute the module
                    let chunk_name = format!("{FILE_CHUNK_PREFIX}{}", resolved_path.display());
                    let chunk_bytes = read_file(resolved_path).await.map_err(|e| {
                        LuaError::runtime(format!(
                            "cannot read '{}': {}",
                            resolved_path.display(),
                            e
                        ))
                    })?;

                    let chunk = lua.load(chunk_bytes).set_name(chunk_name);

                    let thread_id = lua.push_thread_back(chunk, ())?;
                    lua.track_thread(thread_id);
                    lua.wait_for_thread(thread_id).await;

                    let result = lua
                        .get_thread_result(thread_id)
                        .expect("thread tracked and waited");

                    // Cache the result
                    if let Ok(ref res) = result {
                        if let Some(first_value) = res.iter().next() {
                            cache.set(cache_key.as_str(), first_value.clone())?;
                        }
                    }

                    // Notify any waiting requires
                    if tx.receiver_count() > 0 {
                        tx.send(result.clone()).await.ok();
                        tx.close();
                    }

                    state.remove_pending(resolved_path);

                    result
                }
                ResolveResult::FilePath(_relative_path, absolute_path) => {
                    // Resolve to actual filesystem path (handling .luau/.lua extensions)
                    let resolved = LuauModulePath::resolve(&absolute_path).map_err(|e| {
                        LuaError::runtime(format!(
                            "cannot find module '{}': {:?}",
                            absolute_path.display(),
                            e
                        ))
                    })?;

                    let resolved_path = resolved.target().as_file().ok_or_else(|| {
                        LuaError::runtime(format!(
                            "cannot require directory '{}'",
                            absolute_path.display()
                        ))
                    })?;

                    let cache_key = resolved_path.to_string_lossy().to_string();

                    // Check cache first
                    let cache = get_module_cache(&lua)?;
                    if let Ok(cached) = cache.get::<LuaValue>(cache_key.as_str()) {
                        if !cached.is_nil() {
                            return Ok(LuaMultiValue::from_vec(vec![cached]));
                        }
                    }

                    // Check if already being loaded (concurrent require)
                    if let Some(rx) = state.get_pending(resolved_path) {
                        return rx
                            .recv()
                            .await
                            .into_lua_err()
                            .context("require interrupted")?;
                    }

                    let tx = state.create_pending(resolved_path);

                    // Load and execute the module
                    // Use absolute path for chunk name so nested requires can resolve correctly
                    let chunk_name = format!("{FILE_CHUNK_PREFIX}{}", resolved_path.display());
                    let chunk_bytes = read_file(resolved_path).await.map_err(|e| {
                        LuaError::runtime(format!(
                            "cannot read '{}': {}",
                            resolved_path.display(),
                            e
                        ))
                    })?;

                    let chunk = lua.load(chunk_bytes).set_name(chunk_name);

                    let thread_id = lua.push_thread_back(chunk, ())?;
                    lua.track_thread(thread_id);
                    lua.wait_for_thread(thread_id).await;

                    let result = lua
                        .get_thread_result(thread_id)
                        .expect("thread tracked and waited");

                    // Cache the result (first value only, like standard require)
                    if let Ok(ref res) = result {
                        if let Some(first_value) = res.iter().next() {
                            cache.set(cache_key.as_str(), first_value.clone())?;
                        }
                    }

                    // Notify any waiting requires
                    if tx.receiver_count() > 0 {
                        tx.send(result.clone()).await.ok();
                        tx.close();
                    }

                    state.remove_pending(resolved_path);

                    result
                }
            }
        }
    })?;

    // Create a Rust function to capture caller path (sync, doesn't yield)
    let capture_caller = lua.create_function(|lua, ()| {
        let caller_path = get_caller_path(lua);
        lua.set_named_registry_value(
            CALLER_PATH_KEY,
            caller_path.as_ref().map(|p| p.to_string_lossy().to_string()),
        )?;
        Ok(())
    })?;

    // Store our async require and preprocessor in globals for the wrapper to access
    lua.globals().set("__lune_async_require", require_fn)?;
    lua.globals()
        .set("__lune_capture_caller", capture_caller)?;

    // Create a Luau wrapper that:
    // 1. Captures the caller path
    // 2. Delegates everything to our async require which handles:
    //    - Alias paths (@...) - resolved via .luaurc files or registered modules
    //    - ScriptReference userdata
    //    - Absolute paths (/)
    //    - Relative paths (./ ../)
    let wrapper_code = r#"
-- Return a function that wraps require behavior
return function(arg)
    -- Capture the caller path first (sync, doesn't yield)
    __lune_capture_caller()
    -- All paths go through our async require which handles everything
    return __lune_async_require(arg)
end
"#;

    // Load the wrapper code
    // The wrapper's chunk name doesn't matter since we capture the caller path separately
    let wrapper: LuaFunction = lua
        .load(wrapper_code)
        .set_name("=require_wrapper")
        .call(())?;

    Ok(LuaValue::Function(wrapper))
}

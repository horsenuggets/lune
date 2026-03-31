use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use async_channel::{Receiver, Sender};
use async_fs::read as read_file;
use mlua::prelude::*;
use mlua_luau_scheduler::LuaSchedulerExt;
use serde::Deserialize;

use crate::globals::script::{ScriptReference, pop_script_path, push_script_path};
use crate::require::RequireResolver;
use lune_utils::path::{
    LuauModulePath, clean_path_and_make_absolute,
    constants::{FILE_CHUNK_PREFIX, FILE_NAME_CONFIG},
    relative_path_normalize,
};

type RequireResult = LuaResult<LuaMultiValue>;
type RequireResultSender = Sender<RequireResult>;
type RequireResultReceiver = Receiver<RequireResult>;

/// Type for bundled files from standalone executables
type BundledFiles = HashMap<String, Vec<u8>>;

/// Type for bundled aliases from standalone executables
type BundledAliases = HashMap<String, String>;

/// Normalize path separators to forward slashes for consistent bundled
/// file lookups. Bundled keys always use forward slashes, but on Windows
/// path operations produce backslashes.
fn normalize_separators(path: &str) -> String {
    path.replace('\\', "/")
}

/// Normalize a path string to a bundled key format.
///
/// Bundled keys use Unix-style paths with a leading `/` (e.g.,
/// `/Packages/Commandline.luau`). On Windows, OS path operations can
/// corrupt these virtual paths by prepending drive letters (e.g.,
/// `D:/Packages/...`). This function strips any drive prefix and
/// ensures the path starts with `/`.
fn normalize_to_bundle_key(path: &str) -> String {
    let normalized = normalize_separators(path);

    // Strip Windows drive prefix (e.g., "C:/Packages/..." -> "/Packages/...")
    if normalized.len() >= 3
        && normalized.as_bytes()[0].is_ascii_alphabetic()
        && normalized.as_bytes()[1] == b':'
        && normalized.as_bytes()[2] == b'/'
    {
        return normalized[2..].to_string();
    }

    normalized
}

/// Check if a path represents a virtual bundled path.
///
/// Bundled paths start with `/` and exist only in the embedded bundle,
/// not on the real filesystem. OS path operations must not be used on
/// these paths because they produce platform-dependent results (e.g.,
/// on Windows, `/Packages` resolves to `D:\Packages`).
fn is_bundled_path(path: &Path) -> bool {
    let s = path.to_string_lossy();
    let normalized = normalize_separators(&s);
    normalized.starts_with('/')
        && !(normalized.len() >= 3
            && normalized.as_bytes()[1] == b':'
            && normalized.as_bytes()[2] == b'/')
}

/// Resolve a relative path against a bundled caller path using pure
/// string manipulation. This avoids OS path operations that corrupt
/// virtual paths on Windows.
///
/// Given caller `/Packages/Commandline.luau` and target `./_Index/foo`,
/// returns `/Packages/_Index/foo`.
fn resolve_bundled_relative(caller: &Path, relative: &Path) -> PathBuf {
    let caller_str = normalize_separators(&caller.display().to_string());

    // Get the parent directory of the caller
    let caller_dir = if caller_str.ends_with('/') {
        caller_str.trim_end_matches('/').to_string()
    } else {
        match caller_str.rfind('/') {
            Some(idx) => caller_str[..idx].to_string(),
            None => String::new(),
        }
    };

    let rel_str = normalize_separators(&relative.display().to_string());

    // Join caller_dir with the relative path
    let joined = if rel_str.starts_with("./") {
        format!("{}/{}", caller_dir, &rel_str[2..])
    } else if rel_str.starts_with("../") {
        // Walk up directories for each ../
        let mut dir = caller_dir.clone();
        let mut rest = rel_str.as_str();
        while let Some(stripped) = rest.strip_prefix("../") {
            rest = stripped;
            if let Some(idx) = dir.rfind('/') {
                dir = dir[..idx].to_string();
            }
        }
        if rest.is_empty() {
            dir
        } else {
            format!("{}/{}", dir, rest)
        }
    } else {
        format!("{}/{}", caller_dir, rel_str)
    };

    // Clean any remaining ./ components
    let cleaned = joined
        .replace("/./", "/")
        .trim_end_matches("/.")
        .to_string();

    PathBuf::from(cleaned)
}

/// Try to get bundled source for a path from app_data
fn get_bundled_source(lua: &Lua, path: &Path) -> Option<Vec<u8>> {
    let bundled = lua.app_data_ref::<BundledFiles>()?;

    // Normalize to bundle key format, stripping any Windows drive prefix
    let key = normalize_to_bundle_key(&path.display().to_string());
    if let Some(source) = bundled.get(&key) {
        return Some(source.clone());
    }

    // Try canonical path (handles real filesystem paths)
    if let Ok(canonical) = path.canonicalize() {
        let key = normalize_to_bundle_key(&canonical.display().to_string());
        if let Some(source) = bundled.get(&key) {
            return Some(source.clone());
        }
    }

    None
}

/// Try to resolve a module path in bundled files.
/// Returns the resolved path if found in bundled files.
/// This handles .luau/.lua extensions and init.luau patterns.
fn resolve_bundled_module(lua: &Lua, module_path: &Path) -> Option<PathBuf> {
    let bundled = lua.app_data_ref::<BundledFiles>()?;
    // Normalize to bundle key format, stripping Windows drive prefixes
    let base = normalize_to_bundle_key(&module_path.display().to_string());

    // Try exact path first
    if bundled.contains_key(&base) {
        return Some(PathBuf::from(&base));
    }

    // Try with .luau extension
    let with_luau = format!("{}.luau", base);
    if bundled.contains_key(&with_luau) {
        return Some(PathBuf::from(&with_luau));
    }

    // Try with .lua extension
    let with_lua = format!("{}.lua", base);
    if bundled.contains_key(&with_lua) {
        return Some(PathBuf::from(&with_lua));
    }

    // Try as directory with init.luau
    let init_luau = format!("{}/init.luau", base);
    if bundled.contains_key(&init_luau) {
        return Some(PathBuf::from(&init_luau));
    }

    // Try as directory with init.lua
    let init_lua = format!("{}/init.lua", base);
    if bundled.contains_key(&init_lua) {
        return Some(PathBuf::from(&init_lua));
    }

    None
}

/// Try to resolve an alias from bundled aliases
fn get_bundled_alias(lua: &Lua, alias: &str) -> Option<PathBuf> {
    let bundled = lua.app_data_ref::<BundledAliases>()?;

    // Try exact match first
    if let Some(canonical) = bundled.get(alias) {
        return Some(PathBuf::from(canonical));
    }

    None
}

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
            None => break,                         // No more stack frames
            Some(Some(path)) => return Some(path), // Found a valid source
            Some(None) => continue,                // Skip this frame
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
        if is_bundled_path(caller_dir) {
            // In bundled context, use string-based resolution
            let dir_str = normalize_separators(&caller_dir.display().to_string());
            let resolved = match rest {
                Some(rest_path) => format!("{}/{}", dir_str, rest_path),
                None => dir_str,
            };
            return Some(PathBuf::from(resolved));
        }
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
fn resolve_require_arg(arg: &LuaValue, caller_path: Option<&Path>) -> LuaResult<ResolveResult> {
    match arg {
        LuaValue::String(s) => {
            let path_str: String = s.to_str()?.to_string();

            if path_str.starts_with('/') {
                // Absolute path — treat as a bundled virtual path on all
                // platforms. Do NOT use clean_path_and_make_absolute which
                // would prepend the CWD drive letter on Windows.
                let abs = PathBuf::from(&path_str);
                let rel = caller_path
                    .map(|cp| make_relative_path(cp, &abs))
                    .unwrap_or_else(|| PathBuf::from(format!(".{}", path_str)));
                Ok(ResolveResult::FilePath(rel, abs))
            } else if path_str.starts_with("./") || path_str.starts_with("../") {
                // Relative path
                let rel = relative_path_normalize(Path::new(&path_str));
                let abs = if let Some(caller) = caller_path {
                    if is_bundled_path(caller) {
                        // Caller is a virtual bundled path — resolve using
                        // pure string manipulation to avoid OS path
                        // operations that corrupt paths on Windows
                        resolve_bundled_relative(caller, &rel)
                    } else {
                        // For init.luau modules, the caller_path is the
                        // directory itself (chunk name strips "/init.luau"),
                        // so we use it directly instead of calling parent()
                        let caller_dir = if caller.is_dir() {
                            caller
                        } else {
                            caller.parent().unwrap_or(caller)
                        };
                        clean_path_and_make_absolute(&caller_dir.join(&rel))
                    }
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

                    // Handle custom aliases - check bundled aliases first, then .luaurc files.
                    // For init.luau modules, the caller_path is the directory itself
                    // (since the chunk name strips "/init.luau"), so we use it directly.
                    // For regular files, we use the parent directory.
                    let caller_dir = caller_path
                        .as_ref()
                        .map(|p| {
                            if p.is_dir() {
                                p.to_path_buf()
                            } else {
                                p.parent()
                                    .map(|parent| parent.to_path_buf())
                                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
                            }
                        })
                        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

                    // For standalone executables, try bundled aliases first
                    // Bundled aliases return paths that don't need filesystem resolution
                    let resolved_path: PathBuf = if let Some(bundled_path) =
                        get_bundled_alias(&lua, &alias)
                    {
                        bundled_path
                    } else if let Some(alias_path) = resolve_alias(&alias, &caller_dir) {
                        // Try bundled files first (for standalone executables with virtual paths)
                        if let Some(bundled_path) = resolve_bundled_module(&lua, &alias_path) {
                            bundled_path
                        } else {
                            // Fall back to filesystem resolution
                            let resolved = LuauModulePath::resolve(&alias_path).map_err(|e| {
                                LuaError::runtime(format!(
                                    "cannot find module '{}': {:?}",
                                    alias_path.display(),
                                    e
                                ))
                            })?;

                            resolved
                                .target()
                                .as_file()
                                .ok_or_else(|| {
                                    LuaError::runtime(format!(
                                        "cannot require directory '{}'",
                                        alias_path.display()
                                    ))
                                })?
                                .to_path_buf()
                        }
                    } else {
                        return Err(LuaError::runtime(format!("cannot find alias '{}'", alias)));
                    };

                    let cache_key = resolved_path.to_string_lossy().to_string();

                    // Check cache first
                    let cache = get_module_cache(&lua)?;
                    if let Ok(cached) = cache.get::<LuaValue>(cache_key.as_str()) {
                        if !cached.is_nil() {
                            return Ok(LuaMultiValue::from_vec(vec![cached]));
                        }
                    }

                    // Check if already being loaded (concurrent require)
                    if let Some(rx) = state.get_pending(&resolved_path) {
                        return rx
                            .recv()
                            .await
                            .into_lua_err()
                            .context("require interrupted")?;
                    }

                    let tx = state.create_pending(&resolved_path);

                    // Load and execute the module
                    let chunk_name = format!("{FILE_CHUNK_PREFIX}{}", resolved_path.display());

                    // Try bundled source first, then filesystem
                    let chunk_bytes =
                        if let Some(bundled) = get_bundled_source(&lua, &resolved_path) {
                            bundled
                        } else {
                            read_file(&resolved_path).await.map_err(|e| {
                                LuaError::runtime(format!(
                                    "cannot read '{}': {}",
                                    resolved_path.display(),
                                    e
                                ))
                            })?
                        };

                    // Create a custom environment for this module with a static script reference
                    let module_env = lua.create_table()?;
                    let module_script = ScriptReference::new(&resolved_path);
                    module_env.set("script", module_script)?;

                    // Set metatable to inherit from globals
                    let env_mt = lua.create_table()?;
                    env_mt.set("__index", lua.globals())?;
                    env_mt.set("__newindex", lua.globals())?;
                    module_env.set_metatable(Some(env_mt))?;

                    let chunk = lua
                        .load(chunk_bytes)
                        .set_name(chunk_name)
                        .set_environment(module_env);

                    // Push the script path before executing the module (for dynamic fallback)
                    push_script_path(&lua, &resolved_path.display().to_string())?;

                    let thread_id = lua.push_thread_back(chunk, ())?;
                    lua.track_thread(thread_id);
                    lua.wait_for_thread(thread_id).await;

                    let result = lua
                        .get_thread_result(thread_id)
                        .expect("thread tracked and waited");

                    // Pop the script path after module execution
                    pop_script_path(&lua)?;

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

                    state.remove_pending(&resolved_path);

                    result
                }
                ResolveResult::FilePath(_relative_path, absolute_path) => {
                    // Try bundled files first (for standalone executables with virtual paths)
                    // Then fall back to filesystem resolution
                    let resolved_path: PathBuf = if let Some(bundled_path) =
                        resolve_bundled_module(&lua, &absolute_path)
                    {
                        bundled_path
                    } else {
                        // Resolve to actual filesystem path (handling .luau/.lua extensions)
                        let resolved = LuauModulePath::resolve(&absolute_path).map_err(|e| {
                            LuaError::runtime(format!(
                                "cannot find module '{}': {:?}",
                                absolute_path.display(),
                                e
                            ))
                        })?;

                        resolved
                            .target()
                            .as_file()
                            .ok_or_else(|| {
                                LuaError::runtime(format!(
                                    "cannot require directory '{}'",
                                    absolute_path.display()
                                ))
                            })?
                            .to_path_buf()
                    };

                    let cache_key = resolved_path.to_string_lossy().to_string();

                    // Check cache first
                    let cache = get_module_cache(&lua)?;
                    if let Ok(cached) = cache.get::<LuaValue>(cache_key.as_str()) {
                        if !cached.is_nil() {
                            return Ok(LuaMultiValue::from_vec(vec![cached]));
                        }
                    }

                    // Check if already being loaded (concurrent require)
                    if let Some(rx) = state.get_pending(&resolved_path) {
                        return rx
                            .recv()
                            .await
                            .into_lua_err()
                            .context("require interrupted")?;
                    }

                    let tx = state.create_pending(&resolved_path);

                    // Load and execute the module
                    // Use absolute path for chunk name so nested requires can resolve correctly
                    let chunk_name = format!("{FILE_CHUNK_PREFIX}{}", resolved_path.display());

                    // Try bundled source first, then filesystem
                    let chunk_bytes =
                        if let Some(bundled) = get_bundled_source(&lua, &resolved_path) {
                            bundled
                        } else {
                            read_file(&resolved_path).await.map_err(|e| {
                                LuaError::runtime(format!(
                                    "cannot read '{}': {}",
                                    resolved_path.display(),
                                    e
                                ))
                            })?
                        };

                    // Create a custom environment for this module with a static script reference
                    let module_env = lua.create_table()?;
                    let module_script = ScriptReference::new(&resolved_path);
                    module_env.set("script", module_script)?;

                    // Set metatable to inherit from globals
                    let env_mt = lua.create_table()?;
                    env_mt.set("__index", lua.globals())?;
                    env_mt.set("__newindex", lua.globals())?;
                    module_env.set_metatable(Some(env_mt))?;

                    let chunk = lua
                        .load(chunk_bytes)
                        .set_name(chunk_name)
                        .set_environment(module_env);

                    // Push the script path before executing the module (for dynamic fallback)
                    push_script_path(&lua, &resolved_path.display().to_string())?;

                    let thread_id = lua.push_thread_back(chunk, ())?;
                    lua.track_thread(thread_id);
                    lua.wait_for_thread(thread_id).await;

                    let result = lua
                        .get_thread_result(thread_id)
                        .expect("thread tracked and waited");

                    // Pop the script path after module execution
                    pop_script_path(&lua)?;

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

                    state.remove_pending(&resolved_path);

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
            caller_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
        )?;
        Ok(())
    })?;

    // Store our async require and preprocessor in globals for the wrapper to access
    lua.globals().set("__lune_async_require", require_fn)?;
    lua.globals().set("__lune_capture_caller", capture_caller)?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn normalize_separators_converts_backslashes() {
        assert_eq!(
            normalize_separators(r"C:\Users\foo\Source\Module.luau"),
            "C:/Users/foo/Source/Module.luau"
        );
    }

    #[test]
    fn normalize_separators_preserves_forward_slashes() {
        assert_eq!(
            normalize_separators("/Source/Module.luau"),
            "/Source/Module.luau"
        );
    }

    #[test]
    fn normalize_separators_handles_mixed_slashes() {
        assert_eq!(
            normalize_separators(r"/Source\Nested/Module.luau"),
            "/Source/Nested/Module.luau"
        );
    }

    #[test]
    fn bundled_lookup_matches_with_backslash_path() {
        // Simulate bundled files with forward-slash keys (as the bundler
        // produces) and verify that a backslash path would match after
        // normalization
        let mut bundled: HashMap<String, Vec<u8>> = HashMap::new();
        bundled.insert(
            "/Source/MyModule/init.luau".to_string(),
            b"return {}".to_vec(),
        );
        bundled.insert(
            "/Source/MyModule/Helper.luau".to_string(),
            b"return {}".to_vec(),
        );

        // Simulate what Windows would produce
        let windows_path = r"\Source\MyModule\Helper.luau";
        let normalized = normalize_separators(windows_path);
        assert_eq!(normalized, "/Source/MyModule/Helper.luau");
        assert!(bundled.contains_key(&normalized));

        // Forward-slash path should work directly
        let unix_path = "/Source/MyModule/Helper.luau";
        let normalized = normalize_separators(unix_path);
        assert!(bundled.contains_key(&normalized));
    }

    #[test]
    fn bundled_lookup_with_init_luau_pattern() {
        let mut bundled: HashMap<String, Vec<u8>> = HashMap::new();
        bundled.insert(
            "/Source/MyModule/init.luau".to_string(),
            b"return {}".to_vec(),
        );

        // Simulate resolve_bundled_module logic with Windows backslash base
        let base = normalize_separators(r"\Source\MyModule");
        let init_luau = format!("{}/init.luau", base);
        assert_eq!(init_luau, "/Source/MyModule/init.luau");
        assert!(bundled.contains_key(&init_luau));
    }

    // -- normalize_to_bundle_key --

    #[test]
    fn bundle_key_strips_windows_drive_prefix() {
        assert_eq!(
            normalize_to_bundle_key("D:/Packages/_Index/foo/bar"),
            "/Packages/_Index/foo/bar"
        );
    }

    #[test]
    fn bundle_key_strips_windows_drive_with_backslashes() {
        assert_eq!(
            normalize_to_bundle_key(r"D:\Packages\_Index\foo\bar"),
            "/Packages/_Index/foo/bar"
        );
    }

    #[test]
    fn bundle_key_preserves_unix_path() {
        assert_eq!(
            normalize_to_bundle_key("/Packages/Commandline.luau"),
            "/Packages/Commandline.luau"
        );
    }

    #[test]
    fn bundle_key_preserves_relative_path() {
        assert_eq!(
            normalize_to_bundle_key("./Packages/Commandline.luau"),
            "./Packages/Commandline.luau"
        );
    }

    // -- is_bundled_path --

    #[test]
    fn bundled_path_detects_virtual_paths() {
        assert!(is_bundled_path(Path::new("/Packages/Commandline.luau")));
        assert!(is_bundled_path(Path::new("/Source/MyModule/init.luau")));
    }

    #[test]
    fn bundled_path_rejects_relative_paths() {
        assert!(!is_bundled_path(Path::new("./Packages/Commandline.luau")));
        assert!(!is_bundled_path(Path::new("Packages/Commandline.luau")));
    }

    #[test]
    fn bundled_path_rejects_windows_absolute_paths() {
        // Windows absolute paths have a drive letter
        assert!(!is_bundled_path(Path::new("C:/Packages/Commandline.luau")));
    }

    // -- resolve_bundled_relative --

    #[test]
    fn bundled_relative_simple_dot_slash() {
        let caller = Path::new("/Packages/Commandline.luau");
        let relative = Path::new("./_Index/foo/bar");
        let result = resolve_bundled_relative(caller, relative);
        assert_eq!(result, PathBuf::from("/Packages/_Index/foo/bar"));
    }

    #[test]
    fn bundled_relative_dot_dot_slash() {
        let caller = Path::new("/Source/MyModule/SubDir/File.luau");
        let relative = Path::new("../Helper.luau");
        let result = resolve_bundled_relative(caller, relative);
        assert_eq!(result, PathBuf::from("/Source/MyModule/Helper.luau"));
    }

    #[test]
    fn bundled_relative_from_init_dir() {
        // init.luau caller paths are the directory itself
        let caller = Path::new("/Source/MyModule/");
        let relative = Path::new("./Helper");
        let result = resolve_bundled_relative(caller, relative);
        assert_eq!(result, PathBuf::from("/Source/MyModule/Helper"));
    }

    #[test]
    fn bundled_relative_wally_redirect() {
        // This is the exact scenario that fails on Windows:
        // Packages/Commandline.luau contains require("./_Index/horsenuggets_commandline-luau@0.2.0/commandline-luau")
        let caller = Path::new("/Packages/Commandline.luau");
        let relative = Path::new("./_Index/horsenuggets_commandline-luau@0.2.0/commandline-luau");
        let result = resolve_bundled_relative(caller, relative);
        assert_eq!(
            result,
            PathBuf::from("/Packages/_Index/horsenuggets_commandline-luau@0.2.0/commandline-luau")
        );
    }

    #[test]
    fn bundled_lookup_with_windows_drive_prefix() {
        // Verify that even if a Windows drive prefix sneaks through,
        // the bundle lookup still works
        let mut bundled: HashMap<String, Vec<u8>> = HashMap::new();
        bundled.insert(
            "/Packages/_Index/foo/bar.luau".to_string(),
            b"return {}".to_vec(),
        );

        // Simulate what happens on Windows when path gets a drive prefix
        let windows_path = "D:/Packages/_Index/foo/bar.luau";
        let key = normalize_to_bundle_key(windows_path);
        assert_eq!(key, "/Packages/_Index/foo/bar.luau");
        assert!(bundled.contains_key(&key));
    }
}

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use regex::Regex;
use serde::Deserialize;

use lune::JsArtifact;
use lune_std_js::{
    module::ModuleId,
    source::{HostSourceProvider, NamespacedProvider},
    store::ModuleGraph,
};

use super::js_imports::discover_js_imports;

/// Structure representing a .luaurc configuration file
#[derive(Debug, Clone, Deserialize, Default)]
struct LuauConfig {
    #[serde(default)]
    aliases: HashMap<String, String>,
}

/// Result of bundling: files and alias mappings
pub struct BundleResult {
    pub files: HashMap<String, Vec<u8>>,
    pub aliases: HashMap<String, String>,
    pub js_modules: BTreeMap<String, JsArtifact>,
    pub js_roots: BTreeMap<String, String>,
}

/// A bundler that resolves all dependencies of a Luau file
pub struct Bundler {
    /// Base directory for computing relative paths (project root)
    /// This may be expanded as we discover files outside the initial base
    base_dir: PathBuf,
    /// Cached .luaurc configs by directory
    configs: HashMap<PathBuf, Option<LuauConfig>>,
    /// Already processed files to avoid cycles
    processed: HashSet<PathBuf>,
    /// The bundled files: canonical path -> source (relativized at the end)
    files_canonical: HashMap<PathBuf, Vec<u8>>,
    /// Alias mappings: alias -> canonical path (relativized at the end)
    aliases_canonical: HashMap<String, PathBuf>,
    /// JavaScript imports discovered in Luau source files.
    js_imports: Vec<(PathBuf, String, Option<String>)>,
    /// All context roots, even those without literal imports.
    js_roots: BTreeSet<Option<String>>,
    /// Contexts built from options the build cannot inspect, with their file.
    js_unresolved: Vec<(PathBuf, String)>,
    /// Entry directory used as the default JavaScript module root.
    js_entry_dir: PathBuf,
    /// Regex to find require calls
    require_regex: Regex,
}

impl Bundler {
    pub fn new(entry_path: &Path) -> Result<Self> {
        // Find the project root by searching upward for .luaurc files
        let base_dir = Self::find_project_root(entry_path);
        let js_entry_dir = entry_path
            .canonicalize()
            .ok()
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| base_dir.clone());
        Ok(Self {
            base_dir,
            js_entry_dir,
            configs: HashMap::new(),
            processed: HashSet::new(),
            files_canonical: HashMap::new(),
            aliases_canonical: HashMap::new(),
            js_imports: Vec::new(),
            js_roots: BTreeSet::new(),
            js_unresolved: Vec::new(),
            // Match require("...") or require('...')
            require_regex: Regex::new(r#"require\s*\(\s*["']([^"']+)["']\s*\)"#)?,
        })
    }

    /// Get the base directory (project root) for making paths relative
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Find the project root by searching upward for .luaurc files.
    /// Returns the directory containing the highest-level .luaurc,
    /// or the entry file's parent directory if no .luaurc is found.
    fn find_project_root(entry_path: &Path) -> PathBuf {
        let start_dir = entry_path
            .canonicalize()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| {
                entry_path
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
            });

        let mut highest_luaurc_dir: Option<PathBuf> = None;
        let mut search_dir = start_dir.clone();

        loop {
            let config_path = search_dir.join(".luaurc");
            if config_path.exists() {
                highest_luaurc_dir = Some(search_dir.clone());
            }

            if !search_dir.pop() {
                break;
            }
        }

        highest_luaurc_dir.unwrap_or(start_dir)
    }

    /// Find the common ancestor directory of two paths
    fn common_ancestor(path1: &Path, path2: &Path) -> PathBuf {
        let components1: Vec<_> = path1.components().collect();
        let components2: Vec<_> = path2.components().collect();

        let mut common = PathBuf::new();
        for (c1, c2) in components1.iter().zip(components2.iter()) {
            if c1 == c2 {
                common.push(c1);
            } else {
                break;
            }
        }

        // Ensure we return at least the root
        if common.as_os_str().is_empty() {
            PathBuf::from("/")
        } else {
            common
        }
    }

    /// Expand base_dir to include a new path if needed
    fn expand_base_dir(&mut self, path: &Path) {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if !canonical.starts_with(&self.base_dir) {
            self.base_dir = Self::common_ancestor(&self.base_dir, &canonical);
        }
    }

    /// Bundle all dependencies starting from the entry file
    pub fn bundle(&mut self, entry_path: &Path, js_include: &[String]) -> Result<BundleResult> {
        // First pass: collect all files with canonical paths
        self.process_file(entry_path)?;
        let (js_modules, js_roots) = self.process_js_modules(js_include)?;
        // An opaque context is only fatal when nothing could be embedded from it.
        if let Some((file, receiver)) = self.js_unresolved.first() {
            if js_include.is_empty() {
                anyhow::bail!(
                    "JavaScript context \"{receiver}\" in \"{}\" is created from options the build cannot inspect; pass a literal table with a string ModuleRoot, or add --js-include",
                    file.display()
                );
            }
            for (file, receiver) in &self.js_unresolved {
                eprintln!(
                    "Warning: JavaScript context \"{receiver}\" in \"{}\" is created from options the build cannot inspect; make sure --js-include embeds its modules",
                    file.display()
                );
            }
        }

        // Now relativize all paths using the (possibly expanded) base_dir
        let mut files = HashMap::new();
        for (canonical_path, source) in &self.files_canonical {
            let key = self.normalize_path(canonical_path);
            files.insert(key, source.clone());
        }

        let mut aliases = HashMap::new();
        for (alias, canonical_path) in &self.aliases_canonical {
            let relative_path = self.normalize_path(canonical_path);
            aliases.insert(alias.clone(), relative_path);
        }

        Ok(BundleResult {
            files,
            aliases,
            js_modules,
            js_roots,
        })
    }

    /// Process a single file and its dependencies
    fn process_file(&mut self, file_path: &Path) -> Result<()> {
        let canonical = file_path
            .canonicalize()
            .unwrap_or_else(|_| file_path.to_path_buf());

        if self.processed.contains(&canonical) {
            return Ok(());
        }
        self.processed.insert(canonical.clone());

        // Expand base_dir if this file is outside the current base
        self.expand_base_dir(&canonical);

        // Read the file
        let source = fs::read(file_path)
            .with_context(|| format!("failed to read file: {}", file_path.display()))?;

        // Store the file with its canonical path (will be relativized at the end)
        self.files_canonical
            .insert(canonical.clone(), source.clone());

        // Find all require paths first (to avoid borrow issues)
        let source_str = String::from_utf8_lossy(&source);
        let file_dir = file_path.parent().unwrap_or(Path::new(".")).to_path_buf();

        let discovery = discover_js_imports(&source_str).map_err(|errors| {
            anyhow!(
                errors
                    .into_iter()
                    .map(|error| error.to_string())
                    .collect::<Vec<_>>()
                    .join("\n")
            )
            .context(format!(
                "failed to parse Luau file: {}",
                file_path.display()
            ))
        })?;
        self.js_imports.extend(
            discovery
                .imports
                .into_iter()
                .map(|(specifier, module_root)| (canonical.clone(), specifier, module_root)),
        );
        self.js_roots.extend(discovery.context_roots);
        self.js_unresolved.extend(
            discovery
                .unresolved_receivers
                .into_iter()
                .map(|receiver| (file_path.to_path_buf(), receiver)),
        );
        for specifier in discovery.unknown_specifiers {
            eprintln!(
                "Warning: JavaScript import \"{specifier}\" in \"{}\" has an untraced receiver; use --js-include if it belongs to a JavaScript context",
                file_path.display()
            );
        }
        if discovery.has_dynamic_imports {
            eprintln!(
                "Warning: dynamic JavaScript imports in \"{}\" may require --js-include",
                file_path.display()
            );
        }

        let require_paths: Vec<String> = self
            .require_regex
            .captures_iter(&source_str)
            .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_string()))
            .filter(|p| !p.starts_with("@lune/"))
            .collect();

        // Now process each require
        for require_path in require_paths {
            if let Some(resolved) = self.resolve_require(&require_path, &file_dir) {
                let actual_file = self.find_module_file(&resolved);
                if let Some(module_path) = actual_file {
                    if module_path.exists() {
                        self.process_file(&module_path)?;
                    }
                }
            }
        }

        Ok(())
    }

    fn process_js_modules(
        &mut self,
        js_include: &[String],
    ) -> Result<(BTreeMap<String, JsArtifact>, BTreeMap<String, String>)> {
        if self.js_imports.is_empty() && js_include.is_empty() {
            return Ok((BTreeMap::new(), BTreeMap::new()));
        }

        let mut artifacts = BTreeMap::new();

        // Expand the bundle so roots above the entry directory keep a relocated
        // virtual path that matches runtime `ModuleRoot` semantics.
        let mut roots = BTreeSet::from([None]);
        roots.extend(self.js_roots.iter().cloned());
        roots.extend(
            self.js_imports
                .iter()
                .map(|(_, _, module_root)| module_root.clone()),
        );
        let root_paths = roots
            .iter()
            .map(|module_root| self.js_root_for_luau(Path::new(""), module_root.as_deref()))
            .collect::<Result<Vec<_>>>()?;
        for root in &root_paths {
            self.expand_base_dir(root);
        }

        for (caller, specifier, module_root) in &self.js_imports {
            let root = self.js_root_for_luau(caller, module_root.as_deref())?;
            let host = Arc::new(HostSourceProvider::new(&root).map_err(|error| {
                anyhow!(error).context(format!(
                    "failed to open JavaScript source root: {}",
                    root.display()
                ))
            })?);
            let key = lune_std_js::paths::module_root_key(module_root.as_deref());
            let provider = NamespacedProvider::new(key.clone(), host.clone());
            let mut base = lune_std_js::paths::module_base_for_path(caller, host.root(), true)
                .map_err(|error| anyhow!(error.to_string()))?;
            base = ModuleId::parse(format!("{key}{base}"))
                .map_err(|error| anyhow!(error.to_string()))?;
            let specifier = if specifier.starts_with('/') {
                format!("{key}{specifier}")
            } else {
                specifier.clone()
            };
            let graph = ModuleGraph::load_specifier_with_dynamic(&provider, &base, &specifier)
                .map_err(|error| {
                    anyhow!(error).context(format!(
                        "failed to bundle JavaScript import \"{specifier}\" from \"{}\"",
                        caller.display()
                    ))
                })?;
            Self::extend_js_artifacts(&mut artifacts, "", graph);
        }

        // Includes belong to whichever context root contains them. When no root does,
        // the project root namespace is used.
        for pattern in js_include {
            let paths = expand_js_include(pattern)?;
            for path in paths {
                let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                let requested = absolute_path(&path);
                let mut matched = false;
                for module_root in &roots {
                    let root = self.js_root_for_luau(&canonical, module_root.as_deref())?;
                    let canonical_root = root.canonicalize().unwrap_or(root.clone());
                    if !canonical.starts_with(&canonical_root) {
                        continue;
                    }
                    let host = Arc::new(HostSourceProvider::new(&root).map_err(|error| {
                        anyhow!(error).context(format!(
                            "failed to open JavaScript source root: {}",
                            root.display()
                        ))
                    })?);
                    let key = lune_std_js::paths::module_root_key(module_root.as_deref());
                    let provider = NamespacedProvider::new(key.clone(), host.clone());
                    let id = host.module_id_for_path(&requested).map_err(|error| {
                        anyhow!(error).context(format!(
                            "failed to identify included JavaScript module: {}",
                            path.display()
                        ))
                    })?;
                    let id = ModuleId::parse(format!("{key}{id}"))
                        .map_err(|error| anyhow!(error.to_string()))?;
                    let graph = ModuleGraph::load_with_dynamic(&provider, id).map_err(|error| {
                        anyhow!(error).context(format!(
                            "failed to bundle included JavaScript module: {}",
                            path.display()
                        ))
                    })?;
                    Self::extend_js_artifacts(&mut artifacts, "", graph);
                    matched = true;
                }
                if !matched {
                    anyhow::bail!(
                        "included JavaScript module is outside every module root: {}",
                        path.display()
                    );
                }
            }
        }

        let mut relocated_roots = BTreeMap::new();
        for module_root in &roots {
            let root = self.js_root_for_luau(Path::new(""), module_root.as_deref())?;
            let canonical = root.canonicalize().unwrap_or(root);
            relocated_roots.insert(
                module_root.clone().unwrap_or_default(),
                normalize_bundle_path(&canonical, &self.base_dir),
            );
        }
        Ok((artifacts, relocated_roots))
    }

    /// Resolves a JavaScript module root against the project root.
    fn js_root_for_luau(&self, _caller: &Path, module_root: Option<&str>) -> Result<PathBuf> {
        Ok(module_root.map_or_else(
            || self.js_entry_dir.clone(),
            |module_root| {
                let root = PathBuf::from(module_root);
                if root.is_absolute() {
                    root
                } else {
                    self.js_entry_dir.join(root)
                }
            },
        ))
    }

    fn extend_js_artifacts(
        artifacts: &mut BTreeMap<String, JsArtifact>,
        key: &str,
        graph: ModuleGraph,
    ) {
        for (id, module) in graph.into_modules() {
            if module.has_computed_dynamic_import() {
                eprintln!(
                    "Warning: computed dynamic imports in \"{id}\" require matching --js-include paths"
                );
            }
            let (_, code, source_map) = module.into_bundle_parts();
            artifacts.insert(
                format!("{key}{id}"),
                JsArtifact {
                    code: code.into_bytes(),
                    source_map: source_map.map(|map| map.to_json_string().into_bytes()),
                },
            );
        }
    }

    /// Normalize a path for use as a bundle key.
    /// Returns a path relative to the base directory, starting with '/'.
    /// Uses forward slashes on all platforms for portable bundled binaries.
    fn normalize_path(&self, path: &Path) -> String {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        normalize_bundle_path(&canonical, &self.base_dir)
    }

    /// Find the actual module file (handles init.luau pattern)
    fn find_module_file(&self, path: &Path) -> Option<PathBuf> {
        // Try exact path with extensions
        for ext in &["", ".luau", ".lua"] {
            let with_ext = if ext.is_empty() {
                path.to_path_buf()
            } else {
                path.with_extension(&ext[1..])
            };
            if with_ext.is_file() {
                return Some(with_ext);
            }
        }

        // Try as directory with init.luau
        if path.is_dir() {
            let init = path.join("init.luau");
            if init.is_file() {
                return Some(init);
            }
            let init_lua = path.join("init.lua");
            if init_lua.is_file() {
                return Some(init_lua);
            }
        }

        // Try adding /init.luau
        let init = path.join("init.luau");
        if init.is_file() {
            return Some(init);
        }

        None
    }

    /// Resolve a require path to an absolute path
    fn resolve_require(&mut self, require_path: &str, caller_dir: &Path) -> Option<PathBuf> {
        if require_path.starts_with('@') {
            // Alias path
            self.resolve_alias(require_path, caller_dir)
        } else if require_path.starts_with("./") || require_path.starts_with("../") {
            // Relative path
            Some(caller_dir.join(require_path))
        } else if require_path.starts_with('/') {
            // Absolute path
            Some(PathBuf::from(require_path))
        } else {
            // Bare path - treat as relative
            Some(caller_dir.join(require_path))
        }
    }

    /// Resolve an alias like @packages/Foo to an absolute path
    fn resolve_alias(&mut self, alias: &str, caller_dir: &Path) -> Option<PathBuf> {
        let alias_path = alias.strip_prefix('@')?;

        let (alias_name, rest) = match alias_path.find('/') {
            Some(idx) => (&alias_path[..idx], Some(&alias_path[idx + 1..])),
            None => (alias_path, None),
        };

        // Special case: @self refers to caller's directory
        if alias_name == "self" {
            let mut resolved = caller_dir.to_path_buf();
            if let Some(rest_path) = rest {
                resolved = resolved.join(rest_path);
            }
            return Some(resolved);
        }

        // Search for .luaurc files going up from caller_dir
        let mut search_dir = caller_dir.to_path_buf();
        loop {
            let config = self.get_config(&search_dir);
            if let Some(ref cfg) = config {
                if let Some(alias_value) = cfg.aliases.get(alias_name) {
                    let mut resolved = search_dir.join(alias_value);
                    if let Some(rest_path) = rest {
                        resolved = resolved.join(rest_path);
                    }

                    // Record the alias mapping for runtime resolution
                    // Store canonical path (will be relativized at the end)
                    if let Some(actual_file) = self.find_module_file(&resolved) {
                        if let Ok(canonical) = actual_file.canonicalize() {
                            self.aliases_canonical
                                .insert(format!("@{}", alias_path), canonical);
                        }
                    }

                    return Some(resolved);
                }
            }

            if !search_dir.pop() {
                break;
            }
        }

        None
    }

    /// Get or load a .luaurc config for a directory
    fn get_config(&mut self, dir: &Path) -> Option<LuauConfig> {
        if let Some(cached) = self.configs.get(dir) {
            return cached.clone();
        }

        let config_path = dir.join(".luaurc");
        let config = if config_path.exists() {
            fs::read_to_string(&config_path)
                .ok()
                .and_then(|content| serde_json::from_str(&content).ok())
        } else {
            None
        };

        self.configs.insert(dir.to_path_buf(), config.clone());
        config
    }
}

/// Makes an include path absolute so it can be compared against an absolute root.
fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| path.to_path_buf(), |dir| dir.join(path))
    }
}

fn expand_js_include(pattern: &str) -> Result<Vec<PathBuf>> {
    if !pattern.contains(['*', '?', '[']) {
        let path = PathBuf::from(pattern);
        if !path.is_file() {
            bail!("included JavaScript module does not exist: {pattern}");
        }
        return Ok(vec![path]);
    }

    let mut paths = Vec::new();
    for entry in
        glob::glob(pattern).with_context(|| format!("invalid --js-include pattern: {pattern}"))?
    {
        let path = entry.with_context(|| format!("failed to expand pattern: {pattern}"))?;
        if path.is_file() {
            paths.push(path);
        }
    }
    if paths.is_empty() {
        bail!("--js-include pattern matched no files: {pattern}");
    }
    Ok(paths)
}

/// Normalize a canonical path into a portable bundle key.
/// Makes the path relative to base_dir with a leading '/', using forward
/// slashes on all platforms. Falls back to the full path if it's outside
/// base_dir.
pub(super) fn normalize_bundle_path(canonical: &Path, base_dir: &Path) -> String {
    if let Ok(relative) = canonical.strip_prefix(base_dir) {
        format!("/{}", relative.display()).replace('\\', "/")
    } else {
        canonical.display().to_string().replace('\\', "/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- normalize_bundle_path (relative to base_dir) --

    #[test]
    fn unix_simple_relative() {
        let result = normalize_bundle_path(
            Path::new("/home/user/project/src/main.luau"),
            Path::new("/home/user/project"),
        );
        assert_eq!(result, "/src/main.luau");
    }

    #[test]
    fn unix_nested_relative() {
        let result = normalize_bundle_path(
            Path::new("/home/user/project/src/lib/utils/format.luau"),
            Path::new("/home/user/project"),
        );
        assert_eq!(result, "/src/lib/utils/format.luau");
    }

    #[test]
    fn unix_file_at_root() {
        let result = normalize_bundle_path(
            Path::new("/home/user/project/init.luau"),
            Path::new("/home/user/project"),
        );
        assert_eq!(result, "/init.luau");
    }

    #[test]
    fn unix_outside_base_dir() {
        let result = normalize_bundle_path(
            Path::new("/other/location/lib.luau"),
            Path::new("/home/user/project"),
        );
        assert_eq!(result, "/other/location/lib.luau");
    }

    #[test]
    fn unix_packages_alias() {
        let result = normalize_bundle_path(
            Path::new("/home/user/project/Packages/Commandline.luau"),
            Path::new("/home/user/project"),
        );
        assert_eq!(result, "/Packages/Commandline.luau");
    }

    #[test]
    fn unix_init_luau_in_subdir() {
        let result = normalize_bundle_path(
            Path::new("/home/user/project/Source/MyModule/init.luau"),
            Path::new("/home/user/project"),
        );
        assert_eq!(result, "/Source/MyModule/init.luau");
    }

    // -- Windows path simulation --
    //
    // On non-Windows, Path::new("C:\\foo\\bar") is treated as a single
    // component, so strip_prefix won't match. We test the backslash
    // replacement by constructing OsStrings that contain backslashes,
    // which is what actually happens on Windows.

    #[test]
    fn backslashes_in_relative_component() {
        // Simulate what Windows produces after strip_prefix: the relative
        // portion contains backslashes. We can't use strip_prefix
        // cross-platform, so test the replace logic directly.
        let with_backslash = format!("/{}", "Packages\\Commandline.luau");
        let normalized = with_backslash.replace('\\', "/");
        assert_eq!(normalized, "/Packages/Commandline.luau");
    }

    #[test]
    fn backslashes_deeply_nested() {
        let with_backslash = format!("/{}", "Source\\Lib\\Utils\\format.luau");
        let normalized = with_backslash.replace('\\', "/");
        assert_eq!(normalized, "/Source/Lib/Utils/format.luau");
    }

    #[test]
    fn mixed_separators() {
        let with_mixed = format!("/{}", "Source/Lib\\Utils\\format.luau");
        let normalized = with_mixed.replace('\\', "/");
        assert_eq!(normalized, "/Source/Lib/Utils/format.luau");
    }

    #[test]
    fn windows_drive_letter_fallback() {
        // When path is outside base_dir, the full path is used as-is.
        // On Windows this would include the drive letter with backslashes.
        let full_path = "C:\\Users\\dev\\project\\lib.luau";
        let normalized = full_path.replace('\\', "/");
        assert_eq!(normalized, "C:/Users/dev/project/lib.luau");
    }

    #[test]
    fn windows_unc_path_fallback() {
        let unc = "\\\\server\\share\\project\\lib.luau";
        let normalized = unc.replace('\\', "/");
        assert_eq!(normalized, "//server/share/project/lib.luau");
    }

    #[test]
    fn no_double_slashes_on_unix() {
        let result = normalize_bundle_path(
            Path::new("/home/user/project/src/main.luau"),
            Path::new("/home/user/project"),
        );
        assert!(!result.contains("//"), "Should not produce double slashes");
    }

    #[test]
    fn empty_relative_produces_root() {
        // When canonical == base_dir, relative is empty -> "/"
        let result = normalize_bundle_path(
            Path::new("/home/user/project"),
            Path::new("/home/user/project"),
        );
        assert_eq!(result, "/");
    }

    #[test]
    fn dot_luau_and_dot_lua_extensions() {
        let luau = normalize_bundle_path(Path::new("/project/src/mod.luau"), Path::new("/project"));
        let lua = normalize_bundle_path(Path::new("/project/src/mod.lua"), Path::new("/project"));
        assert_eq!(luau, "/src/mod.luau");
        assert_eq!(lua, "/src/mod.lua");
    }

    #[test]
    fn path_with_spaces() {
        let result = normalize_bundle_path(
            Path::new("/home/user/my project/src/main.luau"),
            Path::new("/home/user/my project"),
        );
        assert_eq!(result, "/src/main.luau");
    }

    #[test]
    fn path_with_unicode() {
        let result = normalize_bundle_path(
            Path::new("/home/user/проект/src/main.luau"),
            Path::new("/home/user/проект"),
        );
        assert_eq!(result, "/src/main.luau");
    }
}

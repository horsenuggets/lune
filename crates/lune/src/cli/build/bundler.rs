use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use regex::Regex;
use serde::Deserialize;

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
}

/// A bundler that resolves all dependencies of a Luau file
pub struct Bundler {
    /// Cached .luaurc configs by directory
    configs: HashMap<PathBuf, Option<LuauConfig>>,
    /// Already processed files to avoid cycles
    processed: HashSet<PathBuf>,
    /// The bundled files: path -> source
    files: HashMap<String, Vec<u8>>,
    /// Alias mappings: alias path -> canonical path
    aliases: HashMap<String, String>,
    /// Regex to find require calls
    require_regex: Regex,
}

impl Bundler {
    pub fn new(_entry_path: &Path) -> Result<Self> {
        Ok(Self {
            configs: HashMap::new(),
            processed: HashSet::new(),
            files: HashMap::new(),
            aliases: HashMap::new(),
            // Match require("...") or require('...')
            require_regex: Regex::new(r#"require\s*\(\s*["']([^"']+)["']\s*\)"#)?,
        })
    }

    /// Bundle all dependencies starting from the entry file
    pub fn bundle(&mut self, entry_path: &Path) -> Result<BundleResult> {
        self.process_file(entry_path)?;
        Ok(BundleResult {
            files: self.files.clone(),
            aliases: self.aliases.clone(),
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

        // Read the file
        let source = fs::read(file_path)
            .with_context(|| format!("failed to read file: {}", file_path.display()))?;

        // Store the file with a normalized path key
        let key = self.normalize_path(file_path);
        self.files.insert(key, source.clone());

        // Find all require paths first (to avoid borrow issues)
        let source_str = String::from_utf8_lossy(&source);
        let file_dir = file_path.parent().unwrap_or(Path::new(".")).to_path_buf();

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

    /// Normalize a path for use as a bundle key (always use canonical/absolute paths)
    fn normalize_path(&self, path: &Path) -> String {
        path.canonicalize()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| path.display().to_string())
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
                    // Find the actual module file to get canonical path
                    if let Some(actual_file) = self.find_module_file(&resolved) {
                        if let Ok(canonical) = actual_file.canonicalize() {
                            self.aliases.insert(
                                format!("@{}", alias_path),
                                canonical.display().to_string(),
                            );
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

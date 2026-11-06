use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use mlua::prelude::*;

use crate::paths::is_within_namespace;

use crate::{
    module::{ModuleError, ModuleId, TransformedModule},
    source_map::PortableSourceMap,
};

#[derive(Clone, Copy)]
pub struct ScriptPathProvider(pub fn(&Lua) -> LuaResult<Option<String>>);

/// Directory of the entry Luau script, used as the default JavaScript module root.
#[derive(Clone, Debug)]
pub struct JsModuleBase(pub Option<PathBuf>);

/// Relocated virtual locations for each literal `ModuleRoot`.
#[derive(Clone, Debug, Default)]
pub struct JsVirtualRoots(pub std::collections::BTreeMap<String, String>);

#[derive(Clone, Debug)]
pub enum ModuleSource {
    Original(String),
    Generated {
        code: String,
        source_map: Option<PortableSourceMap>,
    },
}

impl ModuleSource {
    #[must_use]
    pub fn original(source: impl Into<String>) -> Self {
        Self::Original(source.into())
    }
}

/// Prefixes module IDs with a namespace while delegating to another provider.
#[derive(Clone)]
pub struct NamespacedProvider {
    inner: Arc<dyn SourceProvider + Send + Sync>,
    namespace: String,
}

impl NamespacedProvider {
    #[must_use]
    pub fn new(namespace: impl Into<String>, inner: Arc<dyn SourceProvider + Send + Sync>) -> Self {
        Self {
            inner,
            namespace: namespace.into(),
        }
    }

    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    fn bare(&self, id: &ModuleId) -> Result<ModuleId, ModuleError> {
        if !is_within_namespace(id.as_str(), &self.namespace) {
            return Err(ModuleError::NotPreloaded(format!(
                "\"{id}\" is outside namespace \"{}\"",
                self.namespace
            )));
        }
        let bare = id
            .as_str()
            .strip_prefix(&self.namespace)
            .filter(|bare| bare.starts_with('/'))
            .unwrap_or(id.as_str());
        ModuleId::parse(bare)
    }
}

impl SourceProvider for NamespacedProvider {
    fn load(&self, id: &ModuleId) -> Result<ModuleSource, ModuleError> {
        self.inner.load(&self.bare(id)?)
    }

    fn exists(&self, id: &ModuleId) -> Result<bool, ModuleError> {
        self.inner.exists(&self.bare(id)?)
    }

    fn qualify_absolute(&self, specifier: &str) -> String {
        if specifier.starts_with(&self.namespace) {
            specifier.to_string()
        } else {
            format!("{}{specifier}", self.namespace)
        }
    }
}

/// Qualifies absolute specifiers for a provider whose keys are already namespaced.
#[derive(Clone)]
pub struct AbsoluteNsProvider {
    inner: Arc<dyn SourceProvider + Send + Sync>,
    namespace: String,
    /// `ModuleRoot` relative to the entry directory, used to locate modules a
    /// build embedded under the project root instead.
    root_prefix: Option<String>,
}

impl AbsoluteNsProvider {
    #[must_use]
    pub fn new(namespace: impl Into<String>, inner: Arc<dyn SourceProvider + Send + Sync>) -> Self {
        Self {
            inner,
            namespace: namespace.into(),
            root_prefix: None,
        }
    }

    #[must_use]
    pub fn with_root_prefix(mut self, root: Option<&str>) -> Self {
        self.root_prefix = root.and_then(relative_root_prefix);
        self
    }
}

/// Normalizes a relative module root into a leading path segment such as `Source`.
fn relative_root_prefix(root: &str) -> Option<String> {
    let root = root.replace('\\', "/");
    if root.starts_with('/') || Path::new(&root).is_absolute() {
        return None;
    }
    let root = root.trim_start_matches("./").trim_matches('/');
    if root.is_empty() || root.split('/').any(|part| part == ".." || part == ".") {
        return None;
    }
    Some(root.to_string())
}

impl AbsoluteNsProvider {
    fn check(&self, id: &ModuleId) -> Result<(), ModuleError> {
        if is_within_namespace(id.as_str(), &self.namespace) {
            Ok(())
        } else {
            Err(ModuleError::NotPreloaded(format!(
                "\"{id}\" is outside namespace \"{}\"",
                self.namespace
            )))
        }
    }

    /// Modules a build embedded under the project root, used when it could not
    /// see the context and therefore placed an include outside this namespace.
    fn fallbacks(&self, id: &ModuleId) -> Vec<ModuleId> {
        let Some(name) = id.as_str().strip_prefix(&self.namespace) else {
            return Vec::new();
        };
        let mut candidates = Vec::new();
        if let Some(prefix) = &self.root_prefix {
            if let Ok(qualified) = ModuleId::parse(&format!("/{prefix}{name}")) {
                candidates.push(qualified);
            }
        }
        if let Ok(unqualified) = ModuleId::parse(name) {
            candidates.push(unqualified);
        }
        candidates
    }
}

impl SourceProvider for AbsoluteNsProvider {
    fn load(&self, id: &ModuleId) -> Result<ModuleSource, ModuleError> {
        self.check(id)?;
        if let Ok(source) = self.inner.load(id) {
            return Ok(source);
        }
        for fallback in self.fallbacks(id) {
            if let Ok(source) = self.inner.load(&fallback) {
                return Ok(source);
            }
        }
        self.inner.load(id)
    }

    fn exists(&self, id: &ModuleId) -> Result<bool, ModuleError> {
        self.check(id)?;
        if self.inner.exists(id)? {
            return Ok(true);
        }
        for fallback in self.fallbacks(id) {
            if self.inner.exists(&fallback)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn is_bundle(&self) -> bool {
        self.inner.is_bundle()
    }

    fn qualify_absolute(&self, specifier: &str) -> String {
        if specifier.starts_with(&self.namespace) {
            specifier.to_string()
        } else {
            format!("{}{specifier}", self.namespace)
        }
    }
}

/** Source access shared by asynchronous graph crawlers and build tooling. */
pub trait SourceProvider {
    fn load(&self, id: &ModuleId) -> Result<ModuleSource, ModuleError>;

    fn exists(&self, id: &ModuleId) -> Result<bool, ModuleError>;

    /// Whether this provider contains only prebuilt modules. Bundle providers must never
    /// consult the host filesystem.
    fn is_bundle(&self) -> bool {
        false
    }

    /// Qualifies an absolute specifier with any provider namespace.
    fn qualify_absolute(&self, specifier: &str) -> String {
        specifier.to_string()
    }
}

#[derive(Debug, Default)]
pub struct MemorySourceProvider {
    sources: RwLock<BTreeMap<ModuleId, String>>,
}

impl MemorySourceProvider {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(
        &self,
        id: ModuleId,
        source: impl Into<String>,
    ) -> Result<Option<String>, ModuleError> {
        Ok(self
            .sources
            .write()
            .map_err(|_| ModuleError::Provider("The in-memory source lock is poisoned".into()))?
            .insert(id, source.into()))
    }

    pub fn with_modules(
        modules: impl IntoIterator<Item = (ModuleId, String)>,
    ) -> Result<Self, ModuleError> {
        let provider = Self::new();
        for (id, source) in modules {
            provider.insert(id, source)?;
        }
        Ok(provider)
    }
}

impl SourceProvider for MemorySourceProvider {
    fn load(&self, id: &ModuleId) -> Result<ModuleSource, ModuleError> {
        self.sources
            .read()
            .map_err(|_| ModuleError::Provider("The in-memory source lock is poisoned".into()))?
            .get(id)
            .cloned()
            .map(ModuleSource::Original)
            .ok_or_else(|| ModuleError::NotFound(id.to_string()))
    }

    fn exists(&self, id: &ModuleId) -> Result<bool, ModuleError> {
        Ok(self
            .sources
            .read()
            .map_err(|_| ModuleError::Provider("The in-memory source lock is poisoned".into()))?
            .contains_key(id))
    }
}

#[derive(Clone, Debug)]
struct BundleEntry {
    code: String,
    source_map: Option<PortableSourceMap>,
}

/** A filesystem-free provider containing ESM generated by the shared transform. */
#[derive(Debug, Default)]
pub struct BundleSourceProvider {
    modules: RwLock<BTreeMap<ModuleId, BundleEntry>>,
}

impl BundleSourceProvider {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, module: TransformedModule) -> Result<(), ModuleError> {
        let (id, code, source_map) = module.into_bundle_parts();
        self.modules
            .write()
            .map_err(|_| ModuleError::Provider("The bundle source lock is poisoned".into()))?
            .insert(id, BundleEntry { code, source_map });
        Ok(())
    }

    pub fn from_modules(
        modules: impl IntoIterator<Item = TransformedModule>,
    ) -> Result<Self, ModuleError> {
        let provider = Self::new();
        for module in modules {
            provider.insert(module)?;
        }
        Ok(provider)
    }

    pub fn from_generated(
        modules: impl IntoIterator<Item = (String, Vec<u8>, Option<Vec<u8>>)>,
    ) -> Result<Self, ModuleError> {
        let provider = Self::new();
        for (path, code, source_map) in modules {
            let id = ModuleId::parse(path)?;
            let code = String::from_utf8(code)
                .map_err(|error| ModuleError::Provider(error.to_string()))?;
            let source_map = source_map
                .map(|json| {
                    let json = String::from_utf8(json)
                        .map_err(|error| ModuleError::Provider(error.to_string()))?;
                    PortableSourceMap::from_json(&json, &id)
                })
                .transpose()?;
            provider
                .modules
                .write()
                .map_err(|_| ModuleError::Provider("The bundle source lock is poisoned".into()))?
                .insert(id, BundleEntry { code, source_map });
        }
        Ok(provider)
    }
}

impl SourceProvider for BundleSourceProvider {
    fn load(&self, id: &ModuleId) -> Result<ModuleSource, ModuleError> {
        let entry = self
            .modules
            .read()
            .map_err(|_| ModuleError::Provider("The bundle source lock is poisoned".into()))?
            .get(id)
            .cloned()
            .ok_or_else(|| {
                ModuleError::NotPreloaded(format!("\"{id}\" was not included by lune build"))
            })?;
        Ok(ModuleSource::Generated {
            code: entry.code,
            source_map: entry.source_map,
        })
    }

    fn exists(&self, id: &ModuleId) -> Result<bool, ModuleError> {
        Ok(self
            .modules
            .read()
            .map_err(|_| ModuleError::Provider("The bundle source lock is poisoned".into()))?
            .contains_key(id))
    }

    fn is_bundle(&self) -> bool {
        true
    }
}

/** A source provider constrained to a canonical host directory and its non-escaping symlinks. */
#[derive(Clone, Debug)]
pub struct HostSourceProvider {
    root: PathBuf,
}

impl HostSourceProvider {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, ModuleError> {
        let root = root.as_ref();
        let root = fs::canonicalize(root).map_err(|error| {
            ModuleError::Io(format!(
                "Could not canonicalize \"{}\". {error}",
                root.display()
            ))
        })?;
        if !root.is_dir() {
            return Err(ModuleError::InvalidPath(format!(
                "The source root \"{}\" is not a directory",
                root.display()
            )));
        }
        Ok(Self { root })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn module_id_for_path(&self, path: impl AsRef<Path>) -> Result<ModuleId, ModuleError> {
        let path = path.as_ref();
        let canonical = fs::canonicalize(path).map_err(|error| {
            ModuleError::Io(format!(
                "Could not canonicalize \"{}\". {error}",
                path.display()
            ))
        })?;
        self.ensure_within_root(&canonical)?;
        // Prefer the requested name over the canonical target so a root-relative
        // symlink can be embedded under the identity its importers use.
        let relative = path
            .strip_prefix(&self.root)
            .or_else(|_| canonical.strip_prefix(&self.root))
            .map_err(|_| ModuleError::RootEscape(format!("\"{}\"", canonical.display())))?;
        let relative = relative.to_str().ok_or_else(|| {
            ModuleError::InvalidPath("The host module path is not valid UTF-8".into())
        })?;
        ModuleId::parse(format!("/{}", relative.replace('\\', "/")))
    }

    fn candidate_path(&self, id: &ModuleId) -> PathBuf {
        self.root.join(id.as_str().trim_start_matches('/'))
    }

    fn ensure_within_root(&self, path: &Path) -> Result<(), ModuleError> {
        if path.starts_with(&self.root) {
            Ok(())
        } else {
            Err(ModuleError::SymlinkEscape(format!(
                "\"{}\" is outside \"{}\"",
                path.display(),
                self.root.display()
            )))
        }
    }

    fn canonical_file(&self, id: &ModuleId) -> Result<PathBuf, ModuleError> {
        let candidate = self.candidate_path(id);
        let canonical = fs::canonicalize(&candidate).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ModuleError::NotFound(id.to_string())
            } else {
                ModuleError::Io(format!(
                    "Could not canonicalize \"{}\". {error}",
                    candidate.display()
                ))
            }
        })?;
        self.ensure_within_root(&canonical)?;
        if !canonical.is_file() {
            return Err(ModuleError::NotFound(id.to_string()));
        }
        Ok(canonical)
    }
}

impl SourceProvider for HostSourceProvider {
    fn load(&self, id: &ModuleId) -> Result<ModuleSource, ModuleError> {
        let path = self.canonical_file(id)?;
        let source = fs::read_to_string(&path).map_err(|error| {
            ModuleError::Io(format!("Could not read \"{}\". {error}", path.display()))
        })?;
        Ok(ModuleSource::Original(source))
    }

    fn exists(&self, id: &ModuleId) -> Result<bool, ModuleError> {
        let candidate = self.candidate_path(id);
        match fs::canonicalize(&candidate) {
            Ok(canonical) => {
                self.ensure_within_root(&canonical)?;
                Ok(canonical.is_file())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(ModuleError::Io(format!(
                "Could not canonicalize \"{}\". {error}",
                candidate.display()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_namespace_rejects_sibling_roots() {
        let inner = Arc::new(
            MemorySourceProvider::with_modules([
                (
                    ModuleId::parse("/@A/Value.ts").unwrap(),
                    "export {};".to_string(),
                ),
                (
                    ModuleId::parse("/@AB/Value.ts").unwrap(),
                    "export {};".to_string(),
                ),
            ])
            .unwrap(),
        );
        let provider = AbsoluteNsProvider::new("/@A", inner);
        assert!(
            provider
                .exists(&ModuleId::parse("/@A/Value.ts").unwrap())
                .unwrap()
        );
        assert!(
            provider
                .exists(&ModuleId::parse("/@AB/Value.ts").unwrap())
                .is_err()
        );
    }

    #[test]
    fn absolute_namespace_falls_back_to_unqualified_modules() {
        let inner = Arc::new(
            MemorySourceProvider::with_modules([(
                ModuleId::parse("/Value.ts").unwrap(),
                "export {};".to_string(),
            )])
            .unwrap(),
        );
        let provider = AbsoluteNsProvider::new("/@A", inner);
        let id = ModuleId::parse("/@A/Value.ts").unwrap();
        assert!(provider.exists(&id).unwrap());
        assert!(provider.load(&id).is_ok());
    }
}

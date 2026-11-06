use std::collections::BTreeMap;

use crate::{
    module::{DependencyKind, ModuleError, ModuleId, TransformedModule},
    resolver::{resolve_entry, resolve_module},
    source::{ModuleSource, SourceProvider},
    transform::{inspect_generated_module, transform_module},
};

/** A fully resolved and transformed graph ready for a cache-only engine loader. */
#[derive(Debug)]
pub struct ModuleGraph {
    entry: ModuleId,
    modules: BTreeMap<ModuleId, TransformedModule>,
}

impl ModuleGraph {
    /// Loads only static dependencies so runtime dynamic imports stay lazy.
    pub fn load(provider: &dyn SourceProvider, entry: ModuleId) -> Result<Self, ModuleError> {
        Self::load_inner(provider, entry, false)
    }

    /// Loads static and literal dynamic dependencies, for standalone embedding.
    pub fn load_with_dynamic(
        provider: &dyn SourceProvider,
        entry: ModuleId,
    ) -> Result<Self, ModuleError> {
        Self::load_inner(provider, entry, true)
    }

    fn load_inner(
        provider: &dyn SourceProvider,
        entry: ModuleId,
        follow_dynamic: bool,
    ) -> Result<Self, ModuleError> {
        let mut modules = BTreeMap::new();
        let mut pending = vec![entry.clone()];

        while let Some(id) = pending.pop() {
            if modules.contains_key(&id) {
                continue;
            }

            let transformed = match provider.load(&id)? {
                ModuleSource::Original(source) => transform_module(id.clone(), &source)?,
                ModuleSource::Generated { code, source_map } => {
                    inspect_generated_module(id.clone(), &code, source_map)?
                }
            };

            let mut dependencies = Vec::new();
            for dependency in transformed.dependencies() {
                if follow_dynamic || dependency.kind == DependencyKind::Static {
                    dependencies.push(resolve_module(provider, &id, &dependency.specifier)?);
                }
            }
            modules.insert(id, transformed);
            pending.extend(dependencies);
        }

        Ok(Self { entry, modules })
    }

    pub fn load_specifier(
        provider: &dyn SourceProvider,
        base: &ModuleId,
        specifier: &str,
    ) -> Result<Self, ModuleError> {
        let entry = resolve_module(provider, base, specifier)?;
        Self::load(provider, entry)
    }

    pub fn load_specifier_with_dynamic(
        provider: &dyn SourceProvider,
        base: &ModuleId,
        specifier: &str,
    ) -> Result<Self, ModuleError> {
        let entry = resolve_module(provider, base, specifier)?;
        Self::load_with_dynamic(provider, entry)
    }

    pub fn load_entry(provider: &dyn SourceProvider, path: &str) -> Result<Self, ModuleError> {
        let entry = resolve_entry(provider, path)?;
        Self::load(provider, entry)
    }

    pub fn load_entry_with_dynamic(
        provider: &dyn SourceProvider,
        path: &str,
    ) -> Result<Self, ModuleError> {
        let entry = resolve_entry(provider, path)?;
        Self::load_with_dynamic(provider, entry)
    }

    #[must_use]
    pub fn entry(&self) -> &ModuleId {
        &self.entry
    }

    #[must_use]
    pub fn get(&self, id: &ModuleId) -> Option<&TransformedModule> {
        self.modules.get(id)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.modules.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&ModuleId, &TransformedModule)> {
        self.modules.iter()
    }

    #[must_use]
    pub fn into_modules(self) -> BTreeMap<ModuleId, TransformedModule> {
        self.modules
    }
}

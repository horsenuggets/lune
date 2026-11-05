use std::{cell::RefCell, collections::BTreeMap, rc::Rc, sync::Arc};

use rquickjs::{
    Ctx, Error, Module,
    loader::{ImportAttributes, Loader, Resolver},
};

use crate::{
    module::{ModuleError, ModuleId, TransformedModule},
    resolver::{resolve_entry, resolve_module},
    source::{ModuleSource, SourceProvider},
    source_map::PortableSourceMap,
    store::ModuleGraph,
};

/** A mutable, context-local module cache shared by `QuickJS`'s resolver and loader. */
#[derive(Clone, Default)]
pub struct SharedModuleCache {
    fallback: Rc<RefCell<Option<Arc<dyn SourceProvider>>>>,
    modules: Rc<RefCell<BTreeMap<ModuleId, TransformedModule>>>,
}

impl SharedModuleCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the provider used to resolve and transform dynamic imports on demand.
    pub fn set_fallback(&self, provider: Arc<dyn SourceProvider>) {
        self.fallback.borrow_mut().replace(provider);
    }

    fn fallback(&self) -> Option<Arc<dyn SourceProvider>> {
        self.fallback.borrow().clone()
    }

    pub fn insert_graph(&self, graph: ModuleGraph) {
        self.modules.borrow_mut().extend(graph.into_modules());
    }

    pub fn insert(&self, module: TransformedModule) {
        self.modules
            .borrow_mut()
            .insert(module.id().clone(), module);
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.modules.borrow().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.modules.borrow().is_empty()
    }

    #[must_use]
    pub fn contains(&self, id: &ModuleId) -> bool {
        self.modules.borrow().contains_key(id)
    }

    #[must_use]
    pub fn source_map(&self, id: &ModuleId) -> Option<PortableSourceMap> {
        self.modules
            .borrow()
            .get(id)
            .and_then(|module| module.source_map().cloned())
    }
}

impl SourceProvider for SharedModuleCache {
    fn load(&self, id: &ModuleId) -> Result<ModuleSource, ModuleError> {
        let modules = self.modules.borrow();
        let module = modules
            .get(id)
            .ok_or_else(|| ModuleError::NotPreloaded(id.to_string()))?;
        Ok(ModuleSource::Generated {
            code: module.code().to_string(),
            source_map: module.source_map().cloned(),
        })
    }

    fn exists(&self, id: &ModuleId) -> Result<bool, ModuleError> {
        Ok(self.modules.borrow().contains_key(id))
    }
}

impl Resolver for SharedModuleCache {
    fn resolve<'js>(
        &mut self,
        _ctx: &Ctx<'js>,
        base: &str,
        name: &str,
        attributes: Option<ImportAttributes<'js>>,
    ) -> Result<String, Error> {
        if attributes.is_some() {
            return Err(Error::new_resolving_message(
                base,
                name,
                "Import attributes are not supported",
            ));
        }
        let resolved = if name.starts_with('/') {
            resolve_entry(self, name).or_else(|error| match self.fallback() {
                Some(provider) => resolve_entry(provider.as_ref(), name),
                None => Err(error),
            })
        } else {
            let base = ModuleId::parse(base)
                .map_err(|error| Error::new_resolving_message(base, name, error.to_string()))?;
            resolve_module(self, &base, name).or_else(|error| match self.fallback() {
                Some(provider) => resolve_module(provider.as_ref(), &base, name),
                None => Err(error),
            })
        };
        resolved
            .map(|id| id.to_string())
            .map_err(|error| Error::new_resolving_message(base, name, error.to_string()))
    }
}

impl Loader for SharedModuleCache {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        attributes: Option<ImportAttributes<'js>>,
    ) -> Result<Module<'js, rquickjs::module::Declared>, Error> {
        if attributes.is_some() {
            return Err(Error::new_loading_message(
                name,
                "Import attributes are not supported",
            ));
        }
        let id = ModuleId::parse(name)
            .map_err(|error| Error::new_loading_message(name, error.to_string()))?;
        let cached = self
            .modules
            .borrow()
            .get(&id)
            .map(|module| module.code().to_owned());
        let code = if let Some(code) = cached {
            code
        } else {
            let provider = self
                .fallback()
                .ok_or_else(|| Error::new_loading_message(name, "Module is not embedded"))?;
            let graph = ModuleGraph::load(provider.as_ref(), id)
                .map_err(|error| Error::new_loading_message(name, error.to_string()))?;
            let code = graph
                .get(graph.entry())
                .map(|module| module.code().to_owned())
                .ok_or_else(|| Error::new_loading_message(name, "Module is not embedded"))?;
            self.insert_graph(graph);
            code
        };
        Module::declare(ctx.clone(), name, code)
    }
}

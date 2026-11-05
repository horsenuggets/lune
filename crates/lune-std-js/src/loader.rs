use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use rquickjs::{
    Ctx, Error, Module,
    loader::{ImportAttributes, Loader, Resolver},
};

use crate::{
    module::{ModuleError, ModuleId, TransformedModule},
    resolver::{resolve_entry, resolve_module},
    source::{ModuleSource, SourceProvider},
    store::ModuleGraph,
};

/** A mutable, context-local module cache shared by `QuickJS`'s resolver and loader. */
#[derive(Clone, Default)]
pub struct SharedModuleCache {
    modules: Rc<RefCell<BTreeMap<ModuleId, TransformedModule>>>,
}

impl SharedModuleCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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
        if name.starts_with('/') {
            return resolve_entry(self, name)
                .map(|id| id.to_string())
                .map_err(|error| Error::new_resolving_message("/", name, error.to_string()));
        }
        let base = ModuleId::parse(base)
            .map_err(|error| Error::new_resolving_message(base, name, error.to_string()))?;
        resolve_module(self, &base, name)
            .map(|id| id.to_string())
            .map_err(|error| Error::new_resolving_message(base.as_str(), name, error.to_string()))
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
        let modules = self.modules.borrow();
        let module = modules
            .get(&id)
            .ok_or_else(|| Error::new_loading_message(name, "Module is not embedded"))?;
        Module::declare(ctx.clone(), name, module.code())
    }
}

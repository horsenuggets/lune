use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsArtifact {
    /// Transpiled ECMAScript module source.
    pub code: Vec<u8>,
    /// A source map pointing back to the original module.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_map: Option<Vec<u8>>,
}

pub type BundledJsModules = BTreeMap<String, JsArtifact>;
pub type BundledJsRoots = BTreeMap<String, String>;

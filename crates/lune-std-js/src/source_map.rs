use oxc_sourcemap::SourceMap;

use crate::module::{ModuleError, ModuleId};

/** A zero-based position in an original TypeScript or JavaScript source. */
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OriginalPosition {
    pub column: u32,
    pub line: u32,
    pub module_id: ModuleId,
    pub source_content: String,
}

/** An owned source map whose source names and contents cannot leak host checkout paths. */
#[derive(Clone, Debug)]
pub struct PortableSourceMap(SourceMap);

impl PortableSourceMap {
    pub(crate) fn new(mut map: SourceMap, module_id: &ModuleId, original_source: &str) -> Self {
        map.set_file(module_id.as_str());
        map.set_sources([module_id.as_str()]);
        map.set_source_contents(vec![Some(original_source)]);
        Self(map)
    }

    pub fn from_json(json: &str, module_id: &ModuleId) -> Result<Self, ModuleError> {
        let map = SourceMap::from_json_string(json).map_err(|error| {
            ModuleError::Provider(format!(
                "Could not decode the source map for \"{module_id}\". {error}"
            ))
        })?;
        if map.get_sources().count() != 1 {
            return Err(ModuleError::Provider(format!(
                "The source map for \"{module_id}\" must contain exactly one original source"
            )));
        }
        let original_source = map
            .get_source_content(0)
            .map(std::string::ToString::to_string)
            .ok_or_else(|| {
                ModuleError::Provider(format!(
                    "The source map for \"{module_id}\" does not contain its original source"
                ))
            })?;
        Ok(Self::new(map, module_id, &original_source))
    }

    /** Maps a zero-based generated line and UTF-16 column to the original source. */
    #[must_use]
    pub fn original_position(&self, line: u32, column: u32) -> Option<OriginalPosition> {
        let lookup = self.0.generate_lookup_table();
        let token = self.0.lookup_token(&lookup, line, column)?;
        let source_id = token.get_source_id()?;
        let module_id = ModuleId::parse(self.0.get_source(source_id)?).ok()?;
        let source_content = self.0.get_source_content(source_id)?.to_string();
        Some(OriginalPosition {
            column: token.get_src_col(),
            line: token.get_src_line(),
            module_id,
            source_content,
        })
    }

    #[must_use]
    pub fn original_source(&self) -> Option<&str> {
        self.0
            .get_source_content(0)
            .map(std::convert::AsRef::as_ref)
    }

    #[must_use]
    pub fn to_json_string(&self) -> String {
        self.0.to_json_string()
    }

    #[must_use]
    pub fn to_data_url(&self) -> String {
        self.0.to_data_url()
    }
}

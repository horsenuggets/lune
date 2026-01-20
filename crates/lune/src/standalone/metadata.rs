use std::collections::HashMap;
use std::{env, path::PathBuf, sync::LazyLock};

use anyhow::{Result, bail};
use async_fs as fs;
use serde::{Deserialize, Serialize};

pub static CURRENT_EXE: LazyLock<PathBuf> =
    LazyLock::new(|| env::current_exe().expect("failed to get current exe"));
const MAGIC: &[u8; 8] = b"cr3sc3nt";

/**
    Metadata for a standalone Lune executable. Can be used to
    discover and load the source code contained in a standalone binary.

    Stores the entry point source, its path, and all bundled dependencies.
*/
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metadata {
    /// The entry point source code
    pub source: Vec<u8>,
    /// The entry point path (for chunk naming)
    pub entry_path: String,
    /// Bundled module files: canonical path -> source
    #[serde(default)]
    pub files: HashMap<String, Vec<u8>>,
    /// Alias mappings: alias (e.g., "@packages/Foo") -> canonical path
    #[serde(default)]
    pub aliases: HashMap<String, String>,
}

impl Metadata {
    /**
        Returns whether or not the currently executing Lune binary
        is a standalone binary, and if so, the bytes of the binary.
    */
    pub async fn check_env() -> (bool, Vec<u8>) {
        let contents = fs::read(CURRENT_EXE.to_path_buf())
            .await
            .unwrap_or_default();
        let is_standalone = contents.ends_with(MAGIC);
        (is_standalone, contents)
    }

    /**
        Creates a patched standalone binary from the given script contents.
    */
    pub async fn create_env_patched_bin(
        base_exe_path: PathBuf,
        script_contents: impl Into<Vec<u8>>,
        entry_path: impl Into<String>,
        files: HashMap<String, Vec<u8>>,
        aliases: HashMap<String, String>,
    ) -> Result<Vec<u8>> {
        let mut patched_bin = fs::read(base_exe_path).await?;

        let meta = Self {
            source: script_contents.into(),
            entry_path: entry_path.into(),
            files,
            aliases,
        };
        patched_bin.extend_from_slice(&meta.to_bytes()?);

        Ok(patched_bin)
    }

    /**
        Tries to read a standalone binary from the given bytes.
    */
    pub fn from_bytes(bytes: impl AsRef<[u8]>) -> Result<Self> {
        let bytes = bytes.as_ref();
        // Minimum size: 8 (magic) + 8 (json_size) = 16
        if bytes.len() < 16 || !bytes.ends_with(MAGIC) {
            bail!("not a standalone binary")
        }

        // Extract JSON size (8 bytes before magic)
        let json_size_bytes = &bytes[bytes.len() - 16..bytes.len() - 8];
        let json_size = usize::try_from(u64::from_be_bytes(json_size_bytes.try_into().unwrap()))?;

        // Extract JSON data
        let json_start = bytes.len() - 16 - json_size;
        let json_bytes = &bytes[json_start..json_start + json_size];

        // Deserialize
        let meta: Self = serde_json::from_slice(json_bytes)?;
        Ok(meta)
    }

    /**
        Writes the metadata chunk to a byte vector, to later be read using `from_bytes`.

        Format: [json_data][json_size: u64][MAGIC: 8 bytes]
    */
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let json_bytes = serde_json::to_vec(self)?;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&json_bytes);
        bytes.extend_from_slice(&(json_bytes.len() as u64).to_be_bytes());
        bytes.extend_from_slice(MAGIC);
        Ok(bytes)
    }
}

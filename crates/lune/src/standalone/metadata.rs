use std::{env, path::PathBuf, sync::LazyLock};

use anyhow::{Result, bail};
use async_fs as fs;

pub static CURRENT_EXE: LazyLock<PathBuf> =
    LazyLock::new(|| env::current_exe().expect("failed to get current exe"));
const MAGIC: &[u8; 8] = b"cr3sc3nt";

/*
    TODO: Right now all we do is append the bytecode to the end
    of the binary, but we will need a more flexible solution in
    the future to store many files as well as their metadata.

    The best solution here is most likely to use a well-supported
    and rust-native binary serialization format with a stable
    specification, one that also supports byte arrays well without
    overhead, so the best solution seems to currently be Postcard:

    https://github.com/jamesmunns/postcard
    https://crates.io/crates/postcard
*/

/**
    Metadata for a standalone Lune executable. Can be used to
    discover and load the source code contained in a standalone binary.

    Note: We store source code instead of bytecode because the chunk name
    needs to be set at compile time for require resolution to work correctly.
    The source is compiled at runtime with the correct entry path as the chunk name.
*/
#[derive(Debug, Clone)]
pub struct Metadata {
    pub source: Vec<u8>,
    pub entry_path: String,
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

        Note: We store source code instead of pre-compiled bytecode because
        the chunk name needs to be set at compile time for require resolution
        to work correctly. Storing source allows us to compile at runtime with
        the correct entry path as the chunk name.
    */
    pub async fn create_env_patched_bin(
        base_exe_path: PathBuf,
        script_contents: impl Into<Vec<u8>>,
        entry_path: impl Into<String>,
    ) -> Result<Vec<u8>> {
        let mut patched_bin = fs::read(base_exe_path).await?;

        // Store source code (not bytecode) so we can compile with correct chunk name at runtime
        let meta = Self {
            source: script_contents.into(),
            entry_path: entry_path.into(),
        };
        patched_bin.extend_from_slice(&meta.to_bytes());

        Ok(patched_bin)
    }

    /**
        Tries to read a standalone binary from the given bytes.
    */
    pub fn from_bytes(bytes: impl AsRef<[u8]>) -> Result<Self> {
        let bytes = bytes.as_ref();
        // Minimum size: 8 (magic) + 8 (source_size) + 8 (entry_path_size) = 24
        if bytes.len() < 24 || !bytes.ends_with(MAGIC) {
            bail!("not a standalone binary")
        }

        // Extract source size (8 bytes before magic)
        let source_size_bytes = &bytes[bytes.len() - 16..bytes.len() - 8];
        let source_size =
            usize::try_from(u64::from_be_bytes(source_size_bytes.try_into().unwrap()))?;

        // Extract entry_path size (8 bytes before source_size)
        let entry_path_size_bytes = &bytes[bytes.len() - 24..bytes.len() - 16];
        let entry_path_size =
            usize::try_from(u64::from_be_bytes(entry_path_size_bytes.try_into().unwrap()))?;

        // Calculate offsets
        let metadata_size = 24; // magic + source_size + entry_path_size
        let data_start = bytes.len() - metadata_size - source_size - entry_path_size;

        // Extract source
        let source = bytes[data_start..data_start + source_size].to_vec();

        // Extract entry_path
        let entry_path_bytes =
            &bytes[data_start + source_size..data_start + source_size + entry_path_size];
        let entry_path = String::from_utf8(entry_path_bytes.to_vec())?;

        Ok(Self { source, entry_path })
    }

    /**
        Writes the metadata chunk to a byte vector, to later be read using `from_bytes`.

        Format: [source][entry_path][entry_path_size: u64][source_size: u64][MAGIC: 8 bytes]
    */
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        let entry_path_bytes = self.entry_path.as_bytes();
        bytes.extend_from_slice(&self.source);
        bytes.extend_from_slice(entry_path_bytes);
        bytes.extend_from_slice(&(entry_path_bytes.len() as u64).to_be_bytes());
        bytes.extend_from_slice(&(self.source.len() as u64).to_be_bytes());
        bytes.extend_from_slice(MAGIC);
        bytes
    }
}

use std::{env, process::ExitCode};

use anyhow::Result;
use lune::Runtime;

pub(crate) mod metadata;
pub(crate) mod tracer;

use self::metadata::Metadata;

/**
    Returns whether or not the currently executing Lune binary
    is a standalone binary, and if so, the bytes of the binary.
*/
pub async fn check() -> Option<Vec<u8>> {
    let (is_standalone, patched_bin) = Metadata::check_env().await;
    if is_standalone {
        Some(patched_bin)
    } else {
        None
    }
}

/**
    Discovers, loads and executes the bytecode contained in a standalone binary.
*/
pub async fn run(patched_bin: impl AsRef<[u8]>) -> Result<ExitCode> {
    // The first argument is the path to the current executable
    let args = env::args().skip(1).collect::<Vec<_>>();
    let meta = Metadata::from_bytes(patched_bin).expect("must be a standalone binary");

    let mut rt = Runtime::new()?
        .with_args(args)
        .with_bundled_files(meta.files)
        .with_bundled_aliases(meta.aliases)
        .with_executable_path(metadata::CURRENT_EXE.clone());

    // Compile and run the source with the original entry path.
    // This ensures the chunk name is set correctly for require resolution.
    let result = rt.run_source(&meta.entry_path, meta.source).await;

    Ok(match result {
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
        Ok(values) => ExitCode::from(values.status()),
    })
}

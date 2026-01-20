use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::{Context, Result, bail};
use async_fs as fs;
use clap::Parser;
use console::style;

use crate::standalone::metadata::Metadata;

mod base_exe;
mod bundler;
mod files;
mod result;
mod target;

use self::base_exe::get_or_download_base_executable;
use self::bundler::Bundler;
use self::files::{remove_source_file_ext, write_executable_file_to};
use self::target::BuildTarget;

/// Strip shebang line from source code if present
fn strip_shebang(mut contents: Vec<u8>) -> Vec<u8> {
    if contents.starts_with(b"#!") {
        if let Some(idx) = contents.iter().position(|&c| c == b'\n') {
            // Keep the newline to preserve line numbers
            contents.drain(..idx);
        }
    }
    contents
}

/// Find the actual source file for a given path.
/// If the path is a directory containing init.luau or init.lua, return that file.
/// Otherwise return the path as-is.
fn resolve_entry_file(path: &Path) -> PathBuf {
    if path.is_dir() {
        let init_luau = path.join("init.luau");
        if init_luau.is_file() {
            return init_luau;
        }
        let init_lua = path.join("init.lua");
        if init_lua.is_file() {
            return init_lua;
        }
    }
    path.to_path_buf()
}

/// Build a standalone executable
#[derive(Debug, Clone, Parser)]
pub struct BuildCommand {
    /// The path to the input file
    pub input: PathBuf,

    /// The path to the output file - defaults to the
    /// input file path with an executable extension
    #[clap(short, long)]
    pub output: Option<PathBuf>,

    /// The target to compile for in the format `os-arch` -
    /// defaults to the os and arch of the current system
    #[clap(short, long)]
    pub target: Option<BuildTarget>,
}

impl BuildCommand {
    pub async fn run(self) -> Result<ExitCode> {
        // Derive target spec to use, or default to the current host system
        let target = self.target.unwrap_or_else(BuildTarget::current_system);

        // Resolve the entry file (handles directories with init.luau)
        let entry_file = resolve_entry_file(&self.input);
        let is_directory_module = entry_file != self.input;

        // Verify the entry file exists
        if !entry_file.is_file() {
            if self.input.is_dir() {
                bail!(
                    "directory {} does not contain an init.luau or init.lua file",
                    self.input.display()
                );
            }
            bail!("input file {} does not exist", self.input.display());
        }

        // Derive paths to use, and make sure the output path is
        // not the same as the input, so that we don't overwrite it
        // For directory modules, use just the directory name (in cwd) as the output name
        let output_path = self.output.clone().unwrap_or_else(|| {
            if is_directory_module {
                // For directory modules, use the directory name in the current directory
                // This avoids conflicts where output would equal the input directory
                self.input
                    .file_name()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| self.input.clone())
            } else {
                remove_source_file_ext(&self.input)
            }
        });
        let output_path = output_path.with_extension(target.exe_extension());
        if output_path == self.input || output_path == entry_file {
            if self.output.is_some() {
                bail!("output path cannot be the same as input path");
            }
            bail!(
                "output path cannot be the same as input path, please specify a different output path"
            );
        }

        // Try to read the given input file and strip shebang
        let source_code = strip_shebang(
            fs::read(&entry_file)
                .await
                .context("failed to read input file")?,
        );

        // Bundle all dependencies
        let display_path = if is_directory_module {
            format!("{} (init.luau)", self.input.display())
        } else {
            self.input.display().to_string()
        };
        println!("Bundling dependencies for {}", style(&display_path).green());
        let mut bundler = Bundler::new(&entry_file).context("failed to initialize bundler")?;
        let bundle_result = bundler
            .bundle(&entry_file)
            .context("failed to bundle dependencies")?;
        println!(
            "Bundled {} files, {} aliases",
            style(bundle_result.files.len()).cyan(),
            style(bundle_result.aliases.len()).cyan()
        );

        // Derive the base executable path based on the arguments provided
        let base_exe_path = get_or_download_base_executable(target).await?;

        // Read the contents of the lune interpreter as our starting point
        println!(
            "Compiling standalone binary from {}",
            style(&display_path).green()
        );
        // Use relative path from project root for portability
        let canonical_entry = entry_file
            .canonicalize()
            .unwrap_or_else(|_| entry_file.clone());
        let entry_path = if let Ok(relative) = canonical_entry.strip_prefix(bundler.base_dir()) {
            format!("/{}", relative.display())
        } else {
            canonical_entry.display().to_string()
        };
        let patched_bin = Metadata::create_env_patched_bin(
            base_exe_path,
            source_code,
            entry_path,
            bundle_result.files,
            bundle_result.aliases,
        )
        .await
        .context("failed to create patched binary")?;

        // And finally write the patched binary to the output file
        println!(
            "Writing standalone binary to {}",
            style(output_path.display()).blue()
        );
        write_executable_file_to(output_path, patched_bin).await?; // Read & execute for all, write for owner

        Ok(ExitCode::SUCCESS)
    }
}

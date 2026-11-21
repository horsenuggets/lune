#![allow(clippy::cargo_common_metadata)]

use std::io::{self, ErrorKind as IoErrorKind};
use std::path::{Path, PathBuf};

use async_fs as fs;
use bstr::{BString, ByteSlice};
use futures_lite::prelude::*;
use mlua::prelude::*;

use lune_utils::TableBuilder;

mod copy;
mod metadata;
mod options;
mod watch;

use self::copy::copy;
use self::metadata::FsMetadata;
use self::options::FsWriteOptions;
use self::watch::fs_watch;

const TYPEDEFS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/types.d.luau"));

/**
    Returns a string containing type definitions for the `fs` standard library.
*/
#[must_use]
pub fn typedefs() -> String {
    TYPEDEFS.to_string()
}

/**
    Creates the `fs` standard library module.

    # Errors

    Errors when out of memory.
*/
pub fn module(lua: Lua) -> LuaResult<LuaTable> {
    TableBuilder::new(lua)?
        .with_async_function("readFile", fs_read_file)?
        .with_async_function("readDir", fs_read_dir)?
        .with_async_function("writeFile", fs_write_file)?
        .with_async_function("writeDir", fs_write_dir)?
        .with_async_function("createTempDir", fs_create_temp_dir)?
        .with_async_function("removeFile", fs_remove_file)?
        .with_async_function("removeDir", fs_remove_dir)?
        .with_async_function("metadata", fs_metadata)?
        .with_async_function("isFile", fs_is_file)?
        .with_async_function("isDir", fs_is_dir)?
        .with_async_function("move", fs_move)?
        .with_async_function("copy", fs_copy)?
        .with_function("watch", fs_watch)?
        .build_readonly()
}

async fn fs_read_file(lua: Lua, path: String) -> LuaResult<LuaString> {
    let bytes = fs::read(&path).await.into_lua_err()?;

    lua.create_string(bytes)
}

async fn fs_read_dir(_: Lua, path: String) -> LuaResult<Vec<String>> {
    let mut dir_strings = Vec::new();
    let mut dir = fs::read_dir(&path).await.into_lua_err()?;
    while let Some(dir_entry) = dir.try_next().await.into_lua_err()? {
        if let Some(dir_name_str) = dir_entry.file_name().to_str() {
            dir_strings.push(dir_name_str.to_owned());
        } else {
            return Err(LuaError::RuntimeError(format!(
                "File name could not be converted into a string: '{}'",
                dir_entry.file_name().to_string_lossy()
            )));
        }
    }
    Ok(dir_strings)
}

async fn fs_write_file(_: Lua, (path, contents): (String, BString)) -> LuaResult<()> {
    fs::write(&path, contents.as_bytes()).await.into_lua_err()
}

async fn fs_write_dir(_: Lua, path: String) -> LuaResult<()> {
    fs::create_dir_all(&path).await.into_lua_err()
}

fn create_temp_dir(parent: &Path, prefix: &str) -> io::Result<tempfile::TempDir> {
    if parent.as_os_str().is_empty() {
        return Err(io::Error::new(
            IoErrorKind::InvalidInput,
            "The temporary directory parent must be an explicit directory path.",
        ));
    }
    if prefix.is_empty() || matches!(prefix, "." | "..") || prefix.contains(['/', '\\', ':', '\0'])
    {
        return Err(io::Error::new(
            IoErrorKind::InvalidInput,
            "The temporary directory prefix must be one nonempty filename component.",
        ));
    }
    let mut builder = tempfile::Builder::new();
    builder.prefix(prefix);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    builder.tempdir_in(parent)
}

async fn fs_create_temp_dir(lua: Lua, (parent, prefix): (String, String)) -> LuaResult<LuaString> {
    let directory = blocking::unblock(move || create_temp_dir(Path::new(&parent), &prefix))
        .await
        .into_lua_err()?;
    let path = directory.path().to_str().ok_or_else(|| {
        LuaError::RuntimeError("The temporary directory path is not valid UTF-8.".into())
    })?;
    // Keep RAII cleanup armed until the path has been transferred into Lua.
    let result = lua.create_string(path)?;
    let _ = directory.keep();
    Ok(result)
}

async fn fs_remove_file(_: Lua, path: String) -> LuaResult<()> {
    fs::remove_file(&path).await.into_lua_err()
}

async fn fs_remove_dir(_: Lua, path: String) -> LuaResult<()> {
    fs::remove_dir_all(&path).await.into_lua_err()
}

async fn fs_metadata(_: Lua, path: String) -> LuaResult<FsMetadata> {
    match fs::metadata(path).await {
        Err(e) if e.kind() == IoErrorKind::NotFound => Ok(FsMetadata::not_found()),
        Ok(meta) => Ok(FsMetadata::from(meta)),
        Err(e) => Err(e.into()),
    }
}

async fn fs_is_file(_: Lua, path: String) -> LuaResult<bool> {
    match fs::metadata(path).await {
        Err(e) if e.kind() == IoErrorKind::NotFound => Ok(false),
        Ok(meta) => Ok(meta.is_file()),
        Err(e) => Err(e.into()),
    }
}

async fn fs_is_dir(_: Lua, path: String) -> LuaResult<bool> {
    match fs::metadata(path).await {
        Err(e) if e.kind() == IoErrorKind::NotFound => Ok(false),
        Ok(meta) => Ok(meta.is_dir()),
        Err(e) => Err(e.into()),
    }
}

async fn fs_move(_: Lua, (from, to, options): (String, String, FsWriteOptions)) -> LuaResult<()> {
    let path_from = PathBuf::from(from);
    if !path_from.exists() {
        return Err(LuaError::RuntimeError(format!(
            "No file or directory exists at the path '{}'",
            path_from.display()
        )));
    }
    let path_to = PathBuf::from(to);
    if !options.overwrite && path_to.exists() {
        return Err(LuaError::RuntimeError(format!(
            "A file or directory already exists at the path '{}'",
            path_to.display()
        )));
    }
    fs::rename(path_from, path_to).await.into_lua_err()?;
    Ok(())
}

async fn fs_copy(_: Lua, (from, to, options): (String, String, FsWriteOptions)) -> LuaResult<()> {
    copy(from, to, options).await
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, fs};

    use super::*;

    #[test]
    fn temporary_directory_api_works_without_a_scheduler() {
        let parent = tempfile::tempdir().unwrap();
        let lua = Lua::new();
        let function: LuaFunction = module(lua.clone()).unwrap().get("createTempDir").unwrap();
        let path = futures_lite::future::block_on(
            function.call_async::<String>((parent.path().to_str().unwrap(), "draft-")),
        )
        .unwrap();
        let path = Path::new(&path);
        assert!(path.is_dir());
        assert_eq!(path.parent(), Some(parent.path()));
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn temporary_directory_api_rejects_invalid_arguments_without_a_scheduler() {
        let parent = tempfile::tempdir().unwrap();
        let lua = Lua::new();
        let function: LuaFunction = module(lua.clone()).unwrap().get("createTempDir").unwrap();
        for (parent, prefix) in [
            ("", "draft-"),
            (parent.path().to_str().unwrap(), "../outside"),
        ] {
            let error =
                futures_lite::future::block_on(function.call_async::<String>((parent, prefix)))
                    .unwrap_err();
            assert!(error.to_string().contains("temporary directory"));
        }
        assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
    }

    #[test]
    fn temporary_directories_are_unique_under_concurrent_creation() {
        let parent = tempfile::tempdir().unwrap();
        let directories = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..32)
                .map(|_| scope.spawn(|| create_temp_dir(parent.path(), "draft-").unwrap()))
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        let paths: HashSet<_> = directories.iter().map(tempfile::TempDir::path).collect();
        assert_eq!(paths.len(), 32);
        for path in paths {
            assert_eq!(path.parent(), Some(parent.path()));
            assert!(path.is_dir());
            assert!(
                path.file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with("draft-")
            );
        }
    }

    #[test]
    fn temporary_directories_can_be_kept_and_removed_by_the_caller() {
        let parent = tempfile::tempdir().unwrap();
        let path = create_temp_dir(parent.path(), "draft-").unwrap().keep();
        assert!(path.is_dir());
        fs::write(path.join("Place.rbxl"), b"place").unwrap();
        fs::remove_dir_all(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn temporary_directory_prefixes_cannot_escape_the_parent() {
        let parent = tempfile::tempdir().unwrap();
        for prefix in [
            "",
            ".",
            "..",
            "../outside",
            "nested/path",
            r"nested\path",
            "C:drive",
            r"\\host",
            "bad\0prefix",
        ] {
            let error = create_temp_dir(parent.path(), prefix).unwrap_err();
            assert_eq!(error.kind(), IoErrorKind::InvalidInput, "{prefix:?}");
            assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
        }
    }

    #[test]
    fn temporary_directory_creation_requires_an_existing_directory_parent() {
        assert_eq!(
            create_temp_dir(Path::new(""), "draft-").unwrap_err().kind(),
            IoErrorKind::InvalidInput
        );
        let parent = tempfile::tempdir().unwrap();
        let missing = parent.path().join("missing").join("nested");
        assert!(create_temp_dir(&missing, "draft-").is_err());
        assert!(!parent.path().join("missing").exists());
        let file = parent.path().join("file");
        fs::write(&file, b"keep").unwrap();
        assert!(create_temp_dir(&file, "draft-").is_err());
        assert_eq!(fs::read(file).unwrap(), b"keep");
    }

    #[cfg(unix)]
    #[test]
    fn temporary_directories_use_private_permissions_and_preserve_sibling_symlinks() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("drafts");
        fs::create_dir(&parent).unwrap();
        let sibling = parent.join("draft-existing");
        let link = parent.join("draft-link");
        let target = root.path().join("outside");
        fs::write(&sibling, b"keep").unwrap();
        symlink(&target, &link).unwrap();
        let directory = create_temp_dir(&parent, "draft-").unwrap();
        assert_eq!(
            fs::metadata(directory.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(fs::read(&sibling).unwrap(), b"keep");
        assert!(fs::symlink_metadata(link).unwrap().file_type().is_symlink());
        assert!(!target.exists());
    }
}

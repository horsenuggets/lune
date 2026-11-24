use std::{
    io::{Cursor, Read},
    path::PathBuf,
};

use async_fs as fs;
use blocking::unblock;

use crate::standalone::metadata::CURRENT_EXE;

use super::{
    files::write_executable_file_to,
    result::{BuildError, BuildResult},
    target::{BuildTarget, BuildTargetArch},
};

const RELEASE_REQUEST_HEADERS: &[(&str, &str)] = &[
    (
        "User-Agent",
        concat!(
            "Lune/",
            env!("CARGO_PKG_VERSION"),
            " (",
            env!("CARGO_PKG_REPOSITORY"),
            ")"
        ),
    ),
    ("Accept", "application/octet-stream"),
    // ("Accept-Encoding", "gzip"),
];

fn release_download_path(version: &str, target: &BuildTarget) -> String {
    let arch = match target.arch {
        BuildTargetArch::X86_64 => "x64",
        BuildTargetArch::Aarch64 => "arm64",
    };
    format!("{version}/lune-{version}-{os}-{arch}.zip", os = target.os)
}

fn legacy_release_download_path(
    version: &str,
    target: &BuildTarget,
    current_path: &str,
    status: u16,
) -> Option<String> {
    // Published archives are immutable. Only a missing new-style asset should retry
    // the historical name, not authentication failures or transient server errors.
    (status == 404 && current_path == release_download_path(version, target))
        .then(|| format!("{version}/lune-{version}-{target}.zip"))
}

async fn download_release_archive(version: &str, target: &BuildTarget) -> BuildResult<Vec<u8>> {
    let mut release_path = release_download_path(version, target);
    loop {
        let release_url = format!(
            "{base_url}/{release_path}",
            base_url = "https://github.com/horsenuggets/lune/releases/download",
        );
        let url = release_url.parse().expect("release url is valid");
        let headers = RELEASE_REQUEST_HEADERS
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        let res = lune_std_net::fetch(url, None, Some(headers), None)
            .await
            .map_err(BuildError::Download)?;
        let (parts, body) = res.into_inner().into_parts();

        if parts.status.is_success() {
            return Ok(body.into_bytes().to_vec());
        }
        if let Some(legacy_path) =
            legacy_release_download_path(version, target, &release_path, parts.status.as_u16())
        {
            release_path = legacy_path;
            continue;
        }
        if parts.status.as_u16() == 404 {
            return Err(BuildError::ReleaseTargetNotFound(target.clone()));
        }
        let body = body.into_bytes();
        return Err(BuildError::Download(format!(
            "Request was not successful\
            \nStatus: {}\
            \nBody: {}",
            parts.status,
            if body.len() > 128 {
                String::from_utf8_lossy(&body[0..128])
            } else {
                String::from_utf8_lossy(&body)
            }
        )));
    }
}

/**
    Discovers the path to the base executable to use for cross-compilation.

    If the target is the same as the current system, the current executable is used.

    If no binary exists at the target path, it will attempt to download it from the internet.
*/
pub async fn get_or_download_base_executable(target: BuildTarget) -> BuildResult<PathBuf> {
    if target.is_current_system() {
        return Ok(CURRENT_EXE.to_path_buf());
    }
    if target.cache_path().exists() {
        return Ok(target.cache_path());
    }

    // The target is not cached, we must download it
    println!("Requested target '{target}' does not exist in cache");
    let version = env!("CARGO_PKG_VERSION");
    let target_triple = format!("lune-{version}-{target}");

    // NOTE: This is not entirely accurate, but it is clearer for a user
    println!("Downloading {target_triple}{}...", target.exe_suffix());

    // Start reading the zip file after resolving current or historical asset names.
    let zip_file = Cursor::new(download_release_archive(version, &target).await?);

    // Look for and extract the binary file from the zip file
    // NOTE: We use spawn_blocking here since reading a zip
    // archive is a somewhat slow / blocking operation
    let binary_file_name = format!("lune{}", target.exe_suffix());
    let binary_file_handle = unblock(move || {
        let mut archive = zip::ZipArchive::new(zip_file)?;

        let mut binary = Vec::new();
        archive
            .by_name(&binary_file_name)
            .or(Err(BuildError::ZippedBinaryNotFound(binary_file_name)))?
            .read_to_end(&mut binary)?;

        Ok::<_, BuildError>(binary)
    });
    let binary_file_contents = binary_file_handle.await?;

    // Finally, write the extracted binary to the cache
    let cache_path = target.cache_path();
    if let Some(cache_dir) = cache_path.parent() {
        fs::create_dir_all(cache_dir).await?;
    }
    write_executable_file_to(&cache_path, binary_file_contents).await?;
    println!("Downloaded successfully and added to cache");

    Ok(target.cache_path())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_downloads_map_archives_without_changing_target_contracts() {
        for (target, suffix) in [
            ("linux-aarch64", "linux-arm64"),
            ("linux-x86_64", "linux-x64"),
            ("macos-aarch64", "macos-arm64"),
            ("macos-x86_64", "macos-x64"),
            ("windows-aarch64", "windows-arm64"),
            ("windows-x86_64", "windows-x64"),
        ] {
            let parsed: BuildTarget = target.parse().unwrap();
            for version in ["0.11.1-horse.16.3", "1.2.3-fork.1.0"] {
                assert_eq!(
                    release_download_path(version, &parsed),
                    format!("{version}/lune-{version}-{suffix}.zip")
                );
                assert_eq!(
                    legacy_release_download_path(
                        version,
                        &parsed,
                        &release_download_path(version, &parsed),
                        404
                    ),
                    Some(format!("{version}/lune-{version}-{target}.zip"))
                );
            }
            assert_eq!(parsed.to_string(), target);
            let cache_name = format!("{target}{}", parsed.exe_extension());
            assert_eq!(
                parsed.cache_path().file_name().unwrap(),
                cache_name.as_str()
            );
            assert_eq!(suffix.parse::<BuildTarget>().unwrap(), parsed);
        }
    }

    #[test]
    fn historical_assets_are_selected_only_after_not_found() {
        let target = "linux-x64".parse().unwrap();
        let current_path = release_download_path("1.2.3", &target);
        let legacy_path =
            legacy_release_download_path("1.2.3", &target, &current_path, 404).unwrap();
        for status in 100..=599 {
            assert_eq!(
                legacy_release_download_path("1.2.3", &target, &current_path, status).is_some(),
                status == 404,
                "unexpected fallback for HTTP {status}"
            );
            assert_eq!(
                legacy_release_download_path("1.2.3", &target, &legacy_path, status),
                None,
                "historical requests must not retry for HTTP {status}"
            );
        }
    }
}

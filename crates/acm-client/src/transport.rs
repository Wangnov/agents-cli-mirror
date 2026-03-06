use anyhow::{Result, anyhow, bail};
use reqwest::StatusCode;
use reqwest::blocking::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

use crate::ui::{Ui, output, tr};
use crate::{InstallContext, request_with_retry, run_with_spinner};

#[derive(Debug, Deserialize, Clone)]
pub(super) struct ArtifactEntry {
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub(super) struct PlatformEntry {
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
    pub filename: String,
    #[serde(default)]
    pub files: HashMap<String, ArtifactEntry>,
}

pub(super) fn fetch_checksums(
    client: &Client,
    retries: u32,
    mirror_url: &str,
    provider: &str,
) -> Result<HashMap<String, HashMap<String, PlatformEntry>>> {
    super::fetch_json_retry(
        client,
        retries,
        &format!("{}/api/{}/checksums", mirror_url, provider),
    )
}

pub(super) fn select_asset_for_platform(
    checksums: &HashMap<String, HashMap<String, PlatformEntry>>,
    version: &str,
    platform: &str,
) -> Result<(String, ArtifactEntry)> {
    let entries = checksums
        .get(version)
        .ok_or_else(|| anyhow!("version {} not found in checksums", version))?;

    let platform_entry = entries
        .get(platform)
        .or_else(|| entries.get("universal"))
        .or_else(|| entries.values().next())
        .ok_or_else(|| anyhow!("no platform entry in checksums for version {}", version))?;

    if platform_entry.files.is_empty() {
        return Ok((
            platform_entry.filename.clone(),
            ArtifactEntry {
                sha256: platform_entry.sha256.clone(),
                size: platform_entry.size,
            },
        ));
    }

    let hints = platform_hints(platform);
    let mut files = platform_entry
        .files
        .iter()
        .map(|(name, meta)| (name.clone(), meta.clone()))
        .collect::<Vec<_>>();
    files.sort_by(|a, b| a.0.cmp(&b.0));

    if let Some((name, meta)) = files
        .iter()
        .find(|(name, _)| hints.iter().any(|hint| name.contains(hint)))
    {
        return Ok((name.clone(), meta.clone()));
    }

    files
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("empty files map"))
}

fn platform_hints(platform: &str) -> Vec<&'static str> {
    match platform {
        "x86_64-apple-darwin" => vec!["x86_64-apple-darwin", "darwin-x64", "macos-x64"],
        "aarch64-apple-darwin" => vec!["aarch64-apple-darwin", "darwin-arm64", "macos-arm64"],
        "x86_64-unknown-linux-gnu" => vec!["x86_64-unknown-linux-gnu", "linux-x64"],
        "aarch64-unknown-linux-gnu" => vec!["aarch64-unknown-linux-gnu", "linux-arm64"],
        "x86_64-unknown-linux-musl" => vec!["x86_64-unknown-linux-musl", "linux-x64-musl"],
        "aarch64-unknown-linux-musl" => vec!["aarch64-unknown-linux-musl", "linux-arm64-musl"],
        "x86_64-pc-windows-msvc" => vec!["x86_64-pc-windows-msvc", "win32-x64", "windows-x64"],
        "aarch64-pc-windows-msvc" => {
            vec!["aarch64-pc-windows-msvc", "win32-arm64", "windows-arm64"]
        }
        _ => vec![],
    }
}

pub(super) fn ensure_downloaded(
    ctx: &InstallContext,
    url: &str,
    cache_path: &Path,
    expected_sha256: &str,
    expected_size: u64,
    ui: &Ui,
) -> Result<PathBuf> {
    if cache_path.exists() {
        let verifying = tr(output().lang, "verifying");
        let cache_ok = run_with_spinner(ui, verifying, || {
            let checksum_ok = verify_file_sha256(cache_path, expected_sha256)?;
            let size_ok = expected_size == 0 || fs::metadata(cache_path)?.len() == expected_size;
            Ok(checksum_ok && size_ok)
        })?;

        if cache_ok {
            ui.info(&format!("cache hit: {}", cache_path.display()));
            return Ok(cache_path.to_path_buf());
        }
    }

    if let Some(parent) = cache_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let label = ui.label_downloading("artifact");
    ui.info(&format!("{} {}", label, url));

    let mut response = request_with_retry(ctx.retries, || ctx.client.get(url).send())?;
    if response.status() == StatusCode::NOT_FOUND {
        bail!("artifact not found: {}", url);
    }
    if !response.status().is_success() {
        bail!("download failed {}: {}", url, response.status());
    }

    let mut temp_file = NamedTempFile::new_in(
        cache_path
            .parent()
            .ok_or_else(|| anyhow!("cache path has no parent"))?,
    )?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 8192];
    let total_size = if expected_size > 0 {
        Some(expected_size)
    } else {
        response.content_length()
    };
    let mut progress = ui.download_progress(&label, total_size);

    let download_result = (|| -> Result<()> {
        loop {
            let read = response.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            temp_file.write_all(&buffer[..read])?;
            total += read as u64;
            if let Some(progress) = progress.as_mut() {
                progress.update(total);
            }
        }
        if let Some(progress) = progress.as_mut() {
            progress.finish_ok(total);
        }
        Ok(())
    })();
    if let Err(err) = download_result {
        if let Some(progress) = progress.as_mut() {
            progress.finish_err(Some(&err.to_string()));
        }
        return Err(err);
    }

    let actual = hex::encode(hasher.finalize());
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        bail!(
            "checksum mismatch: expected {}, actual {}",
            expected_sha256,
            actual
        );
    }

    if expected_size > 0 && expected_size != total {
        bail!(
            "size mismatch: expected {}, downloaded {}",
            expected_size,
            total
        );
    }

    temp_file
        .persist(cache_path)
        .map_err(|err| anyhow!("persist cache file failed: {}", err))?;

    Ok(cache_path.to_path_buf())
}

pub(super) fn verify_file_sha256(path: &Path, expected_sha256: &str) -> Result<bool> {
    if expected_sha256.trim().is_empty() {
        return Ok(true);
    }

    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];

    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }

    let actual = hex::encode(hasher.finalize());
    Ok(actual.eq_ignore_ascii_case(expected_sha256))
}

pub(super) fn encode_filepath(path: &str) -> String {
    path.split('/')
        .map(|segment| urlencoding::encode(segment).into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

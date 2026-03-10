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
    provider: &str,
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

    let hints = platform_hints(platform, provider);
    let mut files = platform_entry
        .files
        .iter()
        .map(|(name, meta)| (name.clone(), meta.clone()))
        .collect::<Vec<_>>();
    files.sort_by(|a, b| a.0.cmp(&b.0));

    if let Some((name, meta)) = select_installable_asset(&files, &hints, platform, provider) {
        return Ok((name, meta));
    }

    if let Some((name, meta)) = files
        .iter()
        .filter(|(name, _)| hints.iter().any(|hint| matches_platform_hint(name, hint)))
        .min_by(|(name_a, _), (name_b, _)| {
            primary_asset_priority(name_a, provider, &hints)
                .cmp(&primary_asset_priority(name_b, provider, &hints))
                .then_with(|| {
                    platform_hint_priority(name_a, &hints)
                        .cmp(&platform_hint_priority(name_b, &hints))
                })
                .then_with(|| name_a.cmp(name_b))
        })
    {
        return Ok((name.clone(), meta.clone()));
    }

    files
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("empty files map"))
}

fn platform_hints(platform: &str, provider: &str) -> Vec<&'static str> {
    match (provider, platform) {
        ("codex", "x86_64-unknown-linux-gnu") => vec![
            "x86_64-unknown-linux-musl",
            "linux-x64-musl",
            "x86_64-unknown-linux-gnu",
            "linux-x64",
        ],
        ("codex", "aarch64-unknown-linux-gnu") => vec![
            "aarch64-unknown-linux-musl",
            "linux-arm64-musl",
            "aarch64-unknown-linux-gnu",
            "linux-arm64",
        ],
        (_, "x86_64-apple-darwin") => vec!["x86_64-apple-darwin", "darwin-x64", "macos-x64"],
        (_, "aarch64-apple-darwin") => vec!["aarch64-apple-darwin", "darwin-arm64", "macos-arm64"],
        (_, "x86_64-unknown-linux-gnu") => vec!["x86_64-unknown-linux-gnu", "linux-x64"],
        (_, "aarch64-unknown-linux-gnu") => vec!["aarch64-unknown-linux-gnu", "linux-arm64"],
        (_, "x86_64-unknown-linux-musl") => vec!["x86_64-unknown-linux-musl", "linux-x64-musl"],
        (_, "aarch64-unknown-linux-musl") => vec!["aarch64-unknown-linux-musl", "linux-arm64-musl"],
        (_, "x86_64-pc-windows-msvc") => vec!["x86_64-pc-windows-msvc", "win32-x64", "windows-x64"],
        (_, "aarch64-pc-windows-msvc") => {
            vec!["aarch64-pc-windows-msvc", "win32-arm64", "windows-arm64"]
        }
        _ => vec![],
    }
}

fn platform_hint_priority(name: &str, hints: &[&str]) -> usize {
    hints
        .iter()
        .position(|hint| matches_platform_hint(name, hint))
        .unwrap_or(hints.len())
}

fn matches_platform_hint(name: &str, hint: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let hint = hint.to_ascii_lowercase();

    lower.match_indices(&hint).any(|(index, _)| {
        let before = lower[..index].chars().next_back();
        let after = lower[index + hint.len()..].chars().next();
        matches!(before, None | Some('/' | '-' | '_' | '.'))
            && matches!(after, None | Some('/' | '.' | '_'))
    })
}

fn select_installable_asset(
    files: &[(String, ArtifactEntry)],
    hints: &[&str],
    platform: &str,
    provider: &str,
) -> Option<(String, ArtifactEntry)> {
    let hinted = files
        .iter()
        .filter(|(name, _)| hints.iter().any(|hint| matches_platform_hint(name, hint)))
        .filter_map(|(name, meta)| {
            installable_priority(name, platform).map(|priority| (priority, name, meta))
        })
        .min_by(|a, b| {
            primary_asset_priority(a.1, provider, hints)
                .cmp(&primary_asset_priority(b.1, provider, hints))
                .then_with(|| {
                    platform_hint_priority(a.1, hints).cmp(&platform_hint_priority(b.1, hints))
                })
                .then_with(|| a.0.cmp(&b.0))
                .then_with(|| a.1.cmp(b.1))
        });
    if let Some((_, name, meta)) = hinted {
        return Some((name.clone(), meta.clone()));
    }

    files
        .iter()
        .filter_map(|(name, meta)| {
            installable_priority(name, platform).map(|priority| (priority, name, meta))
        })
        .min_by(|a, b| {
            primary_asset_priority(a.1, provider, hints)
                .cmp(&primary_asset_priority(b.1, provider, hints))
                .then_with(|| {
                    platform_hint_priority(a.1, hints).cmp(&platform_hint_priority(b.1, hints))
                })
                .then_with(|| a.0.cmp(&b.0))
                .then_with(|| a.1.cmp(b.1))
        })
        .map(|(_, name, meta)| (name.clone(), meta.clone()))
}

fn primary_asset_priority(name: &str, provider: &str, hints: &[&str]) -> u8 {
    let lower = name.to_ascii_lowercase();
    let provider = provider.to_ascii_lowercase();

    if lower == provider || lower == format!("{provider}.exe") {
        return 0;
    }

    if let Some(remainder) = lower.strip_prefix(&format!("{provider}-")) {
        if hints.iter().any(|hint| remainder.starts_with(hint)) {
            return 0;
        }
        return 1;
    }

    if lower.starts_with(&format!("{provider}.")) {
        return 1;
    }

    2
}

fn installable_priority(name: &str, platform: &str) -> Option<u8> {
    let lower = name.to_ascii_lowercase();

    if lower.ends_with(".sha256") || lower.ends_with(".sum") || lower.ends_with(".json") {
        return None;
    }

    if platform.contains("windows") {
        if lower.ends_with(".zip") {
            return Some(0);
        }
        if lower.ends_with(".exe") {
            return Some(1);
        }
        return None;
    }

    if lower.ends_with(".tar.gz") {
        return Some(0);
    }
    if lower.ends_with(".tgz") {
        return Some(1);
    }
    if lower.ends_with(".tar.xz") {
        return Some(2);
    }
    if lower.ends_with(".zip") {
        return Some(3);
    }
    if !lower.contains('.') {
        return Some(4);
    }

    None
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

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(sha256: &str) -> ArtifactEntry {
        ArtifactEntry {
            sha256: sha256.to_string(),
            size: 1,
        }
    }

    fn entry(filename: &str, files: &[(&str, &str)]) -> PlatformEntry {
        PlatformEntry {
            sha256: "platform-sha".to_string(),
            size: 1,
            filename: filename.to_string(),
            files: files
                .iter()
                .map(|(name, sha)| ((*name).to_string(), artifact(sha)))
                .collect(),
        }
    }

    #[test]
    fn select_asset_for_platform_prefers_installable_archive_over_dmg() {
        let mut checksums = HashMap::new();
        checksums.insert(
            "v1.0.0".to_string(),
            HashMap::from([(
                "universal".to_string(),
                entry(
                    "codex-aarch64-apple-darwin.dmg",
                    &[
                        ("codex-aarch64-apple-darwin.dmg", "sha-dmg"),
                        ("codex-aarch64-apple-darwin.tar.gz", "sha-targz"),
                    ],
                ),
            )]),
        );

        let (name, meta) =
            select_asset_for_platform(&checksums, "v1.0.0", "aarch64-apple-darwin", "codex")
                .expect("select asset");

        assert_eq!(name, "codex-aarch64-apple-darwin.tar.gz");
        assert_eq!(meta.sha256, "sha-targz");
    }

    #[test]
    fn select_asset_for_platform_skips_checksum_sidecars() {
        let mut checksums = HashMap::new();
        checksums.insert(
            "v1.0.0".to_string(),
            HashMap::from([(
                "universal".to_string(),
                entry(
                    "acm-installer-x86_64-pc-windows-msvc.zip",
                    &[
                        ("acm-installer-x86_64-pc-windows-msvc.zip", "sha-zip"),
                        (
                            "acm-installer-x86_64-pc-windows-msvc.zip.sha256",
                            "sha-sidecar",
                        ),
                    ],
                ),
            )]),
        );

        let (name, meta) = select_asset_for_platform(
            &checksums,
            "v1.0.0",
            "x86_64-pc-windows-msvc",
            "acm-installer",
        )
        .expect("select asset");

        assert_eq!(name, "acm-installer-x86_64-pc-windows-msvc.zip");
        assert_eq!(meta.sha256, "sha-zip");
    }

    #[test]
    fn select_asset_for_platform_prefers_primary_codex_binary_over_internal_tools() {
        let mut checksums = HashMap::new();
        checksums.insert(
            "v1.0.0".to_string(),
            HashMap::from([(
                "universal".to_string(),
                entry(
                    "codex-x86_64-unknown-linux-gnu.tar.gz",
                    &[
                        (
                            "codex-responses-api-proxy-x86_64-unknown-linux-gnu.tar.gz",
                            "sha-proxy",
                        ),
                        (
                            "codex-command-runner-x86_64-unknown-linux-gnu.tar.gz",
                            "sha-runner",
                        ),
                        ("codex-x86_64-unknown-linux-gnu.tar.gz", "sha-codex"),
                    ],
                ),
            )]),
        );

        let (name, meta) =
            select_asset_for_platform(&checksums, "v1.0.0", "x86_64-unknown-linux-gnu", "codex")
                .expect("select asset");

        assert_eq!(name, "codex-x86_64-unknown-linux-gnu.tar.gz");
        assert_eq!(meta.sha256, "sha-codex");
    }

    #[test]
    fn select_asset_for_platform_prefers_codex_musl_on_linux_gnu_for_compatibility() {
        let mut checksums = HashMap::new();
        checksums.insert(
            "v1.0.0".to_string(),
            HashMap::from([(
                "universal".to_string(),
                entry(
                    "codex-x86_64-unknown-linux-gnu.tar.gz",
                    &[
                        ("codex-x86_64-unknown-linux-gnu.tar.gz", "sha-gnu"),
                        ("codex-x86_64-unknown-linux-musl.tar.gz", "sha-musl"),
                    ],
                ),
            )]),
        );

        let (name, meta) =
            select_asset_for_platform(&checksums, "v1.0.0", "x86_64-unknown-linux-gnu", "codex")
                .expect("select asset");

        assert_eq!(name, "codex-x86_64-unknown-linux-musl.tar.gz");
        assert_eq!(meta.sha256, "sha-musl");
    }

    #[test]
    fn select_asset_for_platform_does_not_confuse_gnu_with_musl_alias_paths() {
        let mut checksums = HashMap::new();
        checksums.insert(
            "v1.0.0".to_string(),
            HashMap::from([(
                "universal".to_string(),
                entry(
                    "linux-x64/claude",
                    &[
                        ("linux-x64-musl/claude", "sha-musl"),
                        ("linux-x64/claude", "sha-gnu"),
                    ],
                ),
            )]),
        );

        let (name, meta) = select_asset_for_platform(
            &checksums,
            "v1.0.0",
            "x86_64-unknown-linux-gnu",
            "claude-code",
        )
        .expect("select asset");

        assert_eq!(name, "linux-x64/claude");
        assert_eq!(meta.sha256, "sha-gnu");
    }
}

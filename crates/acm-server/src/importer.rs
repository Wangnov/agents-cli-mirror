use anyhow::{Context, Result, bail};
use chrono::Utc;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use tokio::io::AsyncReadExt;
use tracing::warn;

use crate::cache::{CacheManager, FileMetadata, PlatformMetadata, VersionMetadata};
use crate::config::{Config, StorageMode};
use crate::s3;
use crate::storage_clients::StorageClients;

const UNIVERSAL_PLATFORM: &str = "universal";

#[derive(Debug, Clone)]
pub struct ImportSummary {
    pub provider: String,
    pub version: String,
    pub tag: String,
    pub file_count: usize,
    pub total_bytes: u64,
}

#[derive(Debug, Clone)]
struct SourceFile {
    abs: PathBuf,
    rel: String,
}

struct ImportTargetContext<'a> {
    cache: &'a CacheManager,
    config: &'a Config,
    storage_clients: &'a StorageClients,
    provider: &'a str,
    version: &'a str,
}

pub async fn import_provider_version(
    config: &Config,
    provider: &str,
    version: &str,
    from: &Path,
    tag: Option<&str>,
) -> Result<ImportSummary> {
    ensure_provider_configured(config, provider)?;
    ensure_version_valid(version)?;

    let tag = tag.unwrap_or("imported").trim().to_string();
    ensure_tag_valid(&tag)?;

    let files = collect_source_files(from)?;
    if files.is_empty() {
        bail!("import source directory is empty: {}", from.display());
    }

    let cache = CacheManager::new(&config.cache)?;
    let storage_clients = StorageClients::new(&config.storage).await?;
    let existing_files = version_files(&cache, provider, version).await;
    let target = ImportTargetContext {
        cache: &cache,
        config,
        storage_clients: &storage_clients,
        provider,
        version,
    };

    prepare_target(&target, &existing_files).await?;

    let mut file_meta = HashMap::new();
    let mut total_bytes = 0u64;
    for file in files {
        let (sha256, size) = calculate_sha256(&file.abs).await?;
        store_file(&target, &file.rel, &file.abs, &sha256).await?;
        total_bytes += size;
        file_meta.insert(file.rel, FileMetadata { sha256, size });
    }

    let mut names = file_meta.keys().cloned().collect::<Vec<_>>();
    names.sort();
    let primary_name = names
        .first()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no file metadata collected"))?;
    let primary = file_meta
        .get(&primary_name)
        .ok_or_else(|| anyhow::anyhow!("primary file metadata missing"))?;

    let mut platforms = HashMap::new();
    platforms.insert(
        UNIVERSAL_PLATFORM.to_string(),
        PlatformMetadata {
            sha256: primary.sha256.clone(),
            size: primary.size,
            filename: primary_name,
            files: file_meta,
        },
    );

    cache
        .update_provider_metadata(provider, |m| {
            m.versions.insert(
                version.to_string(),
                VersionMetadata {
                    version: version.to_string(),
                    downloaded_at: Utc::now(),
                    platforms,
                },
            );
            m.tags.insert(tag.clone(), version.to_string());
            m.updated_at = Some(Utc::now());
        })
        .await?;
    cache.write_tag(provider, &tag, version).await?;

    Ok(ImportSummary {
        provider: provider.to_string(),
        version: version.to_string(),
        tag,
        file_count: names.len(),
        total_bytes,
    })
}

fn ensure_provider_configured(config: &Config, provider: &str) -> Result<()> {
    if config.providers.iter().any(|item| item.name == provider) {
        return Ok(());
    }
    bail!("provider '{}' is not configured in config.toml", provider)
}

fn ensure_version_valid(version: &str) -> Result<()> {
    let version = version.trim();
    if version.is_empty() {
        bail!("version must not be empty");
    }
    if version.contains("..")
        || version.contains('/')
        || version.contains('\\')
        || version.contains(char::is_whitespace)
    {
        bail!("version contains invalid characters");
    }
    Ok(())
}

fn ensure_tag_valid(tag: &str) -> Result<()> {
    if tag.is_empty() {
        bail!("tag must not be empty");
    }
    if tag.contains('/')
        || tag.contains('\\')
        || tag.contains("..")
        || tag.contains(char::is_whitespace)
    {
        bail!("tag contains invalid characters");
    }
    Ok(())
}

fn collect_source_files(from: &Path) -> Result<Vec<SourceFile>> {
    if !from.exists() {
        bail!("import source directory not found: {}", from.display());
    }
    if !from.is_dir() {
        bail!("import source must be a directory: {}", from.display());
    }

    let root = from.canonicalize().with_context(|| {
        format!(
            "failed to canonicalize import source directory: {}",
            from.display()
        )
    })?;
    let mut files = Vec::new();
    walk_dir(&root, &root, &mut files)?;
    files.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(files)
}

fn walk_dir(root: &Path, current: &Path, files: &mut Vec<SourceFile>) -> Result<()> {
    for entry in std::fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            walk_dir(root, &path, files)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }

        let rel = path
            .strip_prefix(root)
            .context("failed to strip root prefix from source file path")?;
        let rel = normalize_rel_path(rel)?;
        files.push(SourceFile { abs: path, rel });
    }
    Ok(())
}

fn normalize_rel_path(path: &Path) -> Result<String> {
    let mut segments = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                let segment = value
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("path segment is not valid UTF-8"))?;
                if segment.is_empty() || segment == "." || segment == ".." {
                    bail!("invalid path segment '{}'", segment);
                }
                segments.push(segment.to_string());
            }
            _ => bail!("invalid path component in {}", path.display()),
        }
    }
    if segments.is_empty() {
        bail!("empty relative path");
    }
    Ok(segments.join("/"))
}

async fn calculate_sha256(path: &Path) -> Result<(String, u64)> {
    let mut file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("open source file failed: {}", path.display()))?;

    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 8192];
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size += read as u64;
    }

    Ok((hex::encode(hasher.finalize()), size))
}

async fn store_file(
    target: &ImportTargetContext<'_>,
    rel_path: &str,
    source: &Path,
    sha256: &str,
) -> Result<()> {
    match target.config.storage.mode {
        StorageMode::Local => {
            let local_path =
                local_file_path(target.cache, target.provider, target.version, rel_path)?;
            if let Some(parent) = local_path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::copy(source, &local_path)
                .await
                .with_context(|| {
                    format!(
                        "copy source '{}' to '{}' failed",
                        source.display(),
                        local_path.display()
                    )
                })?;
            Ok(())
        }
        StorageMode::S3 => {
            let key = object_key_for_file(target.cache, target.provider, target.version, rel_path)?;
            let client = target.storage_clients.s3().ok_or_else(|| {
                anyhow::anyhow!("storage mode is s3 but s3 client is not initialized")
            })?;
            let path = source.to_path_buf();
            s3::upload_file_with_client(
                client,
                &target.config.storage.s3,
                &key,
                "application/octet-stream",
                &path,
                Some(sha256),
            )
            .await
            .with_context(|| format!("upload '{}' to s3 key '{}' failed", source.display(), key))
        }
    }
}

fn local_file_path(
    cache: &CacheManager,
    provider: &str,
    version: &str,
    rel_path: &str,
) -> Result<PathBuf> {
    let mut path = cache.version_path(provider, version).join("files");
    for segment in rel_path.split('/') {
        if segment.is_empty() {
            bail!("invalid relative path: {}", rel_path);
        }
        path = path.join(segment);
    }
    Ok(path)
}

fn object_key_for_file(
    cache: &CacheManager,
    provider: &str,
    version: &str,
    rel_path: &str,
) -> Result<String> {
    let mut segments = vec![
        "versions".to_string(),
        version.to_string(),
        "files".to_string(),
    ];
    for segment in rel_path.split('/') {
        if segment.is_empty() {
            bail!("invalid relative path: {}", rel_path);
        }
        segments.push(segment.to_string());
    }
    let refs = segments.iter().map(String::as_str).collect::<Vec<_>>();
    cache
        .build_object_key(provider, &refs)
        .ok_or_else(|| anyhow::anyhow!("invalid object key for {}", rel_path))
}

async fn prepare_target(target: &ImportTargetContext<'_>, existing_files: &[String]) -> Result<()> {
    match target.config.storage.mode {
        StorageMode::Local => {
            let version_path = target.cache.version_path(target.provider, target.version);
            if version_path.exists() {
                tokio::fs::remove_dir_all(&version_path)
                    .await
                    .with_context(|| {
                        format!(
                            "remove existing version directory failed: {}",
                            version_path.display()
                        )
                    })?;
            }
        }
        StorageMode::S3 => {
            if existing_files.is_empty() {
                return Ok(());
            }
            let Some(client) = target.storage_clients.s3() else {
                bail!("storage mode is s3 but s3 client is not initialized");
            };
            for file in existing_files {
                let key = object_key_for_file(target.cache, target.provider, target.version, file)?;
                if let Err(err) =
                    s3::delete_object_with_client(client, &target.config.storage.s3, &key).await
                {
                    warn!("failed to delete stale S3 object {}: {}", key, err);
                }
            }
        }
    }
    Ok(())
}

async fn version_files(cache: &CacheManager, provider: &str, version: &str) -> Vec<String> {
    cache
        .with_provider_metadata(provider, |meta| {
            let Some(version_meta) = meta.versions.get(version) else {
                return Vec::new();
            };
            let mut files = HashSet::new();
            for platform in version_meta.platforms.values() {
                if platform.files.is_empty() {
                    files.insert(platform.filename.clone());
                } else {
                    files.extend(platform.files.keys().cloned());
                }
            }
            let mut values = files.into_iter().collect::<Vec<_>>();
            values.sort();
            values
        })
        .await
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        CacheConfig, Config, DynamicProviderConfig, ProviderSource, ProviderUpdatePolicy,
    };
    use tempfile::TempDir;

    fn static_provider(name: &str) -> DynamicProviderConfig {
        DynamicProviderConfig {
            name: name.to_string(),
            enabled: true,
            source: ProviderSource::Static,
            tags: vec!["latest".to_string()],
            update_policy: ProviderUpdatePolicy::Manual,
            platforms: Vec::new(),
            include_prerelease: false,
            files: vec!["artifact.bin".to_string()],
            repo: None,
            upstream_url: None,
            static_version: Some("1.0.0".to_string()),
            ui: Default::default(),
        }
    }

    fn base_config(cache_dir: &Path) -> Config {
        Config {
            cache: CacheConfig {
                dir: cache_dir.to_path_buf(),
                max_versions: 10,
            },
            providers: vec![static_provider("tool-a")],
            ..Config::default()
        }
    }

    #[tokio::test]
    async fn import_to_local_cache_updates_metadata_and_tag() {
        let cache_dir = TempDir::new().expect("cache dir");
        let source = TempDir::new().expect("source dir");
        let nested = source.path().join("nested");
        std::fs::create_dir_all(&nested).expect("create nested dir");
        std::fs::write(source.path().join("a.bin"), b"alpha").expect("write source file");
        std::fs::write(nested.join("b.bin"), b"beta").expect("write source file");

        let config = base_config(cache_dir.path());
        let summary =
            import_provider_version(&config, "tool-a", "9.9.9", source.path(), Some("snapshot"))
                .await
                .expect("import succeeds");

        assert_eq!(summary.provider, "tool-a");
        assert_eq!(summary.version, "9.9.9");
        assert_eq!(summary.tag, "snapshot");
        assert_eq!(summary.file_count, 2);
        assert_eq!(summary.total_bytes, 9);

        let file_a = cache_dir
            .path()
            .join("tool-a")
            .join("versions")
            .join("9.9.9")
            .join("files")
            .join("a.bin");
        let file_b = cache_dir
            .path()
            .join("tool-a")
            .join("versions")
            .join("9.9.9")
            .join("files")
            .join("nested")
            .join("b.bin");
        assert!(file_a.exists());
        assert!(file_b.exists());

        let cache = CacheManager::new(&config.cache).expect("cache manager");
        let version = cache
            .read_tag("tool-a", "snapshot")
            .await
            .expect("tag value");
        assert_eq!(version, "9.9.9");

        let files = version_files(&cache, "tool-a", "9.9.9").await;
        assert_eq!(files, vec!["a.bin".to_string(), "nested/b.bin".to_string()]);
    }

    #[tokio::test]
    async fn import_requires_configured_provider() {
        let cache_dir = TempDir::new().expect("cache dir");
        let source = TempDir::new().expect("source dir");
        std::fs::write(source.path().join("a.bin"), b"alpha").expect("write source file");

        let config = base_config(cache_dir.path());
        let err = import_provider_version(&config, "missing", "1.0.0", source.path(), None)
            .await
            .expect_err("import should fail");
        assert!(err.to_string().contains("is not configured"));
    }
}

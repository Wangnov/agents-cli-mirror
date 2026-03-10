use anyhow::Result;
use chrono::Utc;
use futures::StreamExt;
use reqwest::{Client, StatusCode};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{info, warn};

use crate::cache::{
    CacheManager, FileMetadata, PlatformMetadata, ProviderMetadata, VersionMetadata,
};
use crate::config::{
    DynamicProviderConfig, HttpConfig, ProviderSource, StorageConfig, StorageMode,
};
use crate::error::MirrorError;
use crate::http_client::build_http_client;
use crate::providers::github::{Asset, DownloadResult, GithubClient, Release};
use crate::providers::release_core;
use crate::retry::{RetryPolicy, send_with_retry};
use crate::s3;
use crate::storage_clients::StorageClients;

const UNIVERSAL_PLATFORM: &str = "universal";

pub struct GenericProvider {
    config: DynamicProviderConfig,
    cache: Arc<CacheManager>,
    storage: StorageConfig,
    storage_clients: StorageClients,
    github: Option<GithubClient>,
    client: Client,
}

impl GenericProvider {
    pub fn new(
        config: DynamicProviderConfig,
        cache: Arc<CacheManager>,
        storage: StorageConfig,
        storage_clients: StorageClients,
        http: HttpConfig,
    ) -> Result<Self> {
        if config.name.is_empty() {
            return Err(MirrorError::Provider("provider.name is empty".to_string()).into());
        }

        let github = if matches!(config.source, ProviderSource::GithubRelease) {
            Some(GithubClient::new(&http)?)
        } else {
            None
        };
        let client = build_http_client(&http)?;

        Ok(Self {
            config,
            cache,
            storage,
            storage_clients,
            github,
            client,
        })
    }

    pub fn name(&self) -> &str {
        &self.config.name
    }

    fn release_has_required_assets(&self, release: &Release) -> bool {
        if self.config.files.is_empty() {
            return !release.assets.is_empty();
        }

        self.config
            .files
            .iter()
            .all(|required| release.assets.iter().any(|asset| asset.name == *required))
    }

    fn select_release<'a>(&self, releases: &'a [Release], tag: &str) -> Option<&'a Release> {
        let allow_prerelease = tag == "latest" && self.config.include_prerelease;
        releases.iter().find(|release| {
            !release.draft
                && (allow_prerelease || !release.prerelease)
                && self.release_has_required_assets(release)
        })
    }

    fn asset_digest_sha256(asset: &Asset) -> Option<&str> {
        asset
            .digest
            .as_deref()
            .and_then(|digest| digest.strip_prefix("sha256:"))
    }

    fn split_rel_path(path: &str) -> Result<Vec<String>> {
        if path.is_empty() {
            return Err(MirrorError::Provider("file path is empty".to_string()).into());
        }
        let normalized = path.replace('\\', "/");
        let mut segments = Vec::new();
        for segment in normalized.split('/') {
            if segment.is_empty() || segment == "." || segment == ".." {
                return Err(MirrorError::Provider(format!("invalid file path: {}", path)).into());
            }
            segments.push(segment.to_string());
        }
        Ok(segments)
    }

    fn version_file_path(&self, version: &str, rel_path: &str) -> Result<PathBuf> {
        let mut path = self.cache.version_path(self.name(), version).join("files");
        for segment in Self::split_rel_path(rel_path)? {
            path = path.join(segment);
        }
        Ok(path)
    }

    fn object_key_for_file(&self, version: &str, rel_path: &str) -> Result<String> {
        let mut segments = vec![
            "versions".to_string(),
            version.to_string(),
            "files".to_string(),
        ];
        segments.extend(Self::split_rel_path(rel_path)?);
        let refs = segments.iter().map(String::as_str).collect::<Vec<_>>();
        self.cache
            .build_object_key(self.name(), &refs)
            .ok_or_else(|| {
                MirrorError::Provider(format!("Invalid object key for {}", rel_path)).into()
            })
    }

    async fn github_client(&self) -> Result<&GithubClient> {
        self.github.as_ref().ok_or_else(|| {
            MirrorError::Provider(format!("GitHub client not initialized for {}", self.name()))
                .into()
        })
    }

    pub async fn fetch_upstream_tag(&self, tag: &str) -> Result<String> {
        match self.config.source {
            ProviderSource::GithubRelease => {
                let repo = self
                    .config
                    .repo
                    .as_deref()
                    .ok_or_else(|| MirrorError::Provider("repo is required".to_string()))?;
                let github = self.github_client().await?;

                if tag == "latest" || tag == "stable" {
                    let releases = github.fetch_releases(repo).await?;
                    let release = self
                        .select_release(&releases, tag)
                        .ok_or_else(|| MirrorError::VersionNotFound(tag.to_string()))?;
                    Ok(release.tag_name.clone())
                } else {
                    let release = github.fetch_release_by_tag(repo, tag).await?;
                    Ok(release.tag_name.clone())
                }
            }
            ProviderSource::GcsRelease => {
                let upstream = self.config.upstream_url.as_deref().ok_or_else(|| {
                    MirrorError::Provider("upstream_url is required for gcs_release".to_string())
                })?;
                let base = upstream.trim_end_matches('/');
                let url = format!("{}/{}", base, tag);
                let response = send_with_retry(|| self.client.get(&url), RetryPolicy::default())
                    .await
                    .map_err(|e| {
                        MirrorError::Provider(format!("Failed to fetch gcs tag {}: {}", tag, e))
                    })?;
                if response.status() == StatusCode::NOT_FOUND {
                    return Err(MirrorError::VersionNotFound(tag.to_string()).into());
                }
                if !response.status().is_success() {
                    return Err(MirrorError::Provider(format!(
                        "Failed to fetch gcs tag {}: {}",
                        tag,
                        response.status()
                    ))
                    .into());
                }
                Ok(response.text().await?.trim().to_string())
            }
            ProviderSource::Static => Ok(self
                .config
                .static_version
                .clone()
                .unwrap_or_else(|| tag.to_string())),
        }
    }

    pub async fn sync_tag(&self, tag: &str) -> Result<Option<String>> {
        release_core::sync_tag_common(
            release_core::SyncTagContext {
                cache: self.cache.as_ref(),
                enabled: self.config.enabled,
                storage_mode: &self.storage.mode,
                provider_name: self.name(),
                tag,
            },
            self.config.update_policy,
            |current_tag| async move { self.fetch_upstream_tag(&current_tag).await },
            |version| async move { self.sync_version(&version).await },
            |versions| async move { self.delete_remote_versions(&versions).await },
        )
        .await
    }

    pub async fn sync_version(&self, version: &str) -> Result<()> {
        if self.is_version_complete(version).await {
            info!("Version {} already cached for {}", version, self.name());
            return Ok(());
        }

        match self.config.source {
            ProviderSource::GithubRelease => self.sync_version_from_github(version).await,
            ProviderSource::GcsRelease => self.sync_version_from_gcs(version).await,
            ProviderSource::Static => self.sync_version_from_static(version).await,
        }
    }

    async fn sync_version_from_github(&self, version: &str) -> Result<()> {
        let repo = self
            .config
            .repo
            .as_deref()
            .ok_or_else(|| MirrorError::Provider("repo is required".to_string()))?;
        let github = self.github_client().await?;
        let release = github.fetch_release_by_tag(repo, version).await?;

        let selected_assets = if self.config.files.is_empty() {
            release.assets.clone()
        } else {
            let mut selected = Vec::new();
            for name in &self.config.files {
                let asset = release
                    .assets
                    .iter()
                    .find(|asset| asset.name == *name)
                    .ok_or_else(|| MirrorError::Provider(format!("Asset not found: {}", name)))?;
                selected.push(asset.clone());
            }
            selected
        };

        if selected_assets.is_empty() {
            return Err(MirrorError::Provider(format!("No assets in release {}", version)).into());
        }

        let mut files = HashMap::new();
        for asset in &selected_assets {
            let result = match self.storage.mode {
                StorageMode::Local => {
                    let path = self.version_file_path(version, &asset.name)?;
                    if let Some(parent) = path.parent() {
                        tokio::fs::create_dir_all(parent).await?;
                    }
                    match github
                        .download_asset_to_path(&asset.browser_download_url, &path)
                        .await
                    {
                        Ok(result) => result,
                        Err(err) => {
                            let _ = tokio::fs::remove_file(&path).await;
                            return Err(err);
                        }
                    }
                }
                StorageMode::S3 => {
                    let key = self.object_key_for_file(version, &asset.name)?;
                    github
                        .download_asset_to_storage(
                            &asset.browser_download_url,
                            &self.storage,
                            &self.storage_clients,
                            &key,
                            "application/octet-stream",
                        )
                        .await?
                }
            };

            if let Some(expected) = Self::asset_digest_sha256(asset) {
                if result.sha256 != expected {
                    return Err(MirrorError::ChecksumMismatch {
                        expected: expected.to_string(),
                        actual: result.sha256,
                    }
                    .into());
                }
            }

            files.insert(
                asset.name.clone(),
                FileMetadata {
                    sha256: result.sha256,
                    size: result.size,
                },
            );
        }

        self.persist_version_metadata(version, files).await
    }

    async fn download_gcs_file_to_path(&self, url: &str, path: &Path) -> Result<DownloadResult> {
        let response = send_with_retry(|| self.client.get(url), RetryPolicy::default()).await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Err(MirrorError::VersionNotFound(url.to_string()).into());
        }
        if !response.status().is_success() {
            return Err(MirrorError::Provider(format!(
                "Failed to download {}: {}",
                url,
                response.status()
            ))
            .into());
        }

        let mut file = tokio::fs::File::create(path).await?;
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        let mut stream = response.bytes_stream();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            size += chunk.len() as u64;
            hasher.update(&chunk);
            file.write_all(&chunk).await?;
        }
        file.flush().await?;

        Ok(DownloadResult {
            size,
            sha256: hex::encode(hasher.finalize()),
        })
    }

    async fn download_gcs_file_to_remote(&self, url: &str, key: &str) -> Result<DownloadResult> {
        let response = send_with_retry(|| self.client.get(url), RetryPolicy::default()).await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Err(MirrorError::VersionNotFound(url.to_string()).into());
        }
        if !response.status().is_success() {
            return Err(MirrorError::Provider(format!(
                "Failed to download {}: {}",
                url,
                response.status()
            ))
            .into());
        }

        let Some(client) = self.storage_clients.s3() else {
            return Err(MirrorError::Provider("S3 client not initialized".to_string()).into());
        };
        let upload = s3::upload_stream_with_client(
            client,
            &self.storage.s3,
            key,
            "application/octet-stream",
            response.content_length(),
            response.bytes_stream(),
        )
        .await?;
        Ok(DownloadResult {
            size: upload.size,
            sha256: upload.sha256,
        })
    }

    async fn sync_version_from_gcs(&self, version: &str) -> Result<()> {
        let upstream = self.config.upstream_url.as_deref().ok_or_else(|| {
            MirrorError::Provider("upstream_url is required for gcs_release".to_string())
        })?;
        if self.config.files.is_empty() {
            return Err(MirrorError::Provider(format!(
                "{}: gcs_release requires providers.files",
                self.name()
            ))
            .into());
        }

        let mut files = HashMap::new();
        for file in &self.config.files {
            let url = format!("{}/{}/{}", upstream.trim_end_matches('/'), version, file);
            let result = match self.storage.mode {
                StorageMode::Local => {
                    let path = self.version_file_path(version, file)?;
                    if let Some(parent) = path.parent() {
                        tokio::fs::create_dir_all(parent).await?;
                    }
                    self.download_gcs_file_to_path(&url, &path).await?
                }
                StorageMode::S3 => {
                    let key = self.object_key_for_file(version, file)?;
                    self.download_gcs_file_to_remote(&url, &key).await?
                }
            };

            files.insert(
                file.clone(),
                FileMetadata {
                    sha256: result.sha256,
                    size: result.size,
                },
            );
        }

        self.persist_version_metadata(version, files).await
    }

    async fn sync_version_from_static(&self, version: &str) -> Result<()> {
        if self.config.files.is_empty() {
            return Err(MirrorError::Provider(format!(
                "{}: static source requires providers.files",
                self.name()
            ))
            .into());
        }

        let mut files = HashMap::new();
        for file in &self.config.files {
            let path = self.version_file_path(version, file)?;
            let result = Self::verify_file_checksum(&path).await?;
            if matches!(self.storage.mode, StorageMode::S3) {
                let key = self.object_key_for_file(version, file)?;
                let Some(client) = self.storage_clients.s3() else {
                    return Err(
                        MirrorError::Provider("S3 client not initialized".to_string()).into(),
                    );
                };
                s3::upload_file_with_client(
                    client,
                    &self.storage.s3,
                    &key,
                    "application/octet-stream",
                    &path,
                    Some(result.sha256.as_str()),
                )
                .await?;
            }
            files.insert(
                file.clone(),
                FileMetadata {
                    sha256: result.sha256,
                    size: result.size,
                },
            );
        }

        self.persist_version_metadata(version, files).await
    }

    async fn verify_file_checksum(path: &Path) -> Result<DownloadResult> {
        let mut file = tokio::fs::File::open(path).await?;
        let mut hasher = Sha256::new();
        let mut size: u64 = 0;
        let mut buffer = [0u8; 8192];

        loop {
            let read = file.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            size += read as u64;
            hasher.update(&buffer[..read]);
        }

        Ok(DownloadResult {
            size,
            sha256: hex::encode(hasher.finalize()),
        })
    }

    async fn persist_version_metadata(
        &self,
        version: &str,
        files: HashMap<String, FileMetadata>,
    ) -> Result<()> {
        let mut names = files.keys().cloned().collect::<Vec<_>>();
        names.sort();
        let primary_name = names
            .first()
            .cloned()
            .ok_or_else(|| MirrorError::Provider("no files synchronized".to_string()))?;
        let primary = files
            .get(&primary_name)
            .ok_or_else(|| MirrorError::Provider("primary file metadata missing".to_string()))?;

        let mut platforms = HashMap::new();
        platforms.insert(
            UNIVERSAL_PLATFORM.to_string(),
            PlatformMetadata {
                sha256: primary.sha256.clone(),
                size: primary.size,
                filename: primary_name,
                files,
            },
        );

        self.cache
            .update_provider_metadata(self.name(), |m| {
                m.versions.insert(
                    version.to_string(),
                    VersionMetadata {
                        version: version.to_string(),
                        downloaded_at: Utc::now(),
                        platforms,
                    },
                );
            })
            .await?;

        Ok(())
    }

    async fn is_version_complete(&self, version: &str) -> bool {
        let metadata = self.cache.get_metadata().await;
        let Some(provider) = metadata.provider(self.name()) else {
            return false;
        };
        let Some(version_meta) = provider.versions.get(version) else {
            return false;
        };
        let Some(platform_meta) = version_meta.platforms.get(UNIVERSAL_PLATFORM) else {
            return false;
        };

        if matches!(self.storage.mode, StorageMode::S3) {
            return true;
        }

        let file_names = if platform_meta.files.is_empty() {
            vec![platform_meta.filename.clone()]
        } else {
            platform_meta.files.keys().cloned().collect::<Vec<_>>()
        };
        for file in file_names {
            let Ok(path) = self.version_file_path(version, &file) else {
                return false;
            };
            if !path.exists() {
                return false;
            }
        }
        true
    }

    async fn delete_remote_versions(&self, versions: &[VersionMetadata]) {
        if !matches!(self.storage.mode, StorageMode::S3) {
            return;
        }
        let Some(client) = self.storage_clients.s3() else {
            return;
        };

        for version_meta in versions {
            let Some(platform) = version_meta.platforms.get(UNIVERSAL_PLATFORM) else {
                continue;
            };

            let file_names = if platform.files.is_empty() {
                vec![platform.filename.clone()]
            } else {
                platform.files.keys().cloned().collect::<Vec<_>>()
            };

            for file in file_names {
                let Ok(key) = self.object_key_for_file(&version_meta.version, &file) else {
                    continue;
                };
                if let Err(err) =
                    s3::delete_object_with_client(client, &self.storage.s3, &key).await
                {
                    warn!("Failed to delete S3 object {}: {:?}", key, err);
                }
            }
        }
    }

    pub async fn sync_all(&self) -> Result<Vec<String>> {
        release_core::sync_all_common(
            self.config.update_policy,
            true,
            &self.config.tags,
            |tag| async move { self.sync_tag(&tag).await },
        )
        .await
    }

    pub async fn sync_all_auto(&self) -> Result<Vec<String>> {
        release_core::sync_all_common(
            self.config.update_policy,
            false,
            &self.config.tags,
            |tag| async move { self.sync_tag(&tag).await },
        )
        .await
    }

    pub async fn get_tag_version(&self, tag: &str) -> Option<String> {
        release_core::get_tag_version_common(self.cache.as_ref(), self.name(), tag).await
    }

    pub async fn get_info(&self) -> serde_json::Value {
        let metadata = self.cache.get_metadata().await;
        let empty = ProviderMetadata::default();
        let provider = metadata.provider(self.name()).unwrap_or(&empty);

        let display_version = provider
            .tags
            .get("latest")
            .or_else(|| provider.tags.get("stable"));

        let mut platforms = serde_json::Map::new();
        if let Some(version) = display_version {
            if let Some(version_meta) = provider.versions.get(version) {
                for (platform, meta) in &version_meta.platforms {
                    let mut files_json = serde_json::Map::new();
                    if meta.files.is_empty() {
                        files_json.insert(
                            meta.filename.clone(),
                            serde_json::json!({
                                "url": format!("/{}/{}/files/{}", self.name(), version, meta.filename),
                                "sha256": meta.sha256,
                                "size": meta.size
                            }),
                        );
                    } else {
                        for (file, entry) in &meta.files {
                            files_json.insert(
                                file.clone(),
                                serde_json::json!({
                                    "url": format!("/{}/{}/files/{}", self.name(), version, file),
                                    "sha256": entry.sha256,
                                    "size": entry.size
                                }),
                            );
                        }
                    }
                    platforms.insert(
                        platform.clone(),
                        serde_json::json!({
                            "version": version,
                            "url": format!("/{}/{}/files/{}", self.name(), version, meta.filename),
                            "sha256": meta.sha256,
                            "size": meta.size,
                            "files": files_json
                        }),
                    );
                }
            }
        }

        serde_json::json!({
            "name": self.name(),
            "source": self.config.source,
            "update_policy": self.config.update_policy,
            "tags": provider.tags.clone(),
            "platforms": platforms,
            "updated_at": provider.updated_at,
            "sync": &provider.sync,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        CacheConfig, DynamicProviderConfig, HttpConfig, ProviderSource, ProviderUpdatePolicy,
        StorageConfig, StorageMode,
    };
    use crate::providers::github::{Asset, Release};
    use tempfile::TempDir;

    fn static_provider(name: &str, version: &str, file: &str) -> DynamicProviderConfig {
        DynamicProviderConfig {
            name: name.to_string(),
            enabled: true,
            source: ProviderSource::Static,
            tags: vec!["latest".to_string()],
            update_policy: ProviderUpdatePolicy::Manual,
            platforms: Vec::new(),
            include_prerelease: false,
            files: vec![file.to_string()],
            repo: None,
            upstream_url: None,
            static_version: Some(version.to_string()),
            ui: Default::default(),
        }
    }

    fn github_provider(name: &str) -> DynamicProviderConfig {
        DynamicProviderConfig {
            name: name.to_string(),
            enabled: true,
            source: ProviderSource::GithubRelease,
            tags: vec!["latest".to_string()],
            update_policy: ProviderUpdatePolicy::Tracking,
            platforms: Vec::new(),
            include_prerelease: false,
            files: Vec::new(),
            repo: Some("owner/repo".to_string()),
            upstream_url: None,
            static_version: None,
            ui: Default::default(),
        }
    }

    #[tokio::test]
    async fn static_source_supports_s3_mode_path() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let cache = Arc::new(
            CacheManager::new(&CacheConfig {
                dir: temp_dir.path().to_path_buf(),
                max_versions: 10,
            })
            .expect("create cache"),
        );

        let artifact = cache
            .version_path("tool-a", "1.0.0")
            .join("files")
            .join("artifact.bin");
        if let Some(parent) = artifact.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .expect("create artifact dir");
        }
        tokio::fs::write(&artifact, b"hello")
            .await
            .expect("write artifact");

        let provider = GenericProvider::new(
            static_provider("tool-a", "1.0.0", "artifact.bin"),
            cache,
            StorageConfig {
                mode: StorageMode::S3,
                ..StorageConfig::default()
            },
            StorageClients::default(),
            HttpConfig::default(),
        )
        .expect("create provider");

        let err = provider
            .sync_version("1.0.0")
            .await
            .expect_err("s3 mode without client should fail");
        assert!(err.to_string().contains("S3 client not initialized"));
    }

    #[test]
    fn select_release_skips_empty_asset_release_for_latest() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let cache = Arc::new(
            CacheManager::new(&CacheConfig {
                dir: temp_dir.path().to_path_buf(),
                max_versions: 10,
            })
            .expect("create cache"),
        );

        let provider = GenericProvider::new(
            github_provider("gemini"),
            cache,
            StorageConfig::default(),
            StorageClients::default(),
            HttpConfig::default(),
        )
        .expect("create provider");

        let releases = vec![
            Release {
                tag_name: "v0.33.0-preview.7".to_string(),
                prerelease: false,
                draft: false,
                assets: Vec::new(),
            },
            Release {
                tag_name: "v0.32.1".to_string(),
                prerelease: false,
                draft: false,
                assets: vec![Asset {
                    name: "gemini.js".to_string(),
                    browser_download_url: "https://example.com/gemini.js".to_string(),
                    digest: None,
                }],
            },
        ];

        let selected = provider
            .select_release(&releases, "latest")
            .expect("select release");

        assert_eq!(selected.tag_name, "v0.32.1");
    }
}

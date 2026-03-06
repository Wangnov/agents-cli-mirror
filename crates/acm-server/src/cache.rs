use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::config::CacheConfig;

/// Metadata for a cached version
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionMetadata {
    pub version: String,
    pub downloaded_at: DateTime<Utc>,
    pub platforms: HashMap<String, PlatformMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformMetadata {
    pub sha256: String,
    pub size: u64,
    pub filename: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub files: HashMap<String, FileMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMetadata {
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderSyncStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_success_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// Provider-specific metadata
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderMetadata {
    pub tags: HashMap<String, String>, // tag -> version
    pub versions: HashMap<String, VersionMetadata>,
    pub updated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub sync: ProviderSyncStatus,
}

/// Global cache metadata
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CacheMetadata {
    #[serde(default)]
    pub providers: HashMap<String, ProviderMetadata>,
}

impl CacheMetadata {
    pub fn provider(&self, name: &str) -> Option<&ProviderMetadata> {
        self.providers.get(name)
    }

    pub fn provider_mut(&mut self, name: &str) -> Option<&mut ProviderMetadata> {
        self.providers.get_mut(name)
    }
}

/// Cache manager handles all file operations and metadata
pub struct CacheManager {
    pub config: CacheConfig,
    metadata: Arc<RwLock<CacheMetadata>>,
}

fn provider_metadata_ref<'a>(
    metadata: &'a CacheMetadata,
    provider: &str,
) -> Option<&'a ProviderMetadata> {
    metadata.provider(provider)
}

fn provider_metadata_mut<'a>(
    metadata: &'a mut CacheMetadata,
    provider: &str,
) -> Option<&'a mut ProviderMetadata> {
    metadata.provider_mut(provider)
}

fn provider_metadata_or_insert<'a>(
    metadata: &'a mut CacheMetadata,
    provider: &str,
) -> &'a mut ProviderMetadata {
    metadata.providers.entry(provider.to_string()).or_default()
}

impl CacheManager {
    pub fn new(config: &CacheConfig) -> Result<Self> {
        // Create cache directory if it doesn't exist
        std::fs::create_dir_all(&config.dir).with_context(|| {
            format!("Failed to create cache directory: {}", config.dir.display())
        })?;

        // Load or create metadata
        let metadata_path = config.dir.join("metadata.json");
        let metadata = if metadata_path.exists() {
            let content = std::fs::read_to_string(&metadata_path)?;
            match serde_json::from_str(&content) {
                Ok(metadata) => metadata,
                Err(err) => {
                    tracing::warn!(
                        "Failed to parse {}: {}; starting with empty metadata",
                        metadata_path.display(),
                        err
                    );

                    // Best-effort backup for debugging.
                    let backup_path = metadata_path
                        .with_file_name(format!("metadata.json.bak-{}", Utc::now().timestamp()));
                    if let Err(e) = std::fs::rename(&metadata_path, &backup_path) {
                        tracing::warn!(
                            "Failed to backup invalid metadata.json to {}: {}",
                            backup_path.display(),
                            e
                        );
                    } else {
                        tracing::warn!(
                            "Backed up invalid metadata.json to {}",
                            backup_path.display()
                        );
                    }

                    CacheMetadata::default()
                }
            }
        } else {
            CacheMetadata::default()
        };

        Ok(Self {
            config: config.clone(),
            metadata: Arc::new(RwLock::new(metadata)),
        })
    }

    /// Get the base path for a provider
    pub fn provider_path(&self, provider: &str) -> PathBuf {
        self.config.dir.join(provider)
    }

    /// Get the path for a specific version
    pub fn version_path(&self, provider: &str, version: &str) -> PathBuf {
        self.provider_path(provider).join("versions").join(version)
    }

    /// Get the path for a binary
    pub fn binary_path(
        &self,
        provider: &str,
        version: &str,
        platform: &str,
        filename: &str,
    ) -> PathBuf {
        self.version_path(provider, version)
            .join(platform)
            .join(filename)
    }

    /// Get the path for a tag file
    pub fn tag_path(&self, provider: &str, tag: &str) -> PathBuf {
        self.provider_path(provider).join("tags").join(tag)
    }

    /// Read a tag to get version
    pub async fn read_tag(&self, provider: &str, tag: &str) -> Option<String> {
        if !is_safe_tag(tag) {
            return None;
        }
        let path = self.tag_path(provider, tag);
        tokio::fs::read_to_string(&path)
            .await
            .ok()
            .map(|s| s.trim().to_string())
    }

    /// Write a tag
    pub async fn write_tag(&self, provider: &str, tag: &str, version: &str) -> Result<()> {
        if !is_safe_tag(tag) {
            anyhow::bail!("Invalid tag name: {}", tag);
        }
        let path = self.tag_path(provider, tag);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, version).await?;
        Ok(())
    }

    /// Check if a binary exists
    pub async fn binary_exists(
        &self,
        provider: &str,
        version: &str,
        platform: &str,
        filename: &str,
    ) -> bool {
        self.binary_path(provider, version, platform, filename)
            .exists()
    }

    /// Read metadata
    pub async fn get_metadata(&self) -> CacheMetadata {
        self.metadata.read().await.clone()
    }

    pub async fn with_provider_metadata<R>(
        &self,
        provider: &str,
        f: impl FnOnce(&ProviderMetadata) -> R,
    ) -> Option<R> {
        let metadata = self.metadata.read().await;
        let provider_metadata = provider_metadata_ref(&metadata, provider)?;
        Some(f(provider_metadata))
    }

    /// Update metadata for a provider
    pub async fn update_provider_metadata<F>(&self, provider: &str, updater: F) -> Result<()>
    where
        F: FnOnce(&mut ProviderMetadata),
    {
        let content = {
            let mut metadata = self.metadata.write().await;
            let provider_metadata = provider_metadata_or_insert(&mut metadata, provider);

            updater(provider_metadata);

            serde_json::to_string_pretty(&*metadata)?
        };

        // Save to disk without holding the lock across await.
        let metadata_path = self.config.dir.join("metadata.json");
        write_file_atomic(&metadata_path, content.as_bytes()).await?;

        Ok(())
    }

    /// List all cached versions for a provider
    pub async fn list_versions(&self, provider: &str) -> Vec<String> {
        let metadata = self.metadata.read().await;
        provider_metadata_ref(&metadata, provider)
            .map(|p| p.versions.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Clean up old versions, keeping only max_versions
    pub async fn cleanup_old_versions(&self, provider: &str) -> Result<Vec<VersionMetadata>> {
        let max_versions = self.config.max_versions;

        let (versions_to_remove, removed_metadata) = {
            let metadata = self.metadata.read().await;
            let Some(provider_metadata) = provider_metadata_ref(&metadata, provider) else {
                return Ok(Vec::new());
            };

            // Get versions that are currently tagged (should not be deleted)
            let tagged_versions: std::collections::HashSet<_> =
                provider_metadata.tags.values().cloned().collect();

            // Collect versions to delete (oldest first, excluding tagged)
            let mut versions_with_time: Vec<(String, chrono::DateTime<Utc>)> = provider_metadata
                .versions
                .iter()
                .filter(|(v, _)| !tagged_versions.contains(*v))
                .map(|(v, m)| (v.clone(), m.downloaded_at))
                .collect();

            // Sort by download time (oldest first)
            versions_with_time.sort_by_key(|(_, dt)| *dt);

            // Calculate how many to delete
            let total_versions = provider_metadata.versions.len();
            let deletable = versions_with_time.len();
            let to_delete = if total_versions > max_versions {
                (total_versions - max_versions).min(deletable)
            } else {
                0
            };

            // Get version names to delete
            let versions_to_remove = versions_with_time
                .into_iter()
                .take(to_delete)
                .map(|(v, _)| v)
                .collect::<Vec<String>>();

            let removed_metadata = versions_to_remove
                .iter()
                .filter_map(|version| provider_metadata.versions.get(version).cloned())
                .collect::<Vec<VersionMetadata>>();

            (versions_to_remove, removed_metadata)
        };

        if versions_to_remove.is_empty() {
            return Ok(Vec::new());
        }

        for version in &versions_to_remove {
            let version_path = self.version_path(provider, version);
            if version_path.exists() {
                if let Err(e) = tokio::fs::remove_dir_all(&version_path).await {
                    tracing::warn!(
                        "Failed to delete version directory {}: {}",
                        version_path.display(),
                        e
                    );
                } else {
                    tracing::info!("Deleted old version: {}/{}", provider, version);
                }
            }
        }

        // Save metadata if we deleted anything
        let content = {
            let mut metadata = self.metadata.write().await;
            let Some(provider_metadata) = provider_metadata_mut(&mut metadata, provider) else {
                return Ok(Vec::new());
            };

            for version in &versions_to_remove {
                provider_metadata.versions.remove(version);
            }

            serde_json::to_string_pretty(&*metadata)?
        };

        let metadata_path = self.config.dir.join("metadata.json");
        write_file_atomic(&metadata_path, content.as_bytes()).await?;

        Ok(removed_metadata)
    }

    /// Build a safe local filesystem path under the provider directory.
    ///
    /// Note: this does *not* check whether the path exists. Callers that need an
    /// existence check should do it explicitly.
    pub fn get_file_path(&self, provider: &str, path_segments: &[&str]) -> Option<PathBuf> {
        let base = self.provider_path(provider);
        let mut result = base;
        for segment in path_segments {
            // Prevent path traversal
            if segment.contains("..") || segment.contains('/') || segment.contains('\\') {
                return None;
            }
            result = result.join(segment);
        }
        Some(result)
    }

    /// Build object key for storage backend (without checking existence)
    pub fn build_object_key(&self, provider: &str, path_segments: &[&str]) -> Option<String> {
        if provider.contains("..") || provider.contains('/') || provider.contains('\\') {
            return None;
        }
        let mut key = provider.to_string();
        for segment in path_segments {
            if segment.contains("..") || segment.contains('/') || segment.contains('\\') {
                return None;
            }
            key.push('/');
            key.push_str(segment);
        }
        Some(key)
    }
}

async fn write_file_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Path has no parent: {}", path.display()))?;
    tokio::fs::create_dir_all(parent).await?;

    let filename = path.file_name().and_then(|s| s.to_str()).unwrap_or("file");
    let tmp_path = parent.join(format!(
        ".{}.tmp-{}",
        filename,
        Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));

    tokio::fs::write(&tmp_path, bytes).await?;

    if let Err(err) = tokio::fs::rename(&tmp_path, path).await {
        // Windows doesn't allow rename-over-existing; retry with delete.
        tracing::debug!("rename failed for {}: {}", path.display(), err);
        let _ = tokio::fs::remove_file(path).await;
        tokio::fs::rename(&tmp_path, path).await?;
    }

    Ok(())
}

fn is_safe_tag(tag: &str) -> bool {
    if tag.is_empty() {
        return false;
    }
    if tag.contains("..") || tag.contains('/') || tag.contains('\\') {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_cache() -> (TempDir, CacheManager) {
        let temp_dir = TempDir::new().unwrap();
        let config = CacheConfig {
            dir: temp_dir.path().to_path_buf(),
            max_versions: 3,
        };
        let cache = CacheManager::new(&config).unwrap();
        (temp_dir, cache)
    }

    #[test]
    fn test_cache_manager_creation() {
        let (_temp_dir, cache) = create_test_cache();
        assert_eq!(cache.config.max_versions, 3);
    }

    #[test]
    fn test_provider_path() {
        let (temp_dir, cache) = create_test_cache();
        let path = cache.provider_path("tool-a");
        assert_eq!(path, temp_dir.path().join("tool-a"));
    }

    #[test]
    fn test_version_path() {
        let (temp_dir, cache) = create_test_cache();
        let path = cache.version_path("tool-a", "1.0.0");
        assert_eq!(
            path,
            temp_dir
                .path()
                .join("tool-a")
                .join("versions")
                .join("1.0.0")
        );
    }

    #[test]
    fn test_binary_path() {
        let (temp_dir, cache) = create_test_cache();
        let path = cache.binary_path("tool-a", "1.0.0", "darwin-arm64", "artifact.bin");
        assert_eq!(
            path,
            temp_dir
                .path()
                .join("tool-a")
                .join("versions")
                .join("1.0.0")
                .join("darwin-arm64")
                .join("artifact.bin")
        );
    }

    #[test]
    fn test_tag_path() {
        let (temp_dir, cache) = create_test_cache();
        let path = cache.tag_path("tool-a", "stable");
        assert_eq!(
            path,
            temp_dir.path().join("tool-a").join("tags").join("stable")
        );
    }

    #[test]
    fn test_get_file_path_prevents_traversal() {
        let (_temp_dir, cache) = create_test_cache();

        // Path traversal should return None
        assert!(cache.get_file_path("tool-a", &["../etc/passwd"]).is_none());
        assert!(
            cache
                .get_file_path("tool-a", &["foo", "..", "bar"])
                .is_none()
        );
        assert!(cache.get_file_path("tool-a", &["foo/bar"]).is_none());
    }

    #[test]
    fn test_get_file_path_returns_path_even_if_missing() {
        let (temp_dir, cache) = create_test_cache();

        let path = cache
            .get_file_path("tool-a", &["versions", "1.0.0", "manifest.json"])
            .unwrap();
        assert_eq!(
            path,
            temp_dir
                .path()
                .join("tool-a")
                .join("versions")
                .join("1.0.0")
                .join("manifest.json")
        );
    }

    #[tokio::test]
    async fn test_read_write_tag() {
        let (_temp_dir, cache) = create_test_cache();

        // Write tag
        cache.write_tag("tool-a", "stable", "1.0.0").await.unwrap();

        // Read tag
        let version = cache.read_tag("tool-a", "stable").await;
        assert_eq!(version, Some("1.0.0".to_string()));
    }

    #[tokio::test]
    async fn test_read_nonexistent_tag() {
        let (_temp_dir, cache) = create_test_cache();

        let version = cache.read_tag("tool-a", "nonexistent").await;
        assert!(version.is_none());
    }

    #[tokio::test]
    async fn test_update_provider_metadata_creates_provider_entry() {
        let (_temp_dir, cache) = create_test_cache();

        cache
            .update_provider_metadata("custom-provider", |m| {
                m.tags.insert("latest".to_string(), "1.2.3".to_string());
            })
            .await
            .unwrap();

        let value = cache
            .with_provider_metadata("custom-provider", |m| m.tags.get("latest").cloned())
            .await
            .flatten();
        assert_eq!(value.as_deref(), Some("1.2.3"));
    }
}

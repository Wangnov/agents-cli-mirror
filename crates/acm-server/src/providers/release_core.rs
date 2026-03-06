use anyhow::Result;
use chrono::Utc;
use std::future::Future;
use tracing::{info, warn};

use crate::cache::{CacheManager, VersionMetadata};
use crate::config::{ProviderUpdatePolicy, StorageMode};

pub struct SyncTagContext<'a> {
    pub cache: &'a CacheManager,
    pub enabled: bool,
    pub storage_mode: &'a StorageMode,
    pub provider_name: &'a str,
    pub tag: &'a str,
}

pub async fn get_tag_version_common(
    cache: &CacheManager,
    provider_name: &str,
    tag: &str,
) -> Option<String> {
    if tag == "stable" {
        if let Some(version) = cache.read_tag(provider_name, "stable").await {
            return Some(version);
        }
        return cache.read_tag(provider_name, "latest").await;
    }
    cache.read_tag(provider_name, tag).await
}

pub async fn sync_all_common<F, Fut>(
    update_policy: ProviderUpdatePolicy,
    allow_manual: bool,
    tags: &[String],
    mut sync_tag: F,
) -> Result<Vec<String>>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<Option<String>>>,
{
    if matches!(update_policy, ProviderUpdatePolicy::Manual) && !allow_manual {
        info!("Skip auto sync because update_policy=manual");
        return Ok(Vec::new());
    }

    let mut updated = Vec::new();
    let mut errors = Vec::new();

    for tag in tags {
        match sync_tag(tag.clone()).await {
            Ok(Some(version)) => updated.push(format!("{}: {}", tag, version)),
            Ok(None) => {}
            Err(err) => {
                warn!("Failed to sync tag {}: {:?}", tag, err);
                errors.push(format!("{}: {}", tag, err));
            }
        }
    }

    if errors.is_empty() {
        Ok(updated)
    } else {
        Err(anyhow::anyhow!(errors.join("; ")))
    }
}

pub async fn sync_tag_common<FFetch, FSync, FDelete, FutFetch, FutSync, FutDelete>(
    ctx: SyncTagContext<'_>,
    update_policy: ProviderUpdatePolicy,
    fetch_upstream_tag: FFetch,
    sync_version: FSync,
    delete_remote_versions: FDelete,
) -> Result<Option<String>>
where
    FFetch: FnOnce(String) -> FutFetch,
    FutFetch: Future<Output = Result<String>>,
    FSync: FnOnce(String) -> FutSync,
    FutSync: Future<Output = Result<()>>,
    FDelete: FnOnce(Vec<VersionMetadata>) -> FutDelete,
    FutDelete: Future<Output = ()>,
{
    if !ctx.enabled {
        return Ok(None);
    }

    let cached_version = ctx.cache.read_tag(ctx.provider_name, ctx.tag).await;
    let upstream_version = match update_policy {
        ProviderUpdatePolicy::Tracking | ProviderUpdatePolicy::Manual => {
            fetch_upstream_tag(ctx.tag.to_string()).await?
        }
        ProviderUpdatePolicy::Pinned => {
            if matches!(ctx.tag, "latest" | "stable") {
                warn!(
                    "Pinned update_policy expects explicit version tags, skip {}:{}",
                    ctx.provider_name, ctx.tag
                );
                return Ok(None);
            }
            ctx.tag.to_string()
        }
    };

    if cached_version.as_ref() == Some(&upstream_version) {
        info!("Tag {} is up to date: {}", ctx.tag, upstream_version);
        return Ok(None);
    }

    info!(
        "New version available for tag {}: {} -> {}",
        ctx.tag,
        cached_version.as_deref().unwrap_or("none"),
        upstream_version
    );

    sync_version(upstream_version.clone()).await?;

    ctx.cache
        .write_tag(ctx.provider_name, ctx.tag, &upstream_version)
        .await?;
    ctx.cache
        .update_provider_metadata(ctx.provider_name, |m| {
            m.tags.insert(ctx.tag.to_string(), upstream_version.clone());
            m.updated_at = Some(Utc::now());
        })
        .await?;

    let deleted = ctx.cache.cleanup_old_versions(ctx.provider_name).await?;
    if !deleted.is_empty() {
        info!("Cleaned up {} old versions", deleted.len());
        if matches!(ctx.storage_mode, StorageMode::S3) {
            delete_remote_versions(deleted).await;
        }
    }

    Ok(Some(upstream_version))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CacheConfig;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;

    fn create_test_cache() -> (TempDir, CacheManager) {
        let temp_dir = TempDir::new().expect("create temp dir");
        let config = CacheConfig {
            dir: temp_dir.path().to_path_buf(),
            max_versions: 3,
        };
        let cache = CacheManager::new(&config).expect("create cache manager");
        (temp_dir, cache)
    }

    #[tokio::test]
    async fn test_sync_all_manual_policy_skips_auto() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_closure = calls.clone();

        let tags = vec!["latest".to_string()];
        let updated = sync_all_common(ProviderUpdatePolicy::Manual, false, &tags, move |_| {
            let calls = calls_for_closure.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(None)
            }
        })
        .await
        .expect("sync_all_common should succeed");

        assert!(updated.is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn test_sync_tag_pinned_skips_alias_tags() {
        let (_dir, cache) = create_test_cache();
        let fetch_calls = Arc::new(AtomicUsize::new(0));
        let fetch_calls_for_closure = fetch_calls.clone();

        let result = sync_tag_common(
            SyncTagContext {
                cache: &cache,
                enabled: true,
                storage_mode: &StorageMode::Local,
                provider_name: "test-provider",
                tag: "latest",
            },
            ProviderUpdatePolicy::Pinned,
            move |_| {
                let fetch_calls = fetch_calls_for_closure.clone();
                async move {
                    fetch_calls.fetch_add(1, Ordering::SeqCst);
                    Ok("v1.2.3".to_string())
                }
            },
            |_| async { Ok(()) },
            |_| async {},
        )
        .await
        .expect("sync_tag_common should succeed");

        assert!(result.is_none());
        assert_eq!(fetch_calls.load(Ordering::SeqCst), 0);
    }
}

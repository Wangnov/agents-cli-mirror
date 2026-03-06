use anyhow::Result;
use std::path::Path;
use tracing::warn;

use crate::cache::CacheManager;
use crate::config::{StorageConfig, StorageMode};
use crate::error::MirrorError;
use crate::providers::github::{DownloadResult, GithubClient};
use crate::s3;
use crate::storage_clients::StorageClients;

pub async fn download_asset_bytes(github: &GithubClient, url: &str) -> Result<Vec<u8>> {
    github.download_asset_bytes(url).await
}

pub async fn download_asset_to_path(
    github: &GithubClient,
    url: &str,
    path: &Path,
) -> Result<DownloadResult> {
    github.download_asset_to_path(url, path).await
}

pub async fn download_asset_to_remote(
    github: &GithubClient,
    url: &str,
    storage: &StorageConfig,
    storage_clients: &StorageClients,
    object_key: &str,
) -> Result<DownloadResult> {
    github
        .download_asset_to_storage(
            url,
            storage,
            storage_clients,
            object_key,
            "application/octet-stream",
        )
        .await
}

pub async fn try_use_existing_s3_object(
    storage: &StorageConfig,
    storage_clients: &StorageClients,
    object_key: &str,
    expected_sha256: &str,
    expected_size: u64,
) -> Result<Option<DownloadResult>> {
    let Some(client) = storage_clients.s3() else {
        return Ok(None);
    };
    let info = s3::head_object_info_with_client(client, &storage.s3, object_key).await?;
    let Some(info) = info else {
        return Ok(None);
    };
    let Some(actual_sha256) = info.sha256 else {
        return Ok(None);
    };

    if !actual_sha256.eq_ignore_ascii_case(expected_sha256) {
        return Ok(None);
    }

    if expected_size > 0
        && let Some(size) = info.size
        && size != expected_size
    {
        warn!(
            "S3 object size mismatch for {}: expected {}, got {}",
            object_key, expected_size, size
        );
        return Ok(None);
    }

    Ok(Some(DownloadResult {
        size: info.size.unwrap_or(expected_size),
        sha256: expected_sha256.to_string(),
    }))
}

pub struct SidecarFile<'a> {
    pub provider_name: &'a str,
    pub version: &'a str,
    pub filename: &'a str,
    pub content_type: &'a str,
    pub bytes: &'a [u8],
}

pub async fn persist_sidecar_file(
    cache: &CacheManager,
    storage: &StorageConfig,
    storage_clients: &StorageClients,
    sidecar: SidecarFile<'_>,
) -> Result<()> {
    let key = cache
        .build_object_key(
            sidecar.provider_name,
            &["versions", sidecar.version, sidecar.filename],
        )
        .ok_or_else(|| MirrorError::VersionNotFound(sidecar.version.to_string()))?;

    match storage.mode {
        StorageMode::Local => {
            let path = cache
                .get_file_path(
                    sidecar.provider_name,
                    &["versions", sidecar.version, sidecar.filename],
                )
                .ok_or_else(|| MirrorError::VersionNotFound(sidecar.version.to_string()))?;
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&path, sidecar.bytes).await?;
        }
        StorageMode::S3 => {
            let Some(client) = storage_clients.s3() else {
                return Err(MirrorError::Provider("S3 client not initialized".to_string()).into());
            };
            s3::put_bytes_with_client(
                client,
                &storage.s3,
                &key,
                sidecar.content_type,
                sidecar.bytes.to_vec(),
            )
            .await?;
        }
    }

    Ok(())
}

use anyhow::{Context, Result};
use futures::StreamExt;
use reqwest::{Client, StatusCode, header};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::Path;
use tokio::io::AsyncWriteExt;

use crate::config::{HttpConfig, StorageConfig, StorageMode};
use crate::error::MirrorError;
use crate::http_client::build_http_client;
use crate::retry::{RetryPolicy, send_with_retry};
use crate::s3;
use crate::storage_clients::StorageClients;

pub const GITHUB_API_BASE: &str = "https://api.github.com";

#[derive(Debug, Deserialize, Clone)]
pub struct Release {
    pub tag_name: String,
    pub prerelease: bool,
    pub draft: bool,
    pub assets: Vec<Asset>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
    #[serde(default)]
    pub digest: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DownloadResult {
    pub size: u64,
    pub sha256: String,
}

pub struct GithubClient {
    client: Client,
    token: Option<String>,
}

impl GithubClient {
    pub fn new(http: &HttpConfig) -> Result<Self> {
        let client = build_http_client(http)?;
        let token = std::env::var("GITHUB_TOKEN").ok();
        Ok(Self { client, token })
    }

    fn api_request(&self, url: &str) -> reqwest::RequestBuilder {
        let mut req = self
            .client
            .get(url)
            .header(header::ACCEPT, "application/vnd.github+json");
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        req
    }

    pub async fn fetch_releases(&self, repo: &str) -> Result<Vec<Release>> {
        let url = format!("{}/repos/{}/releases", GITHUB_API_BASE, repo);
        let response = send_with_retry(|| self.api_request(&url), RetryPolicy::default())
            .await
            .with_context(|| format!("Failed to fetch releases from {}", url))?;

        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Err(MirrorError::VersionNotFound("releases".to_string()).into());
        }
        if !status.is_success() {
            return Err(
                MirrorError::Provider(format!("Failed to fetch releases: {}", status)).into(),
            );
        }

        Ok(response.json::<Vec<Release>>().await?)
    }

    pub async fn fetch_release_by_tag(&self, repo: &str, tag: &str) -> Result<Release> {
        let url = format!("{}/repos/{}/releases/tags/{}", GITHUB_API_BASE, repo, tag);
        let response = send_with_retry(|| self.api_request(&url), RetryPolicy::default())
            .await
            .with_context(|| format!("Failed to fetch release tag {}", tag))?;

        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Err(MirrorError::VersionNotFound(tag.to_string()).into());
        }
        if !status.is_success() {
            return Err(MirrorError::Provider(format!(
                "Failed to fetch release {}: {}",
                tag, status
            ))
            .into());
        }

        Ok(response.json::<Release>().await?)
    }

    pub async fn download_asset_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let url = url.to_string();
        let response = send_with_retry(|| self.client.get(&url), RetryPolicy::default())
            .await
            .with_context(|| format!("Failed to download asset {}", url))?;
        let status = response.status();
        if !status.is_success() {
            return Err(MirrorError::Provider(format!(
                "Failed to download asset {}: {}",
                url, status
            ))
            .into());
        }
        let bytes = response.bytes().await?;
        Ok(bytes.to_vec())
    }

    pub async fn download_asset_to_path(&self, url: &str, path: &Path) -> Result<DownloadResult> {
        let url = url.to_string();
        let response = send_with_retry(|| self.client.get(&url), RetryPolicy::default())
            .await
            .with_context(|| format!("Failed to download asset {}", url))?;

        let status = response.status();
        if !status.is_success() {
            return Err(MirrorError::Provider(format!(
                "Failed to download asset {}: {}",
                url, status
            ))
            .into());
        }

        let mut file = tokio::fs::File::create(path).await?;
        let mut hasher = Sha256::new();
        let mut size: u64 = 0;
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

    pub async fn download_asset_to_storage(
        &self,
        url: &str,
        storage: &StorageConfig,
        clients: &StorageClients,
        object_key: &str,
        content_type: &str,
    ) -> Result<DownloadResult> {
        let url = url.to_string();
        let response = send_with_retry(|| self.client.get(&url), RetryPolicy::default())
            .await
            .with_context(|| format!("Failed to download asset {}", url))?;

        let status = response.status();
        if !status.is_success() {
            return Err(MirrorError::Provider(format!(
                "Failed to download asset {}: {}",
                url, status
            ))
            .into());
        }

        let total_size = response.content_length();
        let (size, sha256) = match storage.mode {
            StorageMode::S3 => {
                let Some(client) = clients.s3() else {
                    return Err(
                        MirrorError::Provider("S3 client not initialized".to_string()).into(),
                    );
                };
                let upload = s3::upload_stream_with_client(
                    client,
                    &storage.s3,
                    object_key,
                    content_type,
                    total_size,
                    response.bytes_stream(),
                )
                .await?;
                (upload.size, upload.sha256)
            }
            StorageMode::Local => {
                return Err(MirrorError::Provider(
                    "download_asset_to_storage called in local mode".to_string(),
                )
                .into());
            }
        };

        Ok(DownloadResult { size, sha256 })
    }
}

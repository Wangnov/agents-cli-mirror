use anyhow::{Context, Result};
use aws_config::{BehaviorVersion, Region};
use aws_credential_types::Credentials;
use aws_credential_types::provider::SharedCredentialsProvider;
use aws_sdk_s3::config::StalledStreamProtectionConfig;
use aws_sdk_s3::error::SdkError;
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use bytes::Bytes;
use futures::{Stream, StreamExt};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tracing::{debug, info, warn};

use crate::config::S3Config;
use crate::progress::{format_bytes, format_rate, progress_log_interval};
use crate::storage_clients::S3Client;

const MULTIPART_THRESHOLD: u64 = 16 * 1024 * 1024;
const MIN_PART_SIZE: u64 = 5 * 1024 * 1024;
const MAX_S3_PARTS: usize = 10_000;

pub struct UploadResult {
    pub size: u64,
    pub sha256: String,
}

pub struct HeadObjectInfo {
    pub size: Option<u64>,
    pub sha256: Option<String>,
}

#[derive(Clone)]
struct UploadPartInfo {
    part_number: i32,
    etag: String,
}

#[derive(Copy, Clone)]
enum ETagPolicy {
    Quoted,
    Unquoted,
}

pub async fn presign_get_url(config: &S3Config, object_key: &str) -> Result<String> {
    let client = build_client(config).await?;
    presign_get_url_with_client(&client, config, object_key).await
}

pub async fn presign_get_url_with_client(
    client: &S3Client,
    config: &S3Config,
    object_key: &str,
) -> Result<String> {
    let key = object_key_with_prefix(config, object_key);
    let presign = PresigningConfig::expires_in(Duration::from_secs(config.expires_seconds))
        .context("Failed to create presign config")?;
    let presigned = client
        .get_object()
        .bucket(&config.bucket)
        .key(key)
        .presigned(presign)
        .await
        .context("Failed to presign S3 URL")?;
    Ok(presigned.uri().to_string())
}

pub async fn put_bytes(
    config: &S3Config,
    object_key: &str,
    content_type: &str,
    body: Vec<u8>,
) -> Result<()> {
    let client = build_client(config).await?;
    put_bytes_with_client(&client, config, object_key, content_type, body).await
}

pub async fn put_bytes_with_client(
    client: &S3Client,
    config: &S3Config,
    object_key: &str,
    content_type: &str,
    body: Vec<u8>,
) -> Result<()> {
    let key = object_key_with_prefix(config, object_key);
    let stream = ByteStream::from(body);
    client
        .put_object()
        .bucket(&config.bucket)
        .key(key)
        .content_type(content_type)
        .body(stream)
        .send()
        .await
        .context("Failed to upload object to S3")?;
    Ok(())
}

#[allow(dead_code)]
pub async fn get_object_bytes(config: &S3Config, object_key: &str) -> Result<Bytes> {
    let client = build_client(config).await?;
    get_object_bytes_with_client(&client, config, object_key).await
}

pub async fn get_object_bytes_with_client(
    client: &S3Client,
    config: &S3Config,
    object_key: &str,
) -> Result<Bytes> {
    let key = object_key_with_prefix(config, object_key);
    let response = client
        .get_object()
        .bucket(&config.bucket)
        .key(key)
        .send()
        .await
        .context("Failed to fetch object from S3")?;
    let data = response
        .body
        .collect()
        .await
        .context("Failed to read S3 object body")?;
    Ok(data.into_bytes())
}

pub async fn head_object_info(
    config: &S3Config,
    object_key: &str,
) -> Result<Option<HeadObjectInfo>> {
    let client = build_client(config).await?;
    head_object_info_with_client(&client, config, object_key).await
}

pub async fn head_object_info_with_client(
    client: &S3Client,
    config: &S3Config,
    object_key: &str,
) -> Result<Option<HeadObjectInfo>> {
    let key = object_key_with_prefix(config, object_key);
    let result = client
        .head_object()
        .bucket(&config.bucket)
        .key(key)
        .send()
        .await;

    match result {
        Ok(output) => {
            let size = output
                .content_length()
                .and_then(|value| if value >= 0 { Some(value as u64) } else { None });
            let sha256 = output.metadata().and_then(extract_sha256_meta);
            Ok(Some(HeadObjectInfo { size, sha256 }))
        }
        Err(err) => {
            if let SdkError::ServiceError(service_err) = &err {
                if service_err.err().is_not_found() {
                    return Ok(None);
                }
            }
            Err(err.into())
        }
    }
}

pub async fn upload_stream<S>(
    config: &S3Config,
    object_key: &str,
    content_type: &str,
    total_size: Option<u64>,
    stream: S,
) -> Result<UploadResult>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Send + Unpin + 'static,
{
    let client = build_client(config).await?;
    upload_stream_with_client(
        &client,
        config,
        object_key,
        content_type,
        total_size,
        stream,
    )
    .await
}

pub async fn upload_stream_with_client<S>(
    client: &S3Client,
    config: &S3Config,
    object_key: &str,
    content_type: &str,
    total_size: Option<u64>,
    stream: S,
) -> Result<UploadResult>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Send + Unpin + 'static,
{
    let log_every_bytes = progress_log_interval(total_size);
    let total_hint = total_size
        .filter(|t| *t > 0)
        .map(|t| format!(" (total: {})", format_bytes(t)))
        .unwrap_or_default();
    info!(
        "Downloading upstream stream for S3 upload {}{}",
        object_key, total_hint
    );
    let temp_path = temp_path_for(object_key)?;
    let mut file = tokio::fs::File::create(&temp_path).await?;
    let mut hasher = Sha256::new();
    let mut size: u64 = 0;
    let mut last_logged: u64 = 0;
    let download_started = Instant::now();

    let download_result = async {
        let mut upstream = Box::pin(stream);
        while let Some(chunk) = upstream.next().await {
            let chunk = chunk?;
            size += chunk.len() as u64;
            hasher.update(&chunk);
            file.write_all(&chunk).await?;
            if size.saturating_sub(last_logged) >= log_every_bytes {
                let speed = format_rate(size, download_started.elapsed());
                if let Some(total) = total_size.filter(|t| *t > 0) {
                    let pct = (size as f64 / total as f64) * 100.0;
                    debug!(
                        "Downloaded {}/{} ({:.1}%), {} for S3 object {}",
                        format_bytes(size),
                        format_bytes(total),
                        pct.min(100.0),
                        speed,
                        object_key
                    );
                } else {
                    debug!(
                        "Downloaded {} ({}) for S3 object {}",
                        format_bytes(size),
                        speed,
                        object_key
                    );
                }
                last_logged = size;
            }
        }
        file.flush().await?;
        Ok::<_, anyhow::Error>(UploadResult {
            size,
            sha256: hex::encode(hasher.finalize()),
        })
    }
    .await;

    let download_result = match download_result {
        Ok(result) => result,
        Err(err) => {
            drop(file);
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(err);
        }
    };

    info!(
        "Downloaded upstream to temp file for S3 object {} ({} bytes, sha256: {})",
        object_key, download_result.size, download_result.sha256
    );

    drop(file);

    info!("Uploading to S3 object {}", object_key);
    let upload_started = Instant::now();
    let upload_result = match upload_file_with_client(
        client,
        config,
        object_key,
        content_type,
        &temp_path,
        Some(&download_result.sha256),
    )
    .await
    {
        Ok(()) => download_result,
        Err(err) => {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(err);
        }
    };

    let _ = tokio::fs::remove_file(&temp_path).await;
    info!(
        "Uploaded S3 object {} ({} bytes) in {:?}",
        object_key,
        upload_result.size,
        upload_started.elapsed()
    );
    Ok(upload_result)
}

pub async fn delete_object(config: &S3Config, object_key: &str) -> Result<()> {
    let client = build_client(config).await?;
    delete_object_with_client(&client, config, object_key).await
}

pub async fn delete_object_with_client(
    client: &S3Client,
    config: &S3Config,
    object_key: &str,
) -> Result<()> {
    let key = object_key_with_prefix(config, object_key);
    client
        .delete_object()
        .bucket(&config.bucket)
        .key(key)
        .send()
        .await
        .context("Failed to delete S3 object")?;
    Ok(())
}

pub async fn upload_file(
    config: &S3Config,
    object_key: &str,
    content_type: &str,
    path: &PathBuf,
    sha256: Option<&str>,
) -> Result<()> {
    let client = build_client(config).await?;
    upload_file_with_client(&client, config, object_key, content_type, path, sha256).await
}

pub async fn upload_file_with_client(
    client: &S3Client,
    config: &S3Config,
    object_key: &str,
    content_type: &str,
    path: &PathBuf,
    sha256: Option<&str>,
) -> Result<()> {
    let key = object_key_with_prefix(config, object_key);
    let total_size = tokio::fs::metadata(path)
        .await
        .context("Failed to stat upload file")?
        .len();

    let metadata = build_metadata(sha256);

    if total_size <= MULTIPART_THRESHOLD {
        let body = ByteStream::from_path(path)
            .await
            .context("Failed to open upload file")?;
        let mut request = client
            .put_object()
            .bucket(&config.bucket)
            .key(key)
            .content_type(content_type)
            .body(body);
        if let Some(meta) = metadata.clone() {
            request = request.set_metadata(Some(meta));
        }
        request
            .send()
            .await
            .context("Failed to upload file to S3")?;
        return Ok(());
    }

    let max_parts = multipart_max_parts(config);
    let part_size = multipart_part_size(total_size, max_parts);
    let part_attempts = multipart_part_max_attempts(config);
    info!(
        "Starting multipart upload to S3 object {} (size: {}, part_size: {}, max_parts: {}, part_attempts: {})",
        object_key,
        format_bytes(total_size),
        format_bytes(part_size as u64),
        max_parts,
        part_attempts
    );

    let mut create_request = client
        .create_multipart_upload()
        .bucket(&config.bucket)
        .key(&key)
        .content_type(content_type);
    if let Some(meta) = metadata {
        create_request = create_request.set_metadata(Some(meta));
    }
    let create_output = create_request
        .send()
        .await
        .context("Failed to initiate multipart upload")?;

    let upload_id = create_output
        .upload_id()
        .context("Missing upload id from multipart upload")?
        .to_string();

    let mut file = tokio::fs::File::open(path)
        .await
        .context("Failed to open upload file")?;
    let mut buffer = vec![0u8; part_size];
    let mut part_number: i32 = 1;
    let mut completed_parts: Vec<UploadPartInfo> = Vec::new();
    let mut uploaded: u64 = 0;
    let upload_started = Instant::now();

    let upload_result = async {
        loop {
            let mut filled = 0usize;
            while filled < part_size {
                let read = file.read(&mut buffer[filled..]).await?;
                if read == 0 {
                    break;
                }
                filled += read;
            }

            if filled == 0 {
                break;
            }

            let bytes = Bytes::copy_from_slice(&buffer[..filled]);
            let etag = upload_part_with_retry(
                client,
                &config.bucket,
                &key,
                &upload_id,
                part_number,
                bytes,
                part_attempts,
            )
            .await
            .with_context(|| format!("Failed to upload part {}", part_number))?;
            completed_parts.push(UploadPartInfo { part_number, etag });

            uploaded += filled as u64;
            let speed = format_rate(uploaded, upload_started.elapsed());
            let pct = (uploaded as f64 / total_size as f64) * 100.0;
            debug!(
                "Uploaded {}/{} ({:.1}%), {} for S3 object {}",
                format_bytes(uploaded),
                format_bytes(total_size),
                pct.min(100.0),
                speed,
                object_key
            );

            part_number += 1;
        }

        let complete_result = complete_multipart_upload_with_policy(
            client,
            &config.bucket,
            &key,
            &upload_id,
            &completed_parts,
            ETagPolicy::Quoted,
        )
        .await;

        if let Err(err) = complete_result {
            warn!(
                "Multipart upload completion failed (quoted ETags) for {}: {:#}",
                object_key, err
            );

            let retry_needed = completed_parts.iter().any(|part| {
                etag_for_policy(&part.etag, ETagPolicy::Quoted)
                    != etag_for_policy(&part.etag, ETagPolicy::Unquoted)
            });

            if retry_needed {
                let retry = complete_multipart_upload_with_policy(
                    client,
                    &config.bucket,
                    &key,
                    &upload_id,
                    &completed_parts,
                    ETagPolicy::Unquoted,
                )
                .await;

                if let Err(retry_err) = retry {
                    warn!(
                        "Multipart upload completion failed (unquoted ETags) for {}: {:#}",
                        object_key, retry_err
                    );
                    return Err(retry_err);
                } else {
                    info!(
                        "Multipart upload completed with unquoted ETags for {}",
                        object_key
                    );
                }
            } else {
                return Err(err);
            }
        }

        Ok::<(), anyhow::Error>(())
    }
    .await;

    if let Err(err) = upload_result {
        warn!("Multipart upload failed for {}: {:#}", object_key, err);
        let _ = client
            .abort_multipart_upload()
            .bucket(&config.bucket)
            .key(&key)
            .upload_id(&upload_id)
            .send()
            .await;
        return Err(err);
    }

    Ok(())
}

fn multipart_max_parts(config: &S3Config) -> usize {
    let max_parts = config.multipart_max_parts.max(1);
    max_parts.min(MAX_S3_PARTS)
}

fn multipart_part_max_attempts(config: &S3Config) -> usize {
    config.multipart_part_max_attempts.max(1)
}

fn multipart_part_size(total_size: u64, max_parts: usize) -> usize {
    let max_parts = max_parts.max(1) as u64;
    let mut part_size = total_size.div_ceil(max_parts);
    if part_size < MIN_PART_SIZE {
        part_size = MIN_PART_SIZE;
    }
    part_size as usize
}

fn multipart_retry_delay(attempt: usize) -> Duration {
    let attempt = attempt.clamp(1, 6) as u32;
    let delay_ms = 500u64.saturating_mul(1u64 << (attempt - 1));
    Duration::from_millis(delay_ms.min(5_000))
}

async fn upload_part_with_retry(
    client: &S3Client,
    bucket: &str,
    key: &str,
    upload_id: &str,
    part_number: i32,
    bytes: Bytes,
    max_attempts: usize,
) -> Result<String> {
    let max_attempts = max_attempts.max(1);
    let mut attempt = 0usize;

    loop {
        attempt += 1;
        let result = client
            .upload_part()
            .bucket(bucket)
            .key(key)
            .upload_id(upload_id)
            .part_number(part_number)
            .content_length(bytes.len() as i64)
            .body(ByteStream::from(bytes.clone()))
            .send()
            .await;

        match result {
            Ok(resp) => {
                let etag = resp
                    .e_tag()
                    .map(|s| s.to_string())
                    .context("Missing ETag from upload_part")?;
                return Ok(etag);
            }
            Err(err) => {
                if attempt >= max_attempts {
                    return Err(err.into());
                }
                let delay = multipart_retry_delay(attempt);
                warn!(
                    "Upload part {} failed (attempt {}/{}), retrying in {:?}: {:#}",
                    part_number, attempt, max_attempts, delay, err
                );
                tokio::time::sleep(delay).await;
            }
        }
    }
}

async fn complete_multipart_upload_with_policy(
    client: &S3Client,
    bucket: &str,
    key: &str,
    upload_id: &str,
    parts: &[UploadPartInfo],
    policy: ETagPolicy,
) -> Result<()> {
    let completed = CompletedMultipartUpload::builder()
        .set_parts(Some(build_completed_parts(parts, policy)))
        .build();

    client
        .complete_multipart_upload()
        .bucket(bucket)
        .key(key)
        .upload_id(upload_id)
        .multipart_upload(completed)
        .send()
        .await
        .context("Failed to complete multipart upload")?;

    Ok(())
}

fn build_completed_parts(parts: &[UploadPartInfo], policy: ETagPolicy) -> Vec<CompletedPart> {
    let mut items: Vec<&UploadPartInfo> = parts.iter().collect();
    items.sort_by_key(|part| part.part_number);
    items
        .into_iter()
        .map(|part| {
            CompletedPart::builder()
                .part_number(part.part_number)
                .e_tag(etag_for_policy(&part.etag, policy))
                .build()
        })
        .collect()
}

fn etag_for_policy(etag: &str, policy: ETagPolicy) -> String {
    let trimmed = etag.trim();
    if trimmed.is_empty() {
        return trimmed.to_string();
    }
    match policy {
        ETagPolicy::Quoted => {
            if (trimmed.starts_with('"') && trimmed.ends_with('"'))
                || (trimmed.starts_with("W/\"") && trimmed.ends_with('"'))
            {
                trimmed.to_string()
            } else {
                format!("\"{}\"", trimmed)
            }
        }
        ETagPolicy::Unquoted => {
            if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
                trimmed[1..trimmed.len() - 1].to_string()
            } else {
                trimmed.to_string()
            }
        }
    }
}

fn build_metadata(sha256: Option<&str>) -> Option<HashMap<String, String>> {
    let sha256 = sha256.map(str::trim).filter(|value| !value.is_empty())?;
    let mut meta = HashMap::new();
    meta.insert("sha256".to_string(), sha256.to_string());
    Some(meta)
}

fn extract_sha256_meta(meta: &HashMap<String, String>) -> Option<String> {
    meta.iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("sha256"))
        .map(|(_, value)| value.clone())
}

fn validate_config(config: &S3Config) -> Result<()> {
    if config.endpoint.is_empty() {
        anyhow::bail!("S3 endpoint is not configured");
    }
    if config.bucket.is_empty() {
        anyhow::bail!("S3 bucket is not configured");
    }
    if config.access_key_id.is_empty() {
        anyhow::bail!("S3 access_key_id is not configured");
    }
    if config.secret_access_key.is_empty() {
        anyhow::bail!("S3 secret_access_key is not configured");
    }
    if config.region.is_empty() {
        anyhow::bail!("S3 region is not configured");
    }
    Ok(())
}

fn join_prefix(prefix: &str, key: &str) -> String {
    let prefix = prefix.trim_matches('/');
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{}/{}", prefix, key)
    }
}

fn temp_path_for(object_key: &str) -> Result<PathBuf> {
    let mut name = object_key.replace('/', "_");
    if name.len() > 80 {
        name = name[name.len() - 80..].to_string();
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("Failed to get time for temp path")?
        .as_nanos();
    Ok(std::env::temp_dir().join(format!("dc-mirror-s3-{}-{}.tmp", name, ts)))
}

fn object_key_with_prefix(config: &S3Config, object_key: &str) -> String {
    join_prefix(&config.prefix, object_key)
}

fn normalize_endpoint(endpoint: &str) -> String {
    if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        endpoint.to_string()
    } else {
        format!("https://{}", endpoint)
    }
}

async fn s3_client(config: &S3Config) -> Result<S3Client> {
    validate_config(config)?;

    let credentials = Credentials::new(
        config.access_key_id.clone(),
        config.secret_access_key.clone(),
        config.session_token.clone(),
        None,
        "static",
    );

    let shared_config = aws_config::defaults(BehaviorVersion::latest())
        .credentials_provider(SharedCredentialsProvider::new(credentials))
        .region(Region::new(config.region.clone()))
        .load()
        .await;

    let mut builder = aws_sdk_s3::config::Builder::from(&shared_config)
        .endpoint_url(normalize_endpoint(&config.endpoint))
        .stalled_stream_protection(StalledStreamProtectionConfig::disabled());
    if config.path_style {
        builder = builder.force_path_style(true);
    }

    Ok(S3Client::from_conf(builder.build()))
}

pub async fn build_client(config: &S3Config) -> Result<S3Client> {
    s3_client(config).await
}

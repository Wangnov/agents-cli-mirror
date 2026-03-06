use anyhow::{Result, bail};
use bytes::Bytes;
use futures::Stream;
use std::path::PathBuf;

use crate::config::S3Config;
use crate::storage_clients::S3Client;

pub struct UploadResult {
    pub size: u64,
    pub sha256: String,
}

pub struct HeadObjectInfo {
    pub size: Option<u64>,
    pub sha256: Option<String>,
}

fn unsupported() -> anyhow::Error {
    anyhow::anyhow!("S3 support is not enabled in this build; enable feature `s3`")
}

pub async fn presign_get_url(_config: &S3Config, _object_key: &str) -> Result<String> {
    Err(unsupported())
}

pub async fn presign_get_url_with_client(
    _client: &S3Client,
    _config: &S3Config,
    _object_key: &str,
) -> Result<String> {
    Err(unsupported())
}

pub async fn put_bytes(
    _config: &S3Config,
    _object_key: &str,
    _content_type: &str,
    _body: Vec<u8>,
) -> Result<()> {
    Err(unsupported())
}

pub async fn put_bytes_with_client(
    _client: &S3Client,
    _config: &S3Config,
    _object_key: &str,
    _content_type: &str,
    _body: Vec<u8>,
) -> Result<()> {
    Err(unsupported())
}

#[allow(dead_code)]
pub async fn get_object_bytes(_config: &S3Config, _object_key: &str) -> Result<Bytes> {
    Err(unsupported())
}

pub async fn get_object_bytes_with_client(
    _client: &S3Client,
    _config: &S3Config,
    _object_key: &str,
) -> Result<Bytes> {
    Err(unsupported())
}

pub async fn head_object_info(
    _config: &S3Config,
    _object_key: &str,
) -> Result<Option<HeadObjectInfo>> {
    Err(unsupported())
}

pub async fn head_object_info_with_client(
    _client: &S3Client,
    _config: &S3Config,
    _object_key: &str,
) -> Result<Option<HeadObjectInfo>> {
    Err(unsupported())
}

pub async fn upload_stream<S>(
    _config: &S3Config,
    _object_key: &str,
    _content_type: &str,
    _total_size: Option<u64>,
    _stream: S,
) -> Result<UploadResult>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Send + Unpin + 'static,
{
    Err(unsupported())
}

pub async fn upload_stream_with_client<S>(
    _client: &S3Client,
    _config: &S3Config,
    _object_key: &str,
    _content_type: &str,
    _total_size: Option<u64>,
    _stream: S,
) -> Result<UploadResult>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Send + Unpin + 'static,
{
    Err(unsupported())
}

pub async fn delete_object(_config: &S3Config, _object_key: &str) -> Result<()> {
    Err(unsupported())
}

pub async fn delete_object_with_client(
    _client: &S3Client,
    _config: &S3Config,
    _object_key: &str,
) -> Result<()> {
    Err(unsupported())
}

pub async fn upload_file(
    _config: &S3Config,
    _object_key: &str,
    _content_type: &str,
    _path: &PathBuf,
    _sha256: Option<&str>,
) -> Result<()> {
    Err(unsupported())
}

pub async fn upload_file_with_client(
    _client: &S3Client,
    _config: &S3Config,
    _object_key: &str,
    _content_type: &str,
    _path: &PathBuf,
    _sha256: Option<&str>,
) -> Result<()> {
    Err(unsupported())
}

pub async fn build_client(_config: &S3Config) -> Result<S3Client> {
    bail!(unsupported())
}

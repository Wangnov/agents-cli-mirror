use anyhow::Result;
use axum::{
    body::Body,
    extract::Request,
    http::{HeaderValue, StatusCode, header},
    response::Response,
};
use tower::ServiceExt;
use tower_http::services::ServeFile;
use tracing::error;

use crate::config::StorageMode;
use crate::s3;

use super::AppState;

pub(super) async fn serve_storage_file(
    state: &AppState,
    req: Request,
    provider: &str,
    path_segments: &[&str],
    content_type: &'static str,
    filename: Option<&str>,
) -> Result<Response, StatusCode> {
    let key = state
        .cache
        .build_object_key(provider, path_segments)
        .ok_or(StatusCode::NOT_FOUND)?;

    match state.config.storage.mode {
        StorageMode::Local => serve_local_file(state, req, &key, content_type, filename).await,
        StorageMode::S3 => {
            drop(req);
            serve_s3_redirect(state, &key).await
        }
    }
}

async fn serve_local_file(
    state: &AppState,
    req: Request,
    key: &str,
    content_type: &'static str,
    filename: Option<&str>,
) -> Result<Response, StatusCode> {
    let path = state.cache.config.dir.join(key);
    let response = ServeFile::new(path).oneshot(req).await.map_err(|err| {
        error!("Failed to serve local file: {}", err);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let (mut parts, body) = response.into_parts();

    parts
        .headers
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));

    if let Some(name) = filename {
        let header_value = super::content_disposition_header_value(name)?;
        parts
            .headers
            .insert(header::CONTENT_DISPOSITION, header_value);
    }

    Ok(Response::from_parts(parts, Body::new(body)))
}

async fn serve_s3_redirect(state: &AppState, key: &str) -> Result<Response, StatusCode> {
    let client = state.storage_clients.s3().ok_or_else(|| {
        error!("Storage mode is S3 but S3 client is not initialized");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let url = s3::presign_get_url_with_client(client, &state.config.storage.s3, key)
        .await
        .map_err(|e| {
            error!("Failed to presign S3 URL: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let location = HeaderValue::from_str(&url).map_err(|e| {
        error!("Failed to build S3 Location header: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, location)
        .body(Body::empty())
        .map_err(|err| {
            error!("Failed to build S3 redirect response: {}", err);
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

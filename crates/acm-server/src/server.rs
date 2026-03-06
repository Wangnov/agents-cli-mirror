use anyhow::Result;
use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use std::future::Future;
use std::sync::Arc;
use tokio::time::{Duration, Instant, interval};
use tracing::{error, info, warn};

use crate::config::Config;

mod routes;
pub(crate) mod scripts;
mod state;
mod storage;

const INSTALLER_BINARY_NAME: &str = "acm-installer";

pub use state::{AppState, ProviderInstance, build_state};

pub async fn sync_once(config: Config, cache: crate::cache::CacheManager) -> Result<()> {
    let cache = Arc::new(cache);
    let state = state::build_state(config, cache).await?;
    sync_all_locked(state.as_ref()).await?;
    Ok(())
}

pub async fn sync_provider_once(
    config: Config,
    cache: crate::cache::CacheManager,
    provider: &str,
) -> Result<()> {
    let cache = Arc::new(cache);
    let state = state::build_state(config, cache).await?;
    let _ = provider_refresh_locked(state.as_ref(), provider).await?;
    Ok(())
}

pub async fn run(
    mut config: Config,
    cache: crate::cache::CacheManager,
    skip_initial_sync: bool,
) -> Result<()> {
    if let Some(public_url) = config.server.public_url.clone() {
        config.server.public_url = Some(crate::config::normalize_public_url(&public_url)?);
    }

    let cache = Arc::new(cache);
    let state = state::build_state(config.clone(), cache.clone()).await?;

    if !skip_initial_sync {
        info!("Performing initial cache sync...");
        if let Err(e) = sync_all_locked(state.as_ref()).await {
            error!("Initial sync failed: {}", e);
        }
    }

    if config.update.enabled {
        let update_state = state.clone();
        let interval_minutes = config.update.interval_minutes;
        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(interval_minutes * 60));
            interval.tick().await;
            loop {
                interval.tick().await;
                info!("Running scheduled cache update...");
                if let Err(e) = sync_all_locked(update_state.as_ref()).await {
                    error!("Scheduled sync failed: {}", e);
                }
            }
        });
    }

    if config.server.public_url.is_none() {
        warn!("server.public_url is not set; install scripts will return 503");
    }

    let app = build_router(state);

    let addr = format!("{}:{}", config.server.host, config.server.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("Server listening on {}", addr);

    axum::serve(listener, app).await?;

    Ok(())
}

pub fn build_router(state: Arc<AppState>) -> Router {
    routes::build_router(state)
}

async fn health_check() -> &'static str {
    "OK"
}

async fn provider_tag(
    State(state): State<Arc<AppState>>,
    Path((provider, tag)): Path<(String, String)>,
) -> Result<String, StatusCode> {
    let provider = state
        .providers
        .get(&provider)
        .cloned()
        .ok_or(StatusCode::NOT_FOUND)?;
    let value = provider.get_tag_version(tag).await;
    value.ok_or(StatusCode::NOT_FOUND)
}

async fn provider_generic_file(
    State(state): State<Arc<AppState>>,
    Path((provider, version, filepath)): Path<(String, String, String)>,
    req: Request,
) -> Result<Response, StatusCode> {
    if !state.providers.contains_key(&provider) {
        return Err(StatusCode::NOT_FOUND);
    }

    let allowed = state
        .cache
        .with_provider_metadata(&provider, |metadata| {
            metadata.versions.get(&version).is_some_and(|version_meta| {
                version_meta.platforms.values().any(|platform_meta| {
                    if platform_meta.files.is_empty() {
                        platform_meta.filename == filepath
                    } else {
                        platform_meta.files.contains_key(&filepath)
                    }
                })
            })
        })
        .await
        .unwrap_or(false);
    if !allowed {
        return Err(StatusCode::NOT_FOUND);
    }

    let mut segments = vec!["versions".to_string(), version, "files".to_string()];
    segments.extend(validated_file_segments(&filepath)?);
    let refs = segments.iter().map(String::as_str).collect::<Vec<_>>();
    let filename = filepath.rsplit('/').next().unwrap_or("download");

    storage::serve_storage_file(
        state.as_ref(),
        req,
        &provider,
        &refs,
        "application/octet-stream",
        Some(filename),
    )
    .await
}

async fn script_install(
    State(state): State<Arc<AppState>>,
    Path(provider): Path<String>,
    headers: HeaderMap,
) -> Response {
    command_script(
        state,
        Some(provider),
        scripts::ScriptCommand::Install,
        headers,
    )
    .await
}

async fn script_install_sh(
    State(state): State<Arc<AppState>>,
    Path(provider): Path<String>,
) -> Response {
    fixed_command_script(
        state,
        Some(provider),
        scripts::ScriptCommand::Install,
        scripts::ScriptFlavor::Sh,
    )
    .await
}

async fn script_install_ps1(
    State(state): State<Arc<AppState>>,
    Path(provider): Path<String>,
) -> Response {
    fixed_command_script(
        state,
        Some(provider),
        scripts::ScriptCommand::Install,
        scripts::ScriptFlavor::Ps1,
    )
    .await
}

async fn script_update(
    State(state): State<Arc<AppState>>,
    Path(provider): Path<String>,
    headers: HeaderMap,
) -> Response {
    command_script(
        state,
        Some(provider),
        scripts::ScriptCommand::Update,
        headers,
    )
    .await
}

async fn script_update_sh(
    State(state): State<Arc<AppState>>,
    Path(provider): Path<String>,
) -> Response {
    fixed_command_script(
        state,
        Some(provider),
        scripts::ScriptCommand::Update,
        scripts::ScriptFlavor::Sh,
    )
    .await
}

async fn script_update_ps1(
    State(state): State<Arc<AppState>>,
    Path(provider): Path<String>,
) -> Response {
    fixed_command_script(
        state,
        Some(provider),
        scripts::ScriptCommand::Update,
        scripts::ScriptFlavor::Ps1,
    )
    .await
}

async fn script_uninstall(
    State(state): State<Arc<AppState>>,
    Path(provider): Path<String>,
    headers: HeaderMap,
) -> Response {
    command_script(
        state,
        Some(provider),
        scripts::ScriptCommand::Uninstall,
        headers,
    )
    .await
}

async fn script_uninstall_sh(
    State(state): State<Arc<AppState>>,
    Path(provider): Path<String>,
) -> Response {
    fixed_command_script(
        state,
        Some(provider),
        scripts::ScriptCommand::Uninstall,
        scripts::ScriptFlavor::Sh,
    )
    .await
}

async fn script_uninstall_ps1(
    State(state): State<Arc<AppState>>,
    Path(provider): Path<String>,
) -> Response {
    fixed_command_script(
        state,
        Some(provider),
        scripts::ScriptCommand::Uninstall,
        scripts::ScriptFlavor::Ps1,
    )
    .await
}

async fn script_status(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    command_script(state, None, scripts::ScriptCommand::Status, headers).await
}

async fn script_status_sh(State(state): State<Arc<AppState>>) -> Response {
    fixed_command_script(
        state,
        None,
        scripts::ScriptCommand::Status,
        scripts::ScriptFlavor::Sh,
    )
    .await
}

async fn script_status_ps1(State(state): State<Arc<AppState>>) -> Response {
    fixed_command_script(
        state,
        None,
        scripts::ScriptCommand::Status,
        scripts::ScriptFlavor::Ps1,
    )
    .await
}

async fn script_doctor(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    command_script(state, None, scripts::ScriptCommand::Doctor, headers).await
}

async fn script_doctor_sh(State(state): State<Arc<AppState>>) -> Response {
    fixed_command_script(
        state,
        None,
        scripts::ScriptCommand::Doctor,
        scripts::ScriptFlavor::Sh,
    )
    .await
}

async fn script_doctor_ps1(State(state): State<Arc<AppState>>) -> Response {
    fixed_command_script(
        state,
        None,
        scripts::ScriptCommand::Doctor,
        scripts::ScriptFlavor::Ps1,
    )
    .await
}

async fn command_script(
    state: Arc<AppState>,
    provider: Option<String>,
    command: scripts::ScriptCommand,
    headers: HeaderMap,
) -> Response {
    if let Some(name) = provider.as_deref()
        && !state.providers.contains_key(name)
    {
        return StatusCode::NOT_FOUND.into_response();
    }

    let installer_provider = state.config.server.installer_provider.as_str();
    if !state.providers.contains_key(installer_provider) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "text/plain")],
            format!(
                "installer provider '{}' is not configured",
                installer_provider
            ),
        )
            .into_response();
    }

    let flavor = scripts::negotiate_flavor(&headers);
    let mirror_url = state.config.server.public_url.as_deref();
    match scripts::render_bootstrap_script(
        command,
        provider.as_deref(),
        flavor,
        mirror_url,
        installer_provider,
        INSTALLER_BINARY_NAME,
    ) {
        Ok(script) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, flavor.content_type()),
                (header::VARY, "Accept, User-Agent"),
                (
                    header::HeaderName::from_static("x-acm-shell"),
                    flavor.as_str(),
                ),
                (
                    header::HeaderName::from_static("x-acm-script-flavor"),
                    flavor.as_str(),
                ),
            ],
            script,
        )
            .into_response(),
        Err(StatusCode::SERVICE_UNAVAILABLE) => (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "text/plain")],
            "server.public_url is not configured",
        )
            .into_response(),
        Err(status) => (
            status,
            [(header::CONTENT_TYPE, "text/plain")],
            "failed to render script",
        )
            .into_response(),
    }
}

async fn fixed_command_script(
    state: Arc<AppState>,
    provider: Option<String>,
    command: scripts::ScriptCommand,
    flavor: scripts::ScriptFlavor,
) -> Response {
    if let Some(name) = provider.as_deref()
        && !state.providers.contains_key(name)
    {
        return StatusCode::NOT_FOUND.into_response();
    }

    let installer_provider = state.config.server.installer_provider.as_str();
    if !state.providers.contains_key(installer_provider) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "text/plain")],
            format!(
                "installer provider '{}' is not configured",
                installer_provider
            ),
        )
            .into_response();
    }

    let mirror_url = state.config.server.public_url.as_deref();
    match scripts::render_bootstrap_script(
        command,
        provider.as_deref(),
        flavor,
        mirror_url,
        installer_provider,
        INSTALLER_BINARY_NAME,
    ) {
        Ok(script) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, flavor.content_type()),
                (
                    header::HeaderName::from_static("x-acm-shell"),
                    flavor.as_str(),
                ),
                (
                    header::HeaderName::from_static("x-acm-script-flavor"),
                    flavor.as_str(),
                ),
            ],
            script,
        )
            .into_response(),
        Err(StatusCode::SERVICE_UNAVAILABLE) => (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "text/plain")],
            "server.public_url is not configured",
        )
            .into_response(),
        Err(status) => (
            status,
            [(header::CONTENT_TYPE, "text/plain")],
            "failed to render script",
        )
            .into_response(),
    }
}

fn validated_file_segments(path: &str) -> Result<Vec<String>, StatusCode> {
    if path.is_empty() {
        return Err(StatusCode::NOT_FOUND);
    }
    let normalized = path.replace('\\', "/");
    let mut segments = Vec::new();
    for segment in normalized.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(StatusCode::NOT_FOUND);
        }
        segments.push(segment.to_string());
    }
    if segments.is_empty() {
        Err(StatusCode::NOT_FOUND)
    } else {
        Ok(segments)
    }
}

async fn provider_refresh_locked(
    state: &AppState,
    provider: &str,
) -> Result<Vec<String>, anyhow::Error> {
    let provider_instance = state
        .providers
        .get(provider)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Unknown provider: {}", provider))?;
    let _guard = state.sync_lock.lock().await;
    sync_provider_with_status(state, provider, || provider_instance.sync_all()).await
}

async fn api_provider_info(
    State(state): State<Arc<AppState>>,
    Path(provider): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let provider_instance = state
        .providers
        .get(&provider)
        .cloned()
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(provider_instance.get_info().await))
}

async fn api_provider_versions(
    State(state): State<Arc<AppState>>,
    Path(provider): Path<String>,
) -> Result<Json<Vec<String>>, StatusCode> {
    let provider_name = state
        .providers
        .get(&provider)
        .map(|_| provider.as_str())
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(state.cache.list_versions(provider_name).await))
}

async fn api_provider_checksums(
    State(state): State<Arc<AppState>>,
    Path(provider): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let provider_name = state
        .providers
        .get(&provider)
        .map(|_| provider.as_str())
        .ok_or(StatusCode::NOT_FOUND)?;
    let checksums = state
        .cache
        .with_provider_metadata(provider_name, crate::api::provider_checksums_json)
        .await
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    Ok(Json(checksums))
}

async fn api_provider_refresh(
    State(state): State<Arc<AppState>>,
    Path(provider): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if !state.providers.contains_key(&provider) {
        return Err(StatusCode::NOT_FOUND);
    }
    require_refresh_auth(state.as_ref(), &headers)?;
    check_refresh_throttle(state.as_ref(), &provider).await?;
    match provider_refresh_locked(state.as_ref(), &provider).await {
        Ok(updated) => Ok(Json(serde_json::json!({
            "success": true,
            "updated": updated
        }))),
        Err(e) => {
            error!("{} refresh failed: {}", provider, e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

fn truncate_for_metadata(mut value: String, max_len: usize) -> String {
    if value.len() <= max_len {
        return value;
    }
    value.truncate(max_len);
    value.push_str("...");
    value
}

fn summarize_sync_error(err: &anyhow::Error) -> String {
    let summary = format!("{:#}", err)
        .replace(['\r', '\n'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    truncate_for_metadata(summary, 500)
}

async fn sync_provider_with_status<T, Fut, F>(state: &AppState, provider: &str, f: F) -> Result<T>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let started_at = Utc::now();
    state
        .cache
        .update_provider_metadata(provider, |meta| {
            meta.sync.last_started_at = Some(started_at);
        })
        .await?;

    let started = Instant::now();
    let result = f().await;
    let duration_ms = started.elapsed().as_millis() as u64;
    let finished_at = Utc::now();

    match &result {
        Ok(_) => {
            state
                .cache
                .update_provider_metadata(provider, |meta| {
                    meta.sync.last_success_at = Some(finished_at);
                    meta.sync.last_duration_ms = Some(duration_ms);
                    meta.sync.last_error = None;
                })
                .await?;
        }
        Err(err) => {
            let summary = summarize_sync_error(err);
            state
                .cache
                .update_provider_metadata(provider, |meta| {
                    meta.sync.last_failure_at = Some(finished_at);
                    meta.sync.last_duration_ms = Some(duration_ms);
                    meta.sync.last_error = Some(summary);
                })
                .await?;
        }
    }

    result
}

async fn sync_all_locked(state: &AppState) -> Result<()> {
    let _guard = state.sync_lock.lock().await;
    let mut errors = Vec::new();

    let mut names = state.providers.keys().cloned().collect::<Vec<_>>();
    names.sort();

    for name in names {
        let Some(provider) = state.providers.get(&name).cloned() else {
            continue;
        };
        if let Err(err) = sync_provider_with_status(state, &name, || provider.sync_all_auto()).await
        {
            errors.push(format!("{}: {}", name, err));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(errors.join("; ")))
    }
}

fn require_refresh_auth(state: &AppState, headers: &HeaderMap) -> Result<(), StatusCode> {
    let Some(expected) = state.config.server.refresh_token.as_deref() else {
        warn!("Refresh endpoint called but server.refresh_token is not configured");
        return Err(StatusCode::FORBIDDEN);
    };

    let Some(auth) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return Err(StatusCode::UNAUTHORIZED);
    };

    let token = auth.strip_prefix("Bearer ").unwrap_or("");
    if token == expected {
        Ok(())
    } else {
        warn!("Unauthorized refresh attempt");
        Err(StatusCode::UNAUTHORIZED)
    }
}

async fn check_refresh_throttle(state: &AppState, provider: &str) -> Result<(), StatusCode> {
    let min_secs = state.config.server.refresh_min_interval_seconds;
    if min_secs == 0 {
        return Ok(());
    }

    let interval = Duration::from_secs(min_secs);
    let now = Instant::now();
    let mut guard = state.refresh_throttle.lock().await;

    if let Some(last) = guard.get(provider) {
        if now.duration_since(*last) < interval {
            warn!("Refresh throttled for provider {}", provider);
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
    }

    guard.insert(provider.to_string(), now);
    Ok(())
}

fn sanitize_filename_for_header(filename: &str) -> String {
    let mut out = String::with_capacity(filename.len());
    for ch in filename.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "download".to_string()
    } else {
        out
    }
}

pub(super) fn content_disposition_header_value(filename: &str) -> Result<HeaderValue, StatusCode> {
    let safe = sanitize_filename_for_header(filename);
    let value = format!("attachment; filename=\"{}\"", safe);
    HeaderValue::from_str(&value).map_err(|err| {
        error!("Failed to build Content-Disposition header: {}", err);
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

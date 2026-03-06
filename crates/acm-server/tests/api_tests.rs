//! Integration tests for HTTP API endpoints

use acm_server::{
    cache::{CacheManager, FileMetadata, PlatformMetadata, VersionMetadata},
    config::{CacheConfig, Config, DynamicProviderConfig, ProviderSource, ProviderUpdatePolicy},
    server::{self, AppState},
};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

fn create_request(method: &str, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .expect("build request")
}

fn static_provider(name: &str, version: &str, files: Vec<String>) -> DynamicProviderConfig {
    DynamicProviderConfig {
        name: name.to_string(),
        enabled: true,
        source: ProviderSource::Static,
        tags: vec!["latest".to_string()],
        update_policy: ProviderUpdatePolicy::Manual,
        platforms: Vec::new(),
        include_prerelease: false,
        files,
        repo: None,
        upstream_url: None,
        static_version: Some(version.to_string()),
        ui: Default::default(),
    }
}

async fn create_test_state_with(apply: impl FnOnce(&mut Config)) -> (TempDir, Arc<AppState>) {
    let temp_dir = TempDir::new().expect("create temp dir");
    let mut config = Config {
        cache: CacheConfig {
            dir: temp_dir.path().to_path_buf(),
            max_versions: 3,
        },
        ..Config::default()
    };
    apply(&mut config);

    let cache = Arc::new(CacheManager::new(&config.cache).expect("create cache"));
    let state = server::build_state(config, cache)
        .await
        .expect("build state");
    (temp_dir, state)
}

async fn seed_provider_file(
    state: &Arc<AppState>,
    provider: &str,
    version: &str,
    rel_path: &str,
    data: &[u8],
) {
    let path = state
        .cache
        .version_path(provider, version)
        .join("files")
        .join(rel_path);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .expect("create artifact dir");
    }
    tokio::fs::write(&path, data).await.expect("write artifact");

    let mut hasher = Sha256::new();
    hasher.update(data);
    let sha256 = hex::encode(hasher.finalize());
    let size = data.len() as u64;

    state
        .cache
        .write_tag(provider, "latest", version)
        .await
        .expect("write tag");
    state
        .cache
        .update_provider_metadata(provider, |m| {
            m.tags.insert("latest".to_string(), version.to_string());
            m.versions.insert(
                version.to_string(),
                VersionMetadata {
                    version: version.to_string(),
                    downloaded_at: chrono::Utc::now(),
                    platforms: [(
                        "universal".to_string(),
                        PlatformMetadata {
                            sha256: sha256.clone(),
                            size,
                            filename: rel_path.to_string(),
                            files: [(
                                rel_path.to_string(),
                                FileMetadata {
                                    sha256: sha256.clone(),
                                    size,
                                },
                            )]
                            .into_iter()
                            .collect::<HashMap<_, _>>(),
                        },
                    )]
                    .into_iter()
                    .collect::<HashMap<_, _>>(),
                },
            );
        })
        .await
        .expect("update metadata");
}

#[tokio::test]
async fn test_health_check() {
    let (_tmp, state) = create_test_state_with(|_| {}).await;
    let app = server::build_router(state);

    let response = app
        .oneshot(create_request("GET", "/health"))
        .await
        .expect("request");
    assert_eq!(response.status(), StatusCode::OK);

    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    assert_eq!(&body[..], b"OK");
}

#[tokio::test]
async fn test_install_script_injection() {
    let (_tmp, state) = create_test_state_with(|config| {
        config.server.public_url = Some("http://example.com".to_string());
        config.server.installer_provider = "tool-a".to_string();
        config.providers.push(static_provider(
            "tool-a",
            "1.0.0",
            vec!["tool-a.tar.gz".to_string()],
        ));
    })
    .await;
    let app = server::build_router(state);

    let sh_request = Request::builder()
        .method("GET")
        .uri("/install/tool-a")
        .header("Accept", "text/x-shellscript")
        .body(Body::empty())
        .expect("build shell request");
    let sh_response = app
        .clone()
        .oneshot(sh_request)
        .await
        .expect("shell response");
    assert_eq!(sh_response.status(), StatusCode::OK);
    assert_eq!(
        sh_response
            .headers()
            .get("x-acm-script-flavor")
            .and_then(|v| v.to_str().ok()),
        Some("sh")
    );
    assert_eq!(
        sh_response
            .headers()
            .get("x-acm-shell")
            .and_then(|v| v.to_str().ok()),
        Some("sh")
    );
    let sh_body = sh_response
        .into_body()
        .collect()
        .await
        .expect("shell body")
        .to_bytes();
    let sh_text = String::from_utf8_lossy(&sh_body);
    assert!(sh_text.contains(r#"MIRROR_URL="${MIRROR_URL:-http://example.com}""#));
    assert!(sh_text.contains(r#"BIN_NAME="acm-installer""#));
    assert!(sh_text.contains(r#""install""#));
    assert!(sh_text.contains(r#"COMMAND_ARGS+=("tool-a")"#));

    let ps_request = Request::builder()
        .method("GET")
        .uri("/install/tool-a")
        .header("Accept", "application/x-powershell")
        .body(Body::empty())
        .expect("build powershell request");
    let ps1_response = app.oneshot(ps_request).await.expect("powershell response");
    assert_eq!(ps1_response.status(), StatusCode::OK);
    assert_eq!(
        ps1_response
            .headers()
            .get("x-acm-script-flavor")
            .and_then(|v| v.to_str().ok()),
        Some("ps1")
    );
    assert_eq!(
        ps1_response
            .headers()
            .get("x-acm-shell")
            .and_then(|v| v.to_str().ok()),
        Some("ps1")
    );
    let ps1_body = ps1_response
        .into_body()
        .collect()
        .await
        .expect("powershell body")
        .to_bytes();
    let ps1_text = String::from_utf8_lossy(&ps1_body);
    assert!(ps1_text.contains(
        r#"$MirrorUrl = if ($env:MIRROR_URL) { $env:MIRROR_URL } else { "http://example.com" }"#
    ));
    assert!(ps1_text.contains(r#"$InstallerBin = "acm-installer""#));
    assert!(ps1_text.contains(r#""install""#));
    assert!(ps1_text.contains(r#"$CommandArgs += "tool-a""#));
}

#[tokio::test]
async fn test_install_script_requires_public_url() {
    let (_tmp, state) = create_test_state_with(|config| {
        config.server.public_url = None;
        config.server.installer_provider = "tool-a".to_string();
        config.providers.push(static_provider(
            "tool-a",
            "1.0.0",
            vec!["tool-a.tar.gz".to_string()],
        ));
    })
    .await;
    let app = server::build_router(state);

    let response = app
        .oneshot(create_request("GET", "/install/tool-a"))
        .await
        .expect("request");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn test_uninstall_script_requires_public_url() {
    let (_tmp, state) = create_test_state_with(|config| {
        config.server.public_url = None;
        config.server.installer_provider = "tool-a".to_string();
        config.providers.push(static_provider(
            "tool-a",
            "1.0.0",
            vec!["tool-a.tar.gz".to_string()],
        ));
    })
    .await;
    let app = server::build_router(state);

    let response = app
        .oneshot(create_request("GET", "/uninstall/tool-a"))
        .await
        .expect("request");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn test_status_script_uses_ua_fallback_for_powershell() {
    let (_tmp, state) = create_test_state_with(|config| {
        config.server.public_url = Some("http://example.com".to_string());
        config.server.installer_provider = "installer".to_string();
        config.providers.push(static_provider(
            "installer",
            "1.0.0",
            vec!["acm-installer.zip".to_string()],
        ));
    })
    .await;
    let app = server::build_router(state);

    let request = Request::builder()
        .method("GET")
        .uri("/status")
        .header("User-Agent", "PowerShell/7.4.0")
        .body(Body::empty())
        .expect("build request");
    let response = app.oneshot(request).await.expect("request");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("x-acm-script-flavor")
            .and_then(|v| v.to_str().ok()),
        Some("ps1")
    );
}

#[tokio::test]
async fn test_provider_tag_and_file_route() {
    let (_tmp, state) = create_test_state_with(|config| {
        config.providers.push(static_provider(
            "tool-a",
            "2.1.0",
            vec!["nested/tool-a.tar.gz".to_string()],
        ));
    })
    .await;

    seed_provider_file(
        &state,
        "tool-a",
        "2.1.0",
        "nested/tool-a.tar.gz",
        b"test-archive",
    )
    .await;

    let app = server::build_router(state);

    let tag = app
        .clone()
        .oneshot(create_request("GET", "/tool-a/latest"))
        .await
        .expect("tag response");
    assert_eq!(tag.status(), StatusCode::OK);
    let tag_body = tag
        .into_body()
        .collect()
        .await
        .expect("tag body")
        .to_bytes();
    assert_eq!(&tag_body[..], b"2.1.0");

    let file = app
        .oneshot(create_request(
            "GET",
            "/tool-a/2.1.0/files/nested/tool-a.tar.gz",
        ))
        .await
        .expect("file response");
    assert_eq!(file.status(), StatusCode::OK);
    let file_body = file
        .into_body()
        .collect()
        .await
        .expect("file body")
        .to_bytes();
    assert_eq!(&file_body[..], b"test-archive");
}

#[tokio::test]
async fn test_local_download_supports_range_requests() {
    let (_tmp, state) = create_test_state_with(|config| {
        config.providers.push(static_provider(
            "tool-a",
            "3.0.0",
            vec!["artifact.bin".to_string()],
        ));
    })
    .await;

    let data = b"0123456789abcdef";
    seed_provider_file(&state, "tool-a", "3.0.0", "artifact.bin", data).await;

    let app = server::build_router(state);
    let request = Request::builder()
        .method("GET")
        .uri("/tool-a/3.0.0/files/artifact.bin")
        .header("Range", "bytes=0-3")
        .body(Body::empty())
        .expect("build range request");
    let response = app.oneshot(request).await.expect("range response");

    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    let body = response
        .into_body()
        .collect()
        .await
        .expect("range body")
        .to_bytes();
    assert_eq!(&body[..], &data[..4]);
}

#[tokio::test]
async fn test_checksums_api() {
    let (_tmp, state) = create_test_state_with(|config| {
        config.providers.push(static_provider(
            "tool-a",
            "4.0.0",
            vec!["artifact.bin".to_string()],
        ));
    })
    .await;

    let data = b"checksum-data";
    seed_provider_file(&state, "tool-a", "4.0.0", "artifact.bin", data).await;

    let mut hasher = Sha256::new();
    hasher.update(data);
    let expected_sha256 = hex::encode(hasher.finalize());

    let app = server::build_router(state);
    let response = app
        .oneshot(create_request("GET", "/api/tool-a/checksums"))
        .await
        .expect("checksums response");
    assert_eq!(response.status(), StatusCode::OK);

    let body = response
        .into_body()
        .collect()
        .await
        .expect("checksums body")
        .to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("parse checksums json");

    assert_eq!(json["4.0.0"]["universal"]["sha256"], expected_sha256);
    assert_eq!(
        json["4.0.0"]["universal"]["files"]["artifact.bin"]["size"],
        data.len() as u64
    );
}

#[tokio::test]
async fn test_refresh_requires_token_when_not_configured() {
    let (_tmp, state) = create_test_state_with(|config| {
        let mut provider = static_provider("tool-a", "1.0.0", vec!["artifact.bin".to_string()]);
        provider.enabled = false;
        config.providers.push(provider);
        config.server.refresh_token = None;
    })
    .await;
    let app = server::build_router(state);

    let response = app
        .oneshot(create_request("POST", "/api/tool-a/refresh"))
        .await
        .expect("refresh response");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_refresh_with_token_and_throttle() {
    let (_tmp, state) = create_test_state_with(|config| {
        let mut provider = static_provider("tool-a", "1.0.0", vec!["artifact.bin".to_string()]);
        provider.enabled = false;
        config.providers.push(provider);
        config.server.refresh_token = Some("secret".to_string());
        config.server.refresh_min_interval_seconds = 60;
    })
    .await;
    let app = server::build_router(state);

    let request = Request::builder()
        .method("POST")
        .uri("/api/tool-a/refresh")
        .header("Authorization", "Bearer secret")
        .body(Body::empty())
        .expect("build refresh request");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("first refresh response");
    assert_eq!(response.status(), StatusCode::OK);

    let request = Request::builder()
        .method("POST")
        .uri("/api/tool-a/refresh")
        .header("Authorization", "Bearer secret")
        .body(Body::empty())
        .expect("build refresh request");
    let response = app.oneshot(request).await.expect("second refresh response");
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn test_info_exposes_sync_status() {
    let (_tmp, state) = create_test_state_with(|config| {
        let mut provider = static_provider("tool-a", "1.0.0", vec!["artifact.bin".to_string()]);
        provider.enabled = false;
        config.providers.push(provider);
        config.server.refresh_token = Some("secret".to_string());
    })
    .await;
    let app = server::build_router(state);

    let refresh = Request::builder()
        .method("POST")
        .uri("/api/tool-a/refresh")
        .header("Authorization", "Bearer secret")
        .body(Body::empty())
        .expect("build refresh request");
    let response = app
        .clone()
        .oneshot(refresh)
        .await
        .expect("refresh response");
    assert_eq!(response.status(), StatusCode::OK);

    let info = app
        .oneshot(create_request("GET", "/api/tool-a/info"))
        .await
        .expect("info response");
    assert_eq!(info.status(), StatusCode::OK);

    let body = info
        .into_body()
        .collect()
        .await
        .expect("info body")
        .to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("parse info json");

    assert!(json["sync"].is_object());
    assert!(json["sync"]["last_success_at"].is_string());
    assert!(json["sync"]["last_duration_ms"].is_number());
}

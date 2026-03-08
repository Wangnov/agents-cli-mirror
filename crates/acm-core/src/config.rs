use anyhow::{Context, Result};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Main configuration structure
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,

    #[serde(default)]
    pub client: ClientConfig,

    #[serde(default)]
    pub brand: BrandConfig,

    #[serde(default)]
    pub http: HttpConfig,

    #[serde(default)]
    pub storage: StorageConfig,

    #[serde(default)]
    pub cache: CacheConfig,

    #[serde(default)]
    pub update: UpdateConfig,

    /// Dynamic provider registry.
    ///
    /// This is the only provider configuration entrypoint.
    #[serde(default)]
    pub providers: Vec<DynamicProviderConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "default_port")]
    pub port: u16,

    #[serde(default = "default_host")]
    pub host: String,

    /// Public URL for install scripts (e.g., "http://yourip:1357")
    /// If not set, install script endpoints will return 503
    #[serde(default = "default_public_url")]
    pub public_url: Option<String>,

    /// Admin token required for POST /api/*/refresh.
    ///
    /// Send as `Authorization: Bearer <token>`. If not configured, refresh endpoints
    /// will return 403.
    #[serde(default = "default_refresh_token")]
    pub refresh_token: Option<String>,

    /// Per-provider minimum interval (seconds) between refresh requests.
    /// Set to 0 to disable throttling.
    #[serde(default = "default_refresh_min_interval_seconds")]
    pub refresh_min_interval_seconds: u64,

    /// Provider name for bootstrap installer binary.
    ///
    /// Script endpoints (`/install/*`, `/update/*`, `/uninstall/*`, `/status`, `/doctor`)
    /// will download this provider first, then execute `acm-installer`.
    #[serde(default = "default_installer_provider")]
    pub installer_provider: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientConfig {
    /// Optional default mirror URL used by `acm-client` when no --mirror-url / MIRROR_URL is set.
    #[serde(default = "default_client_default_mirror_url")]
    pub default_mirror_url: Option<String>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            default_mirror_url: default_client_default_mirror_url(),
        }
    }
}

fn default_client_default_mirror_url() -> Option<String> {
    std::env::var("ACM_CLIENT_DEFAULT_MIRROR_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrandConfig {
    #[serde(default = "default_brand_assets_file")]
    pub assets_file: String,
}

impl Default for BrandConfig {
    fn default() -> Self {
        Self {
            assets_file: default_brand_assets_file(),
        }
    }
}

fn default_brand_assets_file() -> String {
    std::env::var("MIRROR_BRAND_ASSETS_FILE").unwrap_or_else(|_| "assets/brand.toml".to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpConfig {
    #[serde(default = "default_http_connect_timeout_seconds")]
    pub connect_timeout_seconds: u64,

    #[serde(default = "default_http_request_timeout_seconds")]
    pub request_timeout_seconds: u64,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            connect_timeout_seconds: default_http_connect_timeout_seconds(),
            request_timeout_seconds: default_http_request_timeout_seconds(),
        }
    }
}

fn default_http_connect_timeout_seconds() -> u64 {
    std::env::var("MIRROR_HTTP_CONNECT_TIMEOUT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10)
}

fn default_http_request_timeout_seconds() -> u64 {
    std::env::var("MIRROR_HTTP_TIMEOUT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3600)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSource {
    #[default]
    GithubRelease,
    GcsRelease,
    Static,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProviderUpdatePolicy {
    #[default]
    Tracking,
    Pinned,
    Manual,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProviderUiPreset {
    #[default]
    Acm,
    Codex,
    Claude,
    Gemini,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct ProviderUiConfig {
    #[serde(default)]
    pub preset: ProviderUiPreset,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DynamicProviderConfig {
    pub name: String,

    #[serde(default = "default_dynamic_provider_enabled")]
    pub enabled: bool,

    #[serde(default)]
    pub source: ProviderSource,

    #[serde(default = "default_tags")]
    pub tags: Vec<String>,

    #[serde(default = "default_provider_update_policy")]
    pub update_policy: ProviderUpdatePolicy,

    #[serde(default)]
    pub platforms: Vec<String>,

    #[serde(default)]
    pub include_prerelease: bool,

    /// Optional explicit file list (mainly for generic gcs_release providers).
    ///
    /// - github_release: empty means mirror all release assets.
    /// - gcs_release: empty is invalid for generic providers.
    #[serde(default)]
    pub files: Vec<String>,

    /// GitHub repo in OWNER/REPO format. Used by source = "github_release".
    #[serde(default)]
    pub repo: Option<String>,

    /// Upstream URL root. Used by source = "gcs_release".
    #[serde(default)]
    pub upstream_url: Option<String>,

    /// Fixed version for source = "static".
    #[serde(default)]
    pub static_version: Option<String>,

    #[serde(default)]
    pub ui: ProviderUiConfig,
}

fn default_dynamic_provider_enabled() -> bool {
    true
}

fn default_provider_update_policy() -> ProviderUpdatePolicy {
    ProviderUpdatePolicy::Tracking
}

fn default_tags() -> Vec<String> {
    vec!["stable".to_string(), "latest".to_string()]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    #[serde(default = "default_storage_mode")]
    pub mode: StorageMode,

    #[serde(default)]
    pub s3: S3Config,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            mode: default_storage_mode(),
            s3: S3Config::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum StorageMode {
    #[default]
    Local,
    #[serde(alias = "r2")]
    S3,
}

fn default_storage_mode() -> StorageMode {
    match std::env::var("MIRROR_STORAGE_MODE")
        .ok()
        .as_deref()
        .map(|s| s.to_lowercase())
    {
        Some(ref v) if v == "s3" || v == "r2" => StorageMode::S3,
        _ => StorageMode::Local,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct S3Config {
    #[serde(default = "default_s3_endpoint")]
    pub endpoint: String,

    #[serde(default = "default_s3_bucket")]
    pub bucket: String,

    #[serde(default = "default_s3_access_key_id")]
    pub access_key_id: String,

    #[serde(default = "default_s3_secret_access_key")]
    pub secret_access_key: String,

    #[serde(default = "default_s3_session_token")]
    pub session_token: Option<String>,

    #[serde(default = "default_s3_region")]
    pub region: String,

    #[serde(default = "default_s3_prefix")]
    pub prefix: String,

    #[serde(default = "default_s3_path_style")]
    pub path_style: bool,

    #[serde(default = "default_s3_expires_seconds")]
    pub expires_seconds: u64,

    #[serde(default = "default_s3_multipart_max_parts")]
    pub multipart_max_parts: usize,

    #[serde(default = "default_s3_multipart_part_max_attempts")]
    pub multipart_part_max_attempts: usize,
}

impl Default for S3Config {
    fn default() -> Self {
        Self {
            endpoint: default_s3_endpoint(),
            bucket: default_s3_bucket(),
            access_key_id: default_s3_access_key_id(),
            secret_access_key: default_s3_secret_access_key(),
            session_token: default_s3_session_token(),
            region: default_s3_region(),
            prefix: default_s3_prefix(),
            path_style: default_s3_path_style(),
            expires_seconds: default_s3_expires_seconds(),
            multipart_max_parts: default_s3_multipart_max_parts(),
            multipart_part_max_attempts: default_s3_multipart_part_max_attempts(),
        }
    }
}

fn default_s3_endpoint() -> String {
    std::env::var("MIRROR_S3_ENDPOINT").unwrap_or_default()
}

fn default_s3_bucket() -> String {
    std::env::var("MIRROR_S3_BUCKET").unwrap_or_default()
}

fn default_s3_access_key_id() -> String {
    std::env::var("MIRROR_S3_ACCESS_KEY_ID").unwrap_or_default()
}

fn default_s3_secret_access_key() -> String {
    std::env::var("MIRROR_S3_SECRET_ACCESS_KEY").unwrap_or_default()
}

fn default_s3_session_token() -> Option<String> {
    std::env::var("MIRROR_S3_SESSION_TOKEN").ok()
}

fn default_s3_region() -> String {
    std::env::var("MIRROR_S3_REGION").unwrap_or_else(|_| "auto".to_string())
}

fn default_s3_prefix() -> String {
    std::env::var("MIRROR_S3_PREFIX").unwrap_or_default()
}

fn default_s3_path_style() -> bool {
    std::env::var("MIRROR_S3_PATH_STYLE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(true)
}

fn default_s3_expires_seconds() -> u64 {
    std::env::var("MIRROR_S3_EXPIRES_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(900)
}

fn default_s3_multipart_max_parts() -> usize {
    std::env::var("MIRROR_S3_MULTIPART_MAX_PARTS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|v| *v >= 1)
        .unwrap_or(10_000)
}

fn default_s3_multipart_part_max_attempts() -> usize {
    std::env::var("MIRROR_S3_MULTIPART_PART_MAX_ATTEMPTS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|v| *v >= 1)
        .unwrap_or(3)
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: default_port(),
            host: default_host(),
            public_url: default_public_url(),
            refresh_token: default_refresh_token(),
            refresh_min_interval_seconds: default_refresh_min_interval_seconds(),
            installer_provider: default_installer_provider(),
        }
    }
}

fn default_public_url() -> Option<String> {
    std::env::var("MIRROR_PUBLIC_URL").ok()
}

fn default_refresh_token() -> Option<String> {
    std::env::var("MIRROR_REFRESH_TOKEN").ok()
}

fn default_refresh_min_interval_seconds() -> u64 {
    std::env::var("MIRROR_REFRESH_MIN_INTERVAL")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10)
}

fn default_installer_provider() -> String {
    std::env::var("MIRROR_INSTALLER_PROVIDER").unwrap_or_else(|_| "installer".to_string())
}

fn default_port() -> u16 {
    std::env::var("MIRROR_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1357)
}

fn default_host() -> String {
    std::env::var("MIRROR_HOST").unwrap_or_else(|_| "0.0.0.0".to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheConfig {
    #[serde(default = "default_cache_dir")]
    pub dir: PathBuf,

    #[serde(default = "default_max_versions")]
    pub max_versions: usize,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            dir: default_cache_dir(),
            max_versions: default_max_versions(),
        }
    }
}

fn default_cache_dir() -> PathBuf {
    std::env::var("MIRROR_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./cache"))
}

fn default_max_versions() -> usize {
    10
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateConfig {
    #[serde(default = "default_interval_minutes")]
    pub interval_minutes: u64,

    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            interval_minutes: default_interval_minutes(),
            enabled: default_enabled(),
        }
    }
}

fn default_interval_minutes() -> u64 {
    std::env::var("MIRROR_UPDATE_INTERVAL")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10)
}

fn default_enabled() -> bool {
    true
}

fn is_valid_provider_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|ch| {
            ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_' | '.')
        })
}

fn validate_repo_format(repo: &str) -> Result<()> {
    let trimmed = repo.trim();
    if trimmed.is_empty() {
        anyhow::bail!("repo is empty");
    }
    if trimmed.contains(char::is_whitespace) {
        anyhow::bail!("repo must not contain whitespace");
    }
    let mut parts = trimmed.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    if owner.is_empty() || name.is_empty() || parts.next().is_some() {
        anyhow::bail!("repo must be in OWNER/REPO format");
    }
    Ok(())
}

fn validate_provider_tag(tag: &str) -> Result<()> {
    if tag.trim().is_empty() {
        anyhow::bail!("tag is empty");
    }
    if tag.contains('/') || tag.contains(char::is_whitespace) {
        anyhow::bail!("tag must not contain '/' or whitespace");
    }
    Ok(())
}

fn validate_provider_file_path(path: &str) -> Result<()> {
    if path.trim().is_empty() {
        anyhow::bail!("file path is empty");
    }
    let normalized = path.replace('\\', "/");
    for segment in normalized.split('/') {
        if segment.is_empty() || matches!(segment, "." | "..") {
            anyhow::bail!("file path contains invalid segment");
        }
    }
    Ok(())
}

impl DynamicProviderConfig {
    fn validate(&self) -> Result<()> {
        if !is_valid_provider_name(&self.name) {
            anyhow::bail!(
                "invalid provider name '{}' (allowed: lowercase letters, digits, '-', '_', '.')",
                self.name
            );
        }

        if self.tags.is_empty() {
            anyhow::bail!("tags must not be empty");
        }
        let mut tags = HashSet::new();
        for tag in &self.tags {
            validate_provider_tag(tag)?;
            if !tags.insert(tag) {
                anyhow::bail!("duplicate tag '{}'", tag);
            }
        }
        if matches!(self.update_policy, ProviderUpdatePolicy::Pinned)
            && self
                .tags
                .iter()
                .any(|t| matches!(t.as_str(), "latest" | "stable"))
        {
            anyhow::bail!("pinned update_policy does not support alias tags latest/stable");
        }

        let mut files = HashSet::new();
        for file in &self.files {
            validate_provider_file_path(file)?;
            if !files.insert(file) {
                anyhow::bail!("duplicate file path '{}'", file);
            }
        }

        match self.source {
            ProviderSource::GithubRelease => {
                let repo = self
                    .repo
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("repo is required for github_release"))?;
                validate_repo_format(repo)?;
                if self.upstream_url.is_some() {
                    anyhow::bail!("upstream_url is not allowed for github_release");
                }
                if self.static_version.is_some() {
                    anyhow::bail!("static_version is not allowed for github_release");
                }
            }
            ProviderSource::GcsRelease => {
                let upstream_url = self
                    .upstream_url
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("upstream_url is required for gcs_release"))?;
                let url = Url::parse(upstream_url).context("upstream_url is not a valid URL")?;
                if !matches!(url.scheme(), "http" | "https") {
                    anyhow::bail!("upstream_url scheme must be http/https");
                }
                if url.host_str().is_none() {
                    anyhow::bail!("upstream_url must include host");
                }
                if self.repo.is_some() {
                    anyhow::bail!("repo is not allowed for gcs_release");
                }
                if self.static_version.is_some() {
                    anyhow::bail!("static_version is not allowed for gcs_release");
                }
                if self.files.is_empty() {
                    anyhow::bail!("gcs_release provider requires files");
                }
            }
            ProviderSource::Static => {
                let static_version = self
                    .static_version
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("static_version is required for static"))?;
                if static_version.trim().is_empty() {
                    anyhow::bail!("static_version is empty");
                }
                if self.files.is_empty() {
                    anyhow::bail!("static provider requires files");
                }
                if self.repo.is_some() {
                    anyhow::bail!("repo is not allowed for static");
                }
                if self.upstream_url.is_some() {
                    anyhow::bail!("upstream_url is not allowed for static");
                }
            }
        }

        Ok(())
    }
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        if !is_valid_provider_name(&self.server.installer_provider) {
            anyhow::bail!(
                "invalid server.installer_provider '{}' (allowed: lowercase letters, digits, '-', '_', '.')",
                self.server.installer_provider
            );
        }

        if let Some(public_url) = self.server.public_url.as_deref() {
            normalize_public_url(public_url)?;
        }
        if let Some(default_mirror_url) = self.client.default_mirror_url.as_deref() {
            normalize_client_mirror_url(default_mirror_url)?;
        }

        if matches!(self.storage.mode, StorageMode::S3) {
            if self.storage.s3.endpoint.trim().is_empty() {
                anyhow::bail!("storage.s3.endpoint is required when storage.mode = \"s3\"");
            }
            if self.storage.s3.bucket.trim().is_empty() {
                anyhow::bail!("storage.s3.bucket is required when storage.mode = \"s3\"");
            }
            if self.storage.s3.access_key_id.trim().is_empty() {
                anyhow::bail!("storage.s3.access_key_id is required when storage.mode = \"s3\"");
            }
            if self.storage.s3.secret_access_key.trim().is_empty() {
                anyhow::bail!(
                    "storage.s3.secret_access_key is required when storage.mode = \"s3\""
                );
            }
            if self.storage.s3.expires_seconds == 0 {
                anyhow::bail!("storage.s3.expires_seconds must be >= 1");
            }
            if self.storage.s3.multipart_max_parts == 0 {
                anyhow::bail!("storage.s3.multipart_max_parts must be >= 1");
            }
            if self.storage.s3.multipart_part_max_attempts == 0 {
                anyhow::bail!("storage.s3.multipart_part_max_attempts must be >= 1");
            }
        }

        let mut names = HashSet::new();
        for provider in &self.providers {
            provider
                .validate()
                .with_context(|| format!("invalid provider '{}'", provider.name))?;
            if !names.insert(provider.name.as_str()) {
                anyhow::bail!("duplicate provider name '{}'", provider.name);
            }
        }

        Ok(())
    }

    /// Load configuration from a TOML file
    pub fn load(path: &Path) -> Result<Self> {
        if path.exists() {
            let content = std::fs::read_to_string(path)
                .with_context(|| format!("Failed to read config file: {}", path.display()))?;
            let config: Config = toml::from_str(&content)
                .with_context(|| format!("Failed to parse config file: {}", path.display()))?;
            config
                .validate()
                .with_context(|| format!("Invalid config file: {}", path.display()))?;
            Ok(config)
        } else {
            // Return default config if file doesn't exist
            tracing::warn!(
                "Config file not found at {}, using defaults",
                path.display()
            );
            let config = Config::default();
            config.validate()?;
            Ok(config)
        }
    }
}

pub fn normalize_public_url(value: &str) -> Result<String> {
    normalize_url_field("server.public_url", value)
}

pub fn normalize_client_mirror_url(value: &str) -> Result<String> {
    normalize_url_field("client.default_mirror_url", value)
}

fn normalize_url_field(field: &str, value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        anyhow::bail!("{field} is empty");
    }
    if trimmed.contains(['\r', '\n']) {
        anyhow::bail!("{field} must not contain newlines");
    }
    // We inject public_url into shell/PowerShell scripts; keep it strict to avoid injection.
    if trimmed.contains(['"', '\'', '`', '$', '\\']) || trimmed.chars().any(char::is_whitespace) {
        anyhow::bail!(
            "{field} contains unsafe characters (quotes/whitespace/$/backtick/backslash)"
        );
    }

    // Avoid accidental double slashes when scripts append paths.
    let normalized = trimmed.trim_end_matches('/').to_string();

    let url = Url::parse(&normalized).with_context(|| format!("{field} is not a valid URL"))?;
    match url.scheme() {
        "http" | "https" => {}
        scheme => anyhow::bail!("{field} scheme must be http/https, got: {}", scheme),
    }
    if url.host_str().is_none() {
        anyhow::bail!("{field} must include a host");
    }
    if url.query().is_some() || url.fragment().is_some() {
        anyhow::bail!("{field} must not include query or fragment");
    }

    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Write};
    use tempfile::NamedTempFile;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.server.port, 1357);
        assert_eq!(config.server.host, "0.0.0.0");
        assert!(matches!(config.storage.mode, StorageMode::Local));
        assert_eq!(config.cache.max_versions, 10);
        assert!(config.update.enabled);
        assert_eq!(config.update.interval_minutes, 10);
        assert!(config.providers.is_empty());
    }

    #[test]
    fn test_load_config_from_file() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"
[server]
port = 8080
host = "127.0.0.1"
public_url = "http://example.com"

[storage]
mode = "s3"

[storage.s3]
endpoint = "https://s3.example.com"
bucket = "example-bucket"
access_key_id = "test-id"
secret_access_key = "test-secret"
prefix = "mirror"
path_style = true
expires_seconds = 600

[cache]
dir = "/tmp/cache"
max_versions = 5

[update]
interval_minutes = 30
enabled = false

[[providers]]
name = "alpha-tool"
enabled = true
source = "gcs_release"
tags = ["stable"]
upstream_url = "https://storage.googleapis.com/example/releases"
files = ["alpha-linux-x64.tar.gz"]
update_policy = "manual"

[[providers]]
name = "beta-tool"
enabled = true
source = "github_release"
tags = ["stable", "latest"]
include_prerelease = true
repo = "owner/beta-tool"
"#
        )
        .unwrap();

        let config = Config::load(file.path()).unwrap();
        assert_eq!(config.server.port, 8080);
        assert_eq!(config.server.host, "127.0.0.1");
        assert_eq!(
            config.server.public_url,
            Some("http://example.com".to_string())
        );
        assert_eq!(config.cache.max_versions, 5);
        assert_eq!(config.update.interval_minutes, 30);
        assert!(!config.update.enabled);
        assert!(matches!(config.storage.mode, StorageMode::S3));
        assert_eq!(config.storage.s3.bucket, "example-bucket");
        assert_eq!(config.storage.s3.expires_seconds, 600);
        let alpha = config
            .providers
            .iter()
            .find(|p| p.name == "alpha-tool")
            .expect("alpha-tool provider should be loaded");
        assert_eq!(alpha.tags, vec!["stable"]);
        assert_eq!(
            alpha.upstream_url.as_deref(),
            Some("https://storage.googleapis.com/example/releases")
        );
        assert_eq!(alpha.files, vec!["alpha-linux-x64.tar.gz"]);
        let beta = config
            .providers
            .iter()
            .find(|p| p.name == "beta-tool")
            .expect("beta-tool provider should be loaded");
        assert_eq!(beta.source, ProviderSource::GithubRelease);
        assert_eq!(beta.repo.as_deref(), Some("owner/beta-tool"));
        assert!(beta.include_prerelease);
    }

    #[test]
    fn test_load_nonexistent_file_returns_default() {
        let config = Config::load(Path::new("/nonexistent/config.toml")).unwrap();
        assert_eq!(config.server.port, 1357);
        assert!(config.providers.is_empty());
    }

    #[test]
    fn test_load_config_accepts_client_default_mirror_url() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"
[client]
default_mirror_url = "https://mirror.example.com"
"#
        )
        .unwrap();

        let config = Config::load(file.path()).unwrap();
        assert_eq!(
            config.client.default_mirror_url.as_deref(),
            Some("https://mirror.example.com")
        );
    }

    #[test]
    fn test_load_config_rejects_invalid_client_default_mirror_url() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"
[client]
default_mirror_url = "https://example.com/with space"
"#
        )
        .unwrap();

        let err = Config::load(file.path()).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("client.default_mirror_url"));
    }

    #[test]
    fn test_load_config_from_dynamic_providers() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"
[[providers]]
name = "custom-tool"
source = "github_release"
repo = "owner/custom-tool"
enabled = true
tags = ["v1.2.3"]
platforms = ["linux-x64"]
include_prerelease = false
update_policy = "pinned"

[providers.ui]
preset = "codex"
"#
        )
        .unwrap();

        let config = Config::load(file.path()).unwrap();
        assert_eq!(config.providers.len(), 1);
        let provider = &config.providers[0];
        assert_eq!(provider.name, "custom-tool");
        assert_eq!(provider.source, ProviderSource::GithubRelease);
        assert_eq!(provider.repo.as_deref(), Some("owner/custom-tool"));
        assert_eq!(provider.tags, vec!["v1.2.3"]);
        assert_eq!(provider.update_policy, ProviderUpdatePolicy::Pinned);
        assert_eq!(provider.ui.preset, ProviderUiPreset::Codex);
    }

    #[test]
    fn test_load_config_rejects_legacy_provider_section() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"
[legacy_provider]
enabled = true
"#
        )
        .unwrap();

        let err = Config::load(file.path()).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("unknown field"));
    }

    #[test]
    fn test_load_config_rejects_github_release_without_repo() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"
[[providers]]
name = "custom-tool"
source = "github_release"
tags = ["latest"]
"#
        )
        .unwrap();

        let err = Config::load(file.path()).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("repo is required for github_release"));
    }

    #[test]
    fn test_load_config_rejects_pinned_alias_tag() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"
[[providers]]
name = "custom-tool"
source = "github_release"
repo = "owner/custom-tool"
update_policy = "pinned"
tags = ["latest"]
"#
        )
        .unwrap();

        let err = Config::load(file.path()).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("pinned update_policy"));
    }

    #[test]
    fn test_load_config_rejects_empty_providers_list() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"
providers = []
"#
        )
        .unwrap();

        let config = Config::load(file.path()).unwrap();
        assert!(config.providers.is_empty());
    }

    #[test]
    fn test_load_config_rejects_gcs_without_files() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"
[[providers]]
name = "gcs-tool"
source = "gcs_release"
upstream_url = "https://storage.googleapis.com/example/releases"
tags = ["latest"]
"#
        )
        .unwrap();

        let err = Config::load(file.path()).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("gcs_release provider requires files"));
    }

    #[test]
    fn test_load_config_rejects_s3_without_required_fields() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"
[storage]
mode = "s3"

[[providers]]
name = "tool-a"
source = "github_release"
repo = "owner/tool-a"
tags = ["latest"]
"#
        )
        .unwrap();

        let err = Config::load(file.path()).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("storage.s3.endpoint is required"));
    }

    #[test]
    fn test_cloud_config_includes_all_public_cli_providers() {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        let config_path = repo_root.join("config.cloud.toml");
        let cloud_config = fs::read_to_string(&config_path).expect("read config.cloud.toml");
        toml::from_str::<toml::Value>(&cloud_config).expect("parse config.cloud.toml");

        assert!(cloud_config.contains("name = \"claude-code\""));
        assert!(cloud_config.contains("name = \"codex\""));
        assert!(cloud_config.contains("name = \"gemini\""));
        assert!(cloud_config.contains("name = \"installer\""));
    }

    #[test]
    fn test_install_tests_workflow_covers_all_public_cli_providers() {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        let workflow_path = repo_root.join(".github/workflows/install-tests.yml");
        let workflow = fs::read_to_string(&workflow_path).expect("read install-tests workflow");

        assert!(workflow.contains("curl -fsSL \"$MIRROR_URL/claude-code/install.sh\" >/dev/null"));
        assert!(workflow.contains("curl -fsSL \"$MIRROR_URL/claude/install.sh\" >/dev/null"));
        assert!(workflow.contains("curl -fsSL \"$MIRROR_URL/codex/install.sh\" >/dev/null"));
        assert!(workflow.contains("curl -fsSL \"$MIRROR_URL/gemini/install.sh\" >/dev/null"));
        assert!(workflow.contains("Invoke-WebRequest -Uri \"$env:MIRROR_URL/claude-code/install.sh\" -UseBasicParsing | Out-Null"));
        assert!(workflow.contains("Invoke-WebRequest -Uri \"$env:MIRROR_URL/claude/install.sh\" -UseBasicParsing | Out-Null"));
        assert!(workflow.contains("Invoke-WebRequest -Uri \"$env:MIRROR_URL/codex/install.sh\" -UseBasicParsing | Out-Null"));
        assert!(workflow.contains("Invoke-WebRequest -Uri \"$env:MIRROR_URL/gemini/install.sh\" -UseBasicParsing | Out-Null"));
        assert!(!workflow.contains("SKIP_CLAUDE: \"1\""));
        assert!(!workflow.contains("SKIP_GEMINI: \"1\""));
    }

    #[test]
    fn test_install_e2e_scripts_match_current_runtime_contracts() {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        let unix_script = fs::read_to_string(repo_root.join(".github/scripts/e2e-install-unix.sh"))
            .expect("read unix install e2e script");
        let windows_script =
            fs::read_to_string(repo_root.join(".github/scripts/e2e-install-windows.ps1"))
                .expect("read windows install e2e script");

        assert!(unix_script.contains("elif is_musl; then"));
        assert!(unix_script.contains("Skipping claude-code on musl"));
        assert!(unix_script.contains("run_cli \"claude-code\" \"claude-code\""));
        assert!(unix_script.contains("run_cli \"gemini\" \"gemini\""));
        assert!(!unix_script.contains("run_cli \"gemini\" \"gemini\" \"--yes\""));

        assert!(
            windows_script
                .contains("Run-Cli -Name \"claude-code\" -Bin \"$BinDir\\claude-code.exe\"")
        );
        assert!(windows_script.contains("Run-Cli -Name \"gemini\" -Bin \"$BinDir\\gemini.cmd\""));
        assert!(!windows_script.contains(
            "Run-Cli -Name \"gemini\" -Bin \"$BinDir\\gemini.cmd\" -UninstallArgs @(\"-Yes\")"
        ));
    }

    #[test]
    fn test_load_config_accepts_s3_with_minimal_required_fields() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"
[storage]
mode = "s3"

[storage.s3]
endpoint = "https://s3.example.com"
bucket = "bucket"
access_key_id = "ak"
secret_access_key = "sk"
region = "auto"
prefix = ""
path_style = true
expires_seconds = 900
multipart_max_parts = 20
multipart_part_max_attempts = 3

[[providers]]
name = "tool-a"
source = "github_release"
repo = "owner/tool-a"
tags = ["latest"]
"#
        )
        .unwrap();

        let config = Config::load(file.path()).unwrap();
        assert!(matches!(config.storage.mode, StorageMode::S3));
        assert_eq!(config.providers.len(), 1);
    }

    #[test]
    fn test_load_config_rejects_unknown_provider_fields() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"
[[providers]]
name = "tool-a"
source = "github_release"
repo = "owner/tool-a"
tags = ["latest"]
scripts = {{ install_sh = "scripts/install.sh" }}
"#
        )
        .unwrap();

        let err = Config::load(file.path()).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("unknown field"));
    }

    #[test]
    fn test_normalize_public_url_trims_and_strips_trailing_slash() {
        let url = normalize_public_url("  http://example.com/  ").unwrap();
        assert_eq!(url, "http://example.com");
    }

    #[test]
    fn test_normalize_public_url_rejects_unsafe_characters() {
        assert!(normalize_public_url("http://example.com/\nfoo").is_err());
        assert!(normalize_public_url("http://example.com/\"bad\"").is_err());
        assert!(normalize_public_url("http://example.com/$bad").is_err());
        assert!(normalize_public_url("http://example.com/`bad`").is_err());
    }
}

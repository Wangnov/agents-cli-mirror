use thiserror::Error;

#[derive(Error, Debug)]
pub enum MirrorError {
    #[error("Configuration error: {0}")]
    Io(#[from] std::io::Error),

    #[error("HTTP request error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON parsing error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("TOML parsing error: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("Version not found: {0}")]
    VersionNotFound(String),

    #[error("Platform not found: {0}")]
    PlatformNotFound(String),

    #[error("Checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },

    #[error("Provider error: {0}")]
    Provider(String),
}

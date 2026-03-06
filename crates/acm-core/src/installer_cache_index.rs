use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Index of cached `acm-installer` artifacts on disk.
///
/// This is a data model only; cache layout and IO are implemented by callers.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct InstallerCacheIndex {
    /// RFC3339 timestamp string when this index was generated (optional).
    #[serde(default)]
    pub generated_at: Option<String>,

    /// version -> platform -> entry
    #[serde(default)]
    pub versions: BTreeMap<String, BTreeMap<String, InstallerCacheEntry>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstallerCacheEntry {
    /// Cached archive filename (e.g. `acm-installer-x86_64-apple-darwin.tar.xz`).
    pub filename: String,

    /// Expected sha256 checksum (lowercase hex).
    pub sha256: String,

    /// Expected file size in bytes (0 if unknown).
    pub size: u64,

    /// Absolute or relative path to the cached archive on disk.
    pub path: String,

    /// RFC3339 timestamp string when this entry was written (optional).
    #[serde(default)]
    pub cached_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installer_cache_index_toml_roundtrip() {
        let mut platforms = BTreeMap::new();
        platforms.insert(
            "x86_64-apple-darwin".to_string(),
            InstallerCacheEntry {
                filename: "acm-installer-x86_64-apple-darwin.tar.xz".to_string(),
                sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                    .to_string(),
                size: 123,
                path: "/tmp/acm-installer/cache/installer/0.1.15/x86_64-apple-darwin/acm-installer-x86_64-apple-darwin.tar.xz".to_string(),
                cached_at: Some("2026-01-01T00:00:00Z".to_string()),
            },
        );

        let mut index = InstallerCacheIndex {
            generated_at: Some("2026-01-01T00:00:01Z".to_string()),
            ..Default::default()
        };
        index.versions.insert("0.1.15".to_string(), platforms);

        let text = toml::to_string_pretty(&index).expect("serialize toml");
        let parsed: InstallerCacheIndex = toml::from_str(&text).expect("parse toml");
        assert_eq!(parsed, index);
    }
}

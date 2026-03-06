use crate::cache::ProviderMetadata;

pub(crate) fn provider_checksums_json(provider: &ProviderMetadata) -> serde_json::Value {
    let mut checksums = serde_json::Map::new();

    for (version, version_meta) in &provider.versions {
        let mut platforms = serde_json::Map::new();
        for (platform, platform_meta) in &version_meta.platforms {
            let mut entry = serde_json::Map::new();
            entry.insert(
                "sha256".to_string(),
                serde_json::Value::String(platform_meta.sha256.clone()),
            );
            entry.insert(
                "size".to_string(),
                serde_json::Value::Number(serde_json::Number::from(platform_meta.size)),
            );
            entry.insert(
                "filename".to_string(),
                serde_json::Value::String(platform_meta.filename.clone()),
            );

            if !platform_meta.files.is_empty() {
                if let Ok(files_value) = serde_json::to_value(&platform_meta.files) {
                    entry.insert("files".to_string(), files_value);
                }
            }

            platforms.insert(platform.clone(), serde_json::Value::Object(entry));
        }
        checksums.insert(version.clone(), serde_json::Value::Object(platforms));
    }

    serde_json::Value::Object(checksums)
}

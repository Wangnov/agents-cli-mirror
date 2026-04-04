use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::sync::Arc;
use tracing::{info, warn};

use crate::cache::{CacheManager, ProviderMetadata};
use crate::config::{Config, StorageMode};
use crate::providers::GenericProvider;
use crate::s3;
use crate::server::scripts::{ScriptCommand, ScriptFlavor};
use crate::storage_clients::StorageClients;

const INSTALLER_BINARY_NAME: &str = "acm-installer";

fn matches_provider_filter(filter: Option<&str>, name: &str) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    if filter == "all" {
        return true;
    }
    filter == name
}

fn stable_fallback_tag(tags: &mut BTreeMap<String, String>) {
    if tags.contains_key("stable") {
        return;
    }
    if let Some(latest) = tags.get("latest").cloned() {
        tags.insert("stable".to_string(), latest);
    }
}

async fn put_text(
    storage_clients: &StorageClients,
    config: &Config,
    object_key: &str,
    content_type: &str,
    content: String,
) -> Result<()> {
    match config.storage.mode {
        StorageMode::Local => {
            let path = config
                .cache
                .dir
                .join("published")
                .join(object_key.replace('/', std::path::MAIN_SEPARATOR_STR));
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&path, content.as_bytes()).await?;
            Ok(())
        }
        StorageMode::S3 => {
            let Some(client) = storage_clients.s3() else {
                bail!("storage.mode = \"s3\" but S3 client is not initialized");
            };
            s3::put_bytes_with_client(
                client,
                &config.storage.s3,
                object_key,
                content_type,
                content.into_bytes(),
            )
            .await?;
            Ok(())
        }
    }
}

async fn put_json_pretty(
    storage_clients: &StorageClients,
    config: &Config,
    object_key: &str,
    value: serde_json::Value,
) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(&value)?;
    match config.storage.mode {
        StorageMode::Local => {
            let path = config
                .cache
                .dir
                .join("published")
                .join(object_key.replace('/', std::path::MAIN_SEPARATOR_STR));
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&path, bytes).await?;
            Ok(())
        }
        StorageMode::S3 => {
            let Some(client) = storage_clients.s3() else {
                bail!("storage.mode = \"s3\" but S3 client is not initialized");
            };
            s3::put_bytes_with_client(
                client,
                &config.storage.s3,
                object_key,
                "application/json",
                bytes,
            )
            .await?;
            Ok(())
        }
    }
}

struct PublishScriptContext<'a> {
    storage_clients: &'a StorageClients,
    config: &'a Config,
    installer_provider: &'a str,
    mirror_url: &'a str,
}

async fn publish_command_script(
    ctx: &PublishScriptContext<'_>,
    provider_name: Option<&str>,
    command: ScriptCommand,
    flavor: ScriptFlavor,
    object_key: &str,
) -> Result<()> {
    let script = crate::server::scripts::render_bootstrap_script(
        command,
        provider_name,
        flavor,
        Some(ctx.mirror_url),
        ctx.installer_provider,
        INSTALLER_BINARY_NAME,
    )
    .map_err(|status| anyhow::anyhow!("render {} failed: {}", object_key, status))?;

    put_text(
        ctx.storage_clients,
        ctx.config,
        object_key,
        flavor.content_type(),
        script,
    )
    .await
}

pub async fn publish(
    mut config: Config,
    cache: CacheManager,
    provider_filter: Option<&str>,
) -> Result<()> {
    if let Some(public_url) = config.server.public_url.clone() {
        config.server.public_url = Some(crate::config::normalize_public_url(&public_url)?);
    }

    if !matches!(config.storage.mode, StorageMode::S3) {
        warn!("publish is running in local mode; objects will be written under cache/published/");
    }

    let cache = Arc::new(cache);
    let storage_clients = StorageClients::new(&config.storage).await?;

    let installer_provider = config.server.installer_provider.clone();
    let providers = config
        .providers
        .iter()
        .filter(|p| p.enabled)
        .map(|p| p.name.clone())
        .collect::<Vec<_>>();

    if !providers.iter().any(|name| name == &installer_provider) {
        bail!(
            "installer provider '{}' is not configured; set [server].installer_provider and add matching [[providers]] entry",
            installer_provider
        );
    }

    if let Some(filter) = provider_filter
        && filter != "all"
        && !providers.iter().any(|p| p == filter)
    {
        bail!("Unknown provider: {filter}");
    }

    let include_installer =
        matches!(provider_filter, Some(filter) if filter != "all" && filter != installer_provider);

    let all_metadata = cache.get_metadata().await;

    for provider_cfg in config.providers.iter().filter(|p| p.enabled) {
        let provider_name = provider_cfg.name.as_str();
        if !(matches_provider_filter(provider_filter, provider_name)
            || (include_installer && provider_name == installer_provider))
        {
            continue;
        }

        info!("Publishing provider: {}", provider_name);

        let empty_meta = ProviderMetadata::default();
        let provider_meta = match all_metadata.provider(provider_name) {
            Some(meta) => meta,
            None => {
                warn!(
                    "No cache metadata found for provider '{}'; publishing empty api payloads",
                    provider_name
                );
                &empty_meta
            }
        };

        let provider = GenericProvider::new(
            provider_cfg.clone(),
            cache.clone(),
            config.storage.clone(),
            storage_clients.clone(),
            config.http.clone(),
        )?;
        let provider_info = provider.get_info().await;
        let checksums = crate::api::provider_checksums_json(provider_meta);
        let mut versions = provider_meta.versions.keys().cloned().collect::<Vec<_>>();
        versions.sort();
        let versions_json = serde_json::to_value(&versions)?;

        let mirror_url = config
            .server
            .public_url
            .as_deref()
            .context("server.public_url is required for publish")?;
        let installer_provider = config.server.installer_provider.as_str();
        let script_ctx = PublishScriptContext {
            storage_clients: &storage_clients,
            config: &config,
            installer_provider,
            mirror_url,
        };

        let public_name = provider_name;
        let mut tags: BTreeMap<String, String> = provider_meta.tags.clone().into_iter().collect();
        stable_fallback_tag(&mut tags);
        for (tag, version) in &tags {
            let object_key = format!("{}/{}", public_name, tag);
            put_text(
                &storage_clients,
                &config,
                &object_key,
                "text/plain",
                format!("{}\n", version),
            )
            .await?;
        }

        put_json_pretty(
            &storage_clients,
            &config,
            &format!("api/{}/checksums", public_name),
            checksums.clone(),
        )
        .await?;

        put_json_pretty(
            &storage_clients,
            &config,
            &format!("api/{}/versions", public_name),
            versions_json.clone(),
        )
        .await?;

        put_json_pretty(
            &storage_clients,
            &config,
            &format!("api/{}/info", public_name),
            provider_info.clone(),
        )
        .await?;

        publish_command_script(
            &script_ctx,
            Some(provider_name),
            ScriptCommand::Install,
            ScriptFlavor::Sh,
            &format!("{}/install.sh", public_name),
        )
        .await?;
        publish_command_script(
            &script_ctx,
            Some(provider_name),
            ScriptCommand::Install,
            ScriptFlavor::Ps1,
            &format!("{}/install.ps1", public_name),
        )
        .await?;
        publish_command_script(
            &script_ctx,
            Some(provider_name),
            ScriptCommand::Install,
            ScriptFlavor::Sh,
            &format!("install/{}", public_name),
        )
        .await?;
        publish_command_script(
            &script_ctx,
            Some(provider_name),
            ScriptCommand::Update,
            ScriptFlavor::Sh,
            &format!("{}/update.sh", public_name),
        )
        .await?;
        publish_command_script(
            &script_ctx,
            Some(provider_name),
            ScriptCommand::Update,
            ScriptFlavor::Ps1,
            &format!("{}/update.ps1", public_name),
        )
        .await?;
        publish_command_script(
            &script_ctx,
            Some(provider_name),
            ScriptCommand::Update,
            ScriptFlavor::Sh,
            &format!("update/{}", public_name),
        )
        .await?;
        publish_command_script(
            &script_ctx,
            Some(provider_name),
            ScriptCommand::Uninstall,
            ScriptFlavor::Sh,
            &format!("{}/uninstall.sh", public_name),
        )
        .await?;
        publish_command_script(
            &script_ctx,
            Some(provider_name),
            ScriptCommand::Uninstall,
            ScriptFlavor::Ps1,
            &format!("{}/uninstall.ps1", public_name),
        )
        .await?;
        publish_command_script(
            &script_ctx,
            Some(provider_name),
            ScriptCommand::Uninstall,
            ScriptFlavor::Sh,
            &format!("uninstall/{}", public_name),
        )
        .await?;
    }

    let mirror_url = config
        .server
        .public_url
        .as_deref()
        .context("server.public_url is required for publish")?;
    let installer_provider = config.server.installer_provider.as_str();
    let script_ctx = PublishScriptContext {
        storage_clients: &storage_clients,
        config: &config,
        installer_provider,
        mirror_url,
    };

    publish_command_script(
        &script_ctx,
        None,
        ScriptCommand::Status,
        ScriptFlavor::Sh,
        "status",
    )
    .await?;
    publish_command_script(
        &script_ctx,
        None,
        ScriptCommand::Status,
        ScriptFlavor::Ps1,
        "status.ps1",
    )
    .await?;
    publish_command_script(
        &script_ctx,
        None,
        ScriptCommand::Doctor,
        ScriptFlavor::Sh,
        "doctor",
    )
    .await?;
    publish_command_script(
        &script_ctx,
        None,
        ScriptCommand::Doctor,
        ScriptFlavor::Ps1,
        "doctor.ps1",
    )
    .await?;

    Ok(())
}

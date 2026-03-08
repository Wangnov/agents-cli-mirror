use std::collections::BTreeSet;

use super::*;

pub(super) fn command_install(
    ctx: &InstallContext,
    config: &LocalConfig,
    args: InstallArgs,
) -> Result<()> {
    let provider = normalize_provider(&args.provider)?;
    let mirror = ctx
        .mirror_url
        .as_deref()
        .filter(|value| !value.trim().is_empty());
    let mut state = load_state(ctx)?;
    let installed = state
        .installed
        .get(&provider)
        .filter(|installed| installed_provider_is_healthy(ctx, &provider, installed))
        .cloned();
    let configured_theme = configured_provider_theme(config, &provider);
    let theme = configured_theme
        .or_else(|| {
            state
                .installed
                .get(&provider)
                .and_then(|item| item.ui_preset.as_deref())
                .and_then(Theme::from_preset)
        })
        .unwrap_or(Theme::Acm);
    let ui = Ui::new(theme);
    ui.banner(&format!("Install {}", provider));

    let tag = args.tag.unwrap_or_else(|| "latest".to_string());
    let version_override = args.version.clone();
    let version = match mirror {
        Some(mirror_url) => resolve_version_from_mirror(
            ctx,
            mirror_url,
            &provider,
            &tag,
            version_override.as_deref(),
        )?,
        None => resolve_version_from_upstream(
            config,
            &provider,
            &tag,
            version_override.as_deref(),
            &ui,
        )?,
    };

    if args.check {
        if let Some(installed) = installed.as_ref() {
            if installed.version == version {
                ui.success(&format!("{} {}", provider, installed.version));
            } else {
                ui.update(&format!(
                    "{}: {} -> {}",
                    provider, installed.version, version
                ));
            }
        } else {
            ui.warn(&format!("{}: none -> {}", provider, version));
        }
        return Ok(());
    }

    if let Some(installed) = installed.as_ref()
        && installed.version == version
        && !args.upgrade
    {
        setup_path(&ctx.bin_dir, &provider, args.no_modify_path, &ui)?;
        ui.success(&format!("{} already at {}", provider, version));
        return Ok(());
    }

    let archive_path = match mirror {
        Some(mirror_url) => resolve_archive_from_mirror(ctx, mirror_url, &provider, &version, &ui)?,
        None => resolve_archive_from_upstream(config, &provider, &version, &ui)?,
    };

    let extracting = tr(output().lang, "extracting");
    let install_result = run_with_spinner(&ui, extracting, || {
        engine_install_from_archive(&InstallRequest {
            provider: &provider,
            version: &version,
            archive_path: &archive_path,
            install_dir: &ctx.install_dir,
            bin_dir: &ctx.bin_dir,
        })
    })?;

    state.installed.insert(
        provider.clone(),
        InstalledProviderState {
            version: version.clone(),
            tag: tag.clone(),
            installed_at: now_rfc3339(),
            executable: install_result.executable.to_string_lossy().to_string(),
            install_path: install_result.install_root.to_string_lossy().to_string(),
            ui_preset: Some(theme.preset_name().to_string()),
        },
    );
    set_runtime_record(
        &mut state,
        RuntimeOp::Install,
        RuntimeRecord {
            at: now_rfc3339(),
            ok: true,
            provider: Some(provider.clone()),
            version: Some(version.clone()),
            tag: Some(tag.clone()),
            detail: None,
        },
    );
    save_state(ctx, &state)?;

    record_event(
        &provider,
        &version,
        &tag,
        Some(install_result.bin_path.clone()),
    );
    setup_path(&ctx.bin_dir, &provider, args.no_modify_path, &ui)?;
    ui.success(&format!("{} installed {}", provider, version));

    if !args.no_modify_path {
        ui.info(&format!("binary path: {}", ctx.bin_dir.display()));
    }

    ui.complete();
    Ok(())
}

fn resolve_version_from_mirror(
    ctx: &InstallContext,
    mirror_url: &str,
    provider: &str,
    tag: &str,
    version_override: Option<&str>,
) -> Result<String> {
    match version_override {
        Some(version) => Ok(version.to_string()),
        None => fetch_text_retry(
            &ctx.client,
            ctx.retries,
            &format!("{}/{}/{}", mirror_url, provider, tag),
        ),
    }
}

fn resolve_archive_from_mirror(
    ctx: &InstallContext,
    mirror_url: &str,
    provider: &str,
    version: &str,
    ui: &Ui,
) -> Result<PathBuf> {
    let platform = detect_platform()?;
    let checksums = fetch_checksums(&ctx.client, ctx.retries, mirror_url, provider)?;
    let (asset_name, asset_meta) =
        select_asset_for_platform(&checksums, version, &platform, provider)?;

    let download_url = format!(
        "{}/{}/{}/files/{}",
        mirror_url,
        provider,
        version,
        encode_filepath(&asset_name)
    );
    let cached_path = engine_cache_path_for(&ctx.cache_dir, provider, version, &asset_name);
    ensure_downloaded(
        ctx,
        &download_url,
        &cached_path,
        &asset_meta.sha256,
        asset_meta.size,
        ui,
    )
}

pub(super) fn resolve_version_from_upstream(
    config: &LocalConfig,
    provider: &str,
    tag: &str,
    version_override: Option<&str>,
    ui: &Ui,
) -> Result<String> {
    if configured_provider(config, provider).is_none() {
        bail!(
            "provider '{}' is not configured; set --mirror-url or add it in config.toml",
            provider
        );
    }

    ui.info("syncing provider from upstream");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("create tokio runtime failed")?;

    let cfg = config.clone();
    let provider_name = provider.to_string();
    let tag_name = tag.to_string();
    let override_version = version_override.map(ToString::to_string);
    runtime.block_on(async move {
        let sync_cache = acm_server::cache::CacheManager::new(&cfg.cache)?;
        acm_server::server::sync_provider_once(cfg.clone(), sync_cache, &provider_name).await?;

        if let Some(version) = override_version {
            return Ok(version);
        }

        let inspect_cache = acm_server::cache::CacheManager::new(&cfg.cache)?;
        inspect_cache
            .read_tag(&provider_name, &tag_name)
            .await
            .ok_or_else(|| {
                anyhow!(
                    "tag '{}' not found for provider '{}'",
                    tag_name,
                    provider_name
                )
            })
    })
}

pub(super) fn resolve_archive_from_upstream(
    config: &LocalConfig,
    provider: &str,
    version: &str,
    ui: &Ui,
) -> Result<PathBuf> {
    let platform = detect_platform()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("create tokio runtime failed")?;

    let cfg = config.clone();
    let provider_name = provider.to_string();
    let version_name = version.to_string();
    let platform_name = platform.clone();
    let (artifact_path, expected_sha256, expected_size) = runtime.block_on(async move {
        let cache = acm_server::cache::CacheManager::new(&cfg.cache)?;
        let selected = cache
            .with_provider_metadata(&provider_name, |meta| {
                let version_meta = meta.versions.get(&version_name)?;
                let platform_meta = version_meta
                    .platforms
                    .get(&platform_name)
                    .or_else(|| version_meta.platforms.get("universal"))
                    .or_else(|| version_meta.platforms.values().next())?;

                if platform_meta.files.is_empty() {
                    return Some((
                        platform_meta.filename.clone(),
                        platform_meta.sha256.clone(),
                        platform_meta.size,
                    ));
                }

                let mut files = platform_meta.files.iter().collect::<Vec<_>>();
                files.sort_by(|a, b| a.0.cmp(b.0));
                let (name, file_meta) = files.into_iter().next()?;
                Some((name.clone(), file_meta.sha256.clone(), file_meta.size))
            })
            .await
            .flatten()
            .ok_or_else(|| {
                anyhow!(
                    "no cached artifact metadata found for provider='{}' version='{}'",
                    provider_name,
                    version_name
                )
            })?;

        let file_path = cache
            .version_path(&provider_name, &version_name)
            .join("files")
            .join(&selected.0);
        Ok::<(PathBuf, String, u64), anyhow::Error>((file_path, selected.1, selected.2))
    })?;

    if !artifact_path.exists() {
        bail!(
            "artifact not found in local cache: {} (direct upstream mode requires local cache files)",
            artifact_path.display()
        );
    }

    if !verify_file_sha256(&artifact_path, &expected_sha256)? {
        bail!(
            "cached artifact checksum mismatch: {}",
            artifact_path.display()
        );
    }
    if expected_size > 0 && fs::metadata(&artifact_path)?.len() != expected_size {
        bail!("cached artifact size mismatch: {}", artifact_path.display());
    }

    ui.info(&format!(
        "using cached upstream artifact: {}",
        artifact_path.display()
    ));
    Ok(artifact_path)
}

pub(super) fn command_update(
    ctx: &InstallContext,
    config: &LocalConfig,
    args: UpdateArgs,
) -> Result<()> {
    let provider = args.provider.trim().to_string();
    if provider == "all" {
        let mut providers = BTreeSet::new();
        let state = load_state(ctx)?;
        providers.extend(state.installed.keys().cloned());
        providers.extend(
            config
                .providers
                .iter()
                .filter(|provider| provider.enabled)
                .map(|provider| provider.name.clone()),
        );

        let mut updated = Vec::new();
        for provider in providers {
            command_install(
                ctx,
                config,
                InstallArgs {
                    provider: provider.clone(),
                    tag: Some("latest".to_string()),
                    version: None,
                    upgrade: true,
                    check: false,
                    no_modify_path: true,
                },
            )?;
            updated.push(provider);
        }
        let mut state = load_state(ctx)?;
        set_runtime_record(
            &mut state,
            RuntimeOp::Update,
            RuntimeRecord {
                at: now_rfc3339(),
                ok: true,
                provider: Some("all".to_string()),
                version: None,
                tag: Some("latest".to_string()),
                detail: Some(format!("updated {} provider(s)", updated.len())),
            },
        );
        save_state(ctx, &state)?;
        return Ok(());
    }

    command_install(
        ctx,
        config,
        InstallArgs {
            provider,
            tag: Some("latest".to_string()),
            version: None,
            upgrade: true,
            check: false,
            no_modify_path: true,
        },
    )?;
    let mut state = load_state(ctx)?;
    set_runtime_record(
        &mut state,
        RuntimeOp::Update,
        RuntimeRecord {
            at: now_rfc3339(),
            ok: true,
            provider: Some(args.provider.trim().to_string()),
            version: None,
            tag: Some("latest".to_string()),
            detail: None,
        },
    );
    save_state(ctx, &state)?;
    Ok(())
}

pub(super) fn command_uninstall(
    ctx: &InstallContext,
    config: &LocalConfig,
    args: UninstallArgs,
) -> Result<()> {
    let provider = normalize_provider(&args.provider)?;
    let mut state = load_state(ctx)?;
    let theme = state
        .installed
        .get(&provider)
        .and_then(|item| item.ui_preset.as_deref())
        .and_then(Theme::from_preset)
        .or_else(|| configured_provider_theme(config, &provider))
        .unwrap_or(Theme::Acm);
    let ui = Ui::new(theme);
    ui.banner(&format!("Uninstall {}", provider));

    let installed = state
        .installed
        .remove(&provider)
        .ok_or_else(|| anyhow!("{} is not installed", provider))?;

    let uninstall_result = engine_uninstall(&UninstallRequest {
        provider: &provider,
        install_dir: &ctx.install_dir,
        install_path: Path::new(&installed.install_path),
        bin_dir: &ctx.bin_dir,
    })?;
    cleanup_path(&ctx.bin_dir, &ctx.install_dir, &provider, &ui)?;

    set_runtime_record(
        &mut state,
        RuntimeOp::Uninstall,
        RuntimeRecord {
            at: now_rfc3339(),
            ok: true,
            provider: Some(provider.clone()),
            version: Some(installed.version),
            tag: Some(installed.tag),
            detail: Some(format!(
                "install_removed={}, bin_removed={}, bin_path={}",
                uninstall_result.install_removed,
                uninstall_result.bin_removed,
                uninstall_result.bin_path.display()
            )),
        },
    );
    save_state(ctx, &state)?;
    ui.success(&format!("{} removed", provider));
    ui.complete();
    Ok(())
}

pub(super) fn command_status(ctx: &InstallContext, args: StatusArgs) -> Result<()> {
    let state = load_state(ctx)?;

    if output().json {
        let value = if let Some(provider) = args.provider {
            let key = normalize_provider(&provider)?;
            serde_json::json!({
                "provider": key,
                "installed": state.installed.get(&key),
                "auto_update": state.auto_update.get(&key).copied().unwrap_or(false),
                "runtime": state.runtime.clone(),
            })
        } else {
            serde_json::json!({
                "installed": state.installed,
                "auto_update": state.auto_update,
                "runtime": state.runtime.clone(),
            })
        };
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    if let Some(provider) = args.provider {
        let key = normalize_provider(&provider)?;
        if let Some(item) = state.installed.get(&key) {
            println!(
                "{}: version={}, tag={}, installed_at={}, auto_update={}",
                key,
                item.version,
                item.tag,
                item.installed_at,
                state.auto_update.get(&key).copied().unwrap_or(false)
            );
        } else {
            println!("{}: not installed", key);
        }
        return Ok(());
    }

    if state.installed.is_empty() {
        println!("no provider installed");
        return Ok(());
    }

    for (provider, item) in state.installed {
        println!(
            "{}: version={}, tag={}, installed_at={}, auto_update={}",
            provider,
            item.version,
            item.tag,
            item.installed_at,
            state.auto_update.get(&provider).copied().unwrap_or(false)
        );
    }

    Ok(())
}

pub(super) fn command_doctor(ctx: &InstallContext, config: &LocalConfig) -> Result<()> {
    let mut checks = Vec::new();
    let state = load_state(ctx).unwrap_or_default();

    checks.push(check_doctor(
        "platform_detection",
        detect_platform().map(|platform| format!("platform={platform}")),
    ));

    checks.push(check_doctor(
        "install_dir_writable",
        ensure_writable_dir(&ctx.install_dir).map(|_| ctx.install_dir.display().to_string()),
    ));

    checks.push(check_doctor(
        "cache_dir_writable",
        ensure_writable_dir(&ctx.cache_dir).map(|_| ctx.cache_dir.display().to_string()),
    ));

    checks.push(check_doctor(
        "state_file_access",
        check_state_file(ctx).map(|_| ctx.state_path.display().to_string()),
    ));

    checks.push(check_doctor(
        "bin_dir_in_path",
        check_bin_in_path(&ctx.bin_dir),
    ));

    checks.push(check_doctor(
        "command_resolution",
        check_command_resolution(ctx, &state),
    ));

    if let Some(mirror) = &ctx.mirror_url {
        checks.push(check_doctor(
            "mirror_health",
            check_mirror_health(&ctx.client, ctx.retries, mirror),
        ));
    } else if config.providers.iter().any(|provider| provider.enabled) {
        checks.push(DoctorCheck {
            name: "mirror_health".to_string(),
            ok: true,
            detail: "mirror url is not configured; using upstream-direct mode".to_string(),
        });
    } else {
        checks.push(DoctorCheck {
            name: "mirror_health".to_string(),
            ok: false,
            detail: "mirror url is not configured".to_string(),
        });
    }

    let ok = checks.iter().all(|check| check.ok);
    let report = DoctorReport { ok, checks };

    if output().json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        for check in &report.checks {
            let icon = if check.ok { "[OK]" } else { "[FAIL]" };
            println!("{} {} - {}", icon, check.name, check.detail);
        }
    }

    {
        let mut state = state;
        set_runtime_record(
            &mut state,
            RuntimeOp::Doctor,
            RuntimeRecord {
                at: now_rfc3339(),
                ok: report.ok,
                provider: None,
                version: None,
                tag: None,
                detail: Some(if report.ok {
                    "doctor passed".to_string()
                } else {
                    "doctor found issues".to_string()
                }),
            },
        );
        let _ = save_state(ctx, &state);
    }

    if report.ok {
        Ok(())
    } else {
        bail!("doctor found issues")
    }
}

pub(super) fn command_import(
    ctx: &InstallContext,
    config: &LocalConfig,
    args: ImportArgs,
) -> Result<()> {
    let provider = normalize_provider(&args.provider)?;
    let version = args.version.clone();
    let source = args.from.clone();
    let tag = args.tag.clone().unwrap_or_else(|| "imported".to_string());
    let cfg = config.clone();
    let provider_name = provider.clone();
    let import_result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("create tokio runtime failed")?
        .block_on(async move {
            import_provider_version(&cfg, &provider_name, &version, &source, Some(tag.as_str()))
                .await
        })?;

    let mut state = load_state(ctx)?;
    set_runtime_record(
        &mut state,
        RuntimeOp::Import,
        RuntimeRecord {
            at: now_rfc3339(),
            ok: true,
            provider: Some(provider.clone()),
            version: Some(args.version.clone()),
            tag: Some(import_result.tag.clone()),
            detail: Some(format!(
                "source={}, files={}, bytes={}",
                args.from.display(),
                import_result.file_count,
                import_result.total_bytes
            )),
        },
    );
    save_state(ctx, &state)?;
    println!(
        "imported {} {} (tag={}, files={}, bytes={})",
        provider,
        args.version,
        import_result.tag,
        import_result.file_count,
        import_result.total_bytes
    );
    Ok(())
}

pub(super) fn command_auto_update(
    ctx: &InstallContext,
    config: &LocalConfig,
    args: AutoUpdateArgs,
) -> Result<()> {
    let mut state = load_state(ctx)?;

    match args.command {
        AutoUpdateSubcommand::Enable(target) => {
            apply_auto_update_toggle(&mut state, config, &target.provider, true)?;
            save_state(ctx, &state)?;
            println!("auto-update enabled for {}", target.provider);
        }
        AutoUpdateSubcommand::Disable(target) => {
            apply_auto_update_toggle(&mut state, config, &target.provider, false)?;
            save_state(ctx, &state)?;
            println!("auto-update disabled for {}", target.provider);
        }
        AutoUpdateSubcommand::Status(target) => {
            print_auto_update_status(&state, target.provider.as_deref())?;
        }
        AutoUpdateSubcommand::Run(target) => {
            let providers = resolve_auto_update_targets(&state, config, target.provider.as_deref());
            for provider in providers {
                command_update(
                    ctx,
                    config,
                    UpdateArgs {
                        provider: provider.clone(),
                    },
                )?;
            }
        }
    }

    Ok(())
}

pub(super) fn command_providers(
    ctx: &InstallContext,
    config: &LocalConfig,
    args: ProvidersArgs,
) -> Result<()> {
    let mirror = require_mirror_url(ctx)?;

    match args.command {
        ProvidersSubcommand::List => {
            for provider in config.providers.iter().filter(|provider| provider.enabled) {
                println!("{}", provider.name);
            }
            Ok(())
        }
        ProvidersSubcommand::Info(args) => {
            let provider = normalize_provider(&args.provider)?;
            let value = fetch_json_retry::<serde_json::Value>(
                &ctx.client,
                ctx.retries,
                &format!("{}/api/{}/info", mirror, provider),
            )?;
            println!("{}", serde_json::to_string_pretty(&value)?);
            Ok(())
        }
    }
}

fn apply_auto_update_toggle(
    state: &mut ClientState,
    config: &LocalConfig,
    provider: &str,
    enabled: bool,
) -> Result<()> {
    if provider == "all" {
        let providers = resolve_all_providers(state, config);
        for provider in providers {
            state.auto_update.insert(provider, enabled);
        }
        return Ok(());
    }

    let normalized = normalize_provider(provider)?;
    state.auto_update.insert(normalized, enabled);
    Ok(())
}

fn print_auto_update_status(state: &ClientState, provider: Option<&str>) -> Result<()> {
    if let Some(provider) = provider {
        let normalized = normalize_provider(provider)?;
        println!(
            "{}: {}",
            normalized,
            state.auto_update.get(&normalized).copied().unwrap_or(false)
        );
        return Ok(());
    }

    if state.auto_update.is_empty() {
        println!("auto-update: none");
        return Ok(());
    }

    for (provider, enabled) in &state.auto_update {
        println!("{}: {}", provider, enabled);
    }

    Ok(())
}

fn resolve_auto_update_targets(
    state: &ClientState,
    config: &LocalConfig,
    provider: Option<&str>,
) -> Vec<String> {
    if let Some(provider) = provider {
        if provider == "all" {
            return state
                .auto_update
                .iter()
                .filter_map(|(provider, enabled)| {
                    if *enabled {
                        Some(provider.clone())
                    } else {
                        None
                    }
                })
                .collect();
        }
        return vec![provider.to_string()];
    }

    if !state.auto_update.is_empty() {
        return state
            .auto_update
            .iter()
            .filter_map(|(provider, enabled)| {
                if *enabled {
                    Some(provider.clone())
                } else {
                    None
                }
            })
            .collect();
    }

    resolve_all_providers(state, config)
}

fn resolve_all_providers(state: &ClientState, config: &LocalConfig) -> Vec<String> {
    let mut providers = BTreeSet::new();
    providers.extend(state.installed.keys().cloned());
    providers.extend(
        config
            .providers
            .iter()
            .filter(|provider| provider.enabled)
            .map(|provider| provider.name.clone()),
    );
    providers.into_iter().collect()
}

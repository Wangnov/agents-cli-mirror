mod commands;
mod transport;
mod ui;

use acm_core::client_state::{
    ClientState, InstalledProviderState, RuntimeRecord, load_client_state, save_client_state,
};
use acm_core::config::{
    Config as LocalConfig, DynamicProviderConfig, ProviderUiPreset as SharedProviderUiPreset,
};
use acm_core::install_engine::{
    InstallRequest, UninstallRequest, bin_dir as engine_bin_dir,
    cache_path_for as engine_cache_path_for, install_from_archive as engine_install_from_archive,
    uninstall_provider as engine_uninstall,
};
use acm_server::importer::import_provider_version;
use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand};
use reqwest::blocking::Client;
use reqwest::{Proxy, StatusCode};
use serde::Serialize;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, TryRecvError};
use std::thread;
use std::time::Duration;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use transport::{
    encode_filepath, ensure_downloaded, fetch_checksums, select_asset_for_platform,
    verify_file_sha256,
};
use ui::{Theme, Ui, detect_lang, emit_json, init_output, output, record_event, tr};

#[derive(Args, Debug, Clone)]
struct GlobalArgs {
    #[arg(long, global = true, default_value = "config.toml")]
    config: PathBuf,

    #[arg(long, global = true, default_value = "__MIRROR_URL__")]
    mirror_url: String,

    #[arg(long, global = true)]
    install_dir: Option<PathBuf>,

    #[arg(long, global = true)]
    cache_dir: Option<PathBuf>,

    #[arg(long, global = true, default_value_t = 3)]
    retries: u32,

    #[arg(long, global = true, default_value_t = 10)]
    connect_timeout_secs: u64,

    #[arg(long, global = true, default_value_t = 300)]
    timeout_secs: u64,

    #[arg(long, global = true)]
    proxy: Option<String>,

    #[arg(long, global = true)]
    no_proxy: bool,

    #[arg(long, global = true)]
    json: bool,

    #[arg(long, global = true)]
    lang: Option<String>,
}

#[derive(Parser, Debug)]
#[command(name = "acm", version, about = "ACM local client")]
struct Cli {
    #[command(flatten)]
    global: GlobalArgs,

    #[command(subcommand)]
    command: CommandGroup,
}

#[derive(Subcommand, Debug)]
enum CommandGroup {
    Install(InstallArgs),
    Update(UpdateArgs),
    Uninstall(UninstallArgs),
    Status(StatusArgs),
    Doctor,
    Import(ImportArgs),
    #[command(name = "auto-update")]
    AutoUpdate(AutoUpdateArgs),
    Providers(ProvidersArgs),
}

#[derive(Parser, Debug)]
#[command(name = "acm-installer", version, about = "ACM bootstrap installer")]
struct InstallerCli {
    #[command(flatten)]
    global: GlobalArgs,

    #[command(subcommand)]
    command: InstallerCommandGroup,
}

#[derive(Subcommand, Debug)]
enum InstallerCommandGroup {
    Install(InstallArgs),
    Update(UpdateArgs),
    Uninstall(UninstallArgs),
    Status(StatusArgs),
    Doctor,
}

#[derive(Args, Debug, Clone)]
struct InstallArgs {
    provider: String,

    #[arg(long)]
    tag: Option<String>,

    #[arg(long)]
    version: Option<String>,

    #[arg(long)]
    upgrade: bool,

    #[arg(long)]
    check: bool,

    #[arg(long)]
    no_modify_path: bool,
}

#[derive(Args, Debug, Clone)]
struct UpdateArgs {
    provider: String,
}

#[derive(Args, Debug, Clone)]
struct UninstallArgs {
    provider: String,
}

#[derive(Args, Debug, Clone)]
struct StatusArgs {
    #[arg(long)]
    provider: Option<String>,
}

#[derive(Args, Debug, Clone)]
struct ImportArgs {
    provider: String,

    #[arg(long)]
    version: String,

    #[arg(long)]
    from: PathBuf,

    #[arg(long)]
    tag: Option<String>,
}

#[derive(Args, Debug, Clone)]
struct AutoUpdateArgs {
    #[command(subcommand)]
    command: AutoUpdateSubcommand,
}

#[derive(Subcommand, Debug, Clone)]
enum AutoUpdateSubcommand {
    Enable(TargetProvider),
    Disable(TargetProvider),
    Status(TargetProviderOptional),
    Run(TargetProviderOptional),
}

#[derive(Args, Debug, Clone)]
struct TargetProvider {
    provider: String,
}

#[derive(Args, Debug, Clone)]
struct TargetProviderOptional {
    provider: Option<String>,
}

#[derive(Args, Debug, Clone)]
struct ProvidersArgs {
    #[command(subcommand)]
    command: ProvidersSubcommand,
}

#[derive(Subcommand, Debug, Clone)]
enum ProvidersSubcommand {
    List,
    Info(ProviderInfoArgs),
}

#[derive(Args, Debug, Clone)]
struct ProviderInfoArgs {
    provider: String,
}

#[derive(Clone)]
struct InstallContext {
    mirror_url: Option<String>,
    install_dir: PathBuf,
    cache_dir: PathBuf,
    state_path: PathBuf,
    client: Client,
    retries: u32,
}

#[derive(Debug, Serialize)]
struct DoctorCheck {
    name: String,
    ok: bool,
    detail: String,
}

#[derive(Debug, Serialize)]
struct DoctorReport {
    ok: bool,
    checks: Vec<DoctorCheck>,
}

pub fn run_client_entry() -> i32 {
    finalize_run(run_client())
}

pub fn run_installer_entry() -> i32 {
    finalize_run(run_installer())
}

fn finalize_run(result: Result<()>) -> i32 {
    match result {
        Ok(()) => {
            if output().json {
                emit_json(true, None);
            }
            0
        }
        Err(err) => {
            if output().json {
                emit_json(false, Some(err.to_string()));
            } else {
                eprintln!("error: {err}");
            }
            1
        }
    }
}

fn run_client() -> Result<()> {
    let cli = Cli::parse();
    run_with_args(cli.global, cli.command)
}

fn run_installer() -> Result<()> {
    let cli = InstallerCli::parse();
    let command = match cli.command {
        InstallerCommandGroup::Install(args) => CommandGroup::Install(args),
        InstallerCommandGroup::Update(args) => CommandGroup::Update(args),
        InstallerCommandGroup::Uninstall(args) => CommandGroup::Uninstall(args),
        InstallerCommandGroup::Status(args) => CommandGroup::Status(args),
        InstallerCommandGroup::Doctor => CommandGroup::Doctor,
    };
    run_with_args(cli.global, command)
}

fn run_with_args(global: GlobalArgs, command: CommandGroup) -> Result<()> {
    let lang = detect_lang(global.lang.as_deref());
    init_output(global.json, lang);

    let config = LocalConfig::load(&global.config)?;

    let resolved_mirror = resolve_mirror_url(&global, &config);

    let install_dir = match global.install_dir {
        Some(path) => expand_tilde(path)?,
        None => default_install_dir()?,
    };
    let cache_dir = match global.cache_dir {
        Some(path) => expand_tilde(path)?,
        None => install_dir.join("cache"),
    };

    fs::create_dir_all(&install_dir)?;
    fs::create_dir_all(&cache_dir)?;

    let state_path = global
        .config
        .parent()
        .map(|dir| dir.join("state.toml"))
        .unwrap_or_else(|| install_dir.join("state.toml"));

    let client = build_client(
        global.proxy.as_deref(),
        global.no_proxy,
        global.connect_timeout_secs,
        global.timeout_secs,
    )?;

    let ctx = InstallContext {
        mirror_url: resolved_mirror,
        install_dir,
        cache_dir,
        state_path,
        client,
        retries: global.retries,
    };

    match command {
        CommandGroup::Install(args) => commands::command_install(&ctx, &config, args),
        CommandGroup::Update(args) => commands::command_update(&ctx, &config, args),
        CommandGroup::Uninstall(args) => commands::command_uninstall(&ctx, &config, args),
        CommandGroup::Status(args) => commands::command_status(&ctx, args),
        CommandGroup::Doctor => commands::command_doctor(&ctx, &config),
        CommandGroup::Import(args) => commands::command_import(&ctx, &config, args),
        CommandGroup::AutoUpdate(args) => commands::command_auto_update(&ctx, &config, args),
        CommandGroup::Providers(args) => commands::command_providers(&ctx, &config, args),
    }
}

fn resolve_mirror_url(global: &GlobalArgs, config: &LocalConfig) -> Option<String> {
    let env_mirror = env::var("MIRROR_URL").ok();
    resolve_mirror_url_with_env(global, config, env_mirror.as_deref())
}

fn resolve_mirror_url_with_env(
    global: &GlobalArgs,
    config: &LocalConfig,
    env_mirror_url: Option<&str>,
) -> Option<String> {
    if !global.mirror_url.contains("__MIRROR_URL__") {
        return Some(global.mirror_url.clone());
    }

    if let Some(env_url) = env_mirror_url
        && !env_url.trim().is_empty()
    {
        return Some(env_url.to_string());
    }

    if let Some(default_mirror) = config.client.default_mirror_url.as_ref()
        && !default_mirror.trim().is_empty()
    {
        return Some(default_mirror.clone());
    }

    config.server.public_url.clone()
}

enum RuntimeOp {
    Install,
    Update,
    Uninstall,
    Import,
    Doctor,
}

fn set_runtime_record(state: &mut ClientState, op: RuntimeOp, record: RuntimeRecord) {
    match op {
        RuntimeOp::Install => state.runtime.last_install = Some(record),
        RuntimeOp::Update => state.runtime.last_update = Some(record),
        RuntimeOp::Uninstall => state.runtime.last_uninstall = Some(record),
        RuntimeOp::Import => state.runtime.last_import = Some(record),
        RuntimeOp::Doctor => state.runtime.last_doctor = Some(record),
    }
}

fn configured_provider<'a>(
    config: &'a LocalConfig,
    provider: &str,
) -> Option<&'a DynamicProviderConfig> {
    config
        .providers
        .iter()
        .find(|item| item.enabled && item.name == provider)
}

fn configured_provider_theme(config: &LocalConfig, provider: &str) -> Option<Theme> {
    configured_provider(config, provider).map(|item| provider_preset_to_theme(item.ui.preset))
}

fn provider_preset_to_theme(preset: SharedProviderUiPreset) -> Theme {
    match preset {
        SharedProviderUiPreset::Acm => Theme::Acm,
        SharedProviderUiPreset::Codex => Theme::Codex,
        SharedProviderUiPreset::Claude => Theme::Claude,
        SharedProviderUiPreset::Gemini => Theme::Gemini,
    }
}

fn build_client(
    proxy: Option<&str>,
    no_proxy: bool,
    connect_timeout_secs: u64,
    timeout_secs: u64,
) -> Result<Client> {
    let mut builder = Client::builder()
        .connect_timeout(Duration::from_secs(connect_timeout_secs))
        .timeout(Duration::from_secs(timeout_secs))
        .user_agent("acm-client");

    if no_proxy {
        builder = builder.no_proxy();
    } else if let Some(proxy_url) = proxy {
        builder = builder.proxy(Proxy::all(proxy_url)?);
    }

    builder.build().context("build http client")
}

fn load_state(ctx: &InstallContext) -> Result<ClientState> {
    load_client_state(&ctx.state_path)
}

fn save_state(ctx: &InstallContext, state: &ClientState) -> Result<()> {
    save_client_state(&ctx.state_path, state)
}

fn run_with_spinner<T, F>(ui: &Ui, label: &str, op: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send,
    T: Send,
{
    if !ui.is_interactive() {
        return op();
    }
    let Some(mut spinner) = ui.spinner(label) else {
        return op();
    };

    thread::scope(|scope| -> Result<T> {
        let (tx, rx) = mpsc::channel();
        scope.spawn(move || {
            let _ = tx.send(op());
        });

        loop {
            match rx.try_recv() {
                Ok(result) => {
                    if result.is_ok() {
                        spinner.finish_ok();
                    } else {
                        spinner.finish_err();
                    }
                    return result;
                }
                Err(TryRecvError::Empty) => {
                    spinner.tick();
                    thread::sleep(Duration::from_millis(120));
                }
                Err(TryRecvError::Disconnected) => {
                    spinner.finish_err();
                    return Err(anyhow!("spinner worker disconnected"));
                }
            }
        }
    })
}

fn request_with_retry<T, F>(retries: u32, mut f: F) -> Result<T>
where
    F: FnMut() -> Result<T, reqwest::Error>,
{
    let max_attempts = retries.max(1);
    let mut attempt = 0;
    loop {
        attempt += 1;
        match f() {
            Ok(value) => return Ok(value),
            Err(err) if attempt < max_attempts => {
                std::thread::sleep(Duration::from_millis((attempt as u64) * 300));
                let _ = err;
                continue;
            }
            Err(err) => return Err(err.into()),
        }
    }
}

fn fetch_text_retry(client: &Client, retries: u32, url: &str) -> Result<String> {
    let response = request_with_retry(retries, || client.get(url).send())?;
    if response.status() == StatusCode::NOT_FOUND {
        bail!("not found: {}", url);
    }
    if !response.status().is_success() {
        bail!("request failed {}: {}", url, response.status());
    }
    Ok(response.text()?.trim().to_string())
}

fn fetch_json_retry<T: serde::de::DeserializeOwned>(
    client: &Client,
    retries: u32,
    url: &str,
) -> Result<T> {
    let response = request_with_retry(retries, || client.get(url).send())?;
    if response.status() == StatusCode::NOT_FOUND {
        bail!("not found: {}", url);
    }
    if !response.status().is_success() {
        bail!("request failed {}: {}", url, response.status());
    }
    Ok(response.json::<T>()?)
}

fn require_mirror_url(ctx: &InstallContext) -> Result<&str> {
    ctx.mirror_url
        .as_deref()
        .filter(|url| !url.trim().is_empty())
        .ok_or_else(|| anyhow!("mirror url is required, set --mirror-url or MIRROR_URL"))
}

fn normalize_provider(input: &str) -> Result<String> {
    let provider = input.trim().to_lowercase();
    if provider.is_empty() {
        bail!("provider is empty");
    }
    if provider.chars().any(|ch| {
        !(ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_' | '.'))
    }) {
        bail!("invalid provider name: {}", input);
    }
    Ok(provider)
}

fn detect_platform() -> Result<String> {
    let os = env::consts::OS;
    let arch = env::consts::ARCH;

    let platform = match (os, arch) {
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("linux", "x86_64") => {
            if is_musl() {
                "x86_64-unknown-linux-musl"
            } else {
                "x86_64-unknown-linux-gnu"
            }
        }
        ("linux", "aarch64") => {
            if is_musl() {
                "aarch64-unknown-linux-musl"
            } else {
                "aarch64-unknown-linux-gnu"
            }
        }
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        _ => bail!("unsupported platform {}-{}", os, arch),
    };

    Ok(platform.to_string())
}

fn is_musl() -> bool {
    #[cfg(target_os = "linux")]
    {
        if let Ok(output) = std::process::Command::new("ldd").arg("--version").output() {
            return String::from_utf8_lossy(&output.stdout).contains("musl")
                || String::from_utf8_lossy(&output.stderr).contains("musl");
        }
    }
    false
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn ensure_writable_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    let probe = path.join(".acm-write-probe");
    fs::write(&probe, b"ok")?;
    fs::remove_file(&probe)?;
    Ok(())
}

fn check_state_file(ctx: &InstallContext) -> Result<()> {
    if let Some(parent) = ctx.state_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if !ctx.state_path.exists() {
        fs::write(
            &ctx.state_path,
            toml::to_string_pretty(&ClientState::default())?,
        )?;
    }
    let _ = fs::read_to_string(&ctx.state_path)?;
    Ok(())
}

fn check_bin_in_path(bin_dir: PathBuf) -> Result<()> {
    let Some(path) = env::var_os("PATH") else {
        bail!("PATH is not set");
    };

    if env::split_paths(&path).any(|item| item == bin_dir) {
        return Ok(());
    }

    bail!("{} is not in PATH", bin_dir.display())
}

fn check_mirror_health(client: &Client, retries: u32, mirror_url: &str) -> Result<String> {
    let url = format!("{}/health", mirror_url.trim_end_matches('/'));
    let response = request_with_retry(retries, || client.get(&url).send())?;
    if !response.status().is_success() {
        bail!("{} -> {}", url, response.status());
    }
    Ok(format!("{} -> {}", url, response.status()))
}

fn check_doctor(name: &str, result: Result<String>) -> DoctorCheck {
    match result {
        Ok(detail) => DoctorCheck {
            name: name.to_string(),
            ok: true,
            detail,
        },
        Err(err) => DoctorCheck {
            name: name.to_string(),
            ok: false,
            detail: err.to_string(),
        },
    }
}

fn expand_tilde(path: PathBuf) -> Result<PathBuf> {
    let value = path.to_string_lossy();
    if !value.starts_with('~') {
        return Ok(path);
    }

    let home = dirs_home()?;
    if value == "~" {
        return Ok(home);
    }

    let rest = value.trim_start_matches("~/");
    Ok(home.join(rest))
}

fn default_install_dir() -> Result<PathBuf> {
    let home = dirs_home()?;
    Ok(home.join(".acm"))
}

fn dirs_home() -> Result<PathBuf> {
    if let Ok(home) = env::var("HOME") {
        return Ok(PathBuf::from(home));
    }
    if let Ok(profile) = env::var("USERPROFILE") {
        return Ok(PathBuf::from(profile));
    }
    bail!("cannot resolve home directory")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{
        command_import, resolve_archive_from_upstream, resolve_version_from_upstream,
    };
    use acm_core::config::{
        CacheConfig, DynamicProviderConfig, ProviderSource, ProviderUiConfig, ProviderUpdatePolicy,
    };
    use acm_server::cache::CacheManager;
    use tempfile::TempDir;

    fn static_provider(name: &str, version: &str, file: &str) -> DynamicProviderConfig {
        DynamicProviderConfig {
            name: name.to_string(),
            enabled: true,
            source: ProviderSource::Static,
            tags: vec!["latest".to_string()],
            update_policy: ProviderUpdatePolicy::Manual,
            platforms: Vec::new(),
            include_prerelease: false,
            files: vec![file.to_string()],
            repo: None,
            upstream_url: None,
            static_version: Some(version.to_string()),
            ui: ProviderUiConfig::default(),
        }
    }

    fn static_config(cache_dir: &Path, provider: &str, version: &str, file: &str) -> LocalConfig {
        LocalConfig {
            cache: CacheConfig {
                dir: cache_dir.to_path_buf(),
                max_versions: 10,
            },
            providers: vec![static_provider(provider, version, file)],
            ..LocalConfig::default()
        }
    }

    fn seed_static_artifact(config: &LocalConfig, provider: &str, version: &str, file: &str) {
        let path = config
            .cache
            .dir
            .join(provider)
            .join("versions")
            .join(version)
            .join("files")
            .join(file);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent directory");
        }
        fs::write(path, b"static-artifact").expect("write artifact");
    }

    #[test]
    fn resolve_version_from_upstream_static_provider() {
        init_output(false, detect_lang(None));

        let temp_dir = TempDir::new().expect("create temp dir");
        let config = static_config(temp_dir.path(), "tool-a", "1.2.3", "artifact.bin");
        seed_static_artifact(&config, "tool-a", "1.2.3", "artifact.bin");

        let ui = Ui::new(Theme::Acm);
        let version = resolve_version_from_upstream(&config, "tool-a", "latest", None, &ui)
            .expect("resolve version");
        assert_eq!(version, "1.2.3");
    }

    #[test]
    fn resolve_archive_from_upstream_static_provider() {
        init_output(false, detect_lang(None));

        let temp_dir = TempDir::new().expect("create temp dir");
        let config = static_config(temp_dir.path(), "tool-a", "1.2.3", "artifact.bin");
        seed_static_artifact(&config, "tool-a", "1.2.3", "artifact.bin");

        let ui = Ui::new(Theme::Acm);
        let version = resolve_version_from_upstream(&config, "tool-a", "latest", None, &ui)
            .expect("resolve version");
        let archive = resolve_archive_from_upstream(&config, "tool-a", &version, &ui)
            .expect("resolve archive");

        assert!(archive.exists());
        assert_eq!(
            archive.file_name().and_then(|name| name.to_str()),
            Some("artifact.bin")
        );
    }

    #[test]
    fn command_import_writes_mirror_cache_and_runtime_state() {
        init_output(false, detect_lang(None));

        let temp_dir = TempDir::new().expect("create temp dir");
        let cache_dir = temp_dir.path().join("mirror-cache");
        let install_dir = temp_dir.path().join("install-root");
        let source_dir = temp_dir.path().join("source");
        fs::create_dir_all(&cache_dir).expect("create cache dir");
        fs::create_dir_all(&install_dir).expect("create install dir");
        fs::create_dir_all(&source_dir).expect("create source dir");
        fs::write(source_dir.join("artifact.bin"), b"mirror-import").expect("write source file");

        let config = static_config(&cache_dir, "tool-a", "1.2.3", "artifact.bin");
        let ctx = InstallContext {
            mirror_url: None,
            install_dir: install_dir.clone(),
            cache_dir: install_dir.join("cache"),
            state_path: temp_dir.path().join("state.toml"),
            client: build_client(None, false, 3, 30).expect("build client"),
            retries: 1,
        };

        command_import(
            &ctx,
            &config,
            ImportArgs {
                provider: "tool-a".to_string(),
                version: "9.9.9".to_string(),
                from: source_dir,
                tag: Some("snapshot".to_string()),
            },
        )
        .expect("import should succeed");

        let cache = CacheManager::new(&config.cache).expect("create cache manager");
        let tag_version = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("create runtime")
            .block_on(async { cache.read_tag("tool-a", "snapshot").await });
        assert_eq!(tag_version.as_deref(), Some("9.9.9"));

        let cached_artifact = config
            .cache
            .dir
            .join("tool-a")
            .join("versions")
            .join("9.9.9")
            .join("files")
            .join("artifact.bin");
        assert!(cached_artifact.exists());

        let state = load_client_state(&ctx.state_path).expect("load state");
        assert!(!state.installed.contains_key("tool-a"));
        assert!(state.runtime.last_import.is_some());
        let runtime = state
            .runtime
            .last_import
            .expect("runtime import record should exist");
        assert_eq!(runtime.provider.as_deref(), Some("tool-a"));
        assert_eq!(runtime.version.as_deref(), Some("9.9.9"));
        assert_eq!(runtime.tag.as_deref(), Some("snapshot"));
    }

    fn make_global_args(mirror_url: &str) -> GlobalArgs {
        GlobalArgs {
            config: PathBuf::from("config.toml"),
            mirror_url: mirror_url.to_string(),
            install_dir: None,
            cache_dir: None,
            retries: 3,
            connect_timeout_secs: 10,
            timeout_secs: 30,
            proxy: None,
            no_proxy: false,
            json: false,
            lang: None,
        }
    }

    #[test]
    fn resolve_mirror_url_precedence() {
        let mut config = LocalConfig::default();
        config.server.public_url = Some("https://server.example.com".to_string());
        config.client.default_mirror_url = Some("https://client.example.com".to_string());

        let explicit = make_global_args("https://explicit.example.com");
        let mirror =
            resolve_mirror_url_with_env(&explicit, &config, Some("https://env.example.com"));
        assert_eq!(mirror.as_deref(), Some("https://explicit.example.com"));

        let placeholder = make_global_args("__MIRROR_URL__");
        let mirror =
            resolve_mirror_url_with_env(&placeholder, &config, Some("https://env.example.com"));
        assert_eq!(mirror.as_deref(), Some("https://env.example.com"));

        let mirror = resolve_mirror_url_with_env(&placeholder, &config, None);
        assert_eq!(mirror.as_deref(), Some("https://client.example.com"));

        config.client.default_mirror_url = None;
        let mirror = resolve_mirror_url_with_env(&placeholder, &config, None);
        assert_eq!(mirror.as_deref(), Some("https://server.example.com"));

        config.server.public_url = None;
        let mirror = resolve_mirror_url_with_env(&placeholder, &config, None);
        assert!(mirror.is_none());
    }
}

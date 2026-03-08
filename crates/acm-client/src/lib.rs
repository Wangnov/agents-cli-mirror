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
    InstallRequest, UninstallRequest, cache_path_for as engine_cache_path_for,
    install_from_archive as engine_install_from_archive, uninstall_provider as engine_uninstall,
};
use acm_server::importer::import_provider_version;
use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand};
use reqwest::blocking::Client;
use reqwest::{Proxy, StatusCode};
use serde::Serialize;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::process::Command;
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
    bin_dir: PathBuf,
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

    let explicit_install_dir = match global.install_dir {
        Some(path) => Some(expand_tilde(path)?),
        None => None,
    };
    let (install_dir, bin_dir) = resolve_install_and_bin_dirs(explicit_install_dir)?;
    let cache_dir = match global.cache_dir {
        Some(path) => expand_tilde(path)?,
        None => install_dir.join("cache"),
    };

    fs::create_dir_all(&install_dir)?;
    fs::create_dir_all(&cache_dir)?;

    let state_path = resolve_state_path(&install_dir);

    let client = build_client(
        global.proxy.as_deref(),
        global.no_proxy,
        global.connect_timeout_secs,
        global.timeout_secs,
    )?;

    let ctx = InstallContext {
        mirror_url: resolved_mirror,
        install_dir,
        bin_dir,
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

const PATH_BLOCK_START: &str = "# >>> agents-cli-mirror PATH >>>";
const PATH_BLOCK_END: &str = "# <<< agents-cli-mirror PATH <<<";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShellConfig {
    Posix,
    Fish,
}

fn ensure_writable_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    let probe = path.join(".acm-write-probe");
    fs::write(&probe, b"ok")?;
    fs::remove_file(&probe)?;
    Ok(())
}

fn provider_binary_name(provider: &str) -> String {
    if cfg!(windows) {
        format!("{provider}.exe")
    } else {
        provider.to_string()
    }
}

fn provider_binary_candidates(provider: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![format!("{provider}.exe"), format!("{provider}.cmd")]
    } else {
        vec![provider.to_string()]
    }
}

fn provider_bin_paths(bin_dir: &Path, provider: &str) -> Vec<PathBuf> {
    provider_binary_candidates(provider)
        .into_iter()
        .map(|binary| bin_dir.join(binary))
        .collect()
}

fn provider_bin_path(bin_dir: &Path, provider: &str) -> PathBuf {
    bin_dir.join(provider_binary_name(provider))
}

fn legacy_provider_bin_path(install_dir: &Path, provider: &str) -> PathBuf {
    install_dir.join("bin").join(provider_binary_name(provider))
}

fn installed_provider_is_healthy(
    ctx: &InstallContext,
    provider: &str,
    installed: &InstalledProviderState,
) -> bool {
    Path::new(&installed.install_path).exists()
        && Path::new(&installed.executable).exists()
        && provider_bin_paths(&ctx.bin_dir, provider)
            .iter()
            .any(|path| path.exists())
}

fn check_state_file(ctx: &InstallContext) -> Result<()> {
    if let Some(parent) = ctx.state_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if !ctx.state_path.exists() {
        save_client_state(&ctx.state_path, &ClientState::default())?;
    }
    let _ = fs::read_to_string(&ctx.state_path)?;
    Ok(())
}

fn normalize_path_for_compare(path: &Path) -> String {
    let value = path.to_string_lossy().to_string();
    if cfg!(windows) {
        value.to_ascii_lowercase()
    } else {
        value
    }
}

fn path_entries_contain(path_var: &OsStr, target: &Path) -> bool {
    let target = normalize_path_for_compare(target);
    env::split_paths(path_var).any(|entry| normalize_path_for_compare(&entry) == target)
}

fn path_contains(target: &Path) -> bool {
    env::var_os("PATH")
        .as_deref()
        .is_some_and(|path_var| path_entries_contain(path_var, target))
}

fn local_shim_dir(home: &Path) -> PathBuf {
    home.join(".local").join("bin")
}

fn shell_rc_file(home: &Path) -> (PathBuf, ShellConfig) {
    let shell = env::var("SHELL").unwrap_or_default();
    let name = Path::new(&shell)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("");

    match name {
        "bash" => (home.join(".bashrc"), ShellConfig::Posix),
        "zsh" => (home.join(".zshrc"), ShellConfig::Posix),
        "fish" => (
            home.join(".config").join("fish").join("config.fish"),
            ShellConfig::Fish,
        ),
        _ => (home.join(".profile"), ShellConfig::Posix),
    }
}

fn shell_rc_candidates(home: &Path) -> Vec<(PathBuf, ShellConfig)> {
    let mut candidates = Vec::new();
    let mut push = |path: PathBuf, shell: ShellConfig| {
        if !candidates.iter().any(|(existing, _)| *existing == path) {
            candidates.push((path, shell));
        }
    };

    let (preferred, preferred_kind) = shell_rc_file(home);
    push(preferred, preferred_kind);
    push(home.join(".bashrc"), ShellConfig::Posix);
    push(home.join(".zshrc"), ShellConfig::Posix);
    push(home.join(".zprofile"), ShellConfig::Posix);
    push(home.join(".profile"), ShellConfig::Posix);
    push(
        home.join(".config").join("fish").join("config.fish"),
        ShellConfig::Fish,
    );
    candidates
}

fn render_home_relative_path(home: &Path, path: &Path) -> String {
    if let Ok(relative) = path.strip_prefix(home) {
        let relative = relative.to_string_lossy().replace('\\', "/");
        return format!("$HOME/{relative}");
    }

    path.to_string_lossy().to_string()
}

fn shell_quote_single(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn render_path_block(home: &Path, bin_dir: &Path, shell: ShellConfig) -> String {
    let path_ref = render_home_relative_path(home, bin_dir);

    match shell {
        ShellConfig::Posix => {
            let dir_test = if path_ref.starts_with("$HOME/") {
                format!("\"{path_ref}\"")
            } else {
                shell_quote_single(&path_ref)
            };
            let export_expr = if path_ref.starts_with("$HOME/") {
                format!("\"{path_ref}:$PATH\"")
            } else {
                format!("{}:\"$PATH\"", shell_quote_single(&path_ref))
            };
            format!(
                "{PATH_BLOCK_START}\nif [ -d {dir_test} ]; then\n  export PATH={export_expr}\nfi\n{PATH_BLOCK_END}\n"
            )
        }
        ShellConfig::Fish => {
            let dir_expr = if path_ref.starts_with("$HOME/") {
                format!("\"{path_ref}\"")
            } else {
                shell_quote_single(&path_ref)
            };
            format!(
                "{PATH_BLOCK_START}\nif test -d {dir_expr}\n    fish_add_path -m {dir_expr}\nend\n{PATH_BLOCK_END}\n"
            )
        }
    }
}

fn strip_managed_path_block(content: &str) -> String {
    let mut lines = Vec::new();
    let mut skipping = false;
    for line in content.lines() {
        if line == PATH_BLOCK_START {
            skipping = true;
            continue;
        }
        if skipping && line == PATH_BLOCK_END {
            skipping = false;
            continue;
        }
        if !skipping {
            lines.push(line);
        }
    }

    let mut cleaned = lines.join("\n");
    if !cleaned.is_empty() {
        cleaned.push('\n');
    }
    cleaned
}

fn upsert_managed_path_block_content(
    content: &str,
    home: &Path,
    bin_dir: &Path,
    shell: ShellConfig,
) -> String {
    let cleaned = strip_managed_path_block(content);
    let cleaned = cleaned.trim_end();
    let block = render_path_block(home, bin_dir, shell);

    if cleaned.is_empty() {
        return block;
    }

    format!("{cleaned}\n\n{block}")
}

fn write_managed_path_block(
    rc_file: &Path,
    home: &Path,
    bin_dir: &Path,
    shell: ShellConfig,
) -> Result<bool> {
    if let Some(parent) = rc_file.parent() {
        fs::create_dir_all(parent)?;
    }

    let existing = fs::read_to_string(rc_file).unwrap_or_default();
    let updated = upsert_managed_path_block_content(&existing, home, bin_dir, shell);
    if updated == existing {
        return Ok(false);
    }

    fs::write(rc_file, updated)?;
    Ok(true)
}

fn remove_managed_path_blocks(home: &Path) -> Result<Vec<PathBuf>> {
    let mut removed = Vec::new();

    for (path, _) in shell_rc_candidates(home) {
        if !path.exists() {
            continue;
        }
        let existing = fs::read_to_string(&path)?;
        let updated = strip_managed_path_block(&existing);
        if updated != existing {
            fs::write(&path, updated)?;
            removed.push(path);
        }
    }

    Ok(removed)
}

#[cfg(unix)]
fn create_or_replace_symlink(target: &Path, link: &Path) -> Result<()> {
    use std::os::unix::fs::symlink;

    if fs::symlink_metadata(link).is_ok() {
        fs::remove_file(link)?;
    }
    symlink(target, link)?;
    Ok(())
}

#[cfg(unix)]
fn remove_symlink_if_points_to(link: &Path, target: &Path) -> Result<bool> {
    let metadata = match fs::symlink_metadata(link) {
        Ok(metadata) => metadata,
        Err(_) => return Ok(false),
    };
    if !metadata.file_type().is_symlink() {
        return Ok(false);
    }

    let link_target = fs::read_link(link)?;
    let resolved = if link_target.is_absolute() {
        link_target
    } else {
        link.parent()
            .unwrap_or_else(|| Path::new("."))
            .join(link_target)
    };

    if normalize_path_for_compare(&resolved) != normalize_path_for_compare(target) {
        return Ok(false);
    }

    fs::remove_file(link)?;
    Ok(true)
}

#[cfg(unix)]
fn setup_unix_path(bin_dir: &Path, provider: &str, ui: &Ui) -> Result<()> {
    let home = dirs_home()?;
    if path_contains(bin_dir) {
        return Ok(());
    }

    let shim_dir = local_shim_dir(&home);
    let binary = provider_binary_name(provider);
    if path_contains(&shim_dir) {
        fs::create_dir_all(&shim_dir)?;
        let shim_path = shim_dir.join(&binary);
        create_or_replace_symlink(&provider_bin_path(bin_dir, provider), &shim_path)?;
        ui.success(&format!(
            "{} {}",
            tr(ui.lang(), "symlink_created"),
            shim_path.display()
        ));
        return Ok(());
    }

    let (rc_file, shell) = shell_rc_file(&home);
    if write_managed_path_block(&rc_file, &home, bin_dir, shell)? {
        ui.info(&format!(
            "{} {}",
            tr(ui.lang(), "path_added"),
            rc_file.display()
        ));
        ui.warn(tr(ui.lang(), "restart_terminal"));
    }
    Ok(())
}

#[cfg(unix)]
fn cleanup_unix_path(bin_dir: &Path, install_dir: &Path, provider: &str, ui: &Ui) -> Result<()> {
    let home = dirs_home()?;
    let shim_path = local_shim_dir(&home).join(provider_binary_name(provider));
    let expected = provider_bin_path(bin_dir, provider);
    let legacy = legacy_provider_bin_path(install_dir, provider);

    if remove_symlink_if_points_to(&shim_path, &expected)?
        || remove_symlink_if_points_to(&shim_path, &legacy)?
    {
        ui.info(&format!(
            "{} {}",
            tr(ui.lang(), "symlink_removed"),
            shim_path.display()
        ));
    }

    for path in remove_managed_path_blocks(&home)? {
        ui.info(&format!(
            "{} {}",
            tr(ui.lang(), "path_removed"),
            path.display()
        ));
    }

    Ok(())
}

#[cfg(windows)]
fn read_windows_user_path() -> Result<String> {
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "[Environment]::GetEnvironmentVariable('Path','User')",
        ])
        .output()
        .context("read Windows user PATH failed")?;
    if !output.status.success() {
        bail!("read Windows user PATH failed");
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(windows)]
fn write_windows_user_path(path_value: &str) -> Result<()> {
    let escaped = path_value.replace('\'', "''");
    let script = format!("[Environment]::SetEnvironmentVariable('Path', '{escaped}', 'User')");
    let status = Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .status()
        .context("write Windows user PATH failed")?;
    if !status.success() {
        bail!("write Windows user PATH failed");
    }
    Ok(())
}

#[cfg(windows)]
fn split_windows_path(path_value: &str) -> Vec<String> {
    path_value
        .split(';')
        .filter(|entry| !entry.trim().is_empty())
        .map(ToString::to_string)
        .collect()
}

#[cfg(windows)]
fn setup_windows_path(bin_dir: &Path, ui: &Ui) -> Result<()> {
    let bin_dir = bin_dir
        .canonicalize()
        .unwrap_or_else(|_| bin_dir.to_path_buf());
    let bin_key = normalize_path_for_compare(&bin_dir);

    let current_user = read_windows_user_path().unwrap_or_default();
    let mut user_entries = split_windows_path(&current_user);
    if !user_entries
        .iter()
        .any(|entry| normalize_path_for_compare(Path::new(entry)) == bin_key)
    {
        user_entries.insert(0, bin_dir.display().to_string());
        let new_user_path = user_entries.join(";");
        write_windows_user_path(&new_user_path)?;
        ui.info(&format!(
            "{} {}",
            tr(ui.lang(), "path_added"),
            bin_dir.display()
        ));
        ui.warn(tr(ui.lang(), "restart_terminal"));
    }

    if !path_contains(&bin_dir) {
        let current = env::var("PATH").unwrap_or_default();
        let updated = if current.is_empty() {
            bin_dir.display().to_string()
        } else {
            format!("{};{current}", bin_dir.display())
        };
        unsafe {
            env::set_var("PATH", &updated);
        }
    }

    Ok(())
}

#[cfg(windows)]
fn cleanup_windows_path(bin_dir: &Path, ui: &Ui) -> Result<()> {
    let bin_dir = bin_dir
        .canonicalize()
        .unwrap_or_else(|_| bin_dir.to_path_buf());
    let bin_key = normalize_path_for_compare(&bin_dir);

    let current_user = read_windows_user_path().unwrap_or_default();
    let user_entries = split_windows_path(&current_user);
    let filtered_entries = user_entries
        .iter()
        .filter(|entry| normalize_path_for_compare(Path::new(entry)) != bin_key)
        .cloned()
        .collect::<Vec<_>>();

    if filtered_entries.len() != user_entries.len() {
        let new_user_path = filtered_entries.join(";");
        write_windows_user_path(&new_user_path)?;
        ui.info(&format!(
            "{} {}",
            tr(ui.lang(), "path_removed"),
            bin_dir.display()
        ));
        ui.warn(tr(ui.lang(), "restart_terminal"));
    }

    if let Some(path_var) = env::var_os("PATH") {
        let filtered = env::split_paths(&path_var)
            .filter(|entry| normalize_path_for_compare(entry) != bin_key)
            .collect::<Vec<_>>();
        let updated = env::join_paths(filtered).unwrap_or_default();
        unsafe {
            env::set_var("PATH", updated);
        }
    }

    Ok(())
}

fn setup_path(bin_dir: &Path, provider: &str, no_modify_path: bool, ui: &Ui) -> Result<()> {
    if no_modify_path {
        return Ok(());
    }

    #[cfg(windows)]
    {
        let _ = provider;
        return setup_windows_path(bin_dir, ui);
    }

    #[cfg(unix)]
    {
        return setup_unix_path(bin_dir, provider, ui);
    }

    #[allow(unreachable_code)]
    Ok(())
}

fn cleanup_path(bin_dir: &Path, install_dir: &Path, provider: &str, ui: &Ui) -> Result<()> {
    #[cfg(windows)]
    {
        let _ = install_dir;
        let _ = provider;
        return cleanup_windows_path(bin_dir, ui);
    }

    #[cfg(unix)]
    {
        return cleanup_unix_path(bin_dir, install_dir, provider, ui);
    }

    #[allow(unreachable_code)]
    Ok(())
}

fn find_command_in_path(binary: &str) -> Option<PathBuf> {
    let path_var = env::var_os("PATH")?;
    for entry in env::split_paths(&path_var) {
        let candidate = entry.join(binary);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn check_bin_in_path(bin_dir: &Path) -> Result<String> {
    if path_contains(bin_dir) {
        return Ok(format!("bin dir detected in PATH: {}", bin_dir.display()));
    }

    #[cfg(unix)]
    {
        let shim_dir = local_shim_dir(&dirs_home()?);
        if path_contains(&shim_dir) {
            return Ok(format!("shim dir detected in PATH: {}", shim_dir.display()));
        }
    }

    bail!("{} is not reachable from PATH", bin_dir.display())
}

fn check_command_resolution(ctx: &InstallContext, state: &ClientState) -> Result<String> {
    if state.installed.is_empty() {
        return Ok("no installed providers".to_string());
    }

    let mut details = Vec::new();
    #[cfg(unix)]
    let shim_dir = local_shim_dir(&dirs_home()?);

    for (provider, installed) in &state.installed {
        if !installed_provider_is_healthy(ctx, provider, installed) {
            bail!("{provider} state exists but installed files are missing");
        }

        let binary = provider_binary_name(provider);
        let actual = provider_binary_candidates(provider)
            .into_iter()
            .find_map(|candidate| find_command_in_path(&candidate))
            .ok_or_else(|| anyhow!("{provider} is not resolvable from PATH"))?;
        let expected_paths = provider_bin_paths(&ctx.bin_dir, provider);

        let matches_expected = expected_paths.iter().any(|expected| {
            normalize_path_for_compare(&actual) == normalize_path_for_compare(expected)
        }) || {
            #[cfg(unix)]
            {
                let shim = shim_dir.join(&binary);
                normalize_path_for_compare(&actual) == normalize_path_for_compare(&shim)
            }
            #[cfg(not(unix))]
            {
                false
            }
        };

        if !matches_expected {
            let expected_display = expected_paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(" or ");
            bail!(
                "{provider} resolves to {} instead of {}",
                actual.display(),
                expected_display
            );
        }

        details.push(format!("{provider} -> {}", actual.display()));
    }

    Ok(details.join(", "))
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

fn default_install_dir_from_home(home: &Path) -> PathBuf {
    home.join(".acm")
}

fn default_bin_dir_from_home(home: &Path) -> PathBuf {
    home.join(".agents").join("bin")
}

fn resolve_install_and_bin_dirs(
    explicit_install_dir: Option<PathBuf>,
) -> Result<(PathBuf, PathBuf)> {
    let home = dirs_home()?;
    resolve_install_and_bin_dirs_with_home(explicit_install_dir, &home)
}

fn resolve_install_and_bin_dirs_with_home(
    explicit_install_dir: Option<PathBuf>,
    home: &Path,
) -> Result<(PathBuf, PathBuf)> {
    Ok(match explicit_install_dir {
        Some(path) => {
            let install_dir = path;
            let bin_dir = install_dir.join("bin");
            (install_dir, bin_dir)
        }
        None => (
            default_install_dir_from_home(home),
            default_bin_dir_from_home(home),
        ),
    })
}

fn resolve_state_path(install_dir: &Path) -> PathBuf {
    install_dir.join("state.toml")
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
            bin_dir: install_dir.join("bin"),
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

    #[test]
    fn resolve_install_and_bin_dirs_default_bin_lives_under_agents() {
        let home = PathBuf::from("/tmp/test-home");
        let (install_dir, bin_dir) =
            resolve_install_and_bin_dirs_with_home(None, &home).expect("resolve default dirs");

        assert_eq!(install_dir, home.join(".acm"));
        assert_eq!(bin_dir, home.join(".agents").join("bin"));
    }

    #[test]
    fn resolve_install_and_bin_dirs_explicit_install_dir_keeps_local_bin() {
        let home = PathBuf::from("/tmp/test-home");
        let explicit = home.join("custom-root");
        let (install_dir, bin_dir) =
            resolve_install_and_bin_dirs_with_home(Some(explicit.clone()), &home)
                .expect("resolve explicit dirs");

        assert_eq!(install_dir, explicit);
        assert_eq!(bin_dir, home.join("custom-root").join("bin"));
    }

    #[test]
    fn resolve_state_path_defaults_to_install_dir_state_toml() {
        let install_dir = PathBuf::from("/tmp/test-home/.acm");
        let state_path = resolve_state_path(&install_dir);

        assert_eq!(state_path, install_dir.join("state.toml"));
    }

    #[test]
    fn resolve_state_path_uses_explicit_install_dir_root() {
        let install_dir = PathBuf::from("/tmp/test-home/custom-root");
        let state_path = resolve_state_path(&install_dir);

        assert_eq!(state_path, install_dir.join("state.toml"));
    }

    #[test]
    fn render_path_block_uses_home_relative_path_for_posix_shells() {
        let home = PathBuf::from("/tmp/test-home");
        let bin_dir = home.join(".agents").join("bin");

        let block = render_path_block(&home, &bin_dir, ShellConfig::Posix);

        assert!(block.contains("export PATH=\"$HOME/.agents/bin:$PATH\""));
    }

    #[test]
    fn upsert_managed_path_block_replaces_existing_block() {
        let home = PathBuf::from("/tmp/test-home");
        let bin_dir = home.join(".agents").join("bin");
        let original = format!(
            "line-1\n{}\nold\n{}\nline-2\n",
            PATH_BLOCK_START, PATH_BLOCK_END
        );

        let updated =
            upsert_managed_path_block_content(&original, &home, &bin_dir, ShellConfig::Posix);

        assert!(updated.contains("line-1"));
        assert!(updated.contains("line-2"));
        assert_eq!(updated.matches(PATH_BLOCK_START).count(), 1);
        assert!(updated.contains("export PATH=\"$HOME/.agents/bin:$PATH\""));
    }

    #[test]
    fn installed_provider_is_healthy_requires_current_bin_and_install_root() {
        let temp_dir = TempDir::new().expect("create temp dir");
        let install_dir = temp_dir.path().join(".acm");
        let bin_dir = temp_dir.path().join(".agents").join("bin");
        let install_path = install_dir.join("providers").join("codex").join("v1");
        let executable = install_path.join("codex");
        fs::create_dir_all(&install_path).expect("create install path");
        fs::create_dir_all(&bin_dir).expect("create bin dir");
        fs::write(&executable, b"codex").expect("write executable");
        fs::write(bin_dir.join("codex"), b"shim").expect("write bin");

        let installed = InstalledProviderState {
            version: "v1".to_string(),
            tag: "latest".to_string(),
            installed_at: "2026-01-01T00:00:00Z".to_string(),
            executable: executable.to_string_lossy().to_string(),
            install_path: install_path.to_string_lossy().to_string(),
            ui_preset: Some("acm".to_string()),
        };

        let ctx = InstallContext {
            mirror_url: None,
            install_dir: install_dir.clone(),
            bin_dir: bin_dir.clone(),
            cache_dir: install_dir.join("cache"),
            state_path: install_dir.join("state.toml"),
            client: build_client(None, false, 3, 30).expect("build client"),
            retries: 1,
        };

        assert!(installed_provider_is_healthy(&ctx, "codex", &installed));

        fs::remove_file(bin_dir.join("codex")).expect("remove bin");
        assert!(!installed_provider_is_healthy(&ctx, "codex", &installed));
    }
}

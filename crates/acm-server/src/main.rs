use acm_server::{cache, config, publish, server};
use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(name = "acm-server")]
#[command(about = "A configurable mirror server for release artifacts")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Serve(ServeArgs),
    Sync(SyncArgs),
    Publish(PublishArgs),
    ValidateConfig(ValidateConfigArgs),
}

#[derive(Args, Debug)]
struct ServeArgs {
    /// Path to config file
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,

    /// Override port
    #[arg(short, long)]
    port: Option<u16>,

    /// Override host
    #[arg(long)]
    host: Option<String>,

    /// Refresh cache on startup
    #[arg(long)]
    refresh: bool,
}

#[derive(Args, Debug)]
struct SyncArgs {
    /// Path to config file
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,

    /// Sync only one provider (default: all providers)
    #[arg(long)]
    provider: Option<String>,
}

#[derive(Args, Debug)]
struct PublishArgs {
    /// Path to config file
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,

    /// Publish only one provider (default: all providers)
    #[arg(long)]
    provider: Option<String>,
}

#[derive(Args, Debug)]
struct ValidateConfigArgs {
    /// Path to config file
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();

    match cli.command {
        Command::Serve(args) => run_serve(args).await,
        Command::Sync(args) => run_sync(args).await,
        Command::Publish(args) => run_publish(args).await,
        Command::ValidateConfig(args) => run_validate_config(args),
    }
}

fn init_tracing() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "acm_server=info,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();
}

async fn run_serve(args: ServeArgs) -> Result<()> {
    let mut config = config::Config::load(&args.config)?;

    if let Some(port) = args.port {
        config.server.port = port;
    }
    if let Some(host) = args.host {
        config.server.host = host;
    }

    info!(
        "Starting ACM Server on {}:{}",
        config.server.host, config.server.port
    );

    let mut skip_initial_sync = false;
    if args.refresh {
        info!("Refreshing cache on startup...");
        let refresh_cache = cache::CacheManager::new(&config.cache)?;
        match server::sync_once(config.clone(), refresh_cache).await {
            Ok(()) => skip_initial_sync = true,
            Err(e) => {
                error!("Refresh failed, continuing with normal startup: {}", e);
            }
        }
    }

    let cache_manager = cache::CacheManager::new(&config.cache)?;
    server::run(config, cache_manager, skip_initial_sync).await?;

    Ok(())
}

async fn run_sync(args: SyncArgs) -> Result<()> {
    let config = config::Config::load(&args.config)?;
    let cache_manager = cache::CacheManager::new(&config.cache)?;

    if let Some(provider) = args.provider {
        if provider == "all" {
            info!("Syncing all providers");
            return server::sync_once(config, cache_manager).await;
        }

        info!("Syncing provider: {}", provider);
        server::sync_provider_once(config, cache_manager, &provider).await?;
        return Ok(());
    }

    info!("Syncing all providers");
    server::sync_once(config, cache_manager).await
}

async fn run_publish(args: PublishArgs) -> Result<()> {
    let config = config::Config::load(&args.config)?;
    let cache_manager = cache::CacheManager::new(&config.cache)?;

    publish::publish(config, cache_manager, args.provider.as_deref()).await
}

fn run_validate_config(args: ValidateConfigArgs) -> Result<()> {
    let config = config::Config::load(&args.config)?;
    config.validate()?;
    println!("config is valid: {}", args.config.display());
    Ok(())
}

use serde::Deserialize;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Default, Deserialize)]
struct RootConfig {
    brand: Option<BrandConfig>,
}

#[derive(Default, Deserialize)]
struct BrandConfig {
    assets_file: Option<String>,
}

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let workspace_root = manifest_dir.join("../..");
    let config_path = workspace_root.join("config.toml");
    let default_assets = workspace_root.join("assets").join("brand.toml");

    let assets_path = resolve_assets_path(&config_path, &workspace_root, &default_assets);

    println!("cargo:rerun-if-changed={}", config_path.display());
    println!("cargo:rerun-if-changed={}", assets_path.display());
    println!(
        "cargo:rustc-env=BRAND_ASSETS_PATH={}",
        assets_path.display()
    );
}

fn resolve_assets_path(
    config_path: &Path,
    workspace_root: &Path,
    default_assets: &Path,
) -> PathBuf {
    if let Ok(value) = env::var("BRAND_ASSETS_FILE") {
        let path = PathBuf::from(value);
        return normalize_path(workspace_root, path);
    }

    if let Ok(text) = fs::read_to_string(config_path) {
        if let Ok(config) = toml::from_str::<RootConfig>(&text) {
            if let Some(path) = config
                .brand
                .and_then(|brand| brand.assets_file)
                .filter(|v| !v.trim().is_empty())
            {
                return normalize_path(workspace_root, PathBuf::from(path));
            }
        }
    }

    default_assets.to_path_buf()
}

fn normalize_path(workspace_root: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        workspace_root.join(path)
    }
}

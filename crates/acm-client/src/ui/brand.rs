use crate::ui::i18n::Lang;
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

const EMBEDDED_BRAND_TOML: &str = include_str!(env!("BRAND_ASSETS_PATH"));
const BRAND_FILE_NAME: &str = "brand.toml";
const BRAND_FILE_ENV: &str = "AGENTS_BRAND_FILE";

#[derive(Debug, Deserialize, Clone)]
pub struct BrandConfig {
    pub brand: BrandSection,
}

#[derive(Debug, Deserialize, Clone)]
pub struct BrandSection {
    pub name: String,
    pub app_name: String,
    pub service_name: String,
    pub app_url: String,
    pub service_url: String,
    pub banner: BannerConfig,
    pub logo: LogoConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct BannerConfig {
    pub zh: BannerText,
    pub en: BannerText,
}

#[derive(Debug, Deserialize, Clone)]
pub struct BannerText {
    pub welcome: String,
    pub tagline: String,
    pub gui_install: String,
    pub app_url_label: String,
    pub service_label: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LogoConfig {
    pub lines: Vec<String>,
}

impl BrandSection {
    pub fn banner(&self, lang: Lang) -> &BannerText {
        match lang {
            Lang::Zh => &self.banner.zh,
            Lang::En => &self.banner.en,
        }
    }

    pub fn has_app_url(&self) -> bool {
        !is_placeholder_url(&self.app_url)
    }

    pub fn has_service_url(&self) -> bool {
        !is_placeholder_url(&self.service_url)
    }
}

fn is_placeholder_url(url: &str) -> bool {
    matches!(
        url.trim(),
        "" | "https://example.com"
            | "http://example.com"
            | "https://mirror.example.com"
            | "http://mirror.example.com"
    )
}

static BRAND: OnceLock<BrandSection> = OnceLock::new();

pub fn brand() -> &'static BrandSection {
    BRAND.get_or_init(load_brand)
}

fn load_brand() -> BrandSection {
    let text = resolve_brand_file()
        .and_then(|path| fs::read_to_string(path).ok())
        .unwrap_or_else(|| EMBEDDED_BRAND_TOML.to_string());

    parse_brand(&text)
        .unwrap_or_else(|| parse_brand(EMBEDDED_BRAND_TOML).unwrap_or_else(default_brand))
}

fn resolve_brand_file() -> Option<PathBuf> {
    if let Ok(path) = std::env::var(BRAND_FILE_ENV) {
        let path = PathBuf::from(path);
        if path.exists() {
            return Some(path);
        }
    }

    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(dir) = exe_path.parent() {
            let candidate = dir.join(BRAND_FILE_NAME);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    None
}

fn parse_brand(text: &str) -> Option<BrandSection> {
    toml::from_str::<BrandConfig>(text).ok().map(|v| v.brand)
}

fn default_brand() -> BrandSection {
    BrandSection {
        name: "Agents".to_string(),
        app_name: "Agents APP".to_string(),
        service_name: "Agents CLI Mirror".to_string(),
        app_url: "https://example.com".to_string(),
        service_url: "https://mirror.example.com".to_string(),
        banner: BannerConfig {
            zh: BannerText {
                welcome: "欢迎使用 Agents APP 镜像 CLI 安装脚本".to_string(),
                tagline: "快速、安全、便捷的 CLI 工具安装服务".to_string(),
                gui_install: "你也可以在 Agents APP 中通过软件界面来安装".to_string(),
                app_url_label: "Agents APP 下载地址".to_string(),
                service_label: "欢迎使用 Agents CLI 镜像服务".to_string(),
            },
            en: BannerText {
                welcome: "Welcome to Agents APP Mirror CLI Installer".to_string(),
                tagline: "Fast, secure, and convenient CLI tool installation".to_string(),
                gui_install: "You can also install via Agents APP GUI".to_string(),
                app_url_label: "Agents APP Download".to_string(),
                service_label: "Welcome to Agents CLI Mirror Service".to_string(),
            },
        },
        logo: LogoConfig { lines: Vec::new() },
    }
}

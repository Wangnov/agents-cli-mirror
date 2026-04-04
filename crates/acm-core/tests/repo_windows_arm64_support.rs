use std::fs;
use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf()
}

fn load_toml(path: &Path) -> toml::Value {
    let content = fs::read_to_string(path).expect("read toml file");
    content.parse::<toml::Value>().expect("parse toml file")
}

fn provider<'a>(doc: &'a toml::Value, name: &str) -> &'a toml::Value {
    doc.get("providers")
        .and_then(toml::Value::as_array)
        .and_then(|providers| {
            providers.iter().find(|provider| {
                provider
                    .get("name")
                    .and_then(toml::Value::as_str)
                    .is_some_and(|value| value == name)
            })
        })
        .unwrap_or_else(|| panic!("provider {name} not found"))
}

fn string_array(value: &toml::Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(toml::Value::as_array)
        .unwrap_or_else(|| panic!("{key} should be an array"))
        .iter()
        .map(|item| {
            item.as_str()
                .unwrap_or_else(|| panic!("{key} should contain strings"))
                .to_string()
        })
        .collect()
}

#[test]
fn config_cloud_advertises_windows_arm64_for_supported_providers() {
    let doc = load_toml(&workspace_root().join("config.cloud.toml"));

    let claude = provider(&doc, "claude");
    let claude_files = string_array(claude, "files");
    let claude_platforms = string_array(claude, "platforms");
    assert!(claude_files.contains(&"win32-arm64/claude.exe".to_string()));
    assert!(claude_platforms.contains(&"win32-arm64".to_string()));

    let codex = provider(&doc, "codex");
    let codex_platforms = string_array(codex, "platforms");
    assert!(codex_platforms.contains(&"win32-arm64".to_string()));

    let gemini = provider(&doc, "gemini");
    let gemini_platforms = string_array(gemini, "platforms");
    assert!(gemini_platforms.contains(&"win32-arm64".to_string()));
}

#[test]
fn config_example_advertises_windows_arm64_for_supported_providers() {
    let doc = load_toml(&workspace_root().join("config.toml.example"));

    let claude = provider(&doc, "claude");
    let claude_files = string_array(claude, "files");
    let claude_platforms = string_array(claude, "platforms");
    assert!(claude_files.contains(&"win32-arm64/claude.exe".to_string()));
    assert!(claude_platforms.contains(&"win32-arm64".to_string()));

    let codex = provider(&doc, "codex");
    let codex_platforms = string_array(codex, "platforms");
    assert!(codex_platforms.contains(&"win32-arm64".to_string()));

    let gemini = provider(&doc, "gemini");
    let gemini_platforms = string_array(gemini, "platforms");
    assert!(gemini_platforms.contains(&"win32-arm64".to_string()));
}

#[test]
fn dist_workspace_releases_windows_arm64_installer_assets() {
    let doc = load_toml(&workspace_root().join("dist-workspace.toml"));

    let targets = doc
        .get("dist")
        .and_then(|dist| dist.get("targets"))
        .and_then(toml::Value::as_array)
        .expect("dist.targets should be present")
        .iter()
        .map(|item| item.as_str().expect("target should be a string"))
        .collect::<Vec<_>>();
    assert!(targets.contains(&"aarch64-pc-windows-msvc"));

    let runner = doc
        .get("dist")
        .and_then(|dist| dist.get("github-custom-runners"))
        .and_then(|table| table.get("aarch64-pc-windows-msvc"))
        .and_then(toml::Value::as_str);
    assert_eq!(runner, Some("windows-11-arm"));
}

#[test]
fn install_tests_cover_windows_arm64_runner() {
    let workflow = fs::read_to_string(workspace_root().join(".github/workflows/install-tests.yml"))
        .expect("read install-tests workflow");

    assert!(workflow.contains("windows-11-arm"));
}

#[test]
fn rust_ci_validates_dist_plan() {
    let workflow = fs::read_to_string(workspace_root().join(".github/workflows/rust-ci.yml"))
        .expect("read rust-ci workflow");

    assert!(workflow.contains("cargo dist plan --output-format=json"));
}

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct InstalledProviderState {
    pub version: String,
    pub tag: String,
    pub installed_at: String,
    pub executable: String,
    pub install_path: String,
    #[serde(default)]
    pub ui_preset: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct ClientState {
    #[serde(default)]
    pub installed: BTreeMap<String, InstalledProviderState>,
    #[serde(default)]
    pub auto_update: BTreeMap<String, bool>,
    #[serde(default)]
    pub runtime: RuntimeState,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct RuntimeState {
    #[serde(default)]
    pub last_install: Option<RuntimeRecord>,
    #[serde(default)]
    pub last_update: Option<RuntimeRecord>,
    #[serde(default)]
    pub last_uninstall: Option<RuntimeRecord>,
    #[serde(default)]
    pub last_import: Option<RuntimeRecord>,
    #[serde(default)]
    pub last_doctor: Option<RuntimeRecord>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RuntimeRecord {
    pub at: String,
    pub ok: bool,
    pub provider: Option<String>,
    pub version: Option<String>,
    pub tag: Option<String>,
    pub detail: Option<String>,
}

pub fn load_client_state(path: &Path) -> Result<ClientState> {
    if !path.exists() {
        return Ok(ClientState::default());
    }

    let text = fs::read_to_string(path)
        .with_context(|| format!("read state failed: {}", path.display()))?;
    toml::from_str(&text).context("parse state.toml failed")
}

pub fn save_client_state(path: &Path, state: &ClientState) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;

    let text = toml::to_string_pretty(state)?;
    let mut temp =
        tempfile::NamedTempFile::new_in(parent).context("create temporary state file failed")?;
    temp.write_all(text.as_bytes())
        .context("write temporary state file failed")?;
    temp.as_file_mut()
        .sync_all()
        .context("sync temporary state file failed")?;

    #[cfg(windows)]
    {
        if path.exists() {
            fs::remove_file(path)
                .with_context(|| format!("replace state failed: {}", path.display()))?;
        }
    }

    temp.persist(path)
        .map_err(|err| err.error)
        .with_context(|| format!("write state failed: {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;

    fn sample_state(version: &str) -> ClientState {
        let mut installed = BTreeMap::new();
        installed.insert(
            "tool-a".to_string(),
            InstalledProviderState {
                version: version.to_string(),
                tag: "latest".to_string(),
                installed_at: "2026-01-01T00:00:00Z".to_string(),
                executable: "/tmp/tool-a".to_string(),
                install_path: "/tmp/providers/tool-a".to_string(),
                ui_preset: Some("acm".to_string()),
            },
        );

        ClientState {
            installed,
            auto_update: BTreeMap::new(),
            runtime: RuntimeState::default(),
        }
    }

    #[test]
    fn state_roundtrip() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("state.toml");
        let state = sample_state("1.2.3");
        save_client_state(&path, &state).expect("save state");
        let loaded = load_client_state(&path).expect("load state");
        assert_eq!(
            loaded
                .installed
                .get("tool-a")
                .map(|item| item.version.as_str()),
            Some("1.2.3")
        );
    }

    #[test]
    fn concurrent_read_write_keeps_state_parseable() {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("state.toml");
        let keep_reading = Arc::new(AtomicBool::new(true));

        let reader_flag = keep_reading.clone();
        let reader_path = path.clone();
        let reader = thread::spawn(move || {
            while reader_flag.load(Ordering::Relaxed) {
                let _ = load_client_state(&reader_path).expect("reader load state");
                thread::sleep(Duration::from_millis(2));
            }
        });

        let mut writers = Vec::new();
        for index in 0..4 {
            let writer_path = path.clone();
            writers.push(thread::spawn(move || {
                for iter in 0..50 {
                    let version = format!("{}.{}", index, iter);
                    let state = sample_state(&version);
                    save_client_state(&writer_path, &state).expect("writer save state");
                    thread::sleep(Duration::from_millis(1));
                }
            }));
        }

        for writer in writers {
            writer.join().expect("writer thread panic");
        }
        keep_reading.store(false, Ordering::Relaxed);
        reader.join().expect("reader thread panic");

        let final_state = load_client_state(&path).expect("load final state");
        assert!(final_state.installed.contains_key("tool-a"));
    }
}

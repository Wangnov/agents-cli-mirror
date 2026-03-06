use crate::ui::banner;
use crate::ui::i18n::{Lang, detect_lang, tr};
use crate::ui::progress::{DownloadProgress, Spinner};
use crate::ui::style::Theme;
use serde::{Deserialize, Serialize};
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, Debug)]
pub struct OutputConfig {
    pub json: bool,
    pub lang: Lang,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct InstallEvent {
    pub provider: String,
    pub version: String,
    pub tag: String,
    pub path: Option<String>,
    pub status: String,
}

static OUTPUT: OnceLock<OutputConfig> = OnceLock::new();
static EVENTS: OnceLock<Mutex<Vec<InstallEvent>>> = OnceLock::new();

fn events() -> &'static Mutex<Vec<InstallEvent>> {
    EVENTS.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn init_output(json: bool, lang: Lang) {
    let _ = OUTPUT.set(OutputConfig { json, lang });
    let _ = EVENTS.set(Mutex::new(Vec::new()));
}

pub fn output() -> &'static OutputConfig {
    OUTPUT.get_or_init(|| OutputConfig {
        json: false,
        lang: detect_lang(None),
    })
}

pub fn record_event(provider: &str, version: &str, tag: &str, path: Option<PathBuf>) {
    let mut events = events().lock().unwrap_or_else(|e| e.into_inner());
    events.push(InstallEvent {
        provider: provider.to_string(),
        version: version.to_string(),
        tag: tag.to_string(),
        path: path.map(|p| p.to_string_lossy().to_string()),
        status: "installed".to_string(),
    });
}

pub fn emit_json(success: bool, error: Option<String>) {
    let events = events().lock().unwrap_or_else(|e| e.into_inner()).clone();
    let value = serde_json::json!({
        "success": success,
        "error": error,
        "events": events,
    });
    if let Ok(text) = serde_json::to_string_pretty(&value) {
        println!("{text}");
    } else {
        println!("{}", serde_json::to_string(&value).unwrap_or_default());
    }
}

#[derive(Clone, Copy, Debug)]
enum LineKind {
    Info,
    Success,
    Update,
}

pub struct Ui {
    theme: Theme,
    config: OutputConfig,
}

impl Ui {
    pub fn new(theme: Theme) -> Self {
        let config = *output();
        Self { theme, config }
    }

    pub fn theme(&self) -> Theme {
        self.theme
    }

    pub fn lang(&self) -> Lang {
        self.config.lang
    }

    pub fn is_json(&self) -> bool {
        self.config.json
    }

    pub fn is_interactive(&self) -> bool {
        !self.config.json && io::stdout().is_terminal()
    }

    pub fn banner(&self, title: &str) {
        if self.is_json() {
            return;
        }
        banner::print_banner(self, title);
    }

    pub fn complete(&self) {
        if self.is_json() {
            return;
        }
        banner::print_complete(self);
    }

    pub fn info(&self, message: &str) {
        self.log(LineKind::Info, message);
    }

    pub fn success(&self, message: &str) {
        self.log(LineKind::Success, message);
    }

    pub fn warn(&self, message: &str) {
        self.log(LineKind::Update, message);
    }

    pub fn update(&self, message: &str) {
        self.log(LineKind::Update, message);
    }

    pub fn download_progress(&self, label: &str, total: Option<u64>) -> Option<DownloadProgress> {
        if !self.is_interactive() {
            return None;
        }
        DownloadProgress::new(self.theme, label.to_string(), total)
    }

    pub fn spinner(&self, label: &str) -> Option<Spinner> {
        if !self.is_interactive() {
            return None;
        }
        Spinner::new(self.theme, label.to_string())
    }

    fn log(&self, kind: LineKind, message: &str) {
        if self.config.json {
            let prefix = match kind {
                LineKind::Info => ">",
                LineKind::Success => "*",
                LineKind::Update => "^",
            };
            eprintln!("{prefix} {message}");
            return;
        }

        let style = self.theme.style();
        let symbols = self.theme.symbols();
        let (color, symbol) = match kind {
            LineKind::Info => (style.primary, symbols.info),
            LineKind::Success => (style.primary, symbols.success),
            LineKind::Update => (style.accent, symbols.update),
        };
        println!("  {color}{symbol}{} {message}", style.reset);
    }

    pub fn label_downloading(&self, name: &str) -> String {
        format!("{} {name}", tr(self.config.lang, "downloading"))
    }
}

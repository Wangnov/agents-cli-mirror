#[derive(Clone, Copy, Debug)]
pub enum Lang {
    Zh,
    En,
}

pub fn detect_lang(cli_lang: Option<&str>) -> Lang {
    let lang = cli_lang
        .map(|v| v.to_string())
        .or_else(|| std::env::var("LC_ALL").ok())
        .or_else(|| std::env::var("LC_MESSAGES").ok())
        .or_else(|| std::env::var("LANG").ok())
        .unwrap_or_default();

    let normalized = lang.to_lowercase();
    if normalized.starts_with("zh") {
        return Lang::Zh;
    }

    if normalized.is_empty()
        || normalized == "c"
        || normalized == "c.utf-8"
        || normalized == "posix"
    {
        #[cfg(target_os = "macos")]
        {
            if let Ok(output) = std::process::Command::new("defaults")
                .args(["read", "-g", "AppleLocale"])
                .output()
                && output.status.success()
                && let Ok(value) = String::from_utf8(output.stdout)
                && value.trim().to_lowercase().starts_with("zh")
            {
                return Lang::Zh;
            }
        }
    }

    Lang::En
}

pub fn tr(lang: Lang, key: &str) -> &'static str {
    match lang {
        Lang::Zh => match key {
            "downloading" => "正在下载",
            "verifying" => "正在校验",
            "extracting" => "正在解压",
            "installer" => "安装程序",
            "complete" => "安装完成!",
            _ => "",
        },
        Lang::En => match key {
            "downloading" => "Downloading",
            "verifying" => "Verifying",
            "extracting" => "Extracting",
            "installer" => "Installer",
            "complete" => "Installation Complete!",
            _ => "",
        },
    }
}

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
            "path_added" => "已写入 PATH 配置",
            "path_removed" => "已移除 PATH 配置",
            "symlink_created" => "已创建命令入口",
            "symlink_removed" => "已移除命令入口",
            "restart_terminal" => "请重启终端或重新加载 shell 配置后再使用",
            _ => "",
        },
        Lang::En => match key {
            "downloading" => "Downloading",
            "verifying" => "Verifying",
            "extracting" => "Extracting",
            "installer" => "Installer",
            "complete" => "Installation Complete!",
            "path_added" => "PATH updated in",
            "path_removed" => "PATH entry removed from",
            "symlink_created" => "Command shim created at",
            "symlink_removed" => "Command shim removed from",
            "restart_terminal" => "Restart or reload your shell to pick up PATH changes",
            _ => "",
        },
    }
}

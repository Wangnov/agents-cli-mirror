use crate::ui::brand::brand;
use crate::ui::i18n::tr;
use crate::ui::logo::{acm_logo_lines, brand_logo_lines};
use crate::ui::output::Ui;
use crate::ui::style::Theme;
use crate::ui::text::{display_width, gradient_text};

const BOX_WIDTH: usize = 46;

pub fn print_banner(ui: &Ui, title: &str) {
    if ui.is_json() {
        return;
    }

    let style = ui.theme().style();
    let bar = format!("{}|{}", style.gold, style.reset);

    let brand = brand();
    let banner = brand.banner(ui.lang());
    let mut lines = Vec::new();
    lines.push(String::new());
    lines.push(format!(
        "  {}{}{}{}",
        style.bold, style.gold, banner.welcome, style.reset
    ));
    lines.push(String::new());
    lines.push(format!("  {}{}{}", style.dim, banner.tagline, style.reset));
    lines.push(String::new());
    lines.push(format!("  {} ({})", brand.service_name, brand.name));
    lines.push(format!(
        "  {}{}{}",
        style.dim, banner.gui_install, style.reset
    ));
    if brand.has_app_url() {
        lines.push(String::new());
        lines.push(format!(
            "  {}{}:{}",
            style.dim, banner.app_url_label, style.reset
        ));
        lines.push(format!(
            "  {}{}{} ({})",
            style.gold, brand.app_url, style.reset, brand.app_name
        ));
    }
    if brand.has_service_url() {
        lines.push(String::new());
        lines.push(format!(
            "  {}{}:{}",
            style.dim, banner.service_label, style.reset
        ));
        lines.push(format!(
            "  {}{}{}",
            style.gold, brand.service_url, style.reset
        ));
    }

    println!();
    let logo_lines = {
        let brand_lines = brand_logo_lines();
        if brand_lines.is_empty() {
            acm_logo_lines()
        } else {
            brand_lines
        }
    };
    for line in logo_lines {
        println!("{line}");
    }
    println!();

    for line in lines {
        println!("{bar}{line}");
    }
    println!("{bar}");
    println!();

    let installer_text = tr(ui.lang(), "installer");
    let title_text = format!("{title} {installer_text}");
    let title_width = display_width(&title_text);
    let left_pad = (BOX_WIDTH.saturating_sub(title_width)) / 2;

    print_border(style.primary, style.reset);
    match ui.theme() {
        Theme::Gemini => {
            let gradient = gradient_text(&title_text, ui.theme().gradient_colors(), style.reset);
            print!("  {}", " ".repeat(left_pad));
            println!("{gradient}");
        }
        _ => {
            print!("  {}{}", " ".repeat(left_pad), style.bold);
            println!("{}{}{}", style.primary, title_text, style.reset);
        }
    }
    print_border(style.primary, style.reset);
    println!();
}

pub fn print_complete(ui: &Ui) {
    if ui.is_json() {
        return;
    }

    let style = ui.theme().style();
    let text = tr(ui.lang(), "complete");
    let text_width = display_width(text);
    let left_pad = (BOX_WIDTH.saturating_sub(text_width + 4)) / 2;

    println!();
    print_border(style.primary, style.reset);
    print!("  {}", " ".repeat(left_pad));
    match ui.theme() {
        Theme::Gemini => {
            let gradient = gradient_text(text, ui.theme().gradient_colors(), style.reset);
            println!("{}[ {} ]{}", style.bold, gradient, style.reset);
        }
        _ => {
            println!("{}{}[ {} ]{}", style.bold, style.primary, text, style.reset);
        }
    }
    print_border(style.primary, style.reset);
    println!();
}

fn print_border(primary: &str, reset: &str) {
    println!("  {}{}{}", primary, "-".repeat(BOX_WIDTH), reset);
}

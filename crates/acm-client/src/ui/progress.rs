use crate::ui::style::Theme;
use crate::ui::text::{display_width, format_size, format_speed, shimmer_text};
use crossterm::{cursor, execute};
use std::io::{Write, stdout};
use std::time::{Duration, Instant};

pub struct DownloadProgress {
    theme: Theme,
    label: String,
    total: Option<u64>,
    last_tick: Instant,
    last_bytes: u64,
    speed: u64,
    wave_pos: usize,
    wave_period: usize,
    spinner_idx: usize,
    spinner_last: Instant,
    spinner_interval: Duration,
    hidden_cursor: bool,
}

impl DownloadProgress {
    pub fn new(theme: Theme, label: String, total: Option<u64>) -> Option<Self> {
        let mut out = stdout();
        let hidden_cursor = execute!(out, cursor::Hide).is_ok();
        let wave_period = match theme {
            Theme::Acm | Theme::Codex => display_width(&label).saturating_add(12).max(1),
            _ => 1,
        };
        let spinner_interval = match theme {
            Theme::Acm => Duration::from_millis(90),
            Theme::Claude => Duration::from_millis(120),
            Theme::Gemini => Duration::from_millis(120),
            Theme::Codex => Duration::from_millis(80),
        };
        Some(Self {
            theme,
            label,
            total,
            last_tick: Instant::now(),
            last_bytes: 0,
            speed: 0,
            wave_pos: 0,
            wave_period,
            spinner_idx: 0,
            spinner_last: Instant::now(),
            spinner_interval,
            hidden_cursor,
        })
    }

    pub fn update(&mut self, downloaded: u64) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_tick);
        if elapsed >= Duration::from_millis(300) {
            let diff = downloaded.saturating_sub(self.last_bytes);
            let millis = elapsed.as_millis().max(1) as u64;
            self.speed = diff.saturating_mul(1000) / millis;
            self.last_bytes = downloaded;
            self.last_tick = now;
        }

        let pct = if let Some(total) = self.total {
            if total > 0 {
                ((downloaded as f64 / total as f64) * 100.0)
                    .min(100.0)
                    .round() as u8
            } else {
                0
            }
        } else {
            0
        };

        let size_str = format_size(downloaded);
        let speed_str = format_speed(self.speed);
        let advance_spinner = now.duration_since(self.spinner_last) >= self.spinner_interval;
        if advance_spinner {
            self.spinner_idx = self.spinner_idx.wrapping_add(1);
            self.spinner_last = now;
        }

        let line = match self.theme {
            Theme::Acm | Theme::Codex => {
                let style = self.theme.style();
                let colors = self.theme.shimmer_colors();
                let padding = 6;
                let band_half = 3;
                let label = shimmer_text(
                    &self.label,
                    self.wave_pos,
                    padding,
                    band_half,
                    colors,
                    style.dim,
                    style.reset,
                );
                self.wave_pos = (self.wave_pos + 1) % self.wave_period;
                format!(
                    "  {dim}•{reset} {label} {dim}{pct:3}% | {size} | {speed}{reset}",
                    dim = style.dim,
                    reset = style.reset,
                    pct = pct,
                    size = size_str,
                    speed = speed_str
                )
            }
            Theme::Claude => {
                let style = self.theme.style();
                let spinner = self.theme.progress_symbols().spinner;
                let symbol = spinner[self.spinner_idx % spinner.len()];
                let circle = self.theme.progress_circle(pct);
                format!(
                    "  {primary}{symbol}{reset} {label} {dim}{circle} {pct:3}% | {size} | {speed}{reset}",
                    primary = style.primary,
                    reset = style.reset,
                    dim = style.dim,
                    symbol = symbol,
                    label = self.label,
                    circle = circle,
                    pct = pct,
                    size = size_str,
                    speed = speed_str
                )
            }
            Theme::Gemini => {
                let style = self.theme.style();
                let spinner = self.theme.progress_symbols().spinner;
                let colors = self.theme.spinner_colors();
                let symbol = spinner[self.spinner_idx % spinner.len()];
                let color = colors
                    .get(self.spinner_idx % colors.len().max(1))
                    .copied()
                    .unwrap_or(74);
                format!(
                    "  \x1b[38;5;{color}m{symbol}{reset} {label} {dim}{pct:3}% | {size} | {speed}{reset}",
                    color = color,
                    reset = style.reset,
                    dim = style.dim,
                    symbol = symbol,
                    label = self.label,
                    pct = pct,
                    size = size_str,
                    speed = speed_str
                )
            }
        };

        render_line(&line);
    }

    pub fn finish_ok(&mut self, downloaded: u64) {
        let style = self.theme.style();
        let symbols = self.theme.progress_symbols();
        let size = format_size(downloaded);
        let line = match self.theme {
            Theme::Claude => format!(
                "  {primary}{symbol}{reset} {label} {dim}● 100% | {size}{reset}",
                primary = style.primary,
                reset = style.reset,
                dim = style.dim,
                symbol = symbols.success,
                label = self.label,
                size = size
            ),
            Theme::Codex => format!(
                "  {primary}{symbol}{reset} {white}{label}{reset} {dim}100% | {size}{reset}",
                primary = style.primary,
                reset = style.reset,
                white = style.white,
                dim = style.dim,
                symbol = symbols.success,
                label = self.label,
                size = size
            ),
            Theme::Gemini => format!(
                "  {primary}{symbol}{reset} {label} {dim}100% | {size}{reset}",
                primary = style.primary,
                reset = style.reset,
                dim = style.dim,
                symbol = symbols.success,
                label = self.label,
                size = size
            ),
            Theme::Acm => format!(
                "  {primary}{symbol}{reset} {label} {dim}100% | {size}{reset}",
                primary = style.primary,
                reset = style.reset,
                dim = style.dim,
                symbol = symbols.success,
                label = self.label,
                size = size
            ),
        };
        render_line(&line);
        println!();
        self.show_cursor();
    }

    pub fn finish_err(&mut self, error: Option<&str>) {
        let style = self.theme.style();
        let symbols = self.theme.progress_symbols();
        let line = match (self.theme, error) {
            (Theme::Codex, Some(msg)) => format!(
                "  {red}{symbol}{reset} {white}{label}{reset} {red}failed{reset} {dim}({msg}){reset}",
                red = style.red,
                reset = style.reset,
                white = style.white,
                dim = style.dim,
                symbol = symbols.error,
                label = self.label,
                msg = msg
            ),
            (Theme::Codex, None) => format!(
                "  {red}{symbol}{reset} {white}{label}{reset} {red}failed{reset}",
                red = style.red,
                reset = style.reset,
                white = style.white,
                symbol = symbols.error,
                label = self.label
            ),
            (_, Some(msg)) => format!(
                "  {red}{symbol}{reset} {label} {red}failed{reset} {dim}({msg}){reset}",
                red = style.red,
                reset = style.reset,
                dim = style.dim,
                symbol = symbols.error,
                label = self.label,
                msg = msg
            ),
            (_, None) => format!(
                "  {red}{symbol}{reset} {label} {red}failed{reset}",
                red = style.red,
                reset = style.reset,
                symbol = symbols.error,
                label = self.label
            ),
        };
        render_line(&line);
        println!();
        self.show_cursor();
    }

    fn show_cursor(&mut self) {
        if self.hidden_cursor {
            let _ = execute!(stdout(), cursor::Show);
            self.hidden_cursor = false;
        }
    }
}

impl Drop for DownloadProgress {
    fn drop(&mut self) {
        self.show_cursor();
    }
}

pub struct Spinner {
    theme: Theme,
    label: String,
    idx: usize,
    wave_pos: usize,
    wave_period: usize,
    hidden_cursor: bool,
}

impl Spinner {
    pub fn new(theme: Theme, label: String) -> Option<Self> {
        let mut out = stdout();
        let hidden_cursor = execute!(out, cursor::Hide).is_ok();
        let wave_period = match theme {
            Theme::Acm | Theme::Codex => display_width(&label).saturating_add(12).max(1),
            _ => 1,
        };
        Some(Self {
            theme,
            label,
            idx: 0,
            wave_pos: 0,
            wave_period,
            hidden_cursor,
        })
    }

    pub fn tick(&mut self) {
        let style = self.theme.style();
        let line = match self.theme {
            Theme::Codex => {
                let colors = self.theme.shimmer_colors();
                let padding = 6;
                let band_half = 3;
                let label = shimmer_text(
                    &self.label,
                    self.wave_pos,
                    padding,
                    band_half,
                    colors,
                    style.dim,
                    style.reset,
                );
                self.wave_pos = (self.wave_pos + 1) % self.wave_period;
                format!(
                    "  {dim}•{reset} {label}",
                    dim = style.dim,
                    reset = style.reset
                )
            }
            _ => {
                let symbols = self.theme.progress_symbols();
                let symbol = symbols.spinner[self.idx % symbols.spinner.len()];
                let colors = self.theme.spinner_colors();
                let color = colors
                    .get(self.idx % colors.len().max(1))
                    .copied()
                    .unwrap_or(0);
                self.idx = self.idx.wrapping_add(1);
                if matches!(self.theme, Theme::Gemini) && color != 0 {
                    format!(
                        "  \x1b[38;5;{color}m{symbol}{reset} {label}",
                        color = color,
                        reset = style.reset,
                        symbol = symbol,
                        label = self.label
                    )
                } else {
                    format!(
                        "  {primary}{symbol}{reset} {label}",
                        primary = style.primary,
                        reset = style.reset,
                        symbol = symbol,
                        label = self.label
                    )
                }
            }
        };
        render_line(&line);
    }

    pub fn finish_ok(&mut self) {
        let style = self.theme.style();
        let line = match self.theme {
            Theme::Codex => format!(
                "  {primary}{symbol}{reset} {white}{label}{reset}",
                primary = style.primary,
                reset = style.reset,
                white = style.white,
                symbol = self.theme.progress_symbols().success,
                label = self.label
            ),
            _ => format!(
                "  {primary}{symbol}{reset} {label}",
                primary = style.primary,
                reset = style.reset,
                symbol = self.theme.progress_symbols().success,
                label = self.label
            ),
        };
        render_line(&line);
        println!();
        self.show_cursor();
    }

    pub fn finish_err(&mut self) {
        let style = self.theme.style();
        let line = match self.theme {
            Theme::Codex => format!(
                "  {red}{symbol}{reset} {white}{label}{reset}",
                red = style.red,
                reset = style.reset,
                white = style.white,
                symbol = self.theme.progress_symbols().error,
                label = self.label
            ),
            _ => format!(
                "  {red}{symbol}{reset} {label}",
                red = style.red,
                reset = style.reset,
                symbol = self.theme.progress_symbols().error,
                label = self.label
            ),
        };
        render_line(&line);
        println!();
        self.show_cursor();
    }

    fn show_cursor(&mut self) {
        if self.hidden_cursor {
            let _ = execute!(stdout(), cursor::Show);
            self.hidden_cursor = false;
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.show_cursor();
    }
}

fn render_line(line: &str) {
    let mut out = stdout();
    let _ = write!(out, "\r{line}\x1b[0K");
    let _ = out.flush();
}

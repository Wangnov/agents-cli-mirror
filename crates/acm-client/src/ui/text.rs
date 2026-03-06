use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Debug)]
struct Segment {
    text: String,
    pos: usize,
}

fn segments(input: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    for g in input.graphemes(true) {
        let width = UnicodeWidthStr::width(g).max(1);
        out.push(Segment {
            text: g.to_string(),
            pos,
        });
        pos += width;
    }
    out
}

pub fn strip_ansi(input: &str) -> String {
    let mut out = String::new();
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && matches!(chars.peek(), Some('[')) {
            chars.next();
            for next in chars.by_ref() {
                if next == 'm' {
                    break;
                }
            }
            continue;
        }
        out.push(ch);
    }
    out
}

pub fn display_width(input: &str) -> usize {
    let plain = strip_ansi(input);
    UnicodeWidthStr::width(plain.as_str())
}

pub fn shimmer_text(
    input: &str,
    wave_pos: usize,
    padding: usize,
    band_half_width: usize,
    colors: &[u8],
    dim: &str,
    reset: &str,
) -> String {
    let mut out = String::new();
    for seg in segments(input) {
        let i_pos = seg.pos + padding;
        let dist = i_pos.abs_diff(wave_pos);
        if dist <= band_half_width && !colors.is_empty() {
            let idx = (dist * 2).min(colors.len().saturating_sub(1));
            let color = colors[idx];
            out.push_str(&format!("\x1b[38;5;{color}m{}", seg.text));
        } else {
            out.push_str(dim);
            out.push_str(&seg.text);
        }
    }
    out.push_str(reset);
    out
}

pub fn gradient_text(input: &str, colors: &[u8], reset: &str) -> String {
    let mut out = String::new();
    let segs = segments(input);
    if segs.is_empty() || colors.is_empty() {
        return input.to_string();
    }
    let total = segs.len();
    for (idx, seg) in segs.into_iter().enumerate() {
        let color_idx = idx * colors.len() / total;
        let color = colors[color_idx.min(colors.len() - 1)];
        out.push_str(&format!("\x1b[38;5;{color}m{}", seg.text));
    }
    out.push_str(reset);
    out
}

pub fn format_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1}G", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.1}M", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1}K", bytes as f64 / 1024.0)
    } else {
        format!("{bytes}B")
    }
}

pub fn format_speed(bytes_per_sec: u64) -> String {
    if bytes_per_sec >= 1024 * 1024 {
        format!("{:.1} MB/s", bytes_per_sec as f64 / (1024.0 * 1024.0))
    } else if bytes_per_sec >= 1024 {
        format!("{:.0} KB/s", bytes_per_sec as f64 / 1024.0)
    } else {
        format!("{bytes_per_sec} B/s")
    }
}

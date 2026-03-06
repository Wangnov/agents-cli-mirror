use std::time::Duration;

pub fn progress_log_interval(total_size: Option<u64>) -> u64 {
    let max_interval = 5 * 1024 * 1024;
    let min_interval = 512 * 1024;
    match total_size {
        Some(total) if total > 0 => {
            let by_percent = total / 20;
            let mut interval = by_percent.clamp(min_interval, max_interval);
            if total < min_interval {
                interval = total;
            }
            if interval == 0 {
                interval = min_interval;
            }
            interval
        }
        _ => max_interval,
    }
}

pub fn format_rate(bytes: u64, elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f64();
    if secs <= 0.0 {
        return "0 B/s".to_string();
    }
    let per_sec = (bytes as f64 / secs).round().max(0.0) as u64;
    format!("{}/s", format_bytes(per_sec))
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut idx = 0usize;
    while size >= 1024.0 && idx < UNITS.len() - 1 {
        size /= 1024.0;
        idx += 1;
    }
    if idx == 0 {
        format!("{} {}", bytes, UNITS[idx])
    } else if size >= 100.0 {
        format!("{:.0} {}", size, UNITS[idx])
    } else if size >= 10.0 {
        format!("{:.1} {}", size, UNITS[idx])
    } else {
        format!("{:.2} {}", size, UNITS[idx])
    }
}

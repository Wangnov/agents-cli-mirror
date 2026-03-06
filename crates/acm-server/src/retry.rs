use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::StatusCode;

#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_attempts: usize,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(8),
        }
    }
}

pub fn is_retryable_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

pub fn is_retryable_error(err: &reqwest::Error) -> bool {
    err.is_timeout() || err.is_connect() || err.is_request()
}

pub fn sync_concurrency() -> usize {
    let default = 2usize;
    let max = 8usize;
    std::env::var("MIRROR_SYNC_CONCURRENCY")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v >= 1)
        .map(|v| v.min(max))
        .unwrap_or(default)
}

fn jitter_duration(max_ms: u64) -> Duration {
    if max_ms == 0 {
        return Duration::from_millis(0);
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    Duration::from_millis(nanos % (max_ms + 1))
}

fn backoff_delay(attempt: usize, policy: &RetryPolicy) -> Duration {
    let exponent = attempt.saturating_sub(1).min(6) as u32;
    let mut delay = policy.base_delay * (1u32 << exponent);
    if delay > policy.max_delay {
        delay = policy.max_delay;
    }

    let mut with_jitter = delay + jitter_duration(300);
    if with_jitter > policy.max_delay {
        with_jitter = policy.max_delay;
    }

    with_jitter
}

pub async fn send_with_retry<F>(
    mut make_request: F,
    policy: RetryPolicy,
) -> Result<reqwest::Response, anyhow::Error>
where
    F: FnMut() -> reqwest::RequestBuilder,
{
    let max_attempts = policy.max_attempts.max(1);
    let mut attempt = 0usize;

    loop {
        attempt += 1;
        let result = make_request().send().await;

        match result {
            Ok(response) => {
                let status = response.status();
                if status.is_success() || !is_retryable_status(status) || attempt >= max_attempts {
                    return Ok(response);
                }
            }
            Err(err) => {
                if attempt >= max_attempts || !is_retryable_error(&err) {
                    return Err(err.into());
                }
            }
        }

        let delay = backoff_delay(attempt, &policy);
        tokio::time::sleep(delay).await;
    }
}

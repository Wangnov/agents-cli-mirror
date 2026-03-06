use anyhow::Result;
use reqwest::Client;
use std::time::Duration;

use crate::config::HttpConfig;

pub fn build_http_client(config: &HttpConfig) -> Result<Client> {
    let mut builder = Client::builder().user_agent(user_agent());

    if config.connect_timeout_seconds > 0 {
        builder = builder.connect_timeout(Duration::from_secs(config.connect_timeout_seconds));
    }

    if config.request_timeout_seconds > 0 {
        builder = builder.timeout(Duration::from_secs(config.request_timeout_seconds));
    }

    Ok(builder.build()?)
}

pub fn user_agent() -> String {
    format!("{}/{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
}

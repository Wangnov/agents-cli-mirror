use anyhow::Result;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::cache::CacheManager;
use crate::config::Config;
use crate::providers::GenericProvider;
use crate::storage_clients::StorageClients;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait ProviderRuntime: Send + Sync {
    fn sync_all(&self) -> BoxFuture<'_, Result<Vec<String>>>;
    fn sync_all_auto(&self) -> BoxFuture<'_, Result<Vec<String>>>;
    fn get_tag_version(&self, tag: String) -> BoxFuture<'_, Option<String>>;
    fn get_info(&self) -> BoxFuture<'_, serde_json::Value>;
}

impl ProviderRuntime for GenericProvider {
    fn sync_all(&self) -> BoxFuture<'_, Result<Vec<String>>> {
        Box::pin(async move { self.sync_all().await })
    }

    fn sync_all_auto(&self) -> BoxFuture<'_, Result<Vec<String>>> {
        Box::pin(async move { self.sync_all_auto().await })
    }

    fn get_tag_version(&self, tag: String) -> BoxFuture<'_, Option<String>> {
        Box::pin(async move { self.get_tag_version(&tag).await })
    }

    fn get_info(&self) -> BoxFuture<'_, serde_json::Value> {
        Box::pin(async move { self.get_info().await })
    }
}

pub type ProviderInstance = Arc<dyn ProviderRuntime>;

/// Shared application state
pub struct AppState {
    pub config: Config,
    pub cache: Arc<CacheManager>,
    pub storage_clients: StorageClients,
    pub providers: HashMap<String, ProviderInstance>,
    pub sync_lock: Mutex<()>,
    pub refresh_throttle: Mutex<HashMap<String, Instant>>,
}

pub async fn build_state(config: Config, cache: Arc<CacheManager>) -> Result<Arc<AppState>> {
    config.validate()?;
    let storage_clients = StorageClients::new(&config.storage).await?;
    let mut providers = HashMap::new();
    for provider in &config.providers {
        if providers.contains_key(&provider.name) {
            return Err(anyhow::anyhow!(
                "Duplicate provider name: {}",
                provider.name
            ));
        }

        let instance: ProviderInstance = Arc::new(GenericProvider::new(
            provider.clone(),
            cache.clone(),
            config.storage.clone(),
            storage_clients.clone(),
            config.http.clone(),
        )?);
        providers.insert(provider.name.clone(), instance);
    }

    Ok(Arc::new(AppState {
        config,
        cache,
        storage_clients,
        providers,
        sync_lock: Mutex::new(()),
        refresh_throttle: Mutex::new(HashMap::new()),
    }))
}

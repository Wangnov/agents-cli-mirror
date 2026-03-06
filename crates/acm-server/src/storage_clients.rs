use anyhow::Result;
#[cfg(feature = "s3")]
use std::sync::Arc;

use crate::config::{StorageConfig, StorageMode};
#[cfg(feature = "s3")]
use crate::s3;

#[cfg(feature = "s3")]
pub type S3Client = aws_sdk_s3::Client;

#[cfg(not(feature = "s3"))]
pub struct S3Client;

#[derive(Clone, Default)]
pub struct StorageClients {
    #[cfg(feature = "s3")]
    pub s3: Option<Arc<S3Client>>,
}

impl StorageClients {
    pub async fn new(storage: &StorageConfig) -> Result<Self> {
        match storage.mode {
            StorageMode::Local => Ok(Self::default()),
            StorageMode::S3 => {
                #[cfg(feature = "s3")]
                {
                    return Ok(Self {
                        s3: Some(Arc::new(s3::build_client(&storage.s3).await?)),
                    });
                }
                #[cfg(not(feature = "s3"))]
                {
                    anyhow::bail!(
                        "storage.mode = \"s3\" is not supported in this build; enable feature `s3`"
                    );
                }
            }
        }
    }

    pub fn s3(&self) -> Option<&S3Client> {
        #[cfg(feature = "s3")]
        {
            self.s3.as_deref()
        }
        #[cfg(not(feature = "s3"))]
        {
            None
        }
    }
}

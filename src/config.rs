use crate::tracing_controller;
use anyhow::{anyhow, Error as AnyError};
use azure_identity::AzureCliCredential;
use azure_security_keyvault::SecretClient;
use config as cfg;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Arc};
use tokio::runtime::Handle as RtHandle;

pub const SERVICE_NAME: &str = "hello-world";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoreConfig {
    pub slot: String,
    pub stage: String,
    pub shared_keyvault: Option<String>,
    pub private_keyvault: Option<String>,
}

/// Core configuration to set up keyvaults.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct PreinitConfig {
    pub core: CoreConfig,
}

impl PreinitConfig {
    fn new() -> Result<PreinitConfig, AnyError> {
        use config::{Environment, File};
        let config_file = "web_config.json";

        let builder = cfg::Config::builder()
            .add_source(Environment::default().separator("--"))
            .add_source(File::from(Path::new(config_file)));

        let s = builder.build()?;
        let cfg: PreinitConfig = s.try_deserialize()?;

        log::info!("preinit configuration: {:#?}", cfg);
        Ok(cfg)
    }
}

#[derive(Clone, Debug)]
pub struct AzureKeyvaultConfigSource {
    rt_handle: RtHandle,
    client: SecretClient,
}

impl AzureKeyvaultConfigSource {
    fn new(
        rt_handle: &RtHandle,
        azure_credentials: &Arc<AzureCliCredential>,
        keyvault_url: &str,
    ) -> Result<AzureKeyvaultConfigSource, AnyError> {
        let client = SecretClient::new(keyvault_url, azure_credentials.clone())?;
        Ok(Self {
            rt_handle: rt_handle.clone(),
            client,
        })
    }
}

impl cfg::Source for AzureKeyvaultConfigSource {
    fn clone_into_box(&self) -> Box<dyn cfg::Source + Send + Sync> {
        Box::new(self.clone())
    }

    fn collect(&self) -> Result<cfg::Map<String, cfg::Value>, cfg::ConfigError> {
        tokio::task::block_in_place(|| {
            self.rt_handle.block_on(async {
                let mut config = cfg::Map::new();

                let mut stream = self.client.list_secrets().into_stream();
                while let Some(response) = stream.next().await {
                    let response = response.map_err(|err| cfg::ConfigError::Foreign(Box::new(err)))?;
                    for raw in &response.value {
                        let key = raw.id.split("/").last();
                        if let Some(key) = key {
                            log::info!("Reading secret {:?}", key);
                            let secret = self
                                .client
                                .get(key)
                                .into_future()
                                .await
                                .map_err(|err| cfg::ConfigError::Foreign(Box::new(err)))?;
                            if secret.attributes.enabled {
                                config.insert(key.to_owned(), secret.value.into());
                            }
                        }
                    }
                }

                //log::info!("{:#?}", config);
                Ok(config)
            })
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub core: CoreConfig,
    pub tracing: tracing_controller::Config,

    #[serde(rename = "FullSqlCns")]
    pub sql_cns: String,
}

impl Config {
    pub fn new(rt_handle: &RtHandle, azure_credentials: &Arc<AzureCliCredential>) -> Result<Config, AnyError> {
        let preinit = PreinitConfig::new()?;

        let shared_keyvault = preinit
            .core
            .shared_keyvault
            .as_ref()
            .map(|uri| AzureKeyvaultConfigSource::new(rt_handle, azure_credentials, &uri))
            .transpose()?;

        let private_keyvault = preinit
            .core
            .private_keyvault
            .as_ref()
            .map(|uri| AzureKeyvaultConfigSource::new(rt_handle, azure_credentials, &uri))
            .transpose()?;

        let config_file = "web_config.json";

        let mut builder = config::Config::builder();
        if let Some(shared_keyvault) = shared_keyvault {
            builder = builder.add_source(shared_keyvault)
        }
        if let Some(private_keyvault) = private_keyvault {
            builder = builder.add_source(private_keyvault)
        }
        builder = builder
            .add_source(cfg::File::from(Path::new(config_file)))
            .add_source(cfg::Environment::default().separator("--"));

        let s = builder.build()?;
        let cfg: Config = s.try_deserialize()?;

        if preinit.core != cfg.core {
            return Err(anyhow!(
                "Preinit and configuration are not matching: {:#?}, {:#?}",
                preinit.core,
                cfg.core
            ));
        }

        log::info!("configuration: {:#?}", cfg);
        Ok(cfg)
    }
}

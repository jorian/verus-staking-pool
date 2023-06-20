use std::path::PathBuf;

use color_eyre::Report;
use serde::Deserialize;
use serde_aux::prelude::deserialize_number_from_string;
use serde_derive::Serialize;
use tracing::{debug, warn};
use vrsc_rpc::json::vrsc::Address;

use crate::chain::Chain;
// shared configuration between pool and discord
pub fn get_coin_configuration(name: &str) -> Result<Option<CoinConfig>, Report> {
    let base_path = std::env::current_dir().expect("Failed to determine the current directory");
    let config_dir = base_path.join("coin_config");
    // let config_file_path = config_dir.join(name);

    if let Ok(config) = config::Config::builder()
        .add_source(config::File::from(
            config_dir.join(&name.to_ascii_lowercase()),
        ))
        .build()?
        .try_deserialize::<CoinConfig>()
    {
        Ok(Some(config))
    } else {
        Ok(None)
    }
}

pub fn get_coin_configurations() -> Result<Vec<CoinConfig>, Report> {
    let base_path = std::env::current_dir().expect("Failed to determine the current directory");
    let config_dir = base_path.join("coin_config");
    let mut coin_settings = vec![];

    if let Ok(dir) = config_dir.read_dir() {
        for entry in dir {
            let entry = entry?;
            let path = PathBuf::from(entry.file_name());
            if let Some(extension) = path.extension() {
                if extension.eq_ignore_ascii_case("toml") {
                    let settings = config::Config::builder()
                        .add_source(config::File::from(config_dir.join(&path)))
                        .build()?
                        .try_deserialize::<CoinConfig>()?;

                    coin_settings.push(settings);
                }
            }
        }
    } else {
        warn!("no `coin_config` directory set in root directory");
    }
    debug!("coin_settings: {:?}", coin_settings);

    Ok(coin_settings)
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct VerusVaultConditions {
    pub min_lock: u32,
    pub strict_recovery_id: bool,
    pub max_primary_addresses: u8,
}

#[derive(Debug, Deserialize)]
pub struct CoinConfig {
    pub currencyid: Address,
    pub name: String,
    pub currencyidhex: String,
    #[serde(deserialize_with = "deserialize_number_from_string")]
    pub default_tx_fee: u32,
    #[serde(deserialize_with = "deserialize_number_from_string")]
    pub default_pool_fee: f32,
    #[serde(deserialize_with = "deserialize_number_from_string")]
    pub pool_fee_discount: f32,
    #[serde(deserialize_with = "deserialize_number_from_string")]
    pub zmq_port_blocknotify: u16,
    #[serde(deserialize_with = "deserialize_number_from_string")]
    pub default_min_payout: u64,
    #[serde(deserialize_with = "deserialize_number_from_string")]
    pub payout_interval: u64,
    pub testnet: bool,
    pub pool_identity_address: Address,
    pub rpc_user: String,
    pub rpc_password: String,
    #[serde(deserialize_with = "deserialize_number_from_string")]
    pub rpc_port: u16,
    pub verus_vault_conditions: VerusVaultConditions,
}

impl From<&CoinConfig> for Chain {
    fn from(coin_config: &CoinConfig) -> Self {
        Chain {
            currencyid: coin_config.currencyid.clone(),
            testnet: coin_config.testnet,
            name: coin_config.name.clone(),
            currencyidhex: coin_config.currencyidhex.clone(),
            default_pool_fee: coin_config.default_pool_fee,
            default_tx_fee: coin_config.default_tx_fee,
            default_min_payout: coin_config.default_min_payout,
            rpc_user: coin_config.rpc_user.clone(),
            rpc_password: coin_config.rpc_password.clone(),
            rpc_port: coin_config.rpc_port,
        }
    }
}

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use tracing::debug;
use url::Url;
use vrsc_rpc::{
    client::Client as VerusClient,
    json::vrsc::{util::amount::serde::as_sat, Address, Amount},
};

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub chain_name: String,
    pub chain_id: Address,
    pub pool_address: Address,
    pub pool_primary_address: Address, // R-address stakers should include
    pub fee: f32,                      // basis points
    #[serde(with = "as_sat")]
    pub min_payout: Amount,
    #[serde(with = "as_sat")]
    pub tx_fee: Amount,
    pub webhook_endpoints: Vec<Url>,
    pub chain_config: ChainConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ChainConfig {
    pub rpc_user: String,
    pub rpc_password: String,
    pub rpc_host: String,
    pub rpc_port: u16,
    pub zmq_port_blocknotify: u16,
}

impl TryFrom<&ChainConfig> for VerusClient {
    type Error = anyhow::Error;

    fn try_from(value: &ChainConfig) -> Result<VerusClient> {
        VerusClient::rpc(vrsc_rpc::Auth::UserPass(
            format!("{}:{}", value.rpc_host, value.rpc_port),
            format!("{}", value.rpc_user),
            format!("{}", value.rpc_password),
        ))
        .context(format!(
            "Could not make Verus client for config:\n{:#?}",
            value
        ))
    }
}

pub fn get_coin_configurations() -> Result<Vec<Config>> {
    let base_path = std::env::current_dir().expect("Failed to determine the current directory");
    let config_dir = base_path.join("coin_configs");
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
                        .try_deserialize::<Config>()?;

                    coin_settings.push(settings);
                }
            }
        }
    } else {
        Err(anyhow!("no `coin_config` directory set in root directory"))?;
    }
    debug!("coin_settings: {:#?}", coin_settings);

    Ok(coin_settings)
}

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tracing::debug;
use url::Url;
use vrsc_rpc::{
    client::Client as VerusClient,
    json::vrsc::{util::amount::serde::as_sat, Address, Amount},
};

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub currency_name: String,
    pub currency_id: Address,
    pub pool_address: Address,
    pub pool_primary_address: Address, // R-address stakers should include
    pub fee: Decimal,                  // basis points
    #[serde(with = "as_sat")]
    pub min_payout: Amount,
    #[serde(with = "as_sat")]
    pub tx_fee: Amount,
    pub vault_conditions: Option<VaultConditions>,
    pub webhook_endpoints: Vec<Url>,
    pub chain_config: ChainConfig,
    pub payout_config: PayoutConfig,
    #[serde(default)]
    pub skip_preflight: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ChainConfig {
    pub rpc_user: String,
    pub rpc_password: String,
    pub rpc_host: String,
    pub rpc_port: u16,
    pub zmq_port_blocknotify: u16,
}

/// Sets the conditions a VerusID must adhere to before being accepted as a staker in this pool.
///
/// There are a couple of conditions that a VerusID requires, regardless of these VaultConditions.
/// - There must be at least 2 primary addresses,
/// - One of the primary addresses must be this pool's staking address
/// - The minimum_signatures of a VerusID must be set to 1. The VerusID does not stake if it's a
/// multisig.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct VaultConditions {
    /// Defines the minimal locktime for the VerusID to be eligible.
    ///
    /// Note that VerusID supports two types of locking, absolute lock and relative lock.
    /// A fixed (absolute) time in the future can be set, by setting min_time_lock to a Unix time
    /// in the future, or set a (relative) unlock window of X blocks that starts ticking down
    /// when a VerusID is unlocked.
    ///
    /// A VerusID is unlocked when the min_time_lock is 0.
    pub min_time_lock: u32,
    /// When set to true, the Revoke and Recover authority must be different
    /// from the main VerusID.
    pub strict_recovery_id: bool,
    /// Defines how many primary addresses are allowed to exist in the VerusID.
    pub max_primary_addresses: u8,
}

impl Default for VaultConditions {
    fn default() -> Self {
        Self {
            min_time_lock: 0,
            strict_recovery_id: false,
            max_primary_addresses: 64,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct PayoutConfig {
    pub check_interval_in_secs: u64,
    pub send_interval_in_secs: u64,
}

impl TryFrom<&ChainConfig> for VerusClient {
    type Error = anyhow::Error;

    fn try_from(value: &ChainConfig) -> Result<VerusClient> {
        VerusClient::rpc(vrsc_rpc::Auth::UserPass(
            format!("{}:{}", value.rpc_host, value.rpc_port),
            value.rpc_user.to_string(),
            value.rpc_password.to_string(),
        ))
        .context(format!(
            "Could not make Verus client for config:\n{:#?}",
            value
        ))
    }
}

pub fn get_coin_configurations() -> Result<Vec<Config>> {
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

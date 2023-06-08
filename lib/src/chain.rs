use std::{fmt::Display, str::FromStr};

use color_eyre::Report;
use vrsc_rpc::{json::vrsc::Address, Auth, Client};

use crate::configuration::get_coin_configuration;

#[derive(Debug, Clone)]
pub struct Chain {
    pub name: String,
    pub currencyid: Address,
    pub testnet: bool,
    pub currencyidhex: String,
    pub rpc_user: String,
    pub rpc_password: String,
    pub rpc_port: u16,
    pub default_pool_fee: f32,   // in basis points (5% = 0.05)
    pub default_tx_fee: u32,     // in basis points (5% = 0.05)
    pub default_min_payout: u64, // in sats
}

impl Chain {
    pub fn verusd_client(&self) -> Result<vrsc_rpc::Client, Report> {
        match self.name.as_ref() {
            "vrsctest" | "VRSC" => Client::vrsc(
                self.testnet,
                Auth::UserPass(
                    format!("http://127.0.0.1:{}", self.rpc_port),
                    self.rpc_user.clone(),
                    self.rpc_password.clone(),
                ),
            )
            .map_err(Report::from),
            _ => Client::chain(
                self.testnet,
                &self.currencyidhex,
                Auth::UserPass(
                    format!("http://127.0.0.1:{}", self.rpc_port),
                    self.rpc_user.clone(),
                    self.rpc_password.clone(),
                ),
            )
            .map_err(Report::from),
        }
    }
}

impl std::fmt::Display for Chain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("{} ({})", self.currencyid, self.name))
    }
}

impl TryFrom<&str> for Chain {
    type Error = ChainError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if let Ok(res) = get_coin_configuration(value) {
            if let Some(config) = res {
                let chain = Chain::from(&config);
                return Ok(chain);
            } else {
                return Err(ChainError::NotFound.into());
            }
        } else {
            return Err(ChainError::NotFound.into());
        }
    }
}

#[derive(Debug, Clone)]
pub enum ChainError {
    NotFound,
}

impl std::error::Error for ChainError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }

    fn cause(&self) -> Option<&dyn std::error::Error> {
        self.source()
    }
}

impl Display for ChainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str("Coin configuration not found"),
        }
    }
}

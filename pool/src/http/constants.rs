use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use vrsc_rpc::json::vrsc::util::amount::serde::as_sat;
use vrsc_rpc::json::vrsc::{Address, Amount};

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct StakingSupply {
    pub staker: f64,
    pub pool: f64,
    pub network: f64,
}

#[derive(Serialize, Debug, Clone, Copy)]
pub struct Stats {
    pub stakes: i64,
    #[serde(with = "as_sat")]
    pub pool_staking_supply: Amount,
    #[serde(with = "as_sat")]
    pub paid: Amount,
    pub stakers: i64,
}

/// Unpaid shares for one staker in the current work round (`round = 0`).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WorkShare {
    pub identity_address: Address,
    pub shares: Decimal,
}

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

/// Pool staking supply at a block, reconstructed from `work.shares` (sats).
///
/// `current` is true when this point is the open round (`work.round = 0`),
/// plotted at the chain's last processed height.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StakingBalancePoint {
    pub height: i64,
    pub sats: String,
    #[serde(default)]
    pub current: bool,
}

/// Cumulative number of pool stakes at a block height.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StakeCountPoint {
    pub height: i64,
    pub count: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PoolHistory {
    pub staking_balance: Vec<StakingBalancePoint>,
    pub stakes: Vec<StakeCountPoint>,
}

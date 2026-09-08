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
    /// Network eligible staking supply from `getmininginfo` (VRSC).
    pub network_staking_supply: f64,
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

/// Eligible pool staking supply at a block (sats), from `work_snapshots`
/// minus spent staking UTXOs that are still immature. The find height itself
/// still shows the work amount so the stake marker sits on that point.
///
/// `current` is true for the latest stored height.
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

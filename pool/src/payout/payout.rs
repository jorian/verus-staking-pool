use anyhow::Result;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use vrsc_rpc::{
    bitcoin::{BlockHash, Txid},
    json::vrsc::{util::amount::serde::as_sat, Address, Amount},
};

use crate::coinstaker::constants::Stake;

pub struct Payout {
    /// Currency for which the payout is generated
    pub currency_address: Address,
    pub block_hash: BlockHash,
    pub block_height: u64,
    pub total_work: Decimal,
    /// The staked reward that is to be paid out to workers.
    /// Should be the sum of `fee` + `paid`
    pub amount: Amount,
    /// The difference between `amount` and the rewards for every payoutmember is the pool fee
    pub fee: Amount,
    /// The amount paid out to workers
    pub paid: Amount,
    /// All the workers that participated in this round
    pub members: Vec<PayoutMember>,
    pool_identity_address: Address,
}

impl Payout {
    pub fn new(pool: &PgPool, stake: &Stake) -> Result<Self> {
        // get work and fee by round

        todo!()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayoutMember {
    pub block_hash: BlockHash,
    pub block_height: u64,
    pub identity_address: Address,
    #[serde(with = "as_sat")]
    pub reward: Amount,
    pub shares: Decimal,
    // fee is in basis points: 5% should be entered as 0.05
    #[serde(with = "as_sat")]
    pub fee: Amount,
    pub txid: Option<Txid>,
}

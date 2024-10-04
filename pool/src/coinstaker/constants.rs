use anyhow::{anyhow, Context};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use vrsc_rpc::{
    bitcoin::{BlockHash, Txid},
    json::{
        vrsc::{util::amount::serde::as_sat, Address, Amount},
        Block,
    },
};

use crate::{
    payout_service::PayoutMember,
    util::verus::{coinbase_value, postxddest, staker_utxo_value},
};

/// Represents a participant in the staking pool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct Staker {
    pub currency_address: Address,
    pub identity_address: Address,
    pub identity_name: String,
    /// The amount in sats that is used to determine when to pay out the rewards of this staker.
    /// Once the accumulated rewards are higher than this threshold, a payout will be done.
    #[serde(with = "as_sat")]
    pub min_payout: Amount,
    /// Can be one of ["active", "cooling_down", "inactive"]
    /// A staker is **active** when the VerusID fulfills all the requirements as set in the
    /// VerusVaultConditions of this pool.
    /// A staker is **cooling down** when it updated its VerusID in the last 150 blocks.
    /// VerusIDs that were updated in the last 150 blocks are ineligible to stake.
    /// A staker that is **inactive** is a VerusID that was active before but updated its
    /// VerusID which made it ineligible according to the VerusVaultConditions.
    pub status: StakerStatus,
    /// The fee percentage that is used to determine how much fee is kept by the staking pool,
    /// when doing a payout. It is expressed as basis points, so 1% should be expressed as 0.01,
    /// 0.3% as 0.003, etc.
    pub fee: Decimal,
}

impl Staker {
    pub fn new(
        currency_address: Address,
        identity_address: Address,
        identity_name: String,
        min_payout: Amount,
        status: StakerStatus,
        fee: Decimal,
    ) -> Self {
        Self {
            currency_address,
            identity_address,
            identity_name,
            min_payout,
            status,
            fee,
        }
    }
}

#[derive(Debug, Deserialize, Clone, Serialize, PartialEq, Eq, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "staker_status", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StakerStatus {
    Active,
    CoolingDown,
    Inactive,
}

impl TryFrom<String> for StakerStatus {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_ref() {
            "ACTIVE" => Ok(Self::Active),
            "COOLING_DOWN" => Ok(Self::CoolingDown),
            "INACTIVE" => Ok(Self::Inactive),
            _ => Err(anyhow!("Unexpected StakerStatus")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stake {
    pub currency_address: Address,
    pub block_hash: BlockHash,
    pub block_height: u64,
    pub found_by: Address,
    pub source_txid: Txid,
    pub source_vout_num: u16,
    #[serde(with = "as_sat")]
    pub source_amount: Amount,
    pub status: StakeStatus,
    #[serde(with = "as_sat")]
    pub amount: Amount,
}

impl Stake {
    pub fn try_new(chain_id: &Address, block: &Block) -> anyhow::Result<Self> {
        let postxddest = postxddest(&block)?;
        let source_amount = staker_utxo_value(block)?;
        let coinbase_value = coinbase_value(&block)?;

        let source_txid = block
            .possourcetxid
            .context("there should always be a txid for the source stake")?;

        let source_vout_num = block
            .possourcevoutnum
            .context("there should always be a stake spend vout")?;

        Ok(Self {
            currency_address: chain_id.clone(),
            block_hash: block.hash,
            block_height: block.height,
            found_by: postxddest,
            source_txid,
            source_vout_num,
            source_amount,
            status: StakeStatus::Maturing,
            amount: coinbase_value,
        })
    }

    pub fn new(
        currency_address: &Address,
        block_hash: &BlockHash,
        block_height: u64,
        found_by: &Address,
        source_txid: Txid,
        source_vout_num: u16,
        source_amount: Amount,
        status: StakeStatus,
        amount: Amount,
    ) -> Self {
        Self {
            currency_address: currency_address.clone(),
            block_hash: *block_hash,
            block_height,
            found_by: found_by.clone(),
            source_txid,
            source_vout_num,
            source_amount,
            status,
            amount,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "stake_status", rename_all = "SCREAMING_SNAKE_CASE")]
#[serde(rename_all = "snake_case")]
pub enum StakeStatus {
    Maturing,
    Matured,
    Stale,
    StakeGuard,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StakerEarnings {
    #[serde(with = "as_sat")]
    pub paid: Amount, // payoutmembers with a txid
    #[serde(with = "as_sat")]
    pub pending: Amount, // payoutmembers without a txid
}

impl From<PayoutMember> for StakerEarnings {
    fn from(value: PayoutMember) -> Self {
        if value.txid.is_some() {
            StakerEarnings {
                paid: value.reward,
                pending: Amount::ZERO,
            }
        } else {
            StakerEarnings {
                paid: Amount::ZERO,
                pending: value.reward,
            }
        }
    }
}

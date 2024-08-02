use anyhow::anyhow;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use vrsc_rpc::{
    bitcoin::{BlockHash, Txid},
    json::vrsc::{util::amount::serde::as_sat, Address, Amount},
};

use crate::payout_service::PayoutMember;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct Staker {
    pub currency_address: Address,
    pub identity_address: Address,
    pub identity_name: String,
    #[serde(with = "as_sat")]
    pub min_payout: Amount,
    pub status: StakerStatus,
    pub fee: Decimal, // in basis points
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
pub struct StakerBalance {
    #[serde(with = "as_sat")]
    pub paid: Amount, // payoutmembers with a txid
    #[serde(with = "as_sat")]
    pub pending: Amount, // payoutmembers without a txid
}

impl From<PayoutMember> for StakerBalance {
    fn from(value: PayoutMember) -> Self {
        if value.txid.is_some() {
            StakerBalance {
                paid: value.reward,
                pending: Amount::ZERO,
            }
        } else {
            StakerBalance {
                paid: Amount::ZERO,
                pending: value.reward,
            }
        }
    }
}

// #[allow(unused)]
// fn block() -> Block {
//     let block = Block {
//         hash: todo!(),
//         validation_type: todo!(),
//         postarget: todo!(),
//         poshashbh: todo!(),
//         poshashtx: todo!(),
//         possourcetxid: todo!(),
//         possourcevoutnum: todo!(),
//         posrewarddest: todo!(),
//         postxddest: todo!(),
//         confirmations: todo!(),
//         size: todo!(),
//         height: todo!(),
//         version: todo!(),
//         merkle_root: todo!(),
//         seg_id: todo!(),
//         final_sapling_root: todo!(),
//         tx: todo!(),
//         time: todo!(),
//         nonce: todo!(),
//         solution: todo!(),
//         bits: todo!(),
//         difficulty: todo!(),
//         chain_work: todo!(),
//         chain_stake: todo!(),
//         anchor: todo!(),
//         block_type: todo!(),
//         value_pools: todo!(),
//         previous_blockhash: todo!(),
//         next_blockhash: todo!(),
//         proofroot: todo!(),
//     };

//     block
// }

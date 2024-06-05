use std::str::FromStr;

use rust_decimal::Decimal;
use vrsc_rpc::{
    bitcoin::{BlockHash, Txid},
    json::vrsc::{Address, Amount},
};

use crate::{
    coinstaker::{
        constants::{Stake, StakeStatus, Staker},
        StakerStatus,
    },
    payout::Worker,
};

pub struct DbStaker {
    pub(super) currency_address: String,
    pub(super) identity_address: String,
    pub(super) identity_name: String,
    pub(super) min_payout: i64,
    pub(super) status: StakerStatus,
}

impl TryFrom<DbStaker> for Staker {
    type Error = sqlx::Error;

    fn try_from(value: DbStaker) -> Result<Self, Self::Error> {
        let staker = Self {
            currency_address: Address::from_str(&value.currency_address)
                .map_err(|e| sqlx::Error::Decode(e.into()))?,
            identity_address: Address::from_str(&value.identity_address)
                .map_err(|e| sqlx::Error::Decode(e.into()))?,
            identity_name: value.identity_name,
            min_payout: Amount::from_sat(value.min_payout as u64),
            status: value.status,
        };

        Ok(staker)
    }
}

pub struct DbStake {
    pub(super) currency_address: String,
    pub(super) block_hash: String,
    pub(super) block_height: i64,
    pub(super) amount: i64,
    pub(super) found_by: String,
    pub(super) source_txid: String,
    pub(super) source_vout_num: i32,
    pub(super) source_amount: i64,
    pub(super) status: StakeStatus,
}

impl TryFrom<DbStake> for Stake {
    type Error = sqlx::Error;

    fn try_from(value: DbStake) -> Result<Self, Self::Error> {
        let stake = Self {
            currency_address: Address::from_str(&value.currency_address)
                .map_err(|e| sqlx::Error::Decode(e.into()))?,
            block_hash: BlockHash::from_str(&value.block_hash)
                .map_err(|e| sqlx::Error::Decode(e.into()))?,
            block_height: value.block_height as u64,
            found_by: Address::from_str(&value.found_by)
                .map_err(|e| sqlx::Error::Decode(e.into()))?,
            source_txid: Txid::from_str(&value.source_txid)
                .map_err(|e| sqlx::Error::Decode(e.into()))?,
            source_vout_num: value.source_vout_num as u16,
            source_amount: Amount::from_sat(value.source_amount as u64),
            status: value.status,
            amount: Amount::from_sat(value.amount as u64),
        };

        Ok(stake)
    }
}

pub struct DbWorker {
    pub(super) identity_address: String,
    pub(super) shares: Decimal,
    pub(super) fee: Decimal,
}

impl TryFrom<DbWorker> for Worker {
    type Error = sqlx::Error;

    fn try_from(value: DbWorker) -> Result<Self, Self::Error> {
        let worker = Self {
            identity_address: Address::from_str(&value.identity_address)
                .map_err(|e| sqlx::Error::Decode(e.into()))?,
            shares: value.shares,
            fee: value.fee,
        };

        Ok(worker)
    }
}

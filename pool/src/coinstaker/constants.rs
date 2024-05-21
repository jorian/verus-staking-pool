use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use std::{fmt::Display, ops::Deref};
use vrsc_rpc::{
    bitcoin::{BlockHash, Txid},
    json::{
        vrsc::{util::amount::serde::as_sat, Address, Amount},
        Block,
    },
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct Staker {
    pub currency_address: Address,
    pub identity_address: Address,
    pub identity_name: String,
    #[serde(with = "as_sat")]
    pub min_payout: Amount,
    pub status: StakerStatus,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Hash)]
pub struct CurrencyId(String);

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Hash)]
pub struct IdentityAddress(Address);

#[derive(Debug, Deserialize, Clone, Serialize, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "staker_status", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StakerStatus {
    Active,
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

impl Display for CurrencyId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Deref for CurrencyId {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl ToString for IdentityAddress {
    fn to_string(&self) -> String {
        self.0.to_string()
    }
}

impl From<&Address> for IdentityAddress {
    fn from(value: &Address) -> Self {
        Self(value.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stake {
    pub currency_address: Address,
    pub blockhash: BlockHash,
    pub blockheight: u64,
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
        blockhash: &BlockHash,
        blockheight: u64,
        found_by: &Address,
        source_txid: Txid,
        source_vout_num: u16,
        source_amount: Amount,
        status: StakeStatus,
        amount: Amount,
    ) -> Self {
        Self {
            currency_address: currency_address.clone(),
            blockhash: blockhash.clone(),
            blockheight,
            found_by: found_by.clone(),
            source_txid,
            source_vout_num,
            source_amount,
            status,
            amount,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StakeStatus {
    Mature,
    Stale,
    Stolen,
    Pending,
}

fn block() -> Block {
    let block = Block {
        hash: todo!(),
        validation_type: todo!(),
        postarget: todo!(),
        poshashbh: todo!(),
        poshashtx: todo!(),
        possourcetxid: todo!(),
        possourcevoutnum: todo!(),
        posrewarddest: todo!(),
        postxddest: todo!(),
        confirmations: todo!(),
        size: todo!(),
        height: todo!(),
        version: todo!(),
        merkle_root: todo!(),
        seg_id: todo!(),
        final_sapling_root: todo!(),
        tx: todo!(),
        time: todo!(),
        nonce: todo!(),
        solution: todo!(),
        bits: todo!(),
        difficulty: todo!(),
        chain_work: todo!(),
        chain_stake: todo!(),
        anchor: todo!(),
        block_type: todo!(),
        value_pools: todo!(),
        previous_blockhash: todo!(),
        next_blockhash: todo!(),
        proofroot: todo!(),
    };

    block
}

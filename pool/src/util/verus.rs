use anyhow::{Context, Result};
use vrsc_rpc::{
    client::{Client, RpcApi},
    json::{
        vrsc::{Address, Amount, SignedAmount},
        Block,
    },
};

use crate::http::constants::StakingSupply;

pub fn get_staking_supply(
    _currency_address: &Address,
    identity_addresses: &Vec<Address>,
    client: &Client,
) -> Result<StakingSupply> {
    let pool_supply = client.get_wallet_info()?.eligible_staking_balance.as_vrsc();
    let network_supply = client.get_mining_info()?.stakingsupply;

    let mut staker_supply = 0.0;
    if !identity_addresses.is_empty() {
        let list_unspent =
            client.list_unspent(Some(150), Some(99999999), Some(identity_addresses))?;
        staker_supply = list_unspent
            .iter()
            .fold(SignedAmount::ZERO, |acc, sum| acc + sum.amount)
            .as_vrsc();
    }

    Ok(StakingSupply {
        staker: staker_supply,
        pool: pool_supply,
        network: network_supply,
    })
}

pub fn coinbase_value(block: &Block) -> Result<Amount> {
    let coinbase_value = block
        .tx
        .first()
        .context("there should always be a coinbase transaction")?
        .vout
        .first()
        .context("there should always be a coinbase output")?
        .value_sat;

    Ok(coinbase_value)
}

pub fn staker_utxo_value(block: &Block) -> Result<Amount> {
    let utxo_value = block
        .tx
        .iter()
        .last()
        .context("there should always be a stake spend utxo")?
        .vin
        .first()
        .context("there should always be an input to a stake spend")?
        .value_sat
        .context("there should always be a positive stake")?;

    Ok(utxo_value)
}

pub fn postxddest(block: &Block) -> Result<Address> {
    let postxddest = block
        .postxddest
        .clone()
        .context("a stake must always have a postxddest")?;

    Ok(postxddest)
}

/// Returns true if a stake was stolen and caught by StakeGuard
pub async fn check_stake_guard(block: &Block) -> Result<bool> {
    if block
        .tx
        .first() // we always need the coinbase, it is always first
        .expect("every block has a coinbase tx")
        .vout
        .first()
        .expect("every tx has at least 1 vout")
        .spent_tx_id
        .is_some()
    {
        return Ok(true);
    }

    Ok(false)
}

pub fn disable_staking(client: Client) -> Result<()> {
    client.set_generate(false, 0)?;

    Ok(())
}

use anyhow::Result;
use vrsc_rpc::{
    client::{Client, RpcApi},
    json::vrsc::{Address, SignedAmount},
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

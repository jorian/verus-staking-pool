use anyhow::Result;
use vrsc_rpc::client::{Client, RpcApi};

use crate::http::constants::StakingSupply;

pub fn get_staking_supply(client: &Client) -> Result<StakingSupply> {
    let pool_supply = client.get_wallet_info()?.eligible_staking_balance.as_vrsc();
    let network_supply = client.get_mining_info()?.stakingsupply;

    Ok(StakingSupply {
        staker: 0.0,
        pool: pool_supply,
        network: network_supply,
    })
}

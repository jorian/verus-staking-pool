use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use anyhow::{anyhow, Result};
use rust_decimal::Decimal;
use sqlx::PgPool;
use tokio::sync::{mpsc, oneshot};
use vrsc_rpc::client::{Client as VerusClient, RpcApi};
use vrsc_rpc::json::vrsc::{Address, Amount, SignedAmount};
use vrsc_rpc::json::ListUnspentResult;

use super::coinstaker::CoinStakerMessage;
use super::config::ChainConfig;
use super::constants::{Stake, StakeStatus, Staker, StakerEarnings};
use super::StakerStatus;
use crate::database;
use crate::http::constants::{PoolHistory, StakingSupply, Stats, WorkShare};
use crate::payout_service::PayoutMember;

#[derive(Debug, Clone)]
pub struct SupplyCache {
    pub height: u64,
    pub eligible: Amount,
    pub network: f64,
}

#[derive(Debug, Clone)]
pub struct UnspentCache {
    pub height: u64,
    pub by_address: HashMap<Address, Amount>,
}

/// Shared by HTTP and the CoinStaker actor. Read-only API calls use this
/// directly (postgres + caches) instead of queueing behind block processing.
#[derive(Clone)]
pub struct CoinStakerHandle {
    pub tx: mpsc::Sender<CoinStakerMessage>,
    pub pool: PgPool,
    pub chain_id: Address,
    pub pool_primary_address: Address,
    pub chain_config: ChainConfig,
    pub supply_cache: Arc<RwLock<Option<SupplyCache>>>,
    pub unspent_cache: Arc<RwLock<Option<UnspentCache>>>,
}

pub(crate) fn fold_unspent(utxos: Vec<ListUnspentResult>) -> HashMap<Address, Amount> {
    utxos
        .into_iter()
        .filter(|utxo| utxo.amount.is_positive())
        .map(|utxo| (utxo.address.unwrap(), utxo.amount.to_unsigned().unwrap()))
        .fold(HashMap::new(), |mut acc, (address, amount)| {
            let _ = *acc
                .entry(address)
                .and_modify(|a| *a += amount)
                .or_insert(amount);
            acc
        })
}

impl CoinStakerHandle {
    pub fn pool_primary_address(&self) -> String {
        self.pool_primary_address.to_string()
    }

    pub async fn statistics(&self) -> Result<Stats> {
        let (stakes, stakers, rewards) = tokio::try_join!(
            database::get_number_of_matured_stakes(&self.pool, &self.chain_id),
            database::get_number_of_active_stakers(&self.pool, &self.chain_id),
            database::get_total_rewards(&self.pool, &self.chain_id)
        )?;
        let (pool_staking_supply, network_staking_supply) = self.ensure_supply().await?;
        Ok(Stats {
            stakes,
            pool_staking_supply,
            network_staking_supply,
            paid: rewards,
            stakers,
        })
    }

    pub async fn history(&self) -> Result<PoolHistory> {
        let (staking_balance, stakes) = tokio::try_join!(
            database::get_work_history(&self.pool, &self.chain_id),
            database::get_stake_count_history(&self.pool, &self.chain_id)
        )?;
        Ok(PoolHistory {
            staking_balance,
            stakes,
        })
    }

    pub async fn work(&self) -> Result<Vec<WorkShare>> {
        database::get_work(&self.pool, &self.chain_id).await
    }

    pub async fn stakers(
        &self,
        identity_addresses: Vec<Address>,
        staker_status: Option<StakerStatus>,
    ) -> Result<Vec<Staker>> {
        let mut stakers = if let Some(status) = staker_status {
            database::get_stakers_by_status(&self.pool, &self.chain_id, status).await?
        } else if identity_addresses.is_empty() {
            database::get_all_stakers(&self.pool, &self.chain_id).await?
        } else {
            database::get_stakers_by_identity_address(
                &self.pool,
                &self.chain_id,
                &identity_addresses,
            )
            .await?
        };
        if !identity_addresses.is_empty() {
            stakers.retain(|s| identity_addresses.contains(&s.identity_address));
        }
        Ok(stakers)
    }

    pub async fn stakes(
        &self,
        stake_status: Option<StakeStatus>,
        limit: Option<u32>,
        before_height: Option<u64>,
    ) -> Result<Vec<Stake>> {
        if limit.is_some() || before_height.is_some() {
            database::get_stakes_page(
                &self.pool,
                &self.chain_id,
                stake_status,
                before_height,
                limit.unwrap_or(200),
            )
            .await
        } else if let Some(status) = stake_status {
            database::get_stakes_by_status(&self.pool, &self.chain_id, status, None).await
        } else {
            database::get_stakes(&self.pool, &self.chain_id, None).await
        }
    }

    pub async fn payouts(
        &self,
        identity_addresses: Vec<Address>,
        limit: Option<u32>,
        before_height: Option<u64>,
    ) -> Result<Vec<PayoutMember>> {
        if limit.is_some() || before_height.is_some() {
            database::get_payout_members_page(
                &self.pool,
                &self.chain_id,
                &identity_addresses,
                before_height,
                limit.unwrap_or(200),
            )
            .await
        } else if identity_addresses.is_empty() {
            database::get_all_payout_members(&self.pool, &self.chain_id).await
        } else {
            let mut conn = self.pool.acquire().await?;
            database::get_payout_members(&mut conn, &self.chain_id, &identity_addresses).await
        }
    }

    pub async fn staker_earnings(
        &self,
        identity_addresses: Vec<Address>,
    ) -> Result<HashMap<Address, StakerEarnings>> {
        let mut conn = self.pool.acquire().await?;
        let payout_members =
            database::get_payout_members(&mut conn, &self.chain_id, &identity_addresses).await?;

        let mut hm = HashMap::new();
        for pm in payout_members {
            hm.entry(pm.identity_address.clone())
                .and_modify(|bal: &mut StakerEarnings| {
                    if pm.txid.is_none() {
                        bal.pending += pm.reward
                    } else {
                        bal.paid += pm.reward
                    }
                })
                .or_insert(StakerEarnings::from(pm));
        }
        Ok(hm)
    }

    pub async fn staking_balance(
        &self,
        identity_addresses: Vec<Address>,
    ) -> Result<HashMap<Address, Amount>> {
        if identity_addresses.is_empty() {
            return self.ensure_unspent().await;
        }

        if let Some(cached) = self.read_unspent() {
            let subset: HashMap<_, _> = identity_addresses
                .iter()
                .filter_map(|addr| cached.get(addr).copied().map(|amt| (addr.clone(), amt)))
                .collect();
            if subset.len() == identity_addresses.len() {
                return Ok(subset);
            }
        }

        let addresses = database::get_stakers_by_identity_address(
            &self.pool,
            &self.chain_id,
            &identity_addresses,
        )
        .await?
        .into_iter()
        .map(|s| s.identity_address)
        .collect::<Vec<_>>();
        self.list_unspent(addresses).await
    }

    pub async fn staking_supply(
        &self,
        identity_addresses: Vec<Address>,
    ) -> Result<StakingSupply> {
        let (pool_amount, network) = self.ensure_supply().await?;
        let mut staker_supply = 0.0;
        if !identity_addresses.is_empty() {
            let cfg = self.chain_config.clone();
            let stakers = database::get_stakers_by_identity_address(
                &self.pool,
                &self.chain_id,
                &identity_addresses,
            )
            .await?;
            staker_supply = tokio::task::spawn_blocking(move || {
                let client: VerusClient = (&cfg).try_into()?;
                let height = client.get_blockchain_info()?.blocks;
                let cooldown = height.saturating_sub(6) as i64;
                let filtered = stakers
                    .into_iter()
                    .filter(|s| {
                        if s.status != StakerStatus::Active {
                            return false;
                        }
                        client
                            .get_identity_history(&s.identity_address.to_string(), 0, 9999999)
                            .map(|identity| identity.blockheight < cooldown)
                            .unwrap_or(false)
                    })
                    .map(|s| s.identity_address)
                    .collect::<Vec<_>>();
                if filtered.is_empty() {
                    return anyhow::Ok(0.0);
                }
                let list_unspent =
                    client.list_unspent(Some(150), Some(99999999), Some(&filtered))?;
                Ok(list_unspent
                    .iter()
                    .fold(SignedAmount::ZERO, |acc, sum| acc + sum.amount)
                    .as_vrsc())
            })
            .await
            .map_err(|e| anyhow!(e))??;
        }

        Ok(StakingSupply {
            staker: staker_supply,
            pool: pool_amount.as_vrsc(),
            network,
        })
    }

    pub async fn set_staker_fee(
        &self,
        address: Address,
        fee: Decimal,
    ) -> Result<Option<Staker>> {
        database::update_staker_fee(&self.pool, &self.chain_id, &address, fee).await
    }

    pub async fn staker_status(&self, address: Address) -> Result<Option<Staker>> {
        let (os_tx, os_rx) = oneshot::channel();
        self.tx
            .send(CoinStakerMessage::StakerStatus(os_tx, address))
            .await
            .map_err(|e| anyhow!("Could not send Coinstaker message: {e}"))?;
        os_rx.await.map_err(|e| anyhow!("Sender dropped: {e}"))
    }

    fn read_supply(&self) -> Option<SupplyCache> {
        self.supply_cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn write_supply(&self, cache: SupplyCache) {
        *self
            .supply_cache
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(cache);
    }

    fn read_unspent(&self) -> Option<HashMap<Address, Amount>> {
        self.unspent_cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|c| c.by_address.clone())
    }

    fn write_unspent(&self, cache: UnspentCache) {
        *self
            .unspent_cache
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(cache);
    }

    async fn ensure_supply(&self) -> Result<(Amount, f64)> {
        if let Some(c) = self.read_supply() {
            return Ok((c.eligible, c.network));
        }
        let cfg = self.chain_config.clone();
        let (height, eligible, network) = tokio::task::spawn_blocking(move || {
            let client: VerusClient = (&cfg).try_into()?;
            let height = client.get_blockchain_info()?.blocks;
            let eligible = client.get_wallet_info()?.eligible_staking_balance;
            let network = client.get_mining_info()?.stakingsupply;
            anyhow::Ok((height, eligible, network))
        })
        .await
        .map_err(|e| anyhow!(e))??;
        self.write_supply(SupplyCache {
            height,
            eligible,
            network,
        });
        Ok((eligible, network))
    }

    async fn ensure_unspent(&self) -> Result<HashMap<Address, Amount>> {
        if let Some(map) = self.read_unspent() {
            return Ok(map);
        }
        let addresses = database::get_stakers_by_status(
            &self.pool,
            &self.chain_id,
            StakerStatus::Active,
        )
        .await?
        .into_iter()
        .map(|s| s.identity_address)
        .collect::<Vec<_>>();
        let cfg = self.chain_config.clone();
        let (height, map) = tokio::task::spawn_blocking(move || {
            let client: VerusClient = (&cfg).try_into()?;
            let height = client.get_blockchain_info()?.blocks;
            if addresses.is_empty() {
                return anyhow::Ok((height, HashMap::new()));
            }
            let utxos = client.list_unspent(Some(150), None, Some(&addresses))?;
            Ok((height, fold_unspent(utxos)))
        })
        .await
        .map_err(|e| anyhow!(e))??;
        self.write_unspent(UnspentCache {
            height,
            by_address: map.clone(),
        });
        Ok(map)
    }

    async fn list_unspent(&self, addresses: Vec<Address>) -> Result<HashMap<Address, Amount>> {
        if addresses.is_empty() {
            return Ok(HashMap::new());
        }
        let cfg = self.chain_config.clone();
        tokio::task::spawn_blocking(move || {
            let client: VerusClient = (&cfg).try_into()?;
            let utxos = client.list_unspent(Some(150), None, Some(&addresses))?;
            anyhow::Ok(fold_unspent(utxos))
        })
        .await
        .map_err(|e| anyhow!(e))?
    }
}

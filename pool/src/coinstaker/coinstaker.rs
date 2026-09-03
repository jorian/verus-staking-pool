use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use anyhow::{anyhow, Result};
use axum::async_trait;
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use sqlx::PgPool;
use tokio::select;
use tokio::sync::{mpsc, oneshot};
use tokio_graceful_shutdown::{IntoSubsystem, SubsystemHandle};
use tracing::{debug, error, info, instrument, trace, warn};
use vrsc_rpc::bitcoin::BlockHash;
use vrsc_rpc::client::{Client as VerusClient, RpcApi};
use vrsc_rpc::json::identity::IdentityPrimary;
use vrsc_rpc::json::vrsc::Address;
use vrsc_rpc::json::{Block, ValidationType};

use crate::coinstaker::constants::{Stake, StakeStatus};
use crate::coinstaker::http::WebhookMessage;
use super::handle::{fold_unspent, CoinStakerHandle, SupplyCache, UnspentCache};
use crate::util::verus::*;
use crate::database;

use super::config::Config as CoinstakerConfig;
use super::constants::Staker;
use super::http::Webhook;
use super::StakerStatus;

#[derive(Debug)]
pub struct CoinStaker {
    pool: PgPool,
    pub config: CoinstakerConfig,
    tx: mpsc::Sender<CoinStakerMessage>,
    rx: mpsc::Receiver<CoinStakerMessage>,
    pub chain_id: Address,
    webhooks: Webhook,
    supply_cache: Arc<RwLock<Option<SupplyCache>>>,
    unspent_cache: Arc<RwLock<Option<UnspentCache>>>,
}

impl CoinStaker {
    pub fn new(
        pool: PgPool,
        config: CoinstakerConfig,
        tx: mpsc::Sender<CoinStakerMessage>,
        rx: mpsc::Receiver<CoinStakerMessage>,
    ) -> Result<Self> {
        let webhooks = Webhook::new(config.webhook_endpoints.clone())?;
        let chain_id = config.currency_id.clone();

        Ok(Self {
            pool,
            config,
            tx,
            rx,
            chain_id,
            webhooks,
            supply_cache: Arc::new(RwLock::new(None)),
            unspent_cache: Arc::new(RwLock::new(None)),
        })
    }

    pub fn handle(&self) -> CoinStakerHandle {
        CoinStakerHandle {
            tx: self.tx.clone(),
            pool: self.pool.clone(),
            chain_id: self.chain_id.clone(),
            pool_primary_address: self.config.pool_primary_address.clone(),
            chain_config: self.config.chain_config.clone(),
            supply_cache: self.supply_cache.clone(),
            unspent_cache: self.unspent_cache.clone(),
        }
    }

    pub fn verusd(&self) -> Result<VerusClient> {
        let verus_client = (&self.config.chain_config).try_into()?;

        Ok(verus_client)
    }

    #[instrument(skip(self), fields(coin = self.config.currency_name))]
    async fn listen(&mut self) -> Result<()> {
        trace!("listening for messages");
        if let Err(err) = self.refresh_http_caches().await {
            warn!(?err, "http cache warmup failed");
        }

        while let Some(msg) = self.rx.recv().await {
            trace!(?msg, "received new ZMQ message");
            match msg {
                CoinStakerMessage::Block(block_hash) => {
                    // 1. check subscription of currently active subscribers.
                    // 2. check if any pending stakes have matured
                    // 3. check if daemon is staking
                    // 4. add work
                    // 5. check if the current block hash is a stake (this moves work until now into pending stake)
                    let verus_client = self.verusd()?;
                    let block = verus_client.get_block(&block_hash, 2)?;
                    info!(?block_hash, height = %block.height, "received new block");
                    // if a staker leaves this round, a last round of work needs to be added to his address,
                    // as he still could have staked this round's block, he needs to be counted
                    // in add_work()
                    // because stakers are active up to and including this round, we need to
                    // count them towards work and check if they staked, **before** we remove them
                    // as active stakers
                    let active_stakers = database::get_stakers_by_status(
                        &self.pool,
                        &self.chain_id,
                        StakerStatus::Active,
                    )
                    .await?;
                    self.check_stakers(
                        &verus_client,
                        block
                            .tx
                            .iter()
                            .flat_map(|tx| {
                                tx.vout.iter().filter_map(|vout| {
                                    vout.script_pubkey
                                        .identityprimary
                                        .clone()
                                        .map(|idp| idp.identityaddress)
                                })
                            })
                            .collect(),
                    )
                    .await?;
                    self.check_maturing_stakes(&verus_client).await?;

                    if self.daemon_is_staking(&verus_client).await? {
                        // Detect the stake first so add_work can credit the spent UTXO at
                        // the find height, but do not store it yet: store_new_stake moves
                        // round 0, which must include this block's work.
                        let this_stake = self.is_stake(&block_hash).await?;
                        self.add_work(&active_stakers, block.height, this_stake.as_ref())
                            .await?;
                        database::update_last_height(&self.pool, &self.chain_id, block.height)
                            .await?;
                        if let Some(stake) = this_stake {
                            self.commit_stake(stake).await?;
                        }
                    }

                    // Eligible staking only changes per block. Warm before the next HTTP
                    // request so a reload does not wait on getwalletinfo/listunspent.
                    if let Err(err) = self.refresh_http_caches().await {
                        warn!(?err, "http cache refresh failed");
                    }
                }
                CoinStakerMessage::StakerStatus(os_tx, identity_address) => {
                    let verus_client = self.verusd()?;
                    let opt_staker = self
                        .check_staker_status(&verus_client, &identity_address)
                        .await?;

                    os_tx
                        .send(opt_staker)
                        .expect("a oneshot message failed to send");
                }
                CoinStakerMessage::SetStaking(enable_staking) => {
                    let verus_client = self.verusd()?;

                    verus_client.set_generate(enable_staking, 0)?;
                }
                CoinStakerMessage::CheckBlockManually(os_tx, height) => {
                    let block_hash = self.verusd()?.get_block_by_height(height, 1)?.hash;
                    if let Some(stake) = self.is_stake(&block_hash).await? {
                        // this is an after the fact stake, don't move the round but copy from 0
                        database::store_new_stake(&self.pool, &stake, false).await?;
                    }
                }
            }
        }

        Ok(())
    }

    async fn check_maturing_stakes(&self, client: &VerusClient) -> Result<()> {
        let maturing_stakes =
            database::get_stakes_by_status(&self.pool, &self.chain_id, StakeStatus::Maturing, None)
                .await?;

        for mut stake in maturing_stakes {
            let block = client.get_block(&stake.block_hash, 2)?;

            if block.confirmations < 0 {
                trace!(block_hash = %block.hash, height = %block.height, amount = %stake.amount.as_vrsc(), "stake is stale");

                database::move_work_to_round_zero(&self.pool, &self.chain_id, block.height).await?;
                stake.status = StakeStatus::Stale;
                database::store_stake(&self.pool, &stake).await?;

                self.webhooks
                    .send(WebhookMessage::StakeStale {
                        hash: stake.block_hash,
                        height: stake.block_height,
                    })
                    .await;

                return Ok(());
            }

            if block.confirmations < 100 {
                if check_stake_guard(&block).await? {
                    trace!("The transaction was spent by stakeguard");
                    stake.status = StakeStatus::StakeGuard;

                    database::store_stake(&self.pool, &stake).await?;
                    // TODO punish perpetrator
                    // TODO send webhook message

                    return Ok(());
                }

                trace!(block_hash = %block.hash, height = %block.height, amount = %stake.amount.as_vrsc(), "stake still maturing");
            } else {
                trace!(block_hash = %block.hash, height = %block.height, amount = %stake.amount.as_vrsc(), "stake has matured");

                stake.status = StakeStatus::Matured;
                database::store_stake(&self.pool, &stake).await?;

                self.webhooks
                    .send(WebhookMessage::StakeMatured {
                        hash: stake.block_hash,
                        height: stake.block_height,
                    })
                    .await;
            }
        }
        // get pending stakes from database
        // check if any has matured
        // check if stake was stolen
        // if stake matured
        // - send webhooks message
        // - send matured_block message to self
        Ok(())
    }

    async fn daemon_is_staking(&self, client: &VerusClient) -> Result<bool> {
        if !client.get_mining_info()?.staking {
            error!("daemon not staking, not counting work");

            return Ok(false);
        }

        Ok(true)
    }

    /// Add work for every staker that was active until this round
    ///
    /// For a staker to have work added, the following conditions apply:
    /// - the verusid is not cooling down (150 blocks after a change)
    /// - the UTXOs that are used for staking must have 150+ confirmations
    ///
    /// An exception is made when an UTXO is cooling down after mining a block
    /// for the staking pool. It is still counted towards work.
    ///
    /// `listunspent(minconf=150)` drops the spent staking UTXO immediately and only
    /// includes the new coinbase at confirmations >= 150 (height N+149). Credit
    /// `source_amount` for find height N through N+148 so the snapshot stays flat.
    async fn add_work(
        &self,
        active_stakers: &[Staker],
        blockheight: u64,
        current_stake: Option<&Stake>,
    ) -> Result<()> {
        let verus_client = self.verusd()?;

        let active_staker_addresses = active_stakers
            .iter()
            .map(|subscriber| subscriber.identity_address.clone())
            .collect::<Vec<Address>>();

        let pool_extra = Self::eligible_sats(&verus_client, vec![self.config.pool_address.clone()])?;

        if active_staker_addresses.is_empty() {
            let mut payload = HashMap::new();
            if let Some(stake) = current_stake {
                Self::credit_spent_stake(&mut payload, stake);
            }
            database::store_work(
                &self.pool,
                &self.chain_id,
                payload,
                blockheight,
                pool_extra,
            )
            .await?;
            return Ok(());
        }

        let eligible_stakers =
            verus_client.list_unspent(Some(150), None, Some(active_staker_addresses.as_ref()))?;

        let mut payload = eligible_stakers
            .into_iter()
            .filter(|lu| lu.amount.is_positive())
            .map(|lu| {
                (
                    lu.address.unwrap(),
                    Decimal::from_u64(lu.amount.to_unsigned().unwrap().as_sat()).unwrap(),
                )
            })
            .fold(HashMap::new(), |mut acc, (address, amount)| {
                let _ = *acc
                    .entry(address)
                    .and_modify(|mut a| a += amount)
                    .or_insert(amount);
                acc
            });

        let stakes_to_compensate =
            database::get_stakes_to_compensate(&self.pool, &self.chain_id, blockheight as i64)
                .await?;

        for stake in &stakes_to_compensate {
            Self::credit_spent_stake(&mut payload, stake);
        }
        if let Some(stake) = current_stake {
            Self::credit_spent_stake(&mut payload, stake);
        }

        debug!(?payload, %pool_extra, "storing work");

        database::store_work(
            &self.pool,
            &self.chain_id,
            payload,
            blockheight,
            pool_extra,
        )
        .await?;

        Ok(())
    }

    fn credit_spent_stake(payload: &mut HashMap<Address, Decimal>, stake: &Stake) {
        let amount = Decimal::from_i64(stake.source_amount.as_sat() as i64).unwrap_or(Decimal::ZERO);
        if amount.is_zero() {
            return;
        }
        debug!(
            amount_to_add = %stake.source_amount.as_vrsc(),
            staker = %stake.found_by,
            blockheight = stake.block_height,
            "compensate work of immature utxo because it staked"
        );
        *payload.entry(stake.found_by.clone()).or_insert(Decimal::ZERO) += amount;
    }

    fn eligible_sats(client: &VerusClient, addresses: Vec<Address>) -> Result<Decimal> {
        if addresses.is_empty() {
            return Ok(Decimal::ZERO);
        }
        let utxos = client.list_unspent(Some(150), None, Some(&addresses))?;
        let mut total = Decimal::ZERO;
        for utxo in utxos {
            if !utxo.amount.is_positive() {
                continue;
            }
            total += Decimal::from_u64(utxo.amount.to_unsigned().unwrap().as_sat()).unwrap();
        }
        Ok(total)
    }

    #[instrument(skip(self))]
    async fn commit_stake(&self, stake: Stake) -> Result<()> {
        info!(height = %stake.block_height, ">>>>>>>>>>>>>>> stake found");

        database::store_new_stake(&self.pool, &stake, true).await?;

        let client = self.verusd()?;
        let currency_name = client
            .get_currency(&stake.currency_address.to_string())?
            .fullyqualifiedname;

        self.webhooks
            .send(WebhookMessage::new_stake(currency_name, &stake))
            .await;

        Ok(())
    }

    async fn is_stake(&self, block_hash: &BlockHash) -> Result<Option<Stake>> {
        let client = self.verusd()?;
        let block = client.get_block(block_hash, 2)?;

        // block.confirmations == -1 indicates it is stale and should be ignored
        if matches!(block.validation_type, ValidationType::Stake) && block.confirmations >= 0 {
            let postxddest = postxddest(&block)?;

            if let Some(stake) = self.is_staked_by_pool(&block, &postxddest).await? {
                return Ok(Some(stake));
            }

            let active_stakers =
                database::get_stakers_by_status(&self.pool, &self.chain_id, StakerStatus::Active)
                    .await?;

            let Some(staker) = active_stakers.iter().find(|s| {
                &s.identity_address == &postxddest && s.currency_address == self.chain_id
            }) else {
                return Ok(None);
            };

            trace!("{} staked a block", staker.identity_address);

            let stake = Stake::try_new(&self.chain_id, &block)?;

            return Ok(Some(stake));
        }

        Ok(None)
    }

    async fn is_staked_by_pool(
        &self,
        block: &Block,
        postxddest: &Address,
    ) -> Result<Option<Stake>> {
        if &self.config.pool_address == postxddest {
            info!(?postxddest, "staked by pool address");

            let stake = Stake::try_new(&self.chain_id, &block)?;

            Ok(Some(stake))
        } else {
            Ok(None)
        }
    }

    fn identity_is_eligible(&self, identity: &IdentityPrimary) -> bool {
        // general conditions that need to be true regardless of vault conditions
        if identity.minimumsignatures == 1
            && identity.primaryaddresses.len() > 1
            && identity
                .primaryaddresses
                .contains(&self.config.pool_primary_address)
        {
            if let Some(conditions) = &self.config.vault_conditions {
                // check vault conditions
                if identity.primaryaddresses.len() <= conditions.max_primary_addresses as usize
                    && if conditions.strict_recovery_id {
                        identity.recoveryauthority != identity.identityaddress
                            && identity.revocationauthority != identity.identityaddress
                    } else {
                        true
                    }
                {
                    match identity.flags {
                        0 => {
                            // no time lock set
                            return true;
                        }
                        // fixed time lock; unlock at x seconds (epoch)
                        1 => {
                            // TODO v2, ineligible until then
                            return false;
                        }
                        // delay lock; unlock after x seconds
                        2 => return identity.timelock >= conditions.min_time_lock as u64,
                        _ => return false,
                    }
                }
            } else {
                return true;
            }
        }

        false
    }

    fn tip_height(&self) -> Result<u64> {
        Ok(self.verusd()?.get_blockchain_info()?.blocks)
    }

    /// Fill supply + unspent caches for this tip. Runs `getwalletinfo` and
    /// `listunspent` in parallel so a miss is ~1.7s instead of both in series.
    async fn refresh_http_caches(&mut self) -> Result<()> {
        let height = self.tip_height()?;
        let need_supply = self
            .supply_cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|c| c.height)
            != Some(height);
        let need_unspent = self
            .unspent_cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|c| c.height)
            != Some(height);
        if !need_supply && !need_unspent {
            return Ok(());
        }

        let chain_config = self.config.chain_config.clone();
        let supply_cfg = need_supply.then(|| chain_config.clone());
        let unspent_cfg = need_unspent.then(|| chain_config);
        let addresses = if need_unspent {
            database::get_stakers_by_status(&self.pool, &self.chain_id, StakerStatus::Active)
                .await?
                .into_iter()
                .map(|s| s.identity_address)
                .collect::<Vec<_>>()
        } else {
            vec![]
        };

        let supply_task = tokio::task::spawn_blocking(move || {
            let Some(cfg) = supply_cfg else {
                return anyhow::Ok(None);
            };
            let client: VerusClient = (&cfg).try_into()?;
            let eligible = client.get_wallet_info()?.eligible_staking_balance;
            let network = client.get_mining_info()?.stakingsupply;
            Ok(Some((eligible, network)))
        });
        let unspent_task = tokio::task::spawn_blocking(move || {
            let Some(cfg) = unspent_cfg else {
                return anyhow::Ok(None);
            };
            if addresses.is_empty() {
                return Ok(Some(HashMap::new()));
            }
            let client: VerusClient = (&cfg).try_into()?;
            let utxos = client.list_unspent(Some(150), None, Some(&addresses))?;
            Ok(Some(fold_unspent(utxos)))
        });

        let (supply, unspent) = tokio::try_join!(supply_task, unspent_task)
            .map_err(|e| anyhow!("http cache refresh: {e}"))?;
        if let Some((eligible, network)) = supply? {
            *self
                .supply_cache
                .write()
                .unwrap_or_else(|e| e.into_inner()) = Some(SupplyCache {
                height,
                eligible,
                network,
            });
        }
        if let Some(map) = unspent? {
            *self
                .unspent_cache
                .write()
                .unwrap_or_else(|e| e.into_inner()) = Some(UnspentCache {
                height,
                by_address: map,
            });
        }
        Ok(())
    }

    async fn check_stakers(
        &self,
        verus_client: &VerusClient,
        identity_address: Vec<Address>,
    ) -> Result<()> {
        for address in identity_address {
            self.check_staker_status(verus_client, &address).await?;
        }

        let cooling_down_stakers =
            database::get_stakers_by_status(&self.pool, &self.chain_id, StakerStatus::CoolingDown)
                .await?;

        let height = verus_client.get_blockchain_info()?.blocks;

        for mut cooling_down_staker in cooling_down_stakers {
            let identity = verus_client.get_identity_history(
                &cooling_down_staker.identity_address.to_string(),
                0,
                99999999,
            )?;
            if identity.blockheight < height.saturating_sub(6) as i64 {
                trace!(?cooling_down_staker, "id has cooled down, activate");
                cooling_down_staker.status = StakerStatus::Active;

                database::store_staker(&self.pool, &cooling_down_staker).await?;

                self.webhooks
                    .send(WebhookMessage::NewStaker {
                        identity_address: cooling_down_staker.identity_address,
                        identity_name: cooling_down_staker.identity_name,
                    })
                    .await;
            } else {
                trace!(?cooling_down_staker, "staker still cooling down");
            }
        }

        Ok(())
    }

    async fn check_staker_status(
        &self,
        client: &VerusClient,
        identity_address: &Address,
    ) -> Result<Option<Staker>> {
        let identity = client.get_identity(&identity_address.to_string())?;
        let currency = client.get_currency(&self.chain_id.to_string())?;

        // if the chain has IDSTAKING enabled, check if this staker has a root id for this chain
        // if not, it's not eligible.
        if currency.options & 0b100 != 0
            && (identity.identity.systemid != self.chain_id
                || identity.identity.parent != self.chain_id)
        {
            return Ok(None);
        }

        if let Some(mut staker) = database::get_staker(
            &self.pool,
            &self.chain_id,
            &identity.identity.identityaddress,
        )
        .await?
        {
            debug!(?staker, "staker found in database");

            match staker.status {
                StakerStatus::Active => {
                    if !self.identity_is_eligible(&identity.identity) {
                        trace!(?identity, "a change to this verusid made it inactive");
                        staker.status = StakerStatus::Inactive;
                        database::store_staker(&self.pool, &staker).await?;

                        self.webhooks
                            .send(WebhookMessage::LeavingStaker {
                                identity_address: staker.identity_address.clone(),
                                identity_name: staker.identity_name.clone(),
                            })
                            .await;
                        // TODO any change to a verusid was supposed to set eligibility for
                        // staking to false, so we would have to wait for that time to pass.
                        // but this doesn't seem to be the case, at least not for some kinds
                        // of upgrade. Needs investigating.
                        // } else {
                        // staker.status = StakerStatus::CoolingDown;
                        // database::store_staker(&self.pool, &staker).await?;
                    }
                }
                StakerStatus::CoolingDown => {
                    // an update was made to a staker that was already cooling down.
                    if !self.identity_is_eligible(&identity.identity) {
                        trace!(?identity, "a change to this verusid made it inactive");

                        staker.status = StakerStatus::Inactive;
                        database::store_staker(&self.pool, &staker).await?;
                    }
                }
                StakerStatus::Inactive => {
                    if self.identity_is_eligible(&identity.identity) {
                        trace!(?staker, "inactive staker got reactivated");
                        staker.status = StakerStatus::CoolingDown;
                        database::store_staker(&self.pool, &staker).await?;
                    }
                }
            }

            return Ok(Some(staker));
        } else {
            trace!("verusid not found in database");

            if self.identity_is_eligible(&identity.identity) {
                let staker = Staker::new(
                    self.chain_id.clone(),
                    identity.identity.identityaddress.clone(),
                    identity.fullyqualifiedname.clone(),
                    self.config.min_payout,
                    StakerStatus::CoolingDown,
                    self.config.fee,
                );

                database::store_staker(&self.pool, &staker).await?;
                trace!("new staker stored in database.");

                return Ok(Some(staker));
            } else {
                trace!(id = &identity.fullyqualifiedname, "verusid not eligible");
            }
            // if the staker does not yet exist, we should check if it contains
            // the primary address of the pool
            // and if it fulfills the vault conditions
        }

        Ok(None)
    }
}

#[cfg(not(feature = "mock"))]
#[async_trait]
impl IntoSubsystem<anyhow::Error> for CoinStaker {
    async fn run(mut self, subsys: SubsystemHandle) -> Result<()> {
        info!("starting coinstaker {}", self.config.currency_name);
        let client = self.verusd()?;

        tokio::spawn(super::zmq::tmq_block_listen(
            self.config.chain_config.zmq_port_blocknotify,
            self.tx.clone(),
        ));

        let identities_with_address = client
            .get_identities_with_address(
                &self.config.pool_primary_address.to_string(),
                Some(3400000),
                None,
                None,
            )?
            .into_iter()
            .map(|idp| idp.identityaddress)
            .collect();

        self.check_stakers(&client, identities_with_address).await?;

        if !self.config.skip_preflight {
            // some preflight checks are needed:
            if let Some(mut last_height) =
                database::get_last_height(&self.pool, &self.chain_id).await?
            {
                trace!(%last_height, "Do some preflight checks");

                let chain_tip = client.get_blockchain_info()?.blocks;

                for i in last_height..=chain_tip {
                    let block = client.get_block_by_height(i, 2)?;

                    let identities = block
                        .tx
                        .into_iter()
                        .flat_map(|tx| {
                            tx.vout.into_iter().filter_map(|vout| {
                                vout.script_pubkey
                                    .identityprimary
                                    .map(|idp| idp.identityaddress)
                            })
                        })
                        .collect();

                    self.check_stakers(&client, identities).await?;

                    last_height += 1;
                }

                self.check_maturing_stakes(&client).await?;

                trace!(%last_height, "Finished doing preflight checks");

                database::update_last_height(&self.pool, &self.chain_id, last_height).await?;
            }
        }

        select! {
            _ = subsys.on_shutdown_requested() => {
                info!("shutting down coinstaker, disable staking");
            },
            r = self.listen() => {
                error!("main event loop stopped");
                if let Err(e) = r { error!("{e:?}") }
            },

        }

        // if pool stops, stop staking
        disable_staking(self.verusd()?)?;

        Ok(())
    }
}

#[derive(Debug)]
pub enum CoinStakerMessage {
    Block(BlockHash),
    StakerStatus(oneshot::Sender<Option<Staker>>, Address),
    SetStaking(bool),
    CheckBlockManually(oneshot::Sender<()>, u64),
}

use std::collections::HashMap;

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
use vrsc_rpc::json::vrsc::{Address, Amount};
use vrsc_rpc::json::{Block, ValidationType};

use crate::coinstaker::constants::{Stake, StakeStatus};
use crate::coinstaker::http::WebhookMessage;
use crate::database;
use crate::http::constants::{StakingSupply, Stats};
use crate::payout_service::PayoutMember;
use crate::util::verus::*;

use super::config::Config as CoinstakerConfig;
use super::constants::{Staker, StakerEarnings};
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
        })
    }

    pub fn verusd(&self) -> Result<VerusClient> {
        let verus_client = (&self.config.chain_config).try_into()?;

        Ok(verus_client)
    }

    #[instrument(skip(self), fields(coin = self.config.currency_name))]
    async fn listen(&mut self) -> Result<()> {
        trace!("listening for messages");

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
                    self.check_stakers(&verus_client, &block).await?;
                    self.check_maturing_stakes(&verus_client).await?;

                    if self.daemon_is_staking(&verus_client).await? == false {
                        continue; // don't add work for not staking daemon
                    };

                    self.add_work(&active_stakers, block.height).await?;
                    database::update_last_height(&self.pool, &self.chain_id, block.height).await?;

                    self.check_for_stake(&block_hash).await?;
                }
                CoinStakerMessage::StakingSupply(os_tx, identity_addresses) => {
                    let res = self.get_staking_supply(identity_addresses).await?;

                    if os_tx.send(res).is_err() {
                        Err(anyhow!("the sender dropped"))?
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
                CoinStakerMessage::GetStakers(os_tx, identity_addresses, staker_status) => {
                    let staker = if let Some(status) = staker_status {
                        // TODO build a better query for this:
                        database::get_stakers_by_status(&self.pool, &self.chain_id, status)
                            .await?
                            .into_iter()
                            .filter(|s| identity_addresses.contains(&s.identity_address))
                            .collect::<Vec<_>>()
                    } else {
                        database::get_stakers_by_identity_address(
                            &self.pool,
                            &self.chain_id,
                            &identity_addresses,
                        )
                        .await?
                    };
                    if os_tx.send(staker).is_err() {
                        Err(anyhow!("the sender dropped"))?
                    }
                }
                CoinStakerMessage::GetPayouts(os_tx, identity_addresses) => {
                    let mut conn = self.pool.acquire().await?;
                    let payout_members = database::get_payout_members(
                        &mut conn,
                        &self.chain_id,
                        &identity_addresses,
                    )
                    .await?;

                    if os_tx.send(payout_members).is_err() {
                        Err(anyhow!("the sender dropped"))?
                    }
                }
                CoinStakerMessage::GetStakes(os_tx, stake_status) => {
                    let stakes = if let Some(status) = stake_status {
                        database::get_stakes_by_status(&self.pool, &self.chain_id, status, None)
                            .await?
                    } else {
                        database::get_stakes(&self.pool, &self.chain_id, None).await?
                    };

                    if os_tx.send(stakes).is_err() {
                        Err(anyhow!("the sender dropped"))?
                    }
                }
                CoinStakerMessage::GetStakerEarnings(os_tx, identity_addresses) => {
                    let mut conn = self.pool.acquire().await?;
                    let payout_members = database::get_payout_members(
                        &mut conn,
                        &self.chain_id,
                        &identity_addresses,
                    )
                    .await?;

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

                    if os_tx.send(hm).is_err() {
                        Err(anyhow!("the sender dropped"))?
                    }
                }
                CoinStakerMessage::GetStakingBalance(os_tx, identity_addresses) => {
                    let verus_client = self.verusd()?;

                    let active_addresses = database::get_stakers_by_identity_address(
                        &self.pool,
                        &self.chain_id,
                        &identity_addresses,
                    )
                    .await?
                    .iter()
                    .map(|staker| staker.identity_address.clone())
                    .collect::<Vec<_>>();

                    let utxos = if !active_addresses.is_empty() {
                        verus_client.list_unspent(
                            Some(150),
                            None,
                            Some(active_addresses.as_ref()),
                        )?
                    } else {
                        vec![]
                    };

                    let payload = utxos
                        .into_iter()
                        .filter(|utxo| utxo.amount.is_positive())
                        // unwrap because we already filtered the positive
                        .map(|utxo| (utxo.address.unwrap(), utxo.amount.to_unsigned().unwrap()))
                        .fold(HashMap::new(), |mut acc, (address, amount)| {
                            let _ = *acc
                                .entry(address)
                                .and_modify(|a| *a += amount)
                                .or_insert(amount);
                            acc
                        });

                    if os_tx.send(payload).is_err() {
                        Err(anyhow!("the sender dropped"))?
                    }
                }
                CoinStakerMessage::PoolPrimaryAddress(os_tx) => {
                    let pool_address = self.config.pool_primary_address.to_string();

                    if os_tx.send(pool_address).is_err() {
                        Err(anyhow!("the sender dropped"))?
                    }
                }
                CoinStakerMessage::SetStaking(enable_staking) => {
                    let verus_client = self.verusd()?;

                    verus_client.set_generate(enable_staking, 0)?;
                }
                CoinStakerMessage::GetStatistics(os_tx) => {
                    let (stakes, stakers, rewards) = tokio::try_join!(
                        database::get_number_of_matured_stakes(&self.pool, &self.chain_id),
                        database::get_number_of_active_stakers(&self.pool, &self.chain_id),
                        database::get_total_rewards(&self.pool, &self.chain_id)
                    )?;

                    let pool_staking_supply =
                        self.verusd()?.get_wallet_info()?.eligible_staking_balance;

                    let stats = Stats {
                        stakes,
                        pool_staking_supply,
                        paid: rewards,
                        stakers,
                    };

                    if os_tx.send(stats).is_err() {
                        Err(anyhow!("the sender dropped"))?
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
    async fn add_work(&self, active_stakers: &[Staker], blockheight: u64) -> Result<()> {
        let verus_client = self.verusd()?;

        let active_staker_addresses = active_stakers
            .iter()
            .map(|subscriber| subscriber.identity_address.clone())
            .collect::<Vec<Address>>();

        if active_staker_addresses.is_empty() {
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

        stakes_to_compensate.iter().for_each(|stake| {
            if payload.contains_key(&stake.found_by) {
                payload.entry(stake.found_by.clone()).and_modify(|v| {
                    debug!(
                        amount_to_add = %stake.source_amount.as_vrsc(),
                        staker = %stake.found_by,
                        blockheight = &stake.block_height,
                        "compensate work of immature utxo because it staked"
                    );
                    *v += Decimal::from_i64(stake.source_amount.as_sat() as i64).unwrap()
                });
            }
        });

        debug!(?payload, "storing work");

        database::store_work(&self.pool, &self.chain_id, payload, blockheight).await?;

        Ok(())
    }

    #[instrument(skip(self))]
    async fn check_for_stake(&self, block_hash: &BlockHash) -> Result<()> {
        if let Some(stake) = self.is_stake(block_hash).await? {
            info!(height = %stake.block_height, ">>>>>>>>>>>>>>> stake found");

            database::store_new_stake(&self.pool, &stake).await?;

            let client = self.verusd()?;
            let currency_name = client
                .get_currency(&stake.currency_address.to_string())?
                .fullyqualifiedname;

            self.webhooks
                .send(WebhookMessage::new_stake(currency_name, &stake))
                .await;
        }

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

    /// Gets the staking supply of the given addresses
    ///
    /// Clients should figure out themselves whether the address is a staker in their pool.
    ///
    /// Addresses that are given but not known in this pool will return 0.
    async fn get_staking_supply(&self, identity_addresses: Vec<Address>) -> Result<StakingSupply> {
        let verus_client = self.verusd()?;
        let block_height = verus_client.get_blockchain_info()?.blocks;

        // let active_addresses = identity_addresses
        let stakers = database::get_stakers_by_identity_address(
            &self.pool,
            &self.chain_id,
            &identity_addresses,
        )
        .await?;

        let identity_addresses = stakers
            .into_iter()
            .filter(|s| {
                let is_subscribed = s.status == StakerStatus::Active;
                let is_cooled_down = if let Ok(identity) =
                    verus_client.get_identity_history(&s.identity_address.to_string(), 0, 9999999)
                {
                    let block = verus_client.get_block_by_height(block_height, 2).unwrap();

                    identity.blockheight < block.height.saturating_sub(6) as i64
                } else {
                    false
                };

                is_subscribed && is_cooled_down
            })
            .map(|s| s.identity_address)
            .collect::<Vec<_>>();

        let staking_supply =
            get_staking_supply(&self.chain_id, &identity_addresses, &verus_client)?;

        Ok(staking_supply)
    }

    async fn check_stakers(&self, verus_client: &VerusClient, block: &Block) -> Result<()> {
        for tx in &block.tx {
            for vout in &tx.vout {
                if let Some(identity_primary) = &vout.script_pubkey.identityprimary {
                    self.check_staker_status(verus_client, &identity_primary.identityaddress)
                        .await?;
                }
            }
        }

        let cooling_down_stakers =
            database::get_stakers_by_status(&self.pool, &self.chain_id, StakerStatus::CoolingDown)
                .await?;

        for mut cooling_down_staker in cooling_down_stakers {
            let identity = verus_client.get_identity_history(
                &cooling_down_staker.identity_address.to_string(),
                0,
                99999999,
            )?;
            if identity.blockheight < block.height.saturating_sub(6) as i64 {
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

        if !self.config.skip_preflight {
            // some preflight checks are needed:
            if let Some(mut last_height) =
                database::get_last_height(&self.pool, &self.chain_id).await?
            {
                trace!(%last_height, "Do some preflight checks");

                let chain_tip = client.get_blockchain_info()?.blocks;

                for i in last_height..=chain_tip {
                    let block = client.get_block_by_height(i, 2)?;

                    self.check_stakers(&client, &block).await?;
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

                disable_staking(self.verusd()?)?;
            },
            r = self.listen() => {
                warn!("stopped listening");
                if let Err(e) = r { error!("{e:?}") }
            },

        }

        Ok(())
    }
}

#[derive(Debug)]
pub enum CoinStakerMessage {
    Block(BlockHash),
    StakingSupply(oneshot::Sender<StakingSupply>, Vec<Address>),
    StakerStatus(oneshot::Sender<Option<Staker>>, Address),
    GetStakers(
        oneshot::Sender<Vec<Staker>>,
        Vec<Address>,
        Option<StakerStatus>,
    ),
    GetStakerEarnings(
        oneshot::Sender<HashMap<Address, StakerEarnings>>,
        Vec<Address>,
    ),
    GetStakingBalance(oneshot::Sender<HashMap<Address, Amount>>, Vec<Address>),
    GetPayouts(oneshot::Sender<Vec<PayoutMember>>, Vec<Address>),
    GetStakes(oneshot::Sender<Vec<Stake>>, Option<StakeStatus>),
    GetStatistics(oneshot::Sender<Stats>),
    PoolPrimaryAddress(oneshot::Sender<String>),
    SetStaking(bool),
}

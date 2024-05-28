use std::collections::HashMap;

use anyhow::{anyhow, Context, Result};
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
use crate::database;
use crate::http::constants::StakingSupply;
use crate::util::verus;
// use crate::util::verus;

use super::config::Config as CoinstakerConfig;
use super::constants::Staker;
use super::StakerStatus;
pub struct CoinStaker {
    pool: PgPool,
    config: CoinstakerConfig,
    tx: mpsc::Sender<CoinStakerMessage>,
    rx: mpsc::Receiver<CoinStakerMessage>,
    pub chain_id: Address,
}

impl CoinStaker {
    pub fn new(
        pool: PgPool,
        config: CoinstakerConfig,
        tx: mpsc::Sender<CoinStakerMessage>,
        rx: mpsc::Receiver<CoinStakerMessage>,
    ) -> Result<Self> {
        let chain_id = config.chain_id.clone();

        Ok(Self {
            pool,
            config,
            tx,
            rx,
            chain_id,
        })
    }

    pub fn verusd(&self) -> Result<VerusClient> {
        let verus_client = (&self.config.chain_config).try_into()?;

        Ok(verus_client)
    }

    async fn listen(&mut self) -> Result<()> {
        trace!("listening for messages");
        while let Some(msg) = self.rx.recv().await {
            match msg {
                CoinStakerMessage::Block(block_hash) => {
                    info!(?block_hash, "received new block!");

                    // 1. check subscription of currenctly active subscribers.
                    // 2. check if any pending stakes have matured
                    // 3. check if daemon is staking
                    // 4. add work
                    // 5. check if the current block hash is a stake (this moves work until now into pending stake)
                    let verus_client = self.verusd()?;
                    let block = verus_client.get_block(&block_hash, 2)?;
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
                    self.check_for_stake(&block_hash, &active_stakers).await?;
                }
                CoinStakerMessage::StakingSupply(os_tx, identity_addresses) => {
                    let res = self.get_staking_supply(identity_addresses).await?;

                    if let Err(_) = os_tx.send(res) {
                        Err(anyhow!("the sender dropped"))?
                    }
                }
                CoinStakerMessage::StakerStatus(os_tx, identity_address) => {
                    let verus_client = self.verusd()?;
                    self.check_staker_status(&verus_client, &identity_address)
                        .await?;

                    os_tx
                        .send("success".to_string())
                        .expect("a oneshot message failed to send");
                }
                CoinStakerMessage::Balance(os_tx) => {
                    let balance = self.verusd()?.get_balance(None, None)?;

                    if let Err(_) = os_tx.send(balance.as_vrsc()) {
                        Err(anyhow!("the sender dropped"))?
                    }
                }
            }
        }

        Ok(())
    }

    async fn check_maturing_stakes(&self, client: &VerusClient) -> Result<()> {
        let maturing_stakes =
            database::get_stakes_by_status(&self.pool, StakeStatus::Maturing).await?;

        for mut stake in maturing_stakes {
            let block = client.get_block(&stake.block_hash, 2)?;

            if block.confirmations < 0 {
                trace!(?stake, "stake is stale");

                stake.status = StakeStatus::Stale;
                database::store_stake(&self.pool, &stake).await?;
                // TODO send webhook message

                return Ok(());
            }

            if block.confirmations < 150 {
                if check_stake_guard(&self.pool, &block, stake.clone()).await? {
                    return Ok(());
                }

                trace!(?stake, "stake still maturing");
            } else {
                trace!(?stake, "stake has matured");
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
    async fn add_work(&self, active_stakers: &Vec<Staker>, blockheight: u64) -> Result<()> {
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

        let maturing_stakes =
            database::get_stakes_by_status(&self.pool, StakeStatus::Maturing).await?;

        maturing_stakes.iter().for_each(|stake| {
            if payload.contains_key(&stake.found_by) {
                trace!("adding work to staker to undo punishment (cooling down utxo after stake)");
                payload.entry(stake.found_by.clone()).and_modify(|v| {
                    debug!("source_amount: {}", stake.source_amount.as_sat() as i64);
                    debug!("sum: {}", v);
                    *v += Decimal::from_i64(stake.source_amount.as_sat() as i64).unwrap()
                });
            }
        });

        database::store_work(&self.pool, &self.chain_id, payload, blockheight).await?;

        Ok(())
    }

    #[instrument(skip(self), fields(chain = self.chain_id.to_string()))]
    async fn check_for_stake(
        &self,
        block_hash: &BlockHash,
        active_stakers: &Vec<Staker>,
    ) -> Result<()> {
        if let Some(stake) = self.is_stake(&block_hash).await? {
            debug!(?stake, "!!!!! STAKE FOUND !!!!!");

            database::store_new_stake(&self.pool, &stake).await?;
        }

        Ok(())
    }

    #[instrument(skip(self), fields(chain = self.chain_id.to_string()))]
    async fn is_stake(&self, block_hash: &BlockHash) -> Result<Option<Stake>> {
        let client = self.verusd()?;
        let block = client.get_block(block_hash, 2)?;

        // block.confirmations == -1 indicates it is stale and should be ignored
        if matches!(block.validation_type, ValidationType::Stake) && block.confirmations >= 0 {
            let postxddest = block
                .postxddest
                .context("a stake must always have a postxddest")?;
            let active_stakers =
                database::get_stakers_by_status(&self.pool, &self.chain_id, StakerStatus::Active)
                    .await?;

            let Some(staker) = active_stakers
                .iter()
                .find(|s| s.identity_address == postxddest && s.currency_address == self.chain_id)
            else {
                return Ok(None);
            };

            trace!("{} staked a block", staker.identity_address);

            let coinbase_value = block
                .tx
                .iter()
                .next()
                .context("there should always be a coinbase transaction")?
                .vout
                .first()
                .context("there should always be a coinbase output")?
                .value_sat;

            let staker_utxo_value = block
                .tx
                .iter()
                .last()
                .context("there should always be a stake spend utxo")?
                .vin
                .first()
                .context("there should always be an input to a stake spend")?
                .value_sat
                .context("there should always be a positive stake")?;

            let stake = Stake::new(
                &self.chain_id,
                block_hash,
                block.height,
                &postxddest,
                block
                    .possourcetxid
                    .context("there should always be a txid for the source stake")?,
                block
                    .possourcevoutnum
                    .context("there should always be a stake spend vout")?,
                staker_utxo_value,
                StakeStatus::Maturing,
                coinbase_value,
            );

            return Ok(Some(stake));
        }
        // get the block details from the daemon
        // we should check whether the staker of the block is an active member of our pool
        Ok(None)
    }

    fn identity_is_eligible(&self, identity: &IdentityPrimary) -> bool {
        // let conditions = &self.config.verus_vault_conditions;

        // general conditions that need to be true regardless of config options
        if identity.minimumsignatures == 1
            && identity.primaryaddresses.len() > 1
            && identity
                .primaryaddresses
                .contains(&self.config.pool_primary_address)
        {
            debug!(?identity, "identity is eligible");

            return true;
            // TODO
            // match identity.flags {
            //     // fixed time lock; unlock at x seconds (epoch)
            //     1 => {
            //         // TODO v2, ineligible until then
            //         return false;
            //     }
            //     // delay lock; unlock after x seconds
            //     2 => {
            //         return identity.timelock >= conditions.min_lock as u64
            //             && identity.primaryaddresses.len()
            //                 <= conditions.max_primary_addresses.try_into().unwrap_or(0)
            //             && identity.recoveryauthority != identity.identityaddress
            //             && identity.revocationauthority != identity.identityaddress
            //     }
            //     _ => return false,
            // }
        }

        // TODO notify an admin if identity was checked but not eligible
        false
    }

    /// Gets the staking supply of the given addresses
    ///
    /// Clients should figure out themselves whether the address is a staker in their pool.
    ///
    /// Addresses that are given but not known in this pool will return 0.
    async fn get_staking_supply(&self, identity_addresses: Vec<Address>) -> Result<StakingSupply> {
        let verus_client = self.verusd()?;
        let block_height = verus_client.get_mining_info()?.blocks;

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
                let is_subscribed = &s.status == &StakerStatus::Active;
                let is_cooled_down = if let Ok(identity) =
                    verus_client.get_identity_history(&s.identity_address.to_string(), 0, 9999999)
                {
                    let block = verus_client.get_block_by_height(block_height, 2).unwrap();

                    identity.blockheight < block.height.saturating_sub(150) as i64
                } else {
                    false
                };

                is_subscribed && is_cooled_down
            })
            .map(|s| s.identity_address)
            .collect::<Vec<_>>();

        let staking_supply =
            verus::get_staking_supply(&self.chain_id, &identity_addresses, &verus_client)?;

        Ok(staking_supply)
    }

    async fn check_stakers(&self, verus_client: &VerusClient, block: &Block) -> Result<()> {
        for tx in &block.tx {
            for vout in &tx.vout {
                if let Some(identity_primary) = &vout.script_pubkey.identityprimary {
                    self.check_staker_status(&verus_client, &identity_primary.identityaddress)
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
            if identity.blockheight < block.height.saturating_sub(150) as i64 {
                trace!(?cooling_down_staker, "id has cooled down, activate");
                cooling_down_staker.status = StakerStatus::Active;
                database::store_staker(&self.pool, &cooling_down_staker).await?;
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
    ) -> Result<()> {
        let identity = client.get_identity(&identity_address.to_string())?;

        if let Some(mut staker) = database::get_staker(
            &self.pool,
            &self.chain_id,
            &identity.identity.identityaddress,
        )
        .await?
        {
            debug!("staker found in database");

            match staker.status {
                StakerStatus::Active => {
                    if self.identity_is_eligible(&identity.identity) == false {
                        trace!(?identity, "a change to this verusid made it inactive");
                        staker.status = StakerStatus::Inactive;
                        database::store_staker(&self.pool, &staker).await?;
                    } else {
                        // if it is active, but it updated their id, staking rules
                        // prevent it from staking for 150 blocks.
                        // need to set a cooldown period

                        staker.status = StakerStatus::CoolingDown;
                        database::store_staker(&self.pool, &staker).await?;
                    }

                    // if staker is not eligible anymore, deactivate
                    // TODO self.webhooks.send(deactivated)
                }
                StakerStatus::CoolingDown => {
                    // an update was made to a staker that was already cooling down.
                    if self.identity_is_eligible(&identity.identity) == false {
                        trace!(?identity, "a change to this verusid made it inactive");

                        database::store_staker(&self.pool, &staker).await?;
                    }
                }
                StakerStatus::Inactive => {
                    if self.identity_is_eligible(&identity.identity) {
                        trace!(?staker, "inactive staker got reactivated");
                        staker.status = StakerStatus::CoolingDown;
                        database::store_staker(&self.pool, &staker).await?;
                    }
                    // if staker became eligible, reactivate
                    // TODO self.webhooks.send(activated)
                }
            }
        } else {
            trace!("verusid not found in database");

            if self.identity_is_eligible(&identity.identity) {
                let staker = Staker::new(
                    self.chain_id.clone(),
                    identity.identity.identityaddress.clone(),
                    identity.fullyqualifiedname.clone(),
                    self.config.min_payout,
                    StakerStatus::CoolingDown,
                );

                database::store_staker(&self.pool, &staker).await?;
                trace!("new staker stored in database.");

                // TODO self.webhooks.send(activated)
            }
            // if the staker does not yet exist, we should check if it contains
            // the primary address of the pool
            // and if it fulfills the vault conditions
        }

        Ok(())
    }
}

/// Returns true if a stake was stolen and caught by StakeGuard
async fn check_stake_guard(pool: &PgPool, block: &Block, mut stake: Stake) -> Result<bool> {
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
        trace!("The transaction was spent by stakeguard");
        stake.status = StakeStatus::StakeGuard;

        database::store_stake(pool, &stake).await?;
        // TODO punish perpetrator
        // TODO send webhook message

        return Ok(true);
    }

    Ok(false)
}

#[async_trait]
impl IntoSubsystem<anyhow::Error> for CoinStaker {
    async fn run(mut self, subsys: SubsystemHandle) -> Result<()> {
        info!("starting coinstaker {}", self.config.chain_name);
        let client = self.verusd()?;

        // some preflight checks are needed:

        // if daemon is not staking, the work will not be counted towards shares
        if client.get_mining_info()?.staking == false {
            warn!("daemon is not staking, subscriber work will not be accumulated");
        }

        tokio::spawn(super::zmq::tmq_block_listen(
            self.config.chain_config.zmq_port_blocknotify,
            self.tx.clone(),
        ));

        select! {
            _ = subsys.on_shutdown_requested() => {
                info!("shutting down coinstaker");
                // ability to do some cleanup before closing here
            },
            r = self.listen() => {
                warn!("stopped listening");
                match r {
                    Err(e) => error!("{e:?}"),
                    _ => {}
                }
            }
        }

        Ok(())
    }
}

#[derive(Debug)]
pub enum CoinStakerMessage {
    Block(BlockHash),
    StakingSupply(oneshot::Sender<StakingSupply>, Vec<Address>),
    StakerStatus(oneshot::Sender<String>, Address),
    Balance(oneshot::Sender<f64>),
}

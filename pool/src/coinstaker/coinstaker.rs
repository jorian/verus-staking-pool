use std::collections::HashMap;
use std::str::FromStr;

use anyhow::{anyhow, Context, Result};
use axum::async_trait;
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use serde_json::Value;
use sqlx::PgPool;
use tokio::select;
use tokio::sync::{mpsc, oneshot};
use tokio_graceful_shutdown::{IntoSubsystem, SubsystemHandle};
use tracing::{debug, error, info, instrument, trace, warn};
use vrsc_rpc::bitcoin::{BlockHash, Txid};
use vrsc_rpc::client::{Client as VerusClient, RpcApi};
use vrsc_rpc::json::identity::{Identity, IdentityPrimary};
use vrsc_rpc::json::vrsc::Address;
use vrsc_rpc::json::{Block, ValidationType};

use crate::coinstaker::constants::{Stake, StakeStatus};
use crate::database;
use crate::http::constants::StakingSupply;
use crate::util::verus;
// use crate::util::verus;

use super::config::Config as CoinstakerConfig;
use super::constants::Staker;
use super::{CurrencyId, IdentityAddress, StakerStatus};
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
                    info!("received new block {block_hash}");

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
                    self.check_active_stakers(&verus_client, &block).await?;
                    self.check_pending_stakes(&verus_client).await?;
                    if self.daemon_is_staking(&verus_client).await? == false {
                        continue; // break here, don't add work for daemons that are not staking
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

    async fn check_pending_stakes(&self, client: &VerusClient) -> Result<()> {
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
        let pending_stakes =
            database::get_stakes_by_status(&self.pool, String::from("pending")).await?;

        let verus_client = self.verusd()?;

        let eligible_stakers = verus_client.list_unspent(
            Some(150),
            None,
            Some(
                active_stakers
                    .iter()
                    .map(|subscriber| subscriber.identity_address.clone())
                    .collect::<Vec<Address>>()
                    .as_ref(),
            ),
        )?;

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

        pending_stakes.iter().for_each(|stake| {
            if payload.contains_key(&stake.found_by) {
                trace!("adding work to staker to undo punishment (cooling down utxo after stake)");
                payload.entry(stake.found_by.clone()).and_modify(|v| {
                    debug!("pos_source_amount: {}", stake.source_amount.as_sat() as i64);
                    debug!("sum: {}", v);
                    *v += Decimal::from_i64(stake.source_amount.as_sat() as i64).unwrap()
                });
            }
        });

        database::store_work_v2(&self.pool, &self.chain_id, payload, blockheight).await?;

        Ok(())
    }

    #[instrument(skip(self), fields(chain = self.chain_id.to_string()))]
    async fn check_for_stake(
        &self,
        block_hash: &BlockHash,
        active_stakers: &Vec<Staker>,
    ) -> Result<()> {
        if let Some(stake) = self.is_stake(&block_hash).await? {
            debug!(?block_hash, "!!!!! STAKE FOUND !!!!!");

            // TODO move all work until now to this round
            // TODO store stake

            // TODO websocket.send(new stake)
        }
        Ok(())
    }

    #[instrument(skip(self), fields(chain = self.chain_id.to_string()))]
    async fn is_stake(&self, block_hash: &BlockHash) -> Result<Option<Stake>> {
        let client = self.verusd()?;
        let block = client.get_block(block_hash, 2)?;
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
                StakeStatus::Pending,
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
            debug!("identity is eligible");
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

    async fn get_staking_supply(
        &self,
        _identity_addresses: Vec<IdentityAddress>,
    ) -> Result<StakingSupply> {
        let verus_client = self.verusd()?;

        // let mut subscriptions =
        //     database::get_subscriptions(&self.pool, &self.chain_id, identity_addresses).await?;

        let staking_supply = verus::get_staking_supply(&verus_client)?;

        Ok(staking_supply)
    }

    async fn check_active_stakers(&self, verus_client: &VerusClient, block: &Block) -> Result<()> {
        for tx in &block.tx {
            for vout in &tx.vout {
                if let Some(identity_primary) = &vout.script_pubkey.identityprimary {
                    self.check_staker_status(
                        &verus_client,
                        &(&identity_primary.identityaddress).into(),
                    )
                    .await?;
                }
            }
        }
        // requirements
        // - get_identity() for every identityprimary in transaction vouts
        // -
        Ok(())
    }

    async fn check_staker_status(
        &self,
        client: &VerusClient,
        identity_address: &IdentityAddress,
    ) -> Result<()> {
        let identity = client.get_identity(&identity_address.to_string())?;

        if let Some(staker) = database::get_staker(
            &self.pool,
            &self.chain_id,
            &identity.identity.identityaddress,
        )
        .await?
        {
            debug!("staker found in database");

            match staker.status {
                StakerStatus::Active => {
                    // if staker is not eligible anymore, deactivate
                    // self.webhooks.send(deactivated)
                }
                // any previously active stakers that update their identity, become
                // inactive.
                StakerStatus::Inactive => {
                    if self.identity_is_eligible(&identity.identity) {
                        database::store_staker(
                            &self.pool,
                            &self.chain_id,
                            &identity,
                            StakerStatus::Active,
                            self.config.min_payout,
                        )
                        .await?;
                        // update in database.
                    }
                    // if staker became eligible, reactivate
                    // self.webhooks.send(activated)
                }
            }
        } else {
            debug!("staker not found in database");
            if self.identity_is_eligible(&identity.identity) {
                debug!("new staker, store in database.");
                database::store_staker(
                    &self.pool,
                    &self.chain_id,
                    &identity,
                    StakerStatus::Active,
                    self.config.min_payout,
                )
                .await?;

                // self.webhooks.send(activated)
            }
            // if the staker does not yet exist, we should check if it contains
            // the primary address of the pool
            // and if it fulfills the vault conditions
        }

        Ok(())
    }
}

#[async_trait]
impl IntoSubsystem<anyhow::Error> for CoinStaker {
    async fn run(mut self, subsys: SubsystemHandle) -> Result<()> {
        info!("starting coinstaker {}", self.config.chain_name);

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
    StakingSupply(oneshot::Sender<StakingSupply>, Vec<IdentityAddress>),
    Balance(oneshot::Sender<f64>),
}

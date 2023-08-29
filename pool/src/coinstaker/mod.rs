mod error;
mod nats;
mod util;

use std::{ops::SubAssign, str::FromStr};

use chrono::{DateTime, Duration, Utc};
use color_eyre::Report;
use futures::StreamExt;
use poollib::{
    chain::Chain,
    configuration::{CoinConfig, VerusVaultConditions},
    database, Payload, PayoutMember, PgPool, Stake, Subscriber,
};
use rust_decimal::{
    prelude::{FromPrimitive, ToPrimitive},
    Decimal,
};
use serde_json::json;

use tokio::sync::{
    mpsc::{self},
    oneshot,
};
use tracing::{debug, error, info, instrument, trace, warn};
use vrsc_rpc::{
    bitcoin::BlockHash,
    json::{
        identity::{Identity, IdentityPrimary},
        vrsc::{Address, Amount, SignedAmount},
    },
    RpcApi,
};

use crate::{coinstaker::error::CoinStakerError, payoutmanager::PayoutManager};

#[derive(Debug)]
pub struct CoinStaker {
    pub chain: Chain,
    pub pool_identity_address: Address,
    pool_fee_discount: Decimal,
    _default_tx_fee: Amount,
    config: CoinConfig,
    cs_tx: mpsc::Sender<CoinStakerMessage>,
    cs_rx: mpsc::Receiver<CoinStakerMessage>,
    pool: PgPool,
    nats_client: async_nats::Client,
}

impl CoinStaker {
    pub async fn new(
        pool: PgPool,
        coin_config: CoinConfig,
        cs_rx: mpsc::Receiver<CoinStakerMessage>,
        cs_tx: mpsc::Sender<CoinStakerMessage>,
    ) -> Self {
        debug!("coin_config: {:?}", &coin_config);
        let nats_client = async_nats::connect("nats://localhost:4222".to_string())
            .await
            .expect("a nats client");
        let pool_identity_address = coin_config.pool_identity_address.clone();

        Self {
            chain: Chain::from(&coin_config),
            pool_identity_address,
            pool_fee_discount: Decimal::from_f32(coin_config.pool_fee_discount)
                .unwrap_or(Decimal::ZERO),
            _default_tx_fee: Amount::from_sat(coin_config.default_tx_fee as u64),
            config: coin_config,
            cs_tx,
            cs_rx,
            pool,
            nats_client,
        }
    }

    pub fn get_mpsc_sender(&self) -> mpsc::Sender<CoinStakerMessage> {
        self.cs_tx.clone()
    }

    pub fn ping_daemon(&self) -> Result<(), Report> {
        let client = self.chain.verusd_client()?;

        client.ping().map_err(|e| e.into())
    }

    pub fn identity_is_eligible(
        &self,
        identity: &IdentityPrimary,
        subscriber: &Subscriber,
    ) -> bool {
        let conditions = &self.config.verus_vault_conditions;

        // general conditions that need to be true regardless of config options
        if identity.minimumsignatures == 1
            && identity.primaryaddresses.len() > 1
            && identity.primaryaddresses.contains(&subscriber.pool_address)
        {
            match identity.flags {
                // fixed time lock; unlock at x seconds (epoch)
                1 => {
                    // TODO v2, ineligible until then
                    return false;
                }
                // delay lock; unlock after x seconds
                2 => {
                    return identity.timelock >= conditions.min_lock as u64
                        && identity.primaryaddresses.len()
                            <= conditions.max_primary_addresses.try_into().unwrap_or(0)
                        && identity.recoveryauthority != identity.identityaddress
                        && identity.revocationauthority != identity.identityaddress
                }
                _ => return false,
            }
        }

        // TODO notify an admin if identity was checked but not eligible
        false
    }
}

#[instrument(level = "trace", skip(cs), fields(chain = cs.chain.name))]
pub async fn run(mut cs: CoinStaker) -> Result<(), Report> {
    if cs.ping_daemon().is_ok() {
        // first check if daemon is running. if not, abort immediately
        let client = cs.chain.verusd_client()?;

        // if daemon is not staking, the work will not be counted towards shares
        if !client.get_mining_info()?.staking {
            warn!("daemon is not staking, subscriber work will not be accumulated");
        }

        // NATS listener
        tokio::spawn({
            let c_tx = cs.get_mpsc_sender();
            let currencyid = cs.chain.currencyid.clone();

            // waits for messages from NATS clients
            async move { nats::nats_server(currencyid, c_tx).await }
        });

        // listen for blocks from the daemon through ZMQ
        tokio::spawn(tmq_block_listen(
            cs.config.zmq_port_blocknotify,
            cs.chain.clone(),
            cs.get_mpsc_sender(),
        ));

        info!("starting to watch for messages on {}", cs.chain);

        // don't do startup sequence when no latest round is found
        //
        // in the period between the latest blockheight and the current blockheight, subscribers may have left.
        // The following is to check who left.
        if let Some(mut latest_blockheight) =
            database::get_latest_round(&cs.pool, &cs.chain.currencyid.to_string()).await?
        {
            trace!("latest_round: {latest_blockheight}"); //need to start 1 block further
            latest_blockheight += 1;

            let current_blockheight = client.get_blockchain_info()?.blocks;

            for i in latest_blockheight.try_into()?..=current_blockheight {
                let block = client.get_block_by_height(i, 2)?;

                for tx in block.tx.iter() {
                    for vout in tx.vout.iter() {
                        util::check_vout(&mut cs, vout).await?;
                    }
                }
            }
        }

        let mut maturing_stakes =
            database::get_pending_stakes(&cs.pool, &cs.chain.currencyid.to_string()).await?;
        debug!("pending txns {:#?}", maturing_stakes);

        let chain_c = cs.chain.clone();
        let c_tx = cs.get_mpsc_sender();

        if let Err(e) = util::check_for_maturity(chain_c, &mut maturing_stakes, c_tx).await {
            error!("wait for maturity: {:?}", e);
        }

        // starts a tokio thread that processes payments every period as defined.
        tokio::spawn({
            let pool = cs.pool.clone();
            let client = cs.chain.verusd_client()?;
            let currencyid = cs.chain.currencyid.clone();
            let pool_identity_address = cs.pool_identity_address.clone();
            let nats_client = cs.nats_client.clone();
            let payout_interval = std::time::Duration::from_secs(cs.config.payout_interval);
            async move {
                let mut interval = tokio::time::interval(payout_interval);

                loop {
                    interval.tick().await;

                    if let Err(e) = util::process_payments(
                        &pool,
                        &client,
                        &nats_client,
                        &currencyid,
                        &pool_identity_address,
                    )
                    .await
                    {
                        error!("an error occurred in PayoutManager, stopping payments\n{e:?}");
                    }
                }
            }
        });

        // The loop that listens for mpsc messages
        while let Some(msg) = cs.cs_rx.recv().await {
            match msg {
                // in BlockNotify we do most of the work:
                // - add work for every subscriber until this block
                // - check if the block that was mined, was staked by one of the subscribers
                // - check if subscribers left
                // - check if new subscribers arrived
                CoinStakerMessage::BlockNotify(blockhash) => {
                    trace!("blocknotify for {}", cs.chain);
                    if let Ok(client) = cs.chain.verusd_client() {
                        let active_subscribers = database::get_subscribers_by_status(
                            &cs.pool,
                            &cs.chain.currencyid.to_string(),
                            "subscribed",
                        )
                        .await?;

                        // get additional information about the incoming block:
                        let block = client.get_block(&blockhash, 2)?;

                        for tx in block.tx.iter() {
                            for vout in tx.vout.iter() {
                                util::check_vout(&mut cs, vout).await?;
                            }
                        }

                        let mut pending_stakes = database::get_pending_stakes(
                            &cs.pool,
                            &cs.chain.currencyid.to_string(),
                        )
                        .await?;

                        util::check_for_maturity(
                            cs.chain.clone(),
                            &mut pending_stakes,
                            cs.get_mpsc_sender(),
                        )
                        .await?;

                        // break if daemon is not staking
                        if !client.get_mining_info()?.staking {
                            warn!("daemon not staking, not counting work");

                            continue;
                        }

                        // FIXME: this has become really ugly.
                        // - check_for_maturity checks if any pending_stakes are stale or have matured.
                        // - this information is required when to add work, but the state is written to the database, and so we need to acquire
                        // this state again from the database to do a proper work calculation
                        // ideally, you would want to keep it in memory and update the memory during these checks,
                        // but then we get to manage 2 different states;
                        // the database and the in-memory temporary state.

                        if let Err(e) =
                            util::check_for_stake(&block, &active_subscribers, &mut cs).await
                        {
                            error!("{:?}\n{:#?}\n{:#?}", e, &block, &active_subscribers);
                        };

                        let pending_stakes = database::get_pending_stakes(
                            &cs.pool,
                            &cs.chain.currencyid.to_string(),
                        )
                        .await?;

                        util::add_work(
                            &active_subscribers
                                .iter()
                                .filter(|subscriber| {
                                    if let Ok(identity) = client.get_identity_history(
                                        &subscriber.identity_address.to_string(),
                                        0,
                                        99999999,
                                    ) {
                                        // identities need a 150 block cooldown if they were updated, before they can stake
                                        identity.blockheight
                                            < block.height.checked_sub(150).unwrap_or(0) as i64
                                    } else {
                                        false
                                    }
                                })
                                .cloned()
                                .collect::<Vec<Subscriber>>(),
                            &pending_stakes,
                            &client,
                            &mut cs,
                            block.height,
                        )
                        .await?;

                        continue;
                    }

                    continue;
                }
                CoinStakerMessage::StaleBlock(mut stake) => {
                    database::move_work_to_current_round(
                        &cs.pool,
                        &cs.chain.currencyid.to_string(),
                        stake.blockheight,
                    )
                    .await?;

                    stake.set_result("stale")?;

                    // need to update postgres
                    database::set_stake_result(&cs.pool, &stake).await?;

                    let payload = Payload {
                        command: "stale".to_string(),
                        data: json!({
                            "blockhash": stake.blockhash,
                            "blockheight": stake.blockheight,
                            "chain_name": cs.chain.name,
                        }),
                    };

                    cs.nats_client
                        .publish(
                            "ipc.coinstaker".into(),
                            serde_json::to_vec(&json!(payload))?.into(),
                        )
                        .await?;
                }
                CoinStakerMessage::StolenBlock(mut stake) => {
                    database::move_work_to_current_round(
                        &cs.pool,
                        &cs.chain.currencyid.to_string(),
                        stake.blockheight,
                    )
                    .await?;

                    stake.set_result("stolen")?;

                    database::set_stake_result(&cs.pool, &stake).await?;

                    cs.get_mpsc_sender()
                        .send(CoinStakerMessage::SetBlacklist(None, stake.mined_by, true))
                        .await?;

                    // blacklist the user
                    // move work back to round 0
                    // nats a message
                }
                CoinStakerMessage::MaturedBlock(mut stake) => {
                    stake.set_result("mature")?;

                    // set the stake to processed, as the wait for maturity is over.
                    database::set_stake_result(&cs.pool, &stake).await?;

                    tokio::spawn({
                        let pool = cs.pool.clone();
                        let stake = stake.clone();
                        let pool_fee_discount = cs.pool_fee_discount;
                        let pool_identity_address = cs.pool_identity_address.clone();
                        let nats_client = cs.nats_client.clone();
                        let chain_name = cs.chain.name.clone();

                        let client = cs.chain.verusd_client()?;
                        let mined_by_name = client
                            .get_identity(&stake.mined_by.to_string())?
                            .fullyqualifiedname;

                        async move {
                            let payout = PayoutManager::create_payout(
                                &pool,
                                &stake,
                                pool_fee_discount,
                                pool_identity_address,
                            )
                            .await?;

                            PayoutManager::store_payout_in_database(&pool, &payout).await?;

                            let payload = Payload {
                                command: "matured".to_string(),
                                data: json!({
                                    "chain": chain_name,
                                    "blockhash": payout.blockhash.to_string(),
                                    "blockheight": stake.blockheight,
                                    "staked_by": mined_by_name,
                                    "rewards": payout.amount_paid_to_subs.as_sat(),
                                    "n_subs": payout.members.len()
                                }),
                            };

                            nats_client
                                .publish(
                                    "ipc.coinstaker".into(),
                                    serde_json::to_vec(&json!(payload))?.into(),
                                )
                                .await?;

                            Ok::<(), Report>(())
                        }
                    });

                    continue;
                }
                CoinStakerMessage::StakingSupply(os_tx, identities) => {
                    let client = cs.chain.verusd_client()?;
                    let wallet_info = client.get_wallet_info()?;
                    let pool_supply = wallet_info.eligible_staking_balance.as_vrsc();
                    let mining_info = client.get_mining_info()?;
                    let network_supply = mining_info.stakingsupply;
                    let my_supply = if identities.len() > 0 {
                        let mut subscriptions = database::get_subscriptions(
                            &cs.pool,
                            &cs.chain.currencyid.to_string(),
                            &identities,
                        )
                        .await?;

                        trace!("subscriptions found: {subscriptions:#?}");

                        subscriptions = subscriptions
                            .into_iter()
                            .filter(|s| {
                                let is_subscribed = &s.status == "subscribed";
                                let is_cooled_down = if let Ok(identity) = client
                                    .get_identity_history(
                                        &s.identity_address.to_string(),
                                        0,
                                        9999999,
                                    ) {
                                    let block =
                                        client.get_block_by_height(mining_info.blocks, 2).unwrap();

                                    identity.blockheight
                                        < block.height.checked_sub(150).unwrap_or(0) as i64
                                } else {
                                    false
                                };

                                is_subscribed && is_cooled_down
                            })
                            .collect::<Vec<_>>();

                        trace!("eligible subscriptions found: {subscriptions:#?}");

                        if !subscriptions.is_empty() {
                            let lu = client
                                .list_unspent(
                                    Some(150),
                                    Some(9999999),
                                    Some(
                                        &subscriptions
                                            .into_iter()
                                            .map(|sub| sub.identity_address)
                                            .collect::<Vec<Address>>(),
                                    ),
                                )?
                                .iter()
                                .fold(SignedAmount::ZERO, |acc, sum| acc + sum.amount);

                            lu.as_vrsc()
                        } else {
                            0.0
                        }
                    } else {
                        0.0
                    };

                    if let Err(e) = os_tx.send((network_supply, pool_supply, my_supply)) {
                        error!("{e:?}");
                    }

                    continue;
                }
                CoinStakerMessage::SetFeeDiscount(os_tx, new_fee) => {
                    cs.pool_fee_discount = Decimal::from_f32(new_fee).unwrap_or(Decimal::ZERO);
                    if let Err(e) = os_tx.send(cs.pool_fee_discount.to_f32().unwrap()) {
                        error!("{e:?}");
                    }

                    continue;
                }
                CoinStakerMessage::GetIdentity(os_tx, s_id) => {
                    debug!("{s_id:?}");
                    let client = cs.chain.verusd_client()?;
                    if let Ok(identity) = client.get_identity(&s_id) {
                        let _ = os_tx.send(Some(identity));
                    } else {
                        let _ = os_tx.send(None);
                    }

                    continue;
                }
                CoinStakerMessage::CheckSubscription(os_tx, s_id) => {
                    let status = util::check_subscription(&cs, &s_id).await?;
                    os_tx
                        .send(json!({
                            "result": "success",
                            "status": status.to_string()
                        }))
                        .unwrap();

                    continue;
                }
                CoinStakerMessage::RecentStakes(os_tx) => {
                    let pool = &cs.pool;
                    let duration = Duration::days(7); //secs(60 * 60 * 24 * 7);
                    let mut since: DateTime<Utc> = chrono::offset::Utc::now();
                    since.sub_assign(duration);
                    let stakes =
                        database::get_recent_stakes(pool, &cs.chain.currencyid.to_string(), since)
                            .await?;

                    os_tx.send(stakes).unwrap();
                }
                CoinStakerMessage::SetMinPayout(os_tx, identity_str, threshold) => {
                    // get the identity address:
                    let client = cs.chain.verusd_client()?;
                    if let Ok(identity) = client.get_identity(&identity_str) {
                        // get the subscriber for this address:
                        if let Some(subscriber) = database::get_subscriber(
                            &cs.pool,
                            &cs.chain.currencyid.to_string(),
                            &identity.identity.identityaddress.to_string(),
                        )
                        .await?
                        {
                            database::update_subscriber_min_payout(
                                &cs.pool,
                                &subscriber.currencyid.to_string(),
                                &subscriber.identity_address.to_string(),
                                threshold,
                            )
                            .await?;

                            let _ = os_tx.send(json!({
                                "result":
                                    format!(
                                        "Minimum payout threshold for `{}` updated to {}",
                                        identity.fullyqualifiedname,
                                        Amount::from_sat(threshold).as_vrsc()
                                    )
                            }));
                        } else {
                            let _ = os_tx.send(json!({
                                "result": format!("No subscriber found for {identity_str}")
                            }));
                        }
                    } else {
                        let _ = os_tx.send(json!({
                            "result":
                                format!("Identity `{identity_str}` not found on {}", cs.chain.name)
                        }));
                    }
                }
                CoinStakerMessage::PendingStakes(os_tx) => {
                    let pool = &cs.pool;

                    let stakes =
                        database::get_pending_stakes(pool, &cs.chain.currencyid.to_string())
                            .await?;

                    os_tx.send(stakes).unwrap();
                }
                CoinStakerMessage::Heartbeat(os_tx) => os_tx.send(()).unwrap(),
                CoinStakerMessage::GetSubscriptions(os_tx, identityaddresses) => {
                    let subscribers = database::get_subscribers(
                        &cs.pool,
                        &cs.chain.currencyid.to_string(),
                        &identityaddresses,
                    )
                    .await?;

                    os_tx.send(subscribers).unwrap();
                }
                CoinStakerMessage::SetStaking(os_tx, generate) => {
                    let client = cs.chain.verusd_client()?;

                    client.set_generate(generate, 0)?;

                    os_tx.send(()).unwrap();
                }
                CoinStakerMessage::NewSubscriber(os_tx, identitystr) => {
                    match client.get_identity(&identitystr) {
                        Ok(identity) => {
                            if let Some(existing_subscriber) = database::get_subscriber(
                                &cs.pool,
                                &cs.chain.currencyid.to_string(),
                                &identity.identity.identityaddress.to_string(),
                            )
                            .await?
                            {
                                debug!("{existing_subscriber:#?}");
                                if ["unsubscribed", "pending", ""]
                                    .contains(&&*existing_subscriber.status)
                                {
                                    trace!("use existing address if user is still pending or is unsubscribed or has no status");
                                    trace!("update db, set subscriber status to pending");

                                    // TODO a unsubscribed user could've unlocked and locked it's ID. At this point, the
                                    // check for eligility should already pass and the user can join the pool immediately

                                    if cs.identity_is_eligible(
                                        &identity.identity,
                                        &existing_subscriber,
                                    ) {
                                        database::update_subscriber_status(
                                            &cs.pool,
                                            &cs.chain.currencyid.to_string(),
                                            &identity.identity.identityaddress.to_string(),
                                            "subscribed",
                                        )
                                        .await?;
                                    } else {
                                        database::update_subscriber_status(
                                            &cs.pool,
                                            &cs.chain.currencyid.to_string(),
                                            &identity.identity.identityaddress.to_string(),
                                            "pending",
                                        )
                                        .await?;
                                    }
                                    os_tx.send(Ok(existing_subscriber)).unwrap();
                                } else {
                                    trace!("the user has an active subscription");

                                    os_tx
                                        .send(Err(CoinStakerError::SubscriberAlreadyExists(
                                            existing_subscriber.identity_name.clone(),
                                        )
                                        .into()))
                                        .unwrap();
                                }
                            } else {
                                trace!("get new pool address for new subscriber");

                                if let Ok(address) = client.get_new_address() {
                                    debug!("new address: {address:?}");

                                    let new_subscriber = database::insert_subscriber(
                                        &cs.pool,
                                        &cs.chain.currencyid.to_string(),
                                        &identity.identity.identityaddress.to_string(),
                                        &identity.fullyqualifiedname,
                                        "pending",
                                        &address.to_string(),
                                        cs.chain.default_pool_fee,
                                        cs.chain.default_min_payout,
                                    )
                                    .await?;

                                    os_tx.send(Ok(new_subscriber)).unwrap();
                                }
                            }
                        }
                        Err(_e) => {
                            os_tx
                                .send(Err(CoinStakerError::IdentityNotValid(
                                    identitystr.to_string(),
                                )
                                .into()))
                                .unwrap();
                        }
                    }
                }
                CoinStakerMessage::GetPayouts(os_tx, identityaddresses) => {
                    // get all payouts from the database for the combination of currencyid and every identityaddress in the vec
                    let payouts = database::get_payouts(
                        &cs.pool,
                        &cs.chain.currencyid.to_string(),
                        &identityaddresses,
                    )
                    .await?;

                    os_tx.send(payouts).unwrap();
                }
                CoinStakerMessage::GetPoolFees(os_tx) => {
                    let fees =
                        database::get_pool_fees(&cs.pool, &cs.chain.currencyid.to_string()).await?;

                    os_tx.send(fees).unwrap();
                }
                CoinStakerMessage::SetBlacklist(opt_os_tx, address, blacklist) => {
                    trace!("Setting blacklist status for {address} to {blacklist}");

                    if let Some(subscriber) = database::get_subscriber(
                        &cs.pool,
                        &cs.chain.currencyid.to_string(),
                        &address.to_string(),
                    )
                    .await?
                    {
                        let new_status = if blacklist {
                            "banned"
                        } else {
                            let client = &cs.chain.verusd_client()?;
                            let identity = client.get_identity(&address.to_string())?;

                            if cs.identity_is_eligible(&identity.identity, &subscriber) {
                                "subscribed"
                            } else {
                                "unsubscribed"
                            }
                        };

                        if let Ok(subscriber) = database::update_subscriber_status(
                            &cs.pool,
                            &cs.chain.currencyid.to_string(),
                            &address.to_string(),
                            new_status,
                        )
                        .await
                        {
                            debug!("subscriber updated in db: {subscriber:?}");
                            if let Some(os_tx) = opt_os_tx {
                                os_tx.send(subscriber).unwrap();
                            }
                        }
                    }
                }
                CoinStakerMessage::GetVaultConditions(os_tx) => {
                    trace!("getting vaultconditions");

                    os_tx
                        .send(cs.config.verus_vault_conditions.clone())
                        .unwrap();
                }
                CoinStakerMessage::UpdateStakeStatus(_stake) => {}
            }
        }

        trace!("no more coinstaker message receiver running, stopping staking");
        cs.chain.verusd_client()?.set_generate(false, 0)?;
    } else {
        error!("{} daemon is not running", cs.chain.name);
    }

    Ok(())
}

#[derive(Debug)]
pub enum CoinStakerMessage {
    BlockNotify(BlockHash),
    StaleBlock(Stake),
    StolenBlock(Stake),
    MaturedBlock(Stake),
    StakingSupply(oneshot::Sender<(f64, f64, f64)>, Vec<String>),
    SetFeeDiscount(oneshot::Sender<f32>, f32),
    GetIdentity(oneshot::Sender<Option<Identity>>, String),
    CheckSubscription(oneshot::Sender<serde_json::Value>, String),
    RecentStakes(oneshot::Sender<Vec<Stake>>),
    PendingStakes(oneshot::Sender<Vec<Stake>>),
    SetMinPayout(oneshot::Sender<serde_json::Value>, String, u64),
    Heartbeat(oneshot::Sender<()>),
    GetSubscriptions(oneshot::Sender<Vec<Subscriber>>, Vec<String>),
    SetStaking(oneshot::Sender<()>, bool),
    NewSubscriber(oneshot::Sender<Result<Subscriber, CoinStakerError>>, String),
    GetPayouts(oneshot::Sender<Vec<PayoutMember>>, Vec<String>),
    GetPoolFees(oneshot::Sender<Amount>),
    SetBlacklist(Option<oneshot::Sender<Subscriber>>, Address, bool),
    GetVaultConditions(oneshot::Sender<VerusVaultConditions>),
    UpdateStakeStatus(Stake),
}

async fn tmq_block_listen(
    port: u16,
    chain: Chain,
    cx_tx: mpsc::Sender<CoinStakerMessage>,
) -> Result<(), Report> {
    let mut socket = tmq::subscribe(&tmq::Context::new())
        .connect(&format!("tcp://127.0.0.1:{}", port))?
        .subscribe(b"hash")?;

    info!("tmq_block_listen enabled for {}", chain);

    loop {
        if let Some(Ok(msg)) = socket.next().await {
            if let Some(hash) = msg.into_iter().nth(1) {
                let block_hash = hash
                    .iter()
                    .map(|byte| format!("{:02x}", *byte))
                    .collect::<Vec<_>>()
                    .join("");
                debug!("{} blockhash: {}", chain.currencyid, block_hash);

                cx_tx
                    .send(CoinStakerMessage::BlockNotify(BlockHash::from_str(
                        &block_hash,
                    )?))
                    .await?;
            } else {
                error!("not a valid message!");
            }
        } else {
            error!("no correct message received");
        }
    }
}

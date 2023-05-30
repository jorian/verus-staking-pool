mod error;
mod nats;
mod util;

use std::{ops::SubAssign, str::FromStr, time::Duration as TimeDuration};

use chrono::{DateTime, Duration, Utc};
use color_eyre::Report;
use futures::StreamExt;
use poollib::{
    chain::Chain, configuration::CoinConfig, database, Payload, PgPool, Stake, Subscriber,
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
    pub bot_identity_address: Address,
    bot_fee_discount: Decimal,
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
        let bot_identity_address = coin_config.bot_identity_address.clone();

        Self {
            chain: Chain::from(&coin_config),
            bot_identity_address,
            bot_fee_discount: Decimal::from_f32(coin_config.bot_fee_discount)
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
        // testnet currencies do not have strict checking to make testing easier.
        if self.chain.testnet {
            if identity.minimumsignatures == 1
                && identity.primaryaddresses.len() > 1
                && identity.primaryaddresses.contains(&subscriber.pool_address)
            {
                return true;
            }
        } else {
            if identity.minimumsignatures == 1
                && identity.primaryaddresses.len() > 1
                && identity.primaryaddresses.contains(&subscriber.pool_address)
                && identity.revocationauthority.ne(&identity.identityaddress)
                && identity.recoveryauthority.ne(&identity.identityaddress)
                && identity.flags == 2
                && (identity.timelock >= 720 || identity.timelock <= 10080)
            {
                return true;
            }
        }

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
                let active_subscribers = database::get_subscribers_by_status(
                    &cs.pool,
                    &cs.chain.currencyid.to_string(),
                    "subscribed",
                )
                .await?;

                let block = client.get_block_by_height(i, 2)?;

                for tx in block.tx.iter() {
                    for vout in tx.vout.iter() {
                        util::check_subscriptions(&mut cs, vout, &active_subscribers).await?;
                    }
                }
            }
        }

        // process any stakes that were still maturing and might have matured now:
        let maturing_stakes =
            database::get_pending_stakes(&cs.pool, &cs.chain.currencyid.to_string()).await?;
        debug!("pending txns {:#?}", maturing_stakes);

        for stake in maturing_stakes {
            let chain_c = cs.chain.clone();
            let c_tx = cs.get_mpsc_sender();

            tokio::spawn(async move {
                if let Err(e) = util::wait_for_maturity(chain_c, stake.clone(), c_tx).await {
                    error!(
                        "wait for maturity: {} ({}): {:?}",
                        &stake.blockhash, &stake.currencyid, e
                    )
                }
            });
        }

        // starts a tokio thread that processes payments every period as defined.
        tokio::spawn({
            let pool = cs.pool.clone();
            let client = cs.chain.verusd_client()?;
            let currencyid = cs.chain.currencyid.clone();
            let bot_identity_address = cs.bot_identity_address.clone();
            let nats_client = cs.nats_client.clone();
            async move {
                util::process_payments(
                    pool,
                    client,
                    nats_client,
                    currencyid,
                    bot_identity_address,
                    TimeDuration::from_secs(60 * 60),
                )
                .await
                .unwrap();
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
                                util::check_subscriptions(&mut cs, vout, &active_subscribers)
                                    .await?;
                            }
                        }

                        // break if daemon is not staking
                        if !client.get_mining_info()?.staking {
                            warn!("daemon not staking, not counting work");

                            continue;
                        }
                        // add the work up until here
                        util::add_work(&active_subscribers, &client, &mut cs, block.height).await?;

                        if let Err(e) =
                            util::check_for_stake(&block, &active_subscribers, &mut cs).await
                        {
                            error!("{:?}\n{:#?}\n{:#?}", e, &block, &active_subscribers);
                        };

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
                    database::set_stake_to_processed(&cs.pool, &stake).await?;

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
                CoinStakerMessage::MaturedBlock(mut stake) => {
                    stake.set_result("mature")?;

                    database::set_stake_to_processed(&cs.pool, &stake).await?;

                    tokio::spawn({
                        let pool = cs.pool.clone();
                        let stake = stake.clone();
                        let bot_fee_discount = cs.bot_fee_discount;
                        let bot_identity_address = cs.bot_identity_address.clone();
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
                                bot_fee_discount,
                                bot_identity_address,
                            )
                            .await?;

                            PayoutManager::store_payout_in_database(&pool, &payout).await?;

                            let payload = Payload {
                                command: "matured".to_string(),
                                data: json!({
                                    "chain": chain_name,
                                    "blockhash": payout.blockhash.to_string(),
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
                CoinStakerMessage::StakingSupply(os_tx, tuples) => {
                    let client = cs.chain.verusd_client()?;
                    let wallet_info = client.get_wallet_info()?;
                    let pool_supply = wallet_info.eligible_staking_balance.as_vrsc();
                    let mining_info = client.get_mining_info()?;
                    let network_supply = mining_info.stakingsupply;
                    let my_supply = if tuples.len() > 0 {
                        let subscriptions = database::get_subscriptions(&cs.pool, &tuples).await?;

                        trace!("subscriptions found: {subscriptions:#?}");

                        let lu = client
                            .list_unspent(
                                Some(150),
                                Some(999999),
                                Some(
                                    &subscriptions
                                        .into_iter()
                                        .filter(|s| &s.status == "subscribed")
                                        .map(|sub| sub.identity_address)
                                        .collect::<Vec<Address>>(),
                                ),
                            )?
                            .iter()
                            .fold(SignedAmount::ZERO, |acc, sum| acc + sum.amount);

                        lu.as_vrsc()
                    } else {
                        0.0
                    };

                    if let Err(e) = os_tx.send((network_supply, pool_supply, my_supply)) {
                        error!("{e:?}");
                    }

                    continue;
                }
                CoinStakerMessage::ProcessPayments() => {}
                CoinStakerMessage::SetFeeDiscount(os_tx, new_fee) => {
                    cs.bot_fee_discount = Decimal::from_f32(new_fee).unwrap_or(Decimal::ZERO);
                    if let Err(e) = os_tx.send(cs.bot_fee_discount.to_f32().unwrap()) {
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
                    debug!("{s_id:?}");
                    let client = cs.chain.verusd_client()?;
                    if let Ok(identity) = client.get_identity(&s_id) {
                        trace!("{:?}", identity);
                        if let Some(subscriber) = database::get_subscriber(
                            &cs.pool,
                            &cs.chain.currencyid.to_string(),
                            &identity.identity.identityaddress.to_string(),
                        )
                        .await?
                        {
                            match &*subscriber.status {
                                "subscribed" => {
                                    if cs.identity_is_eligible(&identity.identity, &subscriber) {
                                        os_tx
                                            .send(json!({
                                                "result":
                                                    format!("{s_id} has an active subscription and a valid identity")
                                            }))
                                            .unwrap();
                                    } else {
                                        database::update_subscriber_status(
                                            &cs.pool,
                                            &cs.chain.currencyid.to_string(),
                                            &identity.identity.identityaddress.to_string(),
                                            "unsubscribed",
                                        )
                                        .await?;

                                        let payload = Payload {
                                            command: "unsubscribed".to_string(),
                                            data: json!({
                                                "identity_name": identity.fullyqualifiedname.clone(),
                                                "identity_address": identity.identity.identityaddress.clone(),
                                                "currency_id": cs.chain.currencyid.clone(),
                                                "currency_name": cs.chain.name
                                            }),
                                        };
                                        cs.nats_client
                                            .publish(
                                                "ipc.coinstaker".into(),
                                                serde_json::to_vec(&json!(payload))?.into(),
                                            )
                                            .await?;

                                        os_tx
                                            .send(json!({
                                                "result":
                                                    format!("{s_id} ineligible, changed from subscribed to unsubscribed")
                                            }))
                                            .unwrap();
                                    }
                                }
                                "pending" => {
                                    if cs.identity_is_eligible(&identity.identity, &subscriber) {
                                        database::update_subscriber_status(
                                            &cs.pool,
                                            &cs.chain.currencyid.to_string(),
                                            &identity.identity.identityaddress.to_string(),
                                            "subscribed",
                                        )
                                        .await?;

                                        let payload = Payload {
                                            command: "subscribed".to_string(),
                                            data: json!({
                                                "identity_name": identity.fullyqualifiedname.clone(),
                                                "identity_address": identity.identity.identityaddress.clone(),
                                                "currency_id": cs.chain.currencyid.clone(),
                                                "currency_name": cs.chain.name

                                            }),
                                        };
                                        cs.nats_client
                                            .publish(
                                                "ipc.coinstaker".into(),
                                                serde_json::to_vec(&json!(payload))?.into(),
                                            )
                                            .await?;

                                        os_tx
                                            .send(json!({
                                                "result":
                                                    format!(
                                                        "{s_id} changed from pending to subscribed"
                                                    )
                                            }))
                                            .unwrap();
                                    }
                                }
                                _ => {}
                            }
                        } else {
                            os_tx
                                .send(json!({
                                    "result": format!("{s_id} not found in subscriptions")
                                }))
                                .unwrap();
                        }
                    } else {
                        let _ = os_tx.send(json!({
                            "result": format!("identity `{s_id}` does not exist")
                        }));
                    }

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

                                    database::update_subscriber_status(
                                        &cs.pool,
                                        &cs.chain.currencyid.to_string(),
                                        &identity.identity.identityaddress.to_string(),
                                        "pending",
                                    )
                                    .await?;

                                    os_tx.send(Ok(existing_subscriber)).unwrap();
                                } else {
                                    trace!("the user has an active subscription");

                                    os_tx
                                        .send(Err(CoinStakerError::SubscriberAlreadyExists.into()))
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
                                        cs.chain.default_bot_fee,
                                        cs.chain.default_min_payout,
                                    )
                                    .await?;

                                    os_tx.send(Ok(new_subscriber)).unwrap();
                                }
                            }
                        }
                        Err(e) => {
                            os_tx
                                .send(Err(CoinStakerError::IdentityNotValid.into()))
                                .unwrap();
                        }
                    }
                }
            }
        }
    } else {
        error!("{} daemon is not running", cs.chain.name);
    }

    Ok(())
}

#[derive(Debug)]
pub enum CoinStakerMessage {
    BlockNotify(BlockHash),
    StaleBlock(Stake),
    MaturedBlock(Stake),
    StakingSupply(oneshot::Sender<(f64, f64, f64)>, Vec<(String, String)>),
    SetFeeDiscount(oneshot::Sender<f32>, f32),
    ProcessPayments(),
    GetIdentity(oneshot::Sender<Option<Identity>>, String),
    CheckSubscription(oneshot::Sender<serde_json::Value>, String),
    RecentStakes(oneshot::Sender<Vec<Stake>>),
    PendingStakes(oneshot::Sender<Vec<Stake>>),
    SetMinPayout(oneshot::Sender<serde_json::Value>, String, u64),
    Heartbeat(oneshot::Sender<()>),
    GetSubscriptions(oneshot::Sender<Vec<Subscriber>>, Vec<String>),
    SetStaking(oneshot::Sender<()>, bool),
    NewSubscriber(oneshot::Sender<Result<Subscriber, CoinStakerError>>, String),
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

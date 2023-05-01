use std::{
    collections::{BTreeMap, HashMap},
    ops::SubAssign,
    str::FromStr,
    time::Duration as TimeDuration,
};

use chrono::{DateTime, Duration, Utc};
use color_eyre::Report;
use futures::StreamExt;
use poollib::{
    chain::Chain, configuration::CoinConfig, database, Payload, PgPool, Stake, StakeResult,
    Subscriber,
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
use tracing::{debug, error, info, instrument, trace};
use vrsc_rpc::{
    bitcoin::BlockHash,
    json::{
        identity::{Identity, IdentityPrimary},
        vrsc::{Address, Amount, SignedAmount},
        Block, TransactionVout, ValidationType,
    },
    Client, RpcApi,
};

use crate::payoutmanager::PayoutManager;

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
                && identity.primaryaddresses.contains(&subscriber.bot_address)
            {
                return true;
            }
        } else {
            if identity.minimumsignatures == 1
                && identity.primaryaddresses.len() > 1
                && identity.primaryaddresses.contains(&subscriber.bot_address)
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
        // first check if daemon is staking. if not, abort immediately
        let client = cs.chain.verusd_client()?;

        if !client.get_mining_info()?.staking {
            error!("daemon is not staking, aborting");

            return Ok(());
        }

        // NATS listener
        tokio::spawn({
            let c_tx = cs.get_mpsc_sender();
            let currencyid = cs.chain.currencyid.clone();

            // waits for messages from NATS clients
            async move { nats_server(currencyid, c_tx).await }
        });

        // listen for blocks through ZMQ
        tokio::spawn(tmq_block_listen(
            cs.config.zmq_port_blocknotify,
            cs.chain.clone(),
            cs.get_mpsc_sender(),
        ));

        info!("starting to watch for messages on {}", cs.chain);

        // don't do startup sequence when no latest round is found for this chain
        if let Some(mut latest_blockheight) =
            database::get_latest_round(&cs.pool, &cs.chain.currencyid.to_string()).await?
        {
            trace!("latest_round: {latest_blockheight}"); //need to start 1 block further
            latest_blockheight += 1;
            // let mut balances = database::get_latest_state_for_subscribers(
            //     &cs.pool,
            //     &cs.chain.currencyid.to_string(),
            // )
            // .await?;

            let current_blockheight = client.get_blockchain_info()?.blocks;

            // let deltas = get_deltas(
            //     &client,
            //     balances.keys().collect::<Vec<&Address>>().as_ref(),
            //     latest_blockheight.try_into().unwrap(),
            //     current_blockheight,
            // )
            // .await?;

            for i in latest_blockheight.try_into()?..=current_blockheight {
                // first add work, then update balances, as a balance change in this block will
                // count towards next block.
                // debug!("payload to insert: {:#?}", &balances);
                // database::upsert_work(&cs.pool, &cs.chain.currencyid.to_string(), &balances, i)
                //     .await?;

                // if let Some((delta_address, delta)) = deltas.get(&i) {
                //     balances
                //         .entry(delta_address.to_owned())
                //         .and_modify(|e| *e += *delta);
                // }
                let active_subscribers = database::get_subscribers_by_status(
                    &cs.pool,
                    &cs.chain.currencyid.to_string(),
                    "subscribed",
                )
                .await?;

                let block = client.get_block_by_height(i, 2)?;

                // check_for_stake(&block, &active_subscribers, &mut cs).await?;

                for tx in block.tx.iter() {
                    for vout in tx.vout.iter() {
                        check_subscriptions(&mut cs, vout, &active_subscribers).await?;
                    }
                }
            }
        }

        let maturing_stakes =
            database::get_pending_stakes(&cs.pool, &cs.chain.currencyid.to_string()).await?;
        debug!("pending txns {:#?}", maturing_stakes);

        for stake in maturing_stakes {
            let chain_c = cs.chain.clone();
            let c_tx = cs.get_mpsc_sender();

            tokio::spawn(async move {
                if let Err(e) = wait_for_maturity(chain_c, stake.clone(), c_tx).await {
                    error!(
                        "wait for maturity: {} ({}): {:?}",
                        &stake.blockhash, &stake.currencyid, e
                    )
                }
            });
        }

        tokio::spawn({
            let pool = cs.pool.clone();
            let client = cs.chain.verusd_client()?;
            let currencyid = cs.chain.currencyid.clone();
            let bot_identity_address = cs.bot_identity_address.clone();
            let nats_client = cs.nats_client.clone();
            async move {
                process_payments(
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
                        // check if daemon is still staking:
                        if !client.get_mining_info()?.staking {
                            error!("daemon not staking anymore, shutting down");

                            return Ok(());
                        }

                        let active_subscribers = database::get_subscribers_by_status(
                            &cs.pool,
                            &cs.chain.currencyid.to_string(),
                            "subscribed",
                        )
                        .await?;

                        // get additional information about the incoming block:
                        let block = client.get_block(&blockhash, 2)?;

                        // add the work up until here
                        add_work(&active_subscribers, &client, &mut cs, block.height).await?;

                        // check if block was staked by us

                        if let Err(e) = check_for_stake(&block, &active_subscribers, &mut cs).await
                        {
                            error!("{:?}\n{:#?}\n{:#?}", e, &block, &active_subscribers);
                        };

                        for tx in block.tx.iter() {
                            for vout in tx.vout.iter() {
                                check_subscriptions(&mut cs, vout, &active_subscribers).await?;
                            }
                        }

                        continue;
                    }

                    continue;
                }

                CoinStakerMessage::StaleBlock(mut stake) => {
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

                    database::move_work_to_current_round(
                        &cs.pool,
                        &cs.chain.currencyid.to_string(),
                        stake.blockheight,
                    )
                    .await?;

                    stake.set_result("stale")?;

                    // need to update postgres
                    database::set_stake_to_processed(&cs.pool, &stake).await?;
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
                CoinStakerMessage::StakingSupply(os_tx, discord_user_id) => {
                    let client = cs.chain.verusd_client()?;
                    let wallet_info = client.get_wallet_info()?;
                    let pool_supply = wallet_info.eligible_staking_balance.as_vrsc();
                    let mining_info = client.get_mining_info()?;
                    let network_supply = mining_info.stakingsupply;
                    let my_supply;
                    if let Some(subscriber) = database::get_subscriptions(&cs.pool, discord_user_id)
                        .await?
                        .iter()
                        .find(|sub| sub.currencyid == cs.chain.currencyid)
                    {
                        trace!("subscriber found: {subscriber:?}");

                        let lu = client
                            .list_unspent(
                                Some(150),
                                Some(999999),
                                Some(&vec![subscriber.identity_address.clone()]),
                            )?
                            .iter()
                            .fold(SignedAmount::ZERO, |acc, sum| acc + sum.amount);

                        my_supply = lu.as_vrsc();
                    } else {
                        trace!("no subscriber -> not staking -> it's 0.0");
                        my_supply = 0.0;
                    }

                    if let Err(e) = os_tx.send((network_supply, pool_supply, my_supply)) {
                        error!("{e:?}");
                    }

                    continue;
                }
                CoinStakerMessage::NewAddress(os_tx) => {
                    let client = cs.chain.verusd_client()?;
                    if let Ok(address) = client.get_new_address() {
                        if let Err(e) = os_tx.send(address) {
                            error!("{e:?}");
                        }
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
                                                "discord_user_id": subscriber.discord_user_id,
                                                "identity_name": identity.identity.name.clone(),
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
                                                "discord_user_id": subscriber.discord_user_id,
                                                "identity_name": identity.identity.name.clone(),
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
            }
        }
    } else {
        error!("{} daemon is not running", cs.chain.name);
    }

    Ok(())
}

#[instrument(level = "trace", skip(chain, c_tx), fields(chain = chain.name))]
async fn wait_for_maturity(
    chain: Chain, // only needed for daemon client
    stake: Stake,
    c_tx: mpsc::Sender<CoinStakerMessage>,
) -> Result<(), Report> {
    trace!(
        "starting wait for maturity loop for {}:{}",
        stake.blockhash,
        stake.blockheight
    );
    loop {
        // client fails if stored in memory, so get a new one every loop
        let client = chain.verusd_client()?;
        // let tx = client.get_transaction(&txid, None)?;
        let block = client.get_block(&stake.blockhash, 2)?;

        let confirmations = block.confirmations;

        if confirmations < 0 {
            trace!(
                "we have staked a stale block :( {}:{}",
                stake.blockhash,
                stake.blockheight
            );

            c_tx.send(CoinStakerMessage::StaleBlock(stake)).await?;

            break;
        } else {
            // blockstomaturity is None if a coinbase tx has matured.
            if confirmations < 100 {
                trace!(
                    "{}:{} not matured, wait 10 minutes (blocks to maturity: {})",
                    stake.blockhash,
                    stake.blockheight,
                    100 - confirmations
                );
                tokio::time::sleep(TimeDuration::from_secs(600)).await;
            } else {
                trace!(
                    "{}:{} has matured, send a message to PayoutMgr",
                    stake.blockhash,
                    stake.blockheight
                );

                // block with round <blockheight> is now mature, let's do the payout.
                c_tx.send(CoinStakerMessage::MaturedBlock(stake)).await?;

                break;
            }
        }
    }

    Ok(())
}

async fn check_for_stake(
    block: &Block,
    active_subscribers: &[Subscriber],
    cs: &mut CoinStaker,
) -> Result<(), Report> {
    if matches!(block.validation_type, ValidationType::Stake) {
        if let Some(postxddest) = block.postxddest.as_ref() {
            if let Some(subscriber) = active_subscribers.iter().find(|sub| {
                &sub.identity_address == postxddest && &sub.currencyid == &cs.chain.currencyid
            }) {
                trace!(
                    "block {} was mined by a subscriber: {postxddest}",
                    block.hash
                );

                debug!("{:#?}", block);

                let blockheight = block.height;

                let mut txns = block.tx.iter();

                if let Some(coinbase_tx) = txns.next() {
                    let block_reward = coinbase_tx.vout.first().unwrap().value_sat;
                    let pos_source_vout_num = block.possourcevoutnum.unwrap();
                    // unwrap: this is risky, but in theory every stake has a pos source transaction
                    let staker_spend = txns.next().unwrap();
                    trace!("staker_spend {:#?}", staker_spend);
                    let pos_source_amount =
                        dbg!(staker_spend.vin.first()).unwrap().value_sat.unwrap();

                    let stake = Stake::new(
                        cs.chain.currencyid.clone(),
                        block.hash.clone(),
                        postxddest.clone(),
                        block.possourcetxid.unwrap().clone(),
                        pos_source_vout_num,
                        pos_source_amount,
                        StakeResult::Pending,
                        block_reward,
                        blockheight,
                    );

                    database::move_work_to_round(
                        &cs.pool,
                        &cs.chain.currencyid.to_string(),
                        0,
                        blockheight.try_into()?,
                    )
                    .await?;

                    database::insert_stake(&cs.pool, &stake).await?;

                    let payload = Payload {
                        command: "stake".to_string(),
                        data: json!({
                            "chain_name": cs.chain.name,
                            "blockheight": blockheight,
                            "blockhash": stake.blockhash,
                            "staked_by": subscriber.identity_name,
                            "amount": block_reward.as_sat()
                        }),
                    };

                    cs.nats_client
                        .publish(
                            "ipc.coinstaker".into(),
                            serde_json::to_vec(&json!(payload))?.into(),
                        )
                        .await?;

                    tokio::spawn({
                        let chain_c = cs.chain.clone();
                        let c_tx = cs.get_mpsc_sender();
                        let tx = coinbase_tx.txid.clone();
                        async move {
                            if let Err(e) = wait_for_maturity(chain_c, stake, c_tx).await {
                                error!("error in wait_for_maturity: {tx}: {:?}", e);
                            }
                        }
                    });
                } else {
                    error!("there was no coinbase tx");
                }
            }
        }
    }

    Ok(())
}

async fn check_subscriptions(
    cs: &mut CoinStaker,
    vout: &TransactionVout,
    active_subscribers: &[Subscriber],
) -> Result<(), Report> {
    if let Some(identityprimary) = &vout.script_pubkey.identityprimary {
        debug!("{:#?}", &vout);
        debug!("{identityprimary:#?}");

        let pending_subscribers = database::get_subscribers_by_status(
            &cs.pool,
            &cs.chain.currencyid.to_string(),
            "pending",
        )
        .await?;

        if let Some(s) = pending_subscribers.iter().find(|db_subscriber| {
            db_subscriber.identity_address == identityprimary.identityaddress
                && db_subscriber.currencyid == cs.chain.currencyid
        }) {
            trace!("pending subscriber found: {s:?}");

            if cs.identity_is_eligible(&identityprimary, &s) {
                trace!("bot address found in primary addresses, the subscriber is eligible and can be made active");
                if database::update_subscriber_status(
                    &cs.pool,
                    &cs.chain.currencyid.to_string(),
                    &identityprimary.identityaddress.to_string(),
                    "subscribed",
                )
                .await
                .is_ok()
                {
                    trace!("db updated, send message to discord");

                    let payload = Payload {
                        command: "subscribed".to_string(),
                        data: json!({
                            "discord_user_id": s.discord_user_id,
                            "identity_name": identityprimary.name.clone(),
                            "identity_address": identityprimary.identityaddress.clone(),
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
                }
            } else {
                trace!(
                    "a pending subscriber changed its ID but did not meet the requirements: {:#?}",
                    identityprimary
                );
            }

            return Ok(());
        }

        // check if active subscriber unsubscribed from specific chain
        if let Some(db_subscriber) = active_subscribers.iter().find(|db_subscriber| {
            db_subscriber.identity_address == identityprimary.identityaddress
                && db_subscriber.currencyid == cs.chain.currencyid
        }) {
            trace!("an active subscriber has changed its identity");
            // need to check if the primary address is still the same as the bot's
            // need to check if the minimumsignatures is 1
            // need to check if the len is more than 1
            if identityprimary.minimumsignatures == 1
                && identityprimary.primaryaddresses.len() > 1
                && identityprimary
                    .primaryaddresses
                    .contains(&db_subscriber.bot_address)
            {
                trace!("the subscription is still ok for the bot")
            } else {
                trace!("the subscription is not ok, we need to unsubscribe the user");

                if database::update_subscriber_status(
                    &cs.pool,
                    &cs.chain.currencyid.to_string(),
                    &identityprimary.identityaddress.to_string(),
                    "unsubscribed",
                )
                .await
                .is_ok()
                {
                    trace!("db updated, send message to discord");

                    let payload = Payload {
                        command: "unsubscribed".to_string(),
                        data: json!({
                            "discord_user_id": db_subscriber.discord_user_id,
                            "identity_name": identityprimary.name.clone(),
                            "identity_address": identityprimary.identityaddress.clone(),
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
                }
            }
        }
    }
    Ok(())
}
async fn add_work(
    active_subscribers: &[Subscriber],
    client: &Client,
    cs: &mut CoinStaker,
    latest_blockheight: u64,
) -> Result<(), Report> {
    if !active_subscribers.is_empty() {
        let payload = client.list_unspent(
            Some(150),
            None,
            Some(
                active_subscribers
                    .iter()
                    .map(|subscriber| subscriber.identity_address.clone())
                    .collect::<Vec<Address>>()
                    .as_ref(),
            ),
        )?;
        let payload = payload
            .into_iter()
            .filter(|lu| lu.amount.is_positive())
            .map(|lu| {
                // an address from the daemon is always correct.
                // amount is always positive as we've filtered out negatives before

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

        if !payload.is_empty() {
            debug!("payload to insert: {:#?}", &payload);
            database::upsert_work(
                &cs.pool,
                &cs.chain.currencyid.to_string(),
                &payload,
                latest_blockheight,
            )
            .await?;
        }
    }

    Ok(())
}

pub async fn get_deltas(
    client: &Client,
    addresses: &[&Address],
    last_blockheight: u64,
    current_blockheight: u64,
) -> Result<BTreeMap<u64, (Address, Decimal)>, Report> {
    let address_deltas =
        client.get_address_deltas(addresses, Some(last_blockheight - 150), Some(999999))?;

    let mut deltas_map = BTreeMap::new();

    for delta in address_deltas.into_iter() {
        // ignore zeroes as it doesn't change the staking balance
        if delta.satoshis != SignedAmount::ZERO {
            // find out about amounts received that will become eligible in the blocks between last_blockheight and current_blockheight
            // to do this, we need to go back 150 blocks in the past and see if any receives came in (spending == false)
            if delta.height < last_blockheight as i64 {
                if delta.spending == false {
                    debug!(
                        "increase eligible staking balance with {} at {}",
                        // delta.height,
                        delta.satoshis,
                        delta.height + 150,
                    );

                    deltas_map.insert(
                        delta.height as u64 + 150,
                        (
                            delta.address.clone(),
                            Decimal::from_i64(delta.satoshis.as_sat()).unwrap(),
                        ),
                    );
                }
            }

            // now all the deltas count:
            // spending == true > decrease the balance at that height; it becomes ineligible for staking
            // spending == false > increase the balance at this height + 150, but don't do it when
            // it exceeds current_blockheight as the main thread already handles new incoming blocks
            if delta.height >= last_blockheight as i64 {
                if delta.spending {
                    debug!(
                        "decrease eligible staking balance with {} at {}",
                        delta.satoshis, delta.height
                    );
                    deltas_map.insert(
                        delta.height as u64,
                        (
                            delta.address,
                            Decimal::from_i64(delta.satoshis.as_sat()).unwrap(),
                        ),
                    );
                } else {
                    if (delta.height + 150) < current_blockheight as i64 {
                        debug!(
                            "increase eligible staking balance with {} at {}",
                            delta.satoshis,
                            delta.height + 150
                        )
                    }
                    deltas_map.insert(
                        delta.height as u64 + 150,
                        (
                            delta.address,
                            Decimal::from_i64(delta.satoshis.as_sat()).unwrap(),
                        ),
                    );
                }
            }
        }
    }

    Ok(deltas_map)
}

#[derive(Debug)]
pub enum CoinStakerMessage {
    BlockNotify(BlockHash),
    StaleBlock(Stake),
    MaturedBlock(Stake),
    StakingSupply(oneshot::Sender<(f64, f64, f64)>, u64),
    SetFeeDiscount(oneshot::Sender<f32>, f32),
    NewAddress(oneshot::Sender<Address>),
    ProcessPayments(),
    GetIdentity(oneshot::Sender<Option<Identity>>, String),
    CheckSubscription(oneshot::Sender<serde_json::Value>, String),
    RecentStakes(oneshot::Sender<Vec<Stake>>),
    PendingStakes(oneshot::Sender<Vec<Stake>>),
    SetMinPayout(oneshot::Sender<serde_json::Value>, String, u64),
    Heartbeat(oneshot::Sender<()>),
}

// IPC API
async fn nats_server(
    currencyid: Address,
    cs_tx: mpsc::Sender<CoinStakerMessage>,
) -> Result<(), Report> {
    let nats_url =
        std::env::var("NATS_URL").unwrap_or_else(|_| "nats://localhost:4222".to_string());

    let client = async_nats::connect(nats_url).await?;

    trace!("starting to await NATS messages on {}", &currencyid);
    let mut requests = client.subscribe(format!("ipc.{}", currencyid)).await?;

    tokio::spawn({
        let client = client.clone();
        async move {
            while let Some(request) = requests.next().await {
                debug!("new NATS message on {}: {:?}", &currencyid, &request);

                if let Some(reply) = request.reply {
                    let payload: Payload =
                        serde_json::from_slice::<Payload>(request.payload.as_ref())?;

                    match &*payload.command {
                        "heartbeat" => {
                            let (os_tx, os_rx) = oneshot::channel::<()>();
                            cs_tx.send(CoinStakerMessage::Heartbeat(os_tx)).await?;

                            if os_rx.await.is_ok() {
                                client
                                    .publish(reply, json!({"alive": true}).to_string().into())
                                    .await?
                            } else {
                                client
                                    .publish(reply, json!({"alive": false}).to_string().into())
                                    .await?
                            }
                        }
                        "newpendingsubscriber" => {}
                        "getsubscriptions" => {}
                        "newaddress" => {
                            let (os_tx, os_rx) = oneshot::channel::<Address>();
                            cs_tx.send(CoinStakerMessage::NewAddress(os_tx)).await?;

                            let address = os_rx.await?;
                            client.publish(reply, address.to_string().into()).await?
                        }
                        "stakingsupply" => {
                            let (os_tx, os_rx) = oneshot::channel::<(f64, f64, f64)>();
                            cs_tx
                                .send(CoinStakerMessage::StakingSupply(
                                    os_tx,
                                    payload.data["discord_user_id"].as_u64().unwrap(),
                                ))
                                .await?;

                            let (network_supply, pool_supply, my_supply) = os_rx.await?;
                            // TODO can i implement Bytes on Address?
                            client
                                .publish(
                                    reply,
                                    json!({
                                        "network_supply": network_supply, 
                                        "pool_supply": pool_supply, 
                                        "my_supply": my_supply})
                                    .to_string()
                                    .into(),
                                )
                                .await?
                        }
                        "feediscount" => {
                            let (os_tx, os_rx) = oneshot::channel::<f32>();
                            cs_tx
                                .send(CoinStakerMessage::SetFeeDiscount(
                                    os_tx,
                                    serde_json::from_value(payload.data)?,
                                ))
                                .await?;

                            let supply = os_rx.await?;
                            // TODO can i implement Bytes on Address?
                            client.publish(reply, supply.to_string().into()).await?
                        }
                        "getidentity" => {
                            debug!("{payload:?}");
                            let (os_tx, os_rx) = oneshot::channel::<Option<Identity>>();
                            let identity = payload.data["identity"].as_str().unwrap().to_owned();

                            cs_tx
                                .send(CoinStakerMessage::GetIdentity(os_tx, identity))
                                .await?;

                            if let Some(identity) = os_rx.await? {
                                let ser = serde_json::to_string(&identity)?;
                                client.publish(reply, ser.into()).await?
                            } else {
                                client
                                    .publish(
                                        reply,
                                        json!({"result": "not found"}).to_string().into(),
                                    )
                                    .await?
                            }
                        }
                        "checksubscriber" => {
                            debug!("{payload:?}");
                            let (os_tx, os_rx) = oneshot::channel::<serde_json::Value>();
                            let s_id = payload.data["identity"].as_str().unwrap().to_owned();
                            debug!("{s_id}");

                            cs_tx
                                .send(CoinStakerMessage::CheckSubscription(os_tx, s_id))
                                .await?;

                            let resp = os_rx.await?;
                            client.publish(reply, resp.to_string().into()).await?;
                        }
                        "recentstakes" => {
                            debug!("{payload:?}");

                            let (os_tx, os_rx) = oneshot::channel::<Vec<Stake>>();

                            cs_tx.send(CoinStakerMessage::RecentStakes(os_tx)).await?;

                            let resp = os_rx.await?;
                            let resp_json = serde_json::to_string(&resp)?;
                            client.publish(reply, resp_json.into()).await?;
                        }
                        "setminpayout" => {
                            debug!("{payload:?}");
                            let threshold = payload.data["threshold"].as_u64().unwrap();
                            let identity = payload.data["identity"].as_str().unwrap().to_owned();

                            let (os_tx, os_rx) = oneshot::channel::<serde_json::Value>();

                            cs_tx
                                .send(CoinStakerMessage::SetMinPayout(os_tx, identity, threshold))
                                .await?;

                            let resp = os_rx.await?;
                            client.publish(reply, resp.to_string().into()).await?;
                        }
                        "pendingstakes" => {
                            debug!("{payload:?}");

                            let (os_tx, os_rx) = oneshot::channel::<Vec<Stake>>();

                            cs_tx.send(CoinStakerMessage::PendingStakes(os_tx)).await?;

                            let resp = os_rx.await?;
                            let resp_json = serde_json::to_string(&resp)?;
                            client.publish(reply, resp_json.into()).await?;
                        }
                        _ => {}
                    }
                }
            }

            Ok::<(), Report>(())
        }
    })
    .await??;

    Ok(())
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

#[instrument(skip(pool, client, bot_identity_address, interval, nats_client))]
async fn process_payments(
    pool: PgPool,
    client: Client,
    nats_client: async_nats::Client,
    currencyid: Address,
    bot_identity_address: Address,
    interval: TimeDuration,
) -> Result<(), Report> {
    let mut interval = tokio::time::interval(interval);

    loop {
        interval.tick().await;
        info!("process pending payments");

        if let Some(eligible) =
            PayoutManager::get_eligible_for_payout(&pool, &currencyid.to_string()).await?
        {
            if let Some(txid) =
                PayoutManager::send_payment(&eligible, &bot_identity_address, &client).await?
            {
                if let Err(e) = database::update_payment_members(
                    &pool,
                    &currencyid.to_string(),
                    eligible.values().flatten(),
                    &txid.to_string(),
                )
                .await
                {
                    error!(
                        "A payment was made but it could not be processed in the database\n
                error: {:?}\n
                txid: {},\n
                eligible payment members: {:#?}",
                        e, txid, &eligible
                    );
                }

                let chain_name = client
                    .get_currency(&currencyid.to_string())?
                    .fullyqualifiedname;

                // send message
                let payload = Payload {
                    command: "payment".to_string(),
                    data: json!({
                        "chain_name": chain_name,
                        "txid": txid.to_string(),
                        "n_subs": eligible.len()
                    }),
                };

                nats_client
                    .publish(
                        "ipc.coinstaker".into(),
                        serde_json::to_vec(&json!(payload))?.into(),
                    )
                    .await?;
            }
        }
    }
}

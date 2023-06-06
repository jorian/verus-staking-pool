use std::collections::{BTreeMap, HashMap};
use std::time::Duration as TimeDuration;

use color_eyre::Report;
use poollib::{chain::Chain, database, Payload, PgPool, Stake, StakeResult, Subscriber};
use rust_decimal::{prelude::FromPrimitive, Decimal};
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{debug, error, info, instrument, trace};
use vrsc_rpc::{
    json::{
        vrsc::{Address, SignedAmount},
        Block, TransactionVout, ValidationType,
    },
    Client, RpcApi,
};

use crate::payoutmanager::{PayoutManager, PayoutManagerError};

use super::{CoinStaker, CoinStakerMessage};

#[instrument(level = "trace", skip(chain, c_tx, pending_stakes), fields(chain = chain.name))]
pub async fn check_for_maturity(
    chain: Chain, // only needed for daemon client
    pending_stakes: Vec<Stake>,
    c_tx: mpsc::Sender<CoinStakerMessage>,
) -> Result<(), Report> {
    trace!(
        "checking {} pending stakes for maturity",
        pending_stakes.len()
    );

    for stake in pending_stakes.into_iter() {
        trace!(
            "starting wait for maturity loop for {}:{}",
            stake.blockhash,
            stake.blockheight
        );

        let client = chain.verusd_client()?;
        let block = client.get_block(&stake.blockhash, 2)?;

        let confirmations = block.confirmations;

        if confirmations < 0 {
            trace!(
                "we have staked a stale block :( {}:{}",
                stake.blockhash,
                stake.blockheight
            );

            c_tx.send(CoinStakerMessage::StaleBlock(stake)).await?;
        } else {
            if confirmations < 150 {
                trace!(
                    "{}:{} not matured, wait 10 minutes (blocks to maturity: {})",
                    stake.blockhash,
                    stake.blockheight,
                    150 - confirmations
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

pub async fn check_for_stake(
    block: &Block,
    active_subscribers: &[Subscriber],
    cs: &mut CoinStaker,
) -> Result<(), Report> {
    if matches!(block.validation_type, ValidationType::Stake) {
        if block.confirmations >= 0 {
            if let Some(postxddest) = block.postxddest.as_ref() {
                debug!("{:#?}", postxddest);
                if let Some(subscriber) = active_subscribers.iter().find(|sub| {
                    &sub.identity_address == postxddest && &sub.currencyid == &cs.chain.currencyid
                }) {
                    trace!(
                        "block {} was mined by a subscriber: {postxddest}",
                        block.hash
                    );

                    let blockheight = block.height;

                    let mut txns = block.tx.iter();

                    if let Some(coinbase_tx) = txns.next() {
                        let block_reward = coinbase_tx.vout.first().unwrap().value_sat;
                        let pos_source_vout_num = block.possourcevoutnum.unwrap();
                        // unwrap: this is risky, but in theory every stake has a pos source transaction
                        let staker_spend = txns.last().unwrap();
                        trace!("staker_spend {:#?}", staker_spend);
                        let pos_source_amount = if let Ok(client) = cs.chain.verusd_client() {
                            // unwrap: we know it's a stake
                            if let Ok(tx) =
                                client.get_raw_transaction_verbose(&block.possourcetxid.unwrap())
                            {
                                if let Some(vout) = tx.vout.get(pos_source_vout_num as usize) {
                                    trace!("found the pos source vout: {vout:?}");
                                    vout.value_sat
                                } else {
                                    staker_spend.vin.first().unwrap().value_sat.unwrap()
                                }
                            } else {
                                staker_spend.vin.first().unwrap().value_sat.unwrap()
                            }
                        } else {
                            staker_spend.vin.first().unwrap().value_sat.unwrap()
                        };

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
                    } else {
                        error!("there was no coinbase tx");
                    }
                }
            }
        } else {
            trace!("this was a stale before we could determine it was a stake");
        }
    }

    Ok(())
}

pub async fn check_subscriptions(
    cs: &mut CoinStaker,
    vout: &TransactionVout,
    active_subscribers: &[Subscriber],
    pending_subscribers: &[Subscriber],
) -> Result<(), Report> {
    if let Some(identityprimary) = &vout.script_pubkey.identityprimary {
        debug!("identityprimary: {identityprimary:#?}");

        let client = cs.chain.verusd_client()?;
        if let Ok(identity) = client.get_identity(&identityprimary.identityaddress.to_string()) {
            debug!("identity: {identity:?}");

            // check if the vout contains an update to an identity that is known to the staking pool:
            if let Some(s) = pending_subscribers.iter().find(|db_subscriber| {
                db_subscriber.identity_address == identityprimary.identityaddress
                    && db_subscriber.currencyid == cs.chain.currencyid
            }) {
                trace!(
                    "an update to an identity of a registered pending subscriber was found: {s:?}"
                );

                if cs.identity_is_eligible(&identityprimary, &s) {
                    trace!("pool address found in primary addresses, the subscriber is eligible and can be made active");
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
                                "identity_name": identity.fullyqualifiedname.clone(),
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
                if cs.identity_is_eligible(&identityprimary, db_subscriber) {
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
                                "identity_name": identity.fullyqualifiedname.clone(),
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
        } else {
            error!(
                "could not get identity: {}",
                &identityprimary.identityaddress.to_string()
            )
        }
    }
    Ok(())
}
pub async fn add_work(
    active_subscribers: &[Subscriber],
    pending_stakes: &[Stake],
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
        let mut payload = payload
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

        // get all the pending stakes
        // check if one of the addresses within this function is there with a pending stake
        // add that amount of work to the address as not to punish stakers
        debug!("{:#?}", pending_stakes);
        debug!("{:#?}", payload);

        pending_stakes
            .iter()
            .inspect(|stake| trace!("{}", stake.blockheight))
            .for_each(|stake| {
                if payload.contains_key(&stake.mined_by) {
                    trace!("adding work to staker to undo stake punishment");
                    payload.entry(stake.mined_by.clone()).and_modify(|v| {
                        debug!(
                            "pos_source_amount: {}",
                            stake.pos_source_amount.as_sat() as i64
                        );
                        debug!("sum: {}", v);
                        *v += Decimal::from_i64(stake.pos_source_amount.as_sat() as i64).unwrap()
                    });
                }
            });

        debug!("{:#?}", payload);

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

// pub async fn get_deltas(
//     client: &Client,
//     addresses: &[&Address],
//     last_blockheight: u64,
//     current_blockheight: u64,
// ) -> Result<BTreeMap<u64, (Address, Decimal)>, Report> {
//     let address_deltas =
//         client.get_address_deltas(addresses, Some(last_blockheight - 150), Some(999999))?;

//     let mut deltas_map = BTreeMap::new();

//     for delta in address_deltas.into_iter() {
//         // ignore zeroes as it doesn't change the staking balance
//         if delta.satoshis != SignedAmount::ZERO {
//             // find out about amounts received that will become eligible in the blocks between
//             // last_blockheight and current_blockheight
//             // to do this, we need to go back 150 blocks in the past and see if any receives came in (spending == false)
//             if delta.height < last_blockheight as i64 {
//                 if delta.spending == false {
//                     debug!(
//                         "increase eligible staking balance with {} at {}",
//                         // delta.height,
//                         delta.satoshis,
//                         delta.height + 150,
//                     );

//                     deltas_map.insert(
//                         delta.height as u64 + 150,
//                         (
//                             delta.address.clone(),
//                             Decimal::from_i64(delta.satoshis.as_sat()).unwrap(),
//                         ),
//                     );
//                 }
//             }

//             // now all the deltas count:
//             // spending == true > decrease the balance at that height; it becomes ineligible for staking
//             // spending == false > increase the balance at this height + 150, but don't do it when
//             // it exceeds current_blockheight as the main thread already handles new incoming blocks
//             if delta.height >= last_blockheight as i64 {
//                 if delta.spending {
//                     debug!(
//                         "decrease eligible staking balance with {} at {}",
//                         delta.satoshis, delta.height
//                     );
//                     deltas_map.insert(
//                         delta.height as u64,
//                         (
//                             delta.address,
//                             Decimal::from_i64(delta.satoshis.as_sat()).unwrap(),
//                         ),
//                     );
//                 } else {
//                     if (delta.height + 150) < current_blockheight as i64 {
//                         debug!(
//                             "increase eligible staking balance with {} at {}",
//                             delta.satoshis,
//                             delta.height + 150
//                         )
//                     }
//                     deltas_map.insert(
//                         delta.height as u64 + 150,
//                         (
//                             delta.address,
//                             Decimal::from_i64(delta.satoshis.as_sat()).unwrap(),
//                         ),
//                     );
//                 }
//             }
//         }
//     }

//     Ok(deltas_map)
// }

#[instrument(skip(pool, client, bot_identity_address, interval, nats_client))]
pub async fn process_payments(
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
            let outputs = PayoutManager::prepare_payment(&eligible, &client)?;
            debug!("outputs: {outputs:#?}");

            if let Some(txid) =
                PayoutManager::send_payment(outputs, &bot_identity_address, &client).await?
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
                        chain: {}\n
                        txid: {},\n
                        error: {:?}\n
                        eligible payment members: {:#?}",
                        &currencyid.to_string(),
                        txid,
                        e,
                        &eligible
                    );

                    return Err(PayoutManagerError::DbWriteFail.into());
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

use color_eyre::Report;
use futures::StreamExt;
use poollib::{Payload, Stake, Subscriber};
use serde_json::json;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, trace};
use vrsc_rpc::json::{identity::Identity, vrsc::Address};

use super::CoinStakerMessage;

// IPC API
pub async fn nats_server(
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
                        // does `setgenerate true 0` when true, `setgenerate false` when false
                        "setstaking" => {
                            let generate = payload.data["staking_enabled"].as_bool().unwrap();
                            let (os_tx, os_rx) = oneshot::channel::<()>();

                            cs_tx
                                .send(CoinStakerMessage::SetStaking(os_tx, generate))
                                .await?;

                            if let Ok(_res) = os_rx.await {
                                client
                                    .publish(
                                        reply,
                                        json!({ "result": "success" }).to_string().into(),
                                    )
                                    .await?
                            } else {
                                client
                                    .publish(
                                        reply,
                                        json!({ "result": "failed" }).to_string().into(),
                                    )
                                    .await?
                            }
                        }
                        // creates a new pending subscriber if it doesn't exist, and returns the subscriber object.
                        // fails if the identityaddress already is an active subscriber on this currencyid.
                        "newpendingsubscriber" => {
                            let (os_tx, os_rx) = oneshot::channel::<Result<Subscriber, Report>>();
                            let identityaddress: &str =
                                payload.data["identityaddress"].as_str().unwrap(); // TODO return error if missing

                            cs_tx
                                .send(CoinStakerMessage::NewSubscriber(
                                    os_tx,
                                    identityaddress.to_owned(),
                                ))
                                .await?;

                            match os_rx.await? {
                                Ok(sub) => {
                                    let serde_str = serde_json::to_string(&sub)?;
                                    client.publish(reply, serde_str.into()).await?
                                }
                                Err(e) => {
                                    client
                                        .publish(
                                            reply,
                                            json!({ "error": format!("{}", e) }).to_string().into(),
                                        )
                                        .await?;
                                }
                            }
                        }
                        // it takes the current currencyid as the currencyid in the database call.
                        "getsubscriptions" => {
                            let (os_tx, os_rx) = oneshot::channel::<Vec<Subscriber>>();
                            let mut data = payload.data;
                            debug!("data: {data:?}");
                            let identityaddresses: Vec<String> =
                                serde_json::from_value(data["identityaddresses"].take())?;
                            debug!("{identityaddresses:?}");

                            cs_tx
                                .send(CoinStakerMessage::GetSubscriptions(
                                    os_tx,
                                    identityaddresses,
                                ))
                                .await?;

                            let subscribers = os_rx.await?;

                            client
                                .publish(
                                    reply,
                                    json!({ "result": &subscribers }).to_string().into(),
                                )
                                .await?
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

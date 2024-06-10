use axum::{debug_handler, extract::Query, Extension, Json};
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot};
use vrsc_rpc::json::vrsc::Address;

use crate::{
    coinstaker::{
        coinstaker::CoinStakerMessage,
        constants::{Stake, StakeStatus, Staker},
        StakerStatus,
    },
    payout::PayoutMember,
};

#[derive(Deserialize, Debug)]
pub struct StakerStatusArgs {
    pub address: Address,
}

#[debug_handler]
pub async fn staker_status(
    Extension(tx): Extension<mpsc::Sender<CoinStakerMessage>>,
    Query(args): Query<StakerStatusArgs>,
) -> Json<String> {
    let (os_tx, os_rx) = oneshot::channel::<String>();

    tx.send(CoinStakerMessage::StakerStatus(os_tx, args.address))
        .await
        .unwrap();

    let res = os_rx.await.unwrap();

    Json(res)
}

#[derive(Deserialize, Debug)]
pub struct GetStakerArgs {
    pub identity_address: Address,
    pub staker_status: Option<StakerStatus>,
}

#[debug_handler]
pub async fn get_staker(
    Extension(tx): Extension<mpsc::Sender<CoinStakerMessage>>,
    Query(args): Query<GetStakerArgs>,
) -> Json<Vec<Staker>> {
    let (os_tx, os_rx) = oneshot::channel::<Vec<Staker>>();

    tx.send(CoinStakerMessage::GetStaker(
        os_tx,
        args.identity_address,
        args.staker_status,
    ))
    .await
    .unwrap();

    let res = os_rx.await.unwrap();

    Json(res)
}

#[derive(Deserialize, Debug)]
pub struct GetPayoutsArgs {
    pub identity_addresses: Vec<Address>,
}

#[debug_handler]
pub async fn get_payouts(
    Extension(tx): Extension<mpsc::Sender<CoinStakerMessage>>,
    Query(args): Query<GetPayoutsArgs>,
) -> Json<Vec<PayoutMember>> {
    let (os_tx, os_rx) = oneshot::channel::<Vec<PayoutMember>>();

    tx.send(CoinStakerMessage::GetPayouts(
        os_tx,
        args.identity_addresses,
    ))
    .await
    .unwrap();

    let res = os_rx.await.unwrap();

    Json(res)
}

#[derive(Deserialize, Debug)]
pub struct GetStakesArgs {
    pub stake_status: Option<StakeStatus>,
}

#[debug_handler]
pub async fn get_stakes(
    Extension(tx): Extension<mpsc::Sender<CoinStakerMessage>>,
    Query(args): Query<GetStakesArgs>,
) -> Json<Vec<Stake>> {
    let (os_tx, os_rx) = oneshot::channel::<Vec<Stake>>();

    tx.send(CoinStakerMessage::GetStakes(os_tx, args.stake_status))
        .await
        .unwrap();

    let res = os_rx.await.unwrap();

    Json(res)
}

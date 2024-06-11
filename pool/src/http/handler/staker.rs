use anyhow::Context;
use axum::{debug_handler, extract::Query, Extension};
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot};
use vrsc_rpc::json::vrsc::Address;

use crate::{
    coinstaker::{
        coinstaker::CoinStakerMessage,
        constants::{Stake, StakeStatus, Staker},
        StakerStatus,
    },
    http::handler::AppJson,
    payout::PayoutMember,
};

use super::AppError;

#[derive(Deserialize, Debug)]
pub struct StakerStatusArgs {
    pub address: Address,
}

#[debug_handler]
pub async fn staker_status(
    Extension(tx): Extension<mpsc::Sender<CoinStakerMessage>>,
    Query(args): Query<StakerStatusArgs>,
) -> Result<AppJson<String>, AppError> {
    let (os_tx, os_rx) = oneshot::channel::<String>();

    tx.send(CoinStakerMessage::StakerStatus(os_tx, args.address))
        .await
        .context("Could not send Coinstaker message")?;

    let res = os_rx.await.context("Sender dropped")?;

    Ok(AppJson(res))
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
) -> Result<AppJson<Vec<Staker>>, AppError> {
    let (os_tx, os_rx) = oneshot::channel::<Vec<Staker>>();

    tx.send(CoinStakerMessage::GetStaker(
        os_tx,
        args.identity_address,
        args.staker_status,
    ))
    .await
    .context("Could not send Coinstaker message")?;

    let res = os_rx.await.context("Sender dropped")?;

    Ok(AppJson(res))
}

#[derive(Deserialize, Debug)]
pub struct GetPayoutsArgs {
    pub identity_addresses: Vec<Address>,
}

#[debug_handler]
pub async fn get_payouts(
    Extension(tx): Extension<mpsc::Sender<CoinStakerMessage>>,
    Query(args): Query<GetPayoutsArgs>,
) -> Result<AppJson<Vec<PayoutMember>>, AppError> {
    let (os_tx, os_rx) = oneshot::channel::<Vec<PayoutMember>>();

    tx.send(CoinStakerMessage::GetPayouts(
        os_tx,
        args.identity_addresses,
    ))
    .await
    .context("Could not send Coinstaker message")?;

    let res = os_rx.await.context("Sender dropped")?;

    Ok(AppJson(res))
}

#[derive(Deserialize, Debug)]
pub struct GetStakesArgs {
    pub stake_status: Option<StakeStatus>,
}

#[debug_handler]
pub async fn get_stakes(
    Extension(tx): Extension<mpsc::Sender<CoinStakerMessage>>,
    Query(args): Query<GetStakesArgs>,
) -> Result<AppJson<Vec<Stake>>, AppError> {
    let (os_tx, os_rx) = oneshot::channel::<Vec<Stake>>();

    tx.send(CoinStakerMessage::GetStakes(os_tx, args.stake_status))
        .await
        .context("Could not send Coinstaker message")?;

    let res = os_rx.await.context("Sender dropped")?;

    Ok(AppJson(res))
}

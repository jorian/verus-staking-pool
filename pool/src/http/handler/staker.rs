use anyhow::Context;
use axum::{debug_handler, extract::Query, Extension};
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot};
use tracing::debug;
use vrsc_rpc::json::vrsc::Address;

use crate::{
    coinstaker::{
        coinstaker::CoinStakerMessage,
        constants::{Staker, StakerBalance},
        StakerStatus,
    },
    http::handler::AppJson,
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
pub async fn get_stakers(
    Extension(tx): Extension<mpsc::Sender<CoinStakerMessage>>,
    Query(args): Query<GetStakerArgs>,
) -> Result<AppJson<Vec<Staker>>, AppError> {
    let (os_tx, os_rx) = oneshot::channel::<Vec<Staker>>();

    tx.send(CoinStakerMessage::GetStakers(
        os_tx,
        args.identity_address,
        args.staker_status,
    ))
    .await
    .context("Could not send Coinstaker message")?;

    let res = os_rx.await.context("Sender dropped")?;

    Ok(AppJson(res))
}

pub async fn get_staker_balance(
    Extension(tx): Extension<mpsc::Sender<CoinStakerMessage>>,
    Query(args): Query<Vec<(String, Address)>>,
) -> Result<AppJson<Vec<(Address, StakerBalance)>>, AppError> {
    dbg!(&args);
    let (os_tx, os_rx) = oneshot::channel::<Vec<(Address, StakerBalance)>>();

    let args = args.into_iter().map(|arg| arg.1).collect::<Vec<_>>();

    tx.send(CoinStakerMessage::GetStakerBalance(os_tx, args))
        .await
        .context("Could not send Coinstaker message")?;

    let map = os_rx.await.context("Sender dropped")?;

    debug!("{:?}", &map);

    Ok(AppJson(map))
}

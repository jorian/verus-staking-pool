use anyhow::Context;
use axum::{debug_handler, extract::Query, Extension};
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot};

use crate::{
    coinstaker::{
        coinstaker::CoinStakerMessage,
        constants::{Stake, StakeStatus},
    },
    http::handler::{AppError, AppJson},
};

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

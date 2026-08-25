use anyhow::Context;
use axum::{debug_handler, Extension};
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot};

use crate::{
    coinstaker::{
        coinstaker::CoinStakerMessage,
        constants::{Stake, StakeStatus},
    },
    http::handler::{AppError, AppJson},
};

#[derive(Deserialize, Debug, Default)]
pub struct GetStakesArgs {
    pub stake_status: Option<StakeStatus>,
    pub limit: Option<u32>,
    pub before_height: Option<u64>,
}

#[debug_handler]
pub async fn get_stakes(
    Extension(tx): Extension<mpsc::Sender<CoinStakerMessage>>,
    axum_extra::extract::Query(args): axum_extra::extract::Query<GetStakesArgs>,
) -> Result<AppJson<Vec<Stake>>, AppError> {
    let (os_tx, os_rx) = oneshot::channel::<Vec<Stake>>();

    tx.send(CoinStakerMessage::GetStakes(
        os_tx,
        args.stake_status,
        args.limit.map(|n| n.clamp(1, 200)),
        args.before_height,
    ))
    .await
    .context("Could not send Coinstaker message")?;

    let res = os_rx.await.context("Sender dropped")?;

    Ok(AppJson(res))
}

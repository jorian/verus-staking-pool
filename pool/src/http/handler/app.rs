use anyhow::Context;
use axum::{debug_handler, extract::State, Extension, Json};
use tokio::sync::{mpsc, oneshot};

use crate::{
    coinstaker::coinstaker::CoinStakerMessage,
    http::{constants::Stats, handler::AppJson, routing::AppState},
};

use super::AppError;

#[debug_handler]
pub async fn info(State(controller): State<AppState>) -> Json<String> {
    let version = controller.controller.version();

    Json(version)
}

/// Returns the primary address of the pool.
///
/// Is to be added to the `primaryaddresses` field of VerusIDs that want to stake in this pool.
pub async fn pool_primary_address(
    Extension(tx): Extension<mpsc::Sender<CoinStakerMessage>>,
) -> Result<AppJson<String>, AppError> {
    let (os_tx, os_rx) = oneshot::channel::<String>();

    tx.send(CoinStakerMessage::PoolPrimaryAddress(os_tx))
        .await
        .context("Could not send Coinstaker message")?;

    let res = os_rx.await.context("Sender dropped")?;

    Ok(AppJson(res))
}

pub async fn statistics(
    Extension(tx): Extension<mpsc::Sender<CoinStakerMessage>>,
) -> Result<AppJson<Stats>, AppError> {
    let (os_tx, os_rx) = oneshot::channel::<Stats>();

    tx.send(CoinStakerMessage::GetStatistics(os_tx))
        .await
        .context("Could not send Coinstaker message")?;

    let stats = os_rx.await.context("Sender dropped")?;

    Ok(AppJson(stats))
}

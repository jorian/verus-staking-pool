use anyhow::Context;
use axum::{debug_handler, extract::State, Extension, Json};
use tokio::sync::{mpsc, oneshot};

use crate::{
    coinstaker::coinstaker::CoinStakerMessage,
    http::{handler::AppJson, routing::AppState},
};

use super::AppError;

#[debug_handler]
pub async fn info(State(controller): State<AppState>) -> Json<String> {
    let version = controller.controller.version();

    Json(version)
}

#[debug_handler]
pub async fn pool_address(
    Extension(tx): Extension<mpsc::Sender<CoinStakerMessage>>,
) -> Result<AppJson<String>, AppError> {
    let (os_tx, os_rx) = oneshot::channel::<String>();

    tx.send(CoinStakerMessage::PoolPrimaryAddress(os_tx))
        .await
        .context("Could not send Coinstaker message")?;

    let res = os_rx.await.context("Sender dropped")?;

    Ok(AppJson(res))
}

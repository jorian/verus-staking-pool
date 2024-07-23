use anyhow::Context;
use axum::{debug_handler, extract::Query, Extension};
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot};
use vrsc_rpc::json::vrsc::Address;

use crate::{
    coinstaker::coinstaker::CoinStakerMessage,
    http::handler::{AppError, AppJson},
    payout::PayoutMember,
};

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

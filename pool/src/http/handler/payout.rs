use anyhow::Context;
use axum::{debug_handler, Extension};
use tokio::sync::{mpsc, oneshot};

use crate::{
    coinstaker::coinstaker::CoinStakerMessage,
    http::handler::{params::GetPayoutsArgs, AppError, AppJson},
    payout_service::PayoutMember,
};

#[debug_handler]
pub async fn get_payouts(
    Extension(tx): Extension<mpsc::Sender<CoinStakerMessage>>,
    axum_extra::extract::Query(args): axum_extra::extract::Query<GetPayoutsArgs>,
) -> Result<AppJson<Vec<PayoutMember>>, AppError> {
    let (os_tx, os_rx) = oneshot::channel::<Vec<PayoutMember>>();
    let (limit, before_height) = args.page();

    tx.send(CoinStakerMessage::GetPayouts(
        os_tx,
        args.identities.addresses(),
        limit,
        before_height,
    ))
    .await
    .context("Could not send Coinstaker message")?;

    let res = os_rx.await.context("Sender dropped")?;

    Ok(AppJson(res))
}

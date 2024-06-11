use anyhow::Context;
use axum::{debug_handler, Extension};
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot};
use vrsc_rpc::json::vrsc::Address;

use crate::{
    coinstaker::coinstaker::CoinStakerMessage,
    http::{constants::StakingSupply, handler::AppJson},
};

use super::AppError;

#[derive(Deserialize, Debug)]
pub struct Identities {
    #[serde(default, rename = "identity_address")]
    identity_addresses: Vec<Address>,
}

#[debug_handler]
pub async fn staking_supply(
    Extension(tx): Extension<mpsc::Sender<CoinStakerMessage>>,
    axum_extra::extract::Query(items): axum_extra::extract::Query<Identities>,
) -> Result<AppJson<StakingSupply>, AppError> {
    let (os_tx, os_rx) = oneshot::channel::<StakingSupply>();

    tx.send(CoinStakerMessage::StakingSupply(
        os_tx,
        items.identity_addresses,
    ))
    .await
    .context("Could not send Coinstaker message")?;

    let ss = os_rx.await.context("Sender dropped")?;

    Ok(AppJson(ss))
}

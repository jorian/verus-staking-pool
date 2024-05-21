use axum::{debug_handler, Extension, Json};
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot};

use crate::{
    coinstaker::{coinstaker::CoinStakerMessage, IdentityAddress},
    http::constants::StakingSupply,
};

#[derive(Deserialize, Debug)]
pub struct Identities {
    #[serde(default, rename = "identity_address")]
    identity_addresses: Vec<IdentityAddress>,
}

#[debug_handler]
pub async fn staking_supply(
    Extension(tx): Extension<mpsc::Sender<CoinStakerMessage>>,
    axum_extra::extract::Query(items): axum_extra::extract::Query<Identities>,
) -> Json<StakingSupply> {
    // dbg!(items.identity_addresses);
    let (os_tx, os_rx) = oneshot::channel::<StakingSupply>();

    tx.send(CoinStakerMessage::StakingSupply(
        os_tx,
        items.identity_addresses,
    ))
    .await
    .unwrap();
    let ss = os_rx.await.unwrap();

    Json(ss)
}

use axum::{debug_handler, extract::Query, Extension, Json};
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot};
use vrsc_rpc::json::vrsc::Address;

use crate::coinstaker::coinstaker::CoinStakerMessage;

#[derive(Deserialize, Debug)]
pub struct StakerStatusArgs {
    pub address: Address,
}

#[debug_handler]
pub async fn staker_status(
    Extension(tx): Extension<mpsc::Sender<CoinStakerMessage>>,
    Query(args): Query<StakerStatusArgs>,
) -> Json<String> {
    dbg!(&args);
    let (os_tx, os_rx) = oneshot::channel::<String>();

    tx.send(CoinStakerMessage::StakerStatus(os_tx, args.address))
        .await
        .unwrap();

    let res = os_rx.await.unwrap();

    Json(res)
}

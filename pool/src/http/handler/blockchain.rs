use anyhow::Context;
use axum::Extension;
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot};
use vrsc_rpc::json::vrsc::Address;

use crate::coinstaker::coinstaker::CoinStakerMessage;
use crate::http::constants::StakingSupply;
use crate::http::handler::AppJson;

use super::AppError;

#[derive(Deserialize, Debug)]
pub struct Identities {
    #[serde(default, rename = "identity_address")]
    identity_addresses: Vec<Address>,
}

/// Returns the staking supply of this pool.
///
/// Returns a JSON of 3 staking supplies:
/// - The current staking supply of the network
/// - The current staking supply of this staking pool
/// - The current staking supply of the VerusIDs supplied in the arguments.
///
/// ```json
/// {
///     "staker": 10.24681657,
///     "pool": 250.12345678,
///     "network": 75565.23456789
/// }
/// ```
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

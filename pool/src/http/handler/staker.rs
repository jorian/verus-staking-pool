use anyhow::Context;
use axum::{extract::Query, Extension};
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot};
use tracing::debug;
use vrsc_rpc::json::vrsc::Address;

use crate::{
    coinstaker::{
        coinstaker::CoinStakerMessage,
        constants::{Staker, StakerBalance},
        StakerStatus,
    },
    http::handler::AppJson,
};

use super::AppError;

#[derive(Deserialize, Debug)]
pub struct StakerStatusArgs {
    pub address: Address,
}

/// Checks the eligibility of the staker, updates it and returns the staker.
///
/// If, for any reason, the pool did not pick up an eligible staker, this endpoint can be used
/// to supply a VerusID and check if it is eligible to stake in this pool.
///
/// Returns a staker object with the following fields:
/// - currency_address: the i-address of the chain this staker is on
/// - identity_address: the i-address of the VerusID of this staker
/// - identity_name: the name of this VerusID
/// - min_payout: the amount (in sats) of the minimum payout threshold
/// - status: The status of this staker. One of ["active", "cooling_down", "inactive"].
/// - fee: the fee percentage in decimals, expressed as basispoints. 0.01 = 1%.
///
/// Response example:
/// ```json
/// {
///     "currency_address": "iJhCezBExJHvtyH3fGhNnt2NhU4Ztkf2yq",
///     "identity_address": "iJcwZBwQ1CHDLp9jmFJxi3k6wCMkWk8Cpz",
///     "identity_name": "identity",
///     "min_payout": 100000000,
///     "status": "cooling_down",
///     "fee": 0.003,
/// }
/// ```
///
/// For more information about the Staker object, see <Staker>
pub async fn staker_status(
    Extension(tx): Extension<mpsc::Sender<CoinStakerMessage>>,
    Query(args): Query<StakerStatusArgs>,
) -> Result<AppJson<Staker>, AppError> {
    let (os_tx, os_rx) = oneshot::channel::<Option<Staker>>();

    tx.send(CoinStakerMessage::StakerStatus(os_tx, args.address))
        .await
        .context("Could not send Coinstaker message")?;

    let res = os_rx.await.context("Sender dropped")?;

    if let Some(staker) = res {
        Ok(AppJson(staker))
    } else {
        Err(AppError::NotFound)
    }
}

#[derive(Deserialize, Debug)]
pub struct GetStakerArgs {
    pub identity_addresses: Vec<Address>,
    pub staker_status: Option<StakerStatus>,
}

/// Finds and returns an array of stakers based on the supplied `identity_addresses` argument,
/// if they are found, optionally filtered by staker status.
///
/// `staker_status` can be one of ["active", "cooling_down", "inactive"].
///
/// If one of the supplied identity addresses is not found in this pool, a 404 NOT FOUND
/// is returned.
pub async fn get_stakers(
    Extension(tx): Extension<mpsc::Sender<CoinStakerMessage>>,
    Query(args): Query<GetStakerArgs>,
) -> Result<AppJson<Vec<Staker>>, AppError> {
    let (os_tx, os_rx) = oneshot::channel::<Vec<Staker>>();

    tx.send(CoinStakerMessage::GetStakers(
        os_tx,
        args.identity_addresses,
        args.staker_status,
    ))
    .await
    .context("Could not send Coinstaker message")?;

    let res = os_rx.await.context("Sender dropped")?;

    Ok(AppJson(res))
}

pub async fn get_staker_balance(
    Extension(tx): Extension<mpsc::Sender<CoinStakerMessage>>,
    Query(args): Query<Vec<(String, Address)>>,
) -> Result<AppJson<Vec<(Address, StakerBalance)>>, AppError> {
    let (os_tx, os_rx) = oneshot::channel::<Vec<(Address, StakerBalance)>>();

    let args = args.into_iter().map(|arg| arg.1).collect::<Vec<_>>();

    tx.send(CoinStakerMessage::GetStakerBalance(os_tx, args))
        .await
        .context("Could not send Coinstaker message")?;

    let map = os_rx.await.context("Sender dropped")?;

    debug!("{:?}", &map);

    Ok(AppJson(map))
}

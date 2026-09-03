use std::collections::HashMap;

use axum::{extract::Query, Extension};
use rust_decimal::Decimal;
use serde::Deserialize;
use vrsc_rpc::json::vrsc::Address;

use crate::{
    coinstaker::{
        constants::{Staker, StakerEarnings},
        CoinStakerHandle,
    },
    http::handler::{
        params::{GetStakerArgs, IdentityQuery},
        AppJson,
    },
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
    Extension(cs): Extension<CoinStakerHandle>,
    Query(args): Query<StakerStatusArgs>,
) -> Result<AppJson<Staker>, AppError> {
    if let Some(staker) = cs.staker_status(args.address).await? {
        Ok(AppJson(staker))
    } else {
        Err(AppError::NotFound)
    }
}

/// Finds and returns an array of stakers.
///
/// Omit `identity_address` to list every staker on this chain. Optionally filter with
/// `staker_status` (`active`, `cooling_down`, `inactive`). Repeated `identity_address`
/// (or `identity_addresses`) limits the result to those VerusIDs; unknown ids are ignored.
pub async fn get_stakers(
    Extension(cs): Extension<CoinStakerHandle>,
    axum_extra::extract::Query(args): axum_extra::extract::Query<GetStakerArgs>,
) -> Result<AppJson<Vec<Staker>>, AppError> {
    Ok(AppJson(
        cs.stakers(args.identities.addresses(), args.staker_status)
            .await?,
    ))
}

/// Returns an array of balances, based on the provided VerusIDs.
///
/// The balances represent how much each staker has earned in the pool
pub async fn get_staker_earnings(
    Extension(cs): Extension<CoinStakerHandle>,
    Query(args): Query<Vec<(String, Address)>>,
) -> Result<AppJson<HashMap<Address, StakerEarnings>>, AppError> {
    let args = args.into_iter().map(|arg| arg.1).collect::<Vec<_>>();
    Ok(AppJson(cs.staker_earnings(args).await?))
}

/// Returns eligible staking balances.
///
/// Omit `identity_address` to return balances for every **active** staker.
pub async fn get_staking_balance(
    Extension(cs): Extension<CoinStakerHandle>,
    axum_extra::extract::Query(args): axum_extra::extract::Query<IdentityQuery>,
) -> Result<AppJson<HashMap<Address, f64>>, AppError> {
    let balances = cs
        .staking_balance(args.addresses())
        .await?
        .into_iter()
        .map(|(k, v)| (k, v.as_vrsc()))
        .collect();
    Ok(AppJson(balances))
}

#[derive(Deserialize, Debug)]
pub struct SetStakerFeeArgs {
    pub address: Address,
    pub fee: Decimal,
}

/// Sets the pool fee for an enrolled staker.
///
/// `fee` is a decimal fraction: 0.01 = 1%. Must be between 0 and 1 inclusive.
/// Returns the updated staker, or 404 if that identity is not enrolled.
pub async fn set_staker_fee(
    Extension(cs): Extension<CoinStakerHandle>,
    AppJson(args): AppJson<SetStakerFeeArgs>,
) -> Result<AppJson<Staker>, AppError> {
    if args.fee < Decimal::ZERO || args.fee > Decimal::ONE {
        return Err(AppError::BadRequest(
            "fee must be a decimal between 0 and 1".to_owned(),
        ));
    }

    if let Some(staker) = cs.set_staker_fee(args.address, args.fee).await? {
        Ok(AppJson(staker))
    } else {
        Err(AppError::NotFound)
    }
}

use axum::Extension;
use serde::Deserialize;
use tracing::debug;
use vrsc_rpc::json::vrsc::Address;

use crate::coinstaker::CoinStakerHandle;
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
    Extension(cs): Extension<CoinStakerHandle>,
    axum_extra::extract::Query(items): axum_extra::extract::Query<Identities>,
) -> Result<AppJson<StakingSupply>, AppError> {
    debug!(?items);
    Ok(AppJson(cs.staking_supply(items.identity_addresses).await?))
}

use axum::{debug_handler, Extension};
use serde::Deserialize;

use crate::{
    coinstaker::{
        constants::{Stake, StakeStatus},
        CoinStakerHandle,
    },
    http::handler::{AppError, AppJson},
};

#[derive(Deserialize, Debug, Default)]
pub struct GetStakesArgs {
    pub stake_status: Option<StakeStatus>,
    pub limit: Option<u32>,
    pub before_height: Option<u64>,
}

#[debug_handler]
pub async fn get_stakes(
    Extension(cs): Extension<CoinStakerHandle>,
    axum_extra::extract::Query(args): axum_extra::extract::Query<GetStakesArgs>,
) -> Result<AppJson<Vec<Stake>>, AppError> {
    Ok(AppJson(
        cs.stakes(
            args.stake_status,
            args.limit.map(|n| n.clamp(1, 200)),
            args.before_height,
        )
        .await?,
    ))
}

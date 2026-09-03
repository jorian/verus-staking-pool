use axum::{debug_handler, Extension};

use crate::{
    coinstaker::CoinStakerHandle,
    http::{
        constants::{PoolHistory, WorkShare},
        handler::AppJson,
    },
};

use super::AppError;

/// Current unpaid work (shares accumulated on round 0) for every staker on this chain.
#[debug_handler]
pub async fn get_work(
    Extension(cs): Extension<CoinStakerHandle>,
) -> Result<AppJson<Vec<WorkShare>>, AppError> {
    Ok(AppJson(cs.work().await?))
}

/// Pool staking balance by block height, plus cumulative stake count.
#[debug_handler]
pub async fn get_history(
    Extension(cs): Extension<CoinStakerHandle>,
) -> Result<AppJson<PoolHistory>, AppError> {
    Ok(AppJson(cs.history().await?))
}

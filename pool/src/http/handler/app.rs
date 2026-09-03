use axum::{debug_handler, extract::State, Extension, Json};

use crate::{
    coinstaker::CoinStakerHandle,
    http::{constants::Stats, handler::AppJson, routing::AppState},
};

use super::AppError;

#[debug_handler]
pub async fn info(State(controller): State<AppState>) -> Json<String> {
    let version = controller.controller.version();

    Json(version)
}

/// Returns the primary address of the pool.
///
/// Is to be added to the `primaryaddresses` field of VerusIDs that want to stake in this pool.
pub async fn pool_primary_address(
    Extension(cs): Extension<CoinStakerHandle>,
) -> Result<AppJson<String>, AppError> {
    Ok(AppJson(cs.pool_primary_address()))
}

pub async fn statistics(
    Extension(cs): Extension<CoinStakerHandle>,
) -> Result<AppJson<Stats>, AppError> {
    Ok(AppJson(cs.statistics().await?))
}

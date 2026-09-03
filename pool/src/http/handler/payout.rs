use axum::{debug_handler, Extension};

use crate::{
    coinstaker::CoinStakerHandle,
    http::handler::{params::GetPayoutsArgs, AppError, AppJson},
    payout_service::PayoutMember,
};

#[debug_handler]
pub async fn get_payouts(
    Extension(cs): Extension<CoinStakerHandle>,
    axum_extra::extract::Query(args): axum_extra::extract::Query<GetPayoutsArgs>,
) -> Result<AppJson<Vec<PayoutMember>>, AppError> {
    let (limit, before_height) = args.page();
    Ok(AppJson(
        cs.payouts(args.identities.addresses(), limit, before_height)
            .await?,
    ))
}

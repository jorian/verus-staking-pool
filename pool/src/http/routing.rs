use std::sync::Arc;

use axum::{
    extract::{MatchedPath, Path, Request, State},
    middleware::{self, Next},
    response::Response,
    routing::{get, put},
};
use reqwest::StatusCode;
use tower_http::trace::{DefaultOnRequest, DefaultOnResponse};
use tracing::Level;
use uuid::Uuid;
use vrsc_rpc::json::vrsc::Address;

use crate::controller::Controller;

use super::handler;

pub fn base_path() -> &'static str {
    "/v1"
}

#[derive(Clone)]
pub struct AppState {
    pub controller: Arc<Controller>,
}

pub fn router(controller: Arc<Controller>) -> axum::Router {
    let state = AppState { controller };

    axum::Router::new()
        .nest(
            &base_path(),
            main_router(state.clone()).nest("/currency", currency_router(state)),
        )
        .layer(
            tower_http::trace::TraceLayer::new_for_http()
                .make_span_with(|req: &Request| {
                    let request_id = Uuid::new_v4();
                    let path = if let Some(path) = req.extensions().get::<MatchedPath>() {
                        path.as_str()
                    } else {
                        req.uri().path()
                    };

                    tracing::info_span!(
                        "http-request", request_id = format!("{}", request_id), %path
                    )
                })
                .on_request(DefaultOnRequest::new().level(Level::INFO))
                .on_response(
                    DefaultOnResponse::new()
                        .level(Level::INFO)
                        .latency_unit(tower_http::LatencyUnit::Micros),
                ),
        )
}

pub fn main_router(state: AppState) -> axum::Router {
    axum::Router::new()
        .route("/info", get(handler::app::info))
        .with_state(state)
}

pub fn currency_router(state: AppState) -> axum::Router {
    axum::Router::new()
        .route(
            "/:currency/stakingsupply",
            get(handler::blockchain::staking_supply),
        )
        .route(
            "/:currency/stakerstatus",
            put(handler::staker::staker_status),
        )
        .route_layer(middleware::from_fn_with_state(state.clone(), my_middleware))
        .with_state(state)
}

async fn my_middleware(
    State(state): State<AppState>,
    Path(currency): Path<Address>,
    mut request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    if let Some(currency_id) = state.controller.coin_stakers.get(&currency).cloned() {
        request.extensions_mut().insert(currency_id);

        Ok(next.run(request).await)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

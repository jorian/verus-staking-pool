use axum::{debug_handler, extract::State, Json};

use crate::http::routing::AppState;

#[debug_handler]
pub async fn info(State(controller): State<AppState>) -> Json<String> {
    let version = controller.controller.version().await.unwrap();

    Json(version)
}

//! `GET /v1/models` — mirrors `routers/models.py`.

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::Value;

use super::AppState;
use crate::model_catalog::list_catalog;

pub fn router() -> Router<AppState> {
    Router::new().route("/models", get(models))
}

async fn models(State(state): State<AppState>) -> Json<Value> {
    Json(list_catalog(&state.config).await)
}

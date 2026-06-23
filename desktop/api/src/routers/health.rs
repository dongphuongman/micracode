//! `GET /v1/health` — mirrors `routers/health.py`.

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use super::AppState;

#[derive(Serialize)]
struct HealthResponse {
    status: String,
    environment: String,
    provider: String,
    model: String,
}

pub fn router() -> Router<AppState> {
    Router::new().route("/health", get(health))
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    let config = &state.config;
    Json(HealthResponse {
        status: "ok".to_string(),
        environment: config.environment.clone(),
        provider: config.llm_provider.as_str().to_string(),
        model: config.active_model().to_string(),
    })
}

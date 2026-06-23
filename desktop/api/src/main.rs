//! Micracode API — a Rust (Axum) reimplementation of the Python FastAPI
//! `/v1` backend. This first pass covers the projects + models surface
//! (see `routers/`); the streaming `/generate` endpoint is intentionally
//! out of scope for now.

mod bindings;
mod config;
mod decider;
mod error;
mod model_catalog;
mod preview;
mod projection;
mod provider;
mod reaper;
mod routers;
mod schemas;
mod starter;
mod storage;
mod terminal;
mod text_generation;
mod vcs;

use std::sync::Arc;

use axum::http::{header, HeaderName, HeaderValue, Method};
use axum::Router;
use tower_http::cors::CorsLayer;

use core_orchestration::Engine;
use core_persistence::EventStore;

use core_provider::{ClaudeConfig, ClaudeDriver, CodexConfig, CodexDriver};

use crate::config::Config;
use crate::preview::PreviewManager;
use crate::projection::ProjectionHandle;
use crate::provider::ProviderManager;
use crate::reaper::SessionRegistry;
use crate::routers::{api_router, AppState};
use crate::storage::Storage;
use crate::terminal::TerminalManager;

#[tokio::main]
async fn main() {
    // Best-effort: load a repo-root `.env` and `apps/api/.env`, matching the
    // files the Python service reads. Process env always takes precedence
    // (dotenvy never overrides already-set vars).
    let _ = dotenvy::dotenv();
    let _ = dotenvy::from_filename("apps/api/.env");

    let config = Config::from_env();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level_filter(&config.log_level))),
        )
        .init();

    let storage = Storage::new(config.opener_apps_dir.clone());
    if let Err(e) = storage.ensure_root() {
        tracing::error!("failed to create storage root: {e}");
        std::process::exit(1);
    }

    // Event-sourced core: a file-backed log next to the app data dir, migrated
    // forward on open. Commands are validated and turned into domain events by
    // the domain `decider` (PRD FR2).
    let events_db = config.opener_apps_dir.join("events.db");
    let store = match EventStore::open(&events_db) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("failed to open event store at {}: {e}", events_db.display());
            std::process::exit(1);
        }
    };
    let engine = Arc::new(Engine::new(store, decider::decide));

    // Read-model projection (PRD FR2): fold the log into threads/turns/messages
    // and keep it live off the store's hot stream. Built before sessions start
    // so it sees every event from the first one.
    let projection = ProjectionHandle::spawn(Arc::clone(&engine));

    // Provider slice (PRD FR1, §4): each session runs on the agent its
    // `start_session` request names — the Codex CLI or the Claude Code CLI, both
    // over stdio. Binaries resolve from `CODEX_BIN` / `CLAUDE_BIN` (defaults
    // `codex` / `claude`) and are verified lazily on the first session start,
    // surfacing a spawn error rather than failing boot.
    let codex_bin = std::env::var_os("CODEX_BIN").unwrap_or_else(|| "codex".into());
    let claude_bin = std::env::var_os("CLAUDE_BIN").unwrap_or_else(|| "claude".into());

    // Reap any session subprocesses orphaned by a previous hard crash before we
    // start new ones (PRD FR1, P4). Recorded pids live next to the event log.
    let registry = SessionRegistry::new(config.opener_apps_dir.join("sessions.pids"));
    let reaped = registry.sweep();
    if !reaped.is_empty() {
        tracing::warn!("reaped {} orphaned session subprocess(es): {reaped:?}", reaped.len());
    }

    let provider = ProviderManager::new(
        CodexDriver::new(CodexConfig {
            program: codex_bin,
            extra_args: Vec::new(),
        }),
        Arc::clone(&engine),
    )
    .with_claude_driver(ClaudeDriver::new(ClaudeConfig {
        program: claude_bin,
        extra_args: Vec::new(),
    }))
    .with_session_registry(registry);

    let state = AppState {
        storage: Arc::new(storage),
        config: Arc::new(config.clone()),
        engine,
        provider: Arc::new(provider),
        projection,
        // Terminal + preview managers (PRD FR7). Both own their children and
        // reap them on drop, so no explicit shutdown wiring is needed.
        terminals: Arc::new(TerminalManager::new()),
        previews: Arc::new(PreviewManager::new()),
    };

    let app = Router::new()
        .nest("/v1", api_router(state))
        .layer(build_cors(&config));

    let addr = format!("{}:{}", config.host, config.port);
    tracing::info!(
        "micracode-api (rust) ready env={} provider={} model={} origins={:?} data_dir={} addr={}",
        config.environment,
        config.llm_provider.as_str(),
        config.active_model(),
        config.cors_allow_origins(),
        config.opener_apps_dir.display(),
        addr,
    );

    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("failed to bind {addr}: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await {
        tracing::error!("server error: {e}");
        std::process::exit(1);
    }
}

fn build_cors(config: &Config) -> CorsLayer {
    let origins: Vec<HeaderValue> = config
        .cors_allow_origins()
        .iter()
        .filter_map(|o| o.parse::<HeaderValue>().ok())
        .collect();

    CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            header::ACCEPT,
        ])
        .allow_credentials(true)
        .expose_headers([HeaderName::from_static("x-request-id")])
        .max_age(std::time::Duration::from_secs(3600))
}

fn level_filter(log_level: &str) -> String {
    // Map the Python LOG_LEVEL to a tracing filter that also quiets noisy deps.
    let lvl = log_level.to_lowercase();
    format!("micracode_api={lvl},tower_http=warn")
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}

//! `/v1/terminals` — PTY-backed terminal sessions (PRD FR7).
//!
//! Start a terminal bound to a project (or explicit workspace), write input,
//! resize it, and stream its output over SSE. Output frames carry the chunk's
//! `seq` and its bytes base64-encoded (terminal output is raw bytes that may
//! split a UTF-8 sequence, so it isn't safe to send as text). A new subscriber
//! replays the scrollback first, then follows live — deduped by `seq`, the same
//! gap-free pattern `/v1/events/stream` uses.

use std::convert::Infallible;
use std::path::PathBuf;

use axum::extract::{Path, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use core_terminal::{TerminalOptions, TerminalOutput};
use futures::stream::{self, Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_stream::wrappers::BroadcastStream;

use super::AppState;
use crate::error::ApiError;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/terminals", get(list_terminals).post(start_terminal))
        .route("/terminals/{terminal_id}/input", post(write_input))
        .route("/terminals/{terminal_id}/resize", post(resize_terminal))
        .route("/terminals/{terminal_id}/stream", get(stream_output))
        .route(
            "/terminals/{terminal_id}",
            axum::routing::delete(kill_terminal),
        )
}

#[derive(Debug, Deserialize)]
pub struct StartTerminalRequest {
    /// Bind the terminal to an existing project's workspace.
    #[serde(default)]
    pub project_id: Option<String>,
    /// Or an explicit workspace path (ignored when `project_id` is set).
    #[serde(default)]
    pub workspace: Option<String>,
    /// Program to run. Omit for the user's default shell.
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cols: Option<u16>,
    #[serde(default)]
    pub rows: Option<u16>,
}

#[derive(Debug, Serialize)]
pub struct StartTerminalResponse {
    pub terminal_id: String,
}

#[derive(Debug, Deserialize)]
pub struct InputRequest {
    /// Bytes to write to the terminal's input, as a UTF-8 string.
    pub data: String,
}

#[derive(Debug, Deserialize)]
pub struct ResizeRequest {
    pub cols: u16,
    pub rows: u16,
}

type ApiResult<T> = Result<Json<T>, ApiError>;

/// Resolve the workspace for a new terminal (project dir or explicit path).
fn resolve_workspace(state: &AppState, req: &StartTerminalRequest) -> Result<PathBuf, ApiError> {
    if let Some(project_id) = &req.project_id {
        let dir = state.storage.project_dir(project_id)?;
        if !dir.exists() {
            return Err(ApiError::NotFound("project not found".to_string()));
        }
        return Ok(dir);
    }
    if let Some(workspace) = &req.workspace {
        return Ok(PathBuf::from(workspace));
    }
    Ok(state.config.opener_apps_dir.clone())
}

async fn start_terminal(
    State(state): State<AppState>,
    Json(req): Json<StartTerminalRequest>,
) -> ApiResult<StartTerminalResponse> {
    let workspace = resolve_workspace(&state, &req)?;
    let opts = TerminalOptions {
        workspace,
        command: req.command.clone(),
        args: req.args.clone(),
        env: Vec::new(),
        cols: req.cols.unwrap_or(80),
        rows: req.rows.unwrap_or(24),
    };
    let terminal_id = state
        .terminals
        .start(opts)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(StartTerminalResponse { terminal_id }))
}

async fn list_terminals(State(state): State<AppState>) -> Json<Vec<String>> {
    Json(state.terminals.list())
}

async fn write_input(
    State(state): State<AppState>,
    Path(terminal_id): Path<String>,
    Json(req): Json<InputRequest>,
) -> ApiResult<Value> {
    let terminal = state
        .terminals
        .get(&terminal_id)
        .ok_or_else(|| ApiError::NotFound("terminal not found".to_string()))?;
    terminal
        .write(req.data.as_bytes())
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(json!({ "accepted": true })))
}

async fn resize_terminal(
    State(state): State<AppState>,
    Path(terminal_id): Path<String>,
    Json(req): Json<ResizeRequest>,
) -> ApiResult<Value> {
    let terminal = state
        .terminals
        .get(&terminal_id)
        .ok_or_else(|| ApiError::NotFound("terminal not found".to_string()))?;
    terminal
        .resize(req.cols, req.rows)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(json!({ "resized": true })))
}

async fn kill_terminal(
    State(state): State<AppState>,
    Path(terminal_id): Path<String>,
) -> ApiResult<Value> {
    if !state.terminals.kill(&terminal_id) {
        return Err(ApiError::NotFound("terminal not found".to_string()));
    }
    Ok(Json(json!({ "killed": true })))
}

/// Encode one output chunk as an SSE event: `{ "seq": N, "data": "<base64>" }`.
fn output_event(chunk: TerminalOutput) -> Event {
    let data = base64::engine::general_purpose::STANDARD.encode(&chunk.bytes);
    Event::default()
        .json_data(json!({ "seq": chunk.seq, "data": data }))
        .unwrap_or_else(|_| Event::default())
}

async fn stream_output(
    State(state): State<AppState>,
    Path(terminal_id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let terminal = state
        .terminals
        .get(&terminal_id)
        .ok_or_else(|| ApiError::NotFound("terminal not found".to_string()))?;

    // Subscribe before snapshotting scrollback so nothing produced in between
    // is lost; the seq filter below drops anything the replay already covered.
    let live = BroadcastStream::new(terminal.subscribe());
    let backlog = terminal.scrollback();
    let last_replayed = backlog.last().map(|c| c.seq);

    let replay = stream::iter(backlog).map(|chunk| Ok(output_event(chunk)));
    let live = live.filter_map(move |res| async move {
        let chunk = res.ok()?; // drop lag errors; the client resyncs via scrollback
        if let Some(last) = last_replayed {
            if chunk.seq <= last {
                return None; // already delivered in the replay
            }
        }
        Some(Ok(output_event(chunk)))
    });

    Ok(Sse::new(replay.chain(live)).keep_alive(KeepAlive::default()))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::preview::PreviewManager;
    use crate::projection::ProjectionHandle;
    use crate::provider::ProviderManager;
    use crate::storage::Storage;
    use crate::terminal::TerminalManager;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use core_orchestration::{Command, Engine};
    use core_persistence::EventStore;
    use core_provider::CodexDriver;
    use std::sync::Arc;
    use std::time::Duration;
    use tower::ServiceExt;

    fn test_app(root: &std::path::Path) -> Router {
        let engine = Arc::new(Engine::new(
            EventStore::open_in_memory().unwrap(),
            |_: &Command| Ok(Vec::new()),
        ));
        let state = AppState {
            storage: Arc::new(Storage::new(root.to_path_buf())),
            config: Arc::new(Config::from_env()),
            engine: Arc::clone(&engine),
            provider: Arc::new(ProviderManager::new(
                CodexDriver::with_program("codex"),
                Arc::clone(&engine),
            )),
            projection: ProjectionHandle::spawn(engine),
            terminals: Arc::new(TerminalManager::new()),
            previews: Arc::new(PreviewManager::new()),
        };
        super::router().with_state(state)
    }

    async fn json_body(resp: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// Read the SSE body until a base64-decoded frame contains `needle`, or the
    /// timeout elapses. Returns the concatenated decoded output seen.
    async fn read_stream_until(resp: axum::response::Response, needle: &str) -> String {
        let mut body = resp.into_body().into_data_stream();
        let mut acc = String::new();
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(chunk) = body.next().await {
                let Ok(chunk) = chunk else { break };
                let text = String::from_utf8_lossy(&chunk);
                // SSE frames look like `data: {"seq":N,"data":"<base64>"}`.
                for line in text.lines() {
                    let Some(payload) = line.strip_prefix("data:") else {
                        continue;
                    };
                    if let Ok(v) = serde_json::from_str::<Value>(payload.trim()) {
                        if let Some(b64) = v.get("data").and_then(Value::as_str) {
                            if let Ok(bytes) =
                                base64::engine::general_purpose::STANDARD.decode(b64)
                            {
                                acc.push_str(&String::from_utf8_lossy(&bytes));
                            }
                        }
                    }
                }
                if acc.contains(needle) {
                    break;
                }
            }
        })
        .await;
        acc
    }

    #[tokio::test]
    async fn start_stream_input_resize_and_kill() {
        let dir = tempfile::tempdir().unwrap();
        let app = test_app(dir.path());

        // Start a terminal in an explicit workspace.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/terminals")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "workspace": dir.path(), "command": "/bin/sh", "args": ["-i"] })
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let id = json_body(resp).await["terminal_id"].as_str().unwrap().to_string();

        // It appears in the list.
        let resp = app
            .clone()
            .oneshot(Request::builder().uri("/terminals").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let list = json_body(resp).await;
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert_eq!(list[0], id);

        // Open the output stream, then write a command; its echo/output shows up.
        let stream_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/terminals/{id}/stream"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(stream_resp.status(), StatusCode::OK);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/terminals/{id}/input"))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "data": "echo MICRA_HTTP\n" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let seen = read_stream_until(stream_resp, "MICRA_HTTP").await;
        assert!(seen.contains("MICRA_HTTP"), "stream output: {seen:?}");

        // Resize succeeds.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/terminals/{id}/resize"))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "cols": 120, "rows": 40 }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Kill it, then it's gone (404 on a second kill).
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/terminals/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/terminals/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn unknown_terminal_is_404() {
        let dir = tempfile::tempdir().unwrap();
        let app = test_app(dir.path());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/terminals/nope/stream")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}

//! `/v1/projects/{id}/preview` — dev-server preview manager (PRD FR7).
//!
//! `POST` starts (or restarts) the project's dev server and scans for the port
//! it binds; `GET` reports the live [`PreviewStatus`](core_terminal::PreviewStatus)
//! (`starting` / `running` + url / `stopped` / `failed`); `DELETE` stops it.
//! One preview per project, owned by [`PreviewManager`](crate::preview).

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use core_terminal::{PreviewOptions, PreviewStatus};
use serde::Deserialize;
use serde_json::{json, Value};

use super::AppState;
use crate::error::ApiError;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/projects/{project_id}/preview",
        get(get_preview).post(start_preview).delete(stop_preview),
    )
}

#[derive(Debug, Default, Deserialize)]
pub struct StartPreviewRequest {
    /// Program to run. Defaults to `npm`.
    #[serde(default)]
    pub command: Option<String>,
    /// Arguments. Defaults to `["run", "dev"]`.
    #[serde(default)]
    pub args: Option<Vec<String>>,
    /// Candidate ports to scan. Defaults to 3000–3009.
    #[serde(default)]
    pub ports: Option<Vec<u16>>,
}

type ApiResult<T> = Result<Json<T>, ApiError>;

/// Resolve a project's workspace directory, 404-ing if it doesn't exist.
fn workspace_path(state: &AppState, project_id: &str) -> Result<std::path::PathBuf, ApiError> {
    let dir = state.storage.project_dir(project_id)?;
    if !dir.exists() {
        return Err(ApiError::NotFound("project not found".to_string()));
    }
    Ok(dir)
}

async fn start_preview(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    body: Option<Json<StartPreviewRequest>>,
) -> ApiResult<PreviewStatus> {
    let dir = workspace_path(&state, &project_id)?;
    let req = body.map(|Json(b)| b).unwrap_or_default();

    let mut opts = PreviewOptions::npm_dev(dir);
    if let Some(command) = req.command {
        opts.command = command;
    }
    if let Some(args) = req.args {
        opts.args = args;
    }
    if let Some(ports) = req.ports {
        if !ports.is_empty() {
            opts.ports = ports;
        }
    }

    let status = state
        .previews
        .start(project_id, opts)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(status))
}

async fn get_preview(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> ApiResult<PreviewStatus> {
    // Validate the project exists so an unknown id is a 404, not "no preview".
    workspace_path(&state, &project_id)?;
    state
        .previews
        .status(&project_id)
        .map(Json)
        .ok_or_else(|| ApiError::NotFound("no preview running for project".to_string()))
}

async fn stop_preview(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> ApiResult<Value> {
    if !state.previews.stop(&project_id) {
        return Err(ApiError::NotFound("no preview running for project".to_string()));
    }
    Ok(Json(json!({ "stopped": true })))
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
    use tokio::net::TcpListener;
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

    #[tokio::test]
    async fn start_get_and_stop_a_preview() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::new(root.path().to_path_buf());
        storage.ensure_root().unwrap();
        let project_id = storage.create_project("demo", "blank").unwrap().id;
        let app = test_app(root.path());

        // Stand in for "the dev server is up" by binding a port ourselves and
        // pointing the scan at it; the spawned command just stays alive.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/projects/{project_id}/preview"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "command": "/bin/sh", "args": ["-c", "sleep 30"], "ports": [port] })
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Poll GET until the scan reports the preview running on our port.
        let running = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let resp = app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .uri(format!("/projects/{project_id}/preview"))
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let body = json_body(resp).await;
                if body["state"] == "running" {
                    return body;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("preview became running");
        assert_eq!(running["port"], port);
        assert_eq!(running["url"], format!("http://localhost:{port}"));

        // Stop it; a second stop is a 404.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/projects/{project_id}/preview"))
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
                    .uri(format!("/projects/{project_id}/preview"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn preview_for_unknown_project_is_404() {
        let root = tempfile::tempdir().unwrap();
        let app = test_app(root.path());
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/projects/nope/preview")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}

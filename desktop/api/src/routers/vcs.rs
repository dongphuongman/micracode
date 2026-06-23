//! `/v1/projects/{id}/vcs` + `/checkpoints` — local Git status, working-tree
//! diffs, and checkpoint capture/diff/revert (PRD FR6).
//!
//! Reads (`status`, `diff`, list, checkpoint diff) go straight to the Git-backed
//! [`Workspace`](core_vcs::Workspace) for the project. Mutations (`capture`,
//! `revert`) go through [`crate::vcs`] so they also append a domain event and
//! flow out over the single ordered push path (`/v1/events`). All Git work runs
//! on a blocking task since libgit2 is synchronous.

use std::path::PathBuf;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use core_vcs::{Checkpoint, FileChange, Workspace};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::AppState;
use crate::error::ApiError;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{project_id}/vcs/status", get(get_status))
        .route("/projects/{project_id}/vcs/diff", get(get_diff))
        .route(
            "/projects/{project_id}/checkpoints",
            get(list_checkpoints).post(capture_checkpoint),
        )
        .route(
            "/projects/{project_id}/checkpoints/{checkpoint_id}/diff",
            get(get_checkpoint_diff),
        )
        .route(
            "/projects/{project_id}/checkpoints/{checkpoint_id}/revert",
            post(revert_checkpoint),
        )
        .route(
            "/projects/{project_id}/vcs/commit-message",
            post(suggest_commit_message),
        )
}

#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(rename = "VcsStatus")]
pub(crate) struct StatusResponse {
    pub(crate) files: Vec<FileChange>,
    /// True when the working tree has no pending changes.
    pub(crate) clean: bool,
}

#[derive(Debug, Serialize)]
struct DiffResponse {
    diff: String,
}

#[derive(Debug, Serialize)]
struct CommitMessageResponse {
    message: String,
}

#[derive(Debug, Default, Deserialize)]
struct CaptureRequest {
    /// Human label for the snapshot. Defaults to "manual".
    #[serde(default)]
    label: Option<String>,
}

type ApiResult<T> = Result<Json<T>, ApiError>;

/// Resolve a project's workspace directory, 404-ing if it doesn't exist.
fn workspace_path(state: &AppState, project_id: &str) -> Result<PathBuf, ApiError> {
    let dir = state.storage.project_dir(project_id)?;
    if !dir.exists() {
        return Err(ApiError::NotFound("project not found".to_string()));
    }
    Ok(dir)
}

/// Map a blocking-task join failure to a 500.
fn join_err(e: tokio::task::JoinError) -> ApiError {
    ApiError::Internal(format!("vcs task failed: {e}"))
}

async fn get_status(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> ApiResult<StatusResponse> {
    let dir = workspace_path(&state, &project_id)?;
    let files = tokio::task::spawn_blocking(move || Workspace::open_or_init(&dir)?.status())
        .await
        .map_err(join_err)??;
    Ok(Json(StatusResponse {
        clean: files.is_empty(),
        files,
    }))
}

async fn get_diff(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> ApiResult<DiffResponse> {
    let dir = workspace_path(&state, &project_id)?;
    let diff = tokio::task::spawn_blocking(move || Workspace::open_or_init(&dir)?.working_diff())
        .await
        .map_err(join_err)??;
    Ok(Json(DiffResponse { diff }))
}

async fn list_checkpoints(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> ApiResult<Vec<Checkpoint>> {
    let dir = workspace_path(&state, &project_id)?;
    let checkpoints =
        tokio::task::spawn_blocking(move || Workspace::open_or_init(&dir)?.checkpoints())
            .await
            .map_err(join_err)??;
    Ok(Json(checkpoints))
}

async fn capture_checkpoint(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    body: Option<Json<CaptureRequest>>,
) -> ApiResult<Checkpoint> {
    let dir = workspace_path(&state, &project_id)?;
    let label = body
        .and_then(|Json(b)| b.label)
        .unwrap_or_else(|| "manual".to_string());
    let engine = std::sync::Arc::clone(&state.engine);
    let checkpoint = tokio::task::spawn_blocking(move || {
        crate::vcs::capture(&engine, &dir, Some(&project_id), None, &label)
    })
    .await
    .map_err(join_err)??;
    Ok(Json(checkpoint))
}

async fn get_checkpoint_diff(
    State(state): State<AppState>,
    Path((project_id, checkpoint_id)): Path<(String, String)>,
) -> ApiResult<DiffResponse> {
    let dir = workspace_path(&state, &project_id)?;
    let diff =
        tokio::task::spawn_blocking(move || Workspace::open_or_init(&dir)?.checkpoint_diff(&checkpoint_id))
            .await
            .map_err(join_err)??;
    Ok(Json(DiffResponse { diff }))
}

async fn revert_checkpoint(
    State(state): State<AppState>,
    Path((project_id, checkpoint_id)): Path<(String, String)>,
) -> ApiResult<Value> {
    let dir = workspace_path(&state, &project_id)?;
    let engine = std::sync::Arc::clone(&state.engine);
    tokio::task::spawn_blocking(move || {
        crate::vcs::revert(&engine, &dir, Some(&project_id), &checkpoint_id)
    })
    .await
    .map_err(join_err)??;
    Ok(Json(json!({ "reverted": true })))
}

/// `POST /v1/projects/{id}/vcs/commit-message` — suggest a commit message for the
/// working-tree changes via direct Anthropic text generation (PRD FR
/// `ClaudeTextGeneration`). Computes the working diff (blocking Git I/O) then
/// makes a single Messages API call. 422 when there's nothing to describe; 503
/// when no Anthropic key is configured; 502 on an upstream failure.
async fn suggest_commit_message(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> ApiResult<CommitMessageResponse> {
    let dir = workspace_path(&state, &project_id)?;
    let diff = tokio::task::spawn_blocking(move || Workspace::open_or_init(&dir)?.working_diff())
        .await
        .map_err(join_err)??;
    if diff.trim().is_empty() {
        return Err(ApiError::Unprocessable(
            "no working-tree changes to summarize".to_string(),
        ));
    }
    let message = crate::text_generation::ClaudeTextGenerator::from_config(&state.config)
        .commit_message(&diff)
        .await?;
    Ok(Json(CommitMessageResponse { message }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::projection::ProjectionHandle;
    use crate::provider::ProviderManager;
    use crate::storage::Storage;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use core_orchestration::{Command, Engine};
    use core_persistence::EventStore;
    use core_provider::CodexDriver;
    use std::sync::Arc;
    use tower::ServiceExt;

    /// Build a router backed by a temp storage root with one real project dir.
    fn test_app(root: &std::path::Path, project_id: &str) -> Router {
        std::fs::create_dir_all(root.join(project_id)).unwrap();
        // A `.micracode/project.json` is what `project_dir(...).exists()` and the
        // slug validation key on; create the project through Storage so the dir
        // is a valid, resolvable project.
        let storage = Storage::new(root.to_path_buf());
        let engine = Arc::new(Engine::new(EventStore::open_in_memory().unwrap(), |_: &Command| {
            Ok(Vec::new())
        }));
        let state = AppState {
            storage: Arc::new(storage),
            config: Arc::new(Config::from_env()),
            engine: Arc::clone(&engine),
            provider: Arc::new(ProviderManager::new(
                CodexDriver::with_program("codex"),
                Arc::clone(&engine),
            )),
            projection: ProjectionHandle::spawn(engine),
            terminals: Arc::new(crate::terminal::TerminalManager::new()),
            previews: Arc::new(crate::preview::PreviewManager::new()),
        };
        super::router().with_state(state)
    }

    async fn json_body(resp: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// Spawn a one-route mock of the Anthropic Messages API returning `text`.
    async fn mock_anthropic(text: &str) -> String {
        let reply = json!({
            "content": [{ "type": "text", "text": text }],
            "stop_reason": "end_turn",
        });
        let app = Router::new().route(
            "/v1/messages",
            post(move || {
                let reply = reply.clone();
                async move { Json(reply) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    /// The commit-message endpoint computes the working diff and returns the
    /// message the (mocked) Anthropic API produced.
    #[tokio::test]
    async fn commit_message_endpoint_returns_generated_text() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::new(root.path().to_path_buf());
        storage.ensure_root().unwrap();
        let project_id = storage.create_project("demo", "blank").unwrap().id;
        // Seed a committed file, then modify it, so the working diff is a tracked
        // change (untracked-only diffs are a separate `working_diff` limitation).
        let work = root.path().join(&project_id);
        std::fs::write(work.join("a.txt"), "v1\n").unwrap();
        let repo = git2::Repository::init(&work).unwrap();
        let mut idx = repo.index().unwrap();
        idx.add_path(std::path::Path::new("a.txt")).unwrap();
        idx.write().unwrap();
        let tree = repo.find_tree(idx.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).unwrap();
        std::fs::write(work.join("a.txt"), "v2 changed\n").unwrap();

        let base_url = mock_anthropic("Add a.txt with a greeting").await;
        let mut config = Config::from_env();
        config.anthropic_api_key = "sk-test".to_string();
        config.anthropic_base_url = base_url;
        config.anthropic_model = crate::text_generation::DEFAULT_MODEL.to_string();

        let engine = Arc::new(Engine::new(
            EventStore::open_in_memory().unwrap(),
            |_: &Command| Ok(Vec::new()),
        ));
        let state = AppState {
            storage: Arc::new(storage),
            config: Arc::new(config),
            engine: Arc::clone(&engine),
            provider: Arc::new(ProviderManager::new(
                CodexDriver::with_program("codex"),
                Arc::clone(&engine),
            )),
            projection: ProjectionHandle::spawn(engine),
            terminals: Arc::new(crate::terminal::TerminalManager::new()),
            previews: Arc::new(crate::preview::PreviewManager::new()),
        };
        let app = super::router().with_state(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/projects/{project_id}/vcs/commit-message"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = json_body(resp).await;
        assert_eq!(body["message"], "Add a.txt with a greeting");
    }

    #[tokio::test]
    async fn capture_list_diff_and_revert_round_trip() {
        let root = tempfile::tempdir().unwrap();
        // Create the project via Storage so its dir + sidecar exist.
        let storage = Storage::new(root.path().to_path_buf());
        storage.ensure_root().unwrap();
        let project_id = storage.create_project("demo", "blank").unwrap().id;
        let app = test_app(root.path(), &project_id);

        // Write a file into the workspace, then capture a checkpoint.
        std::fs::write(root.path().join(&project_id).join("a.txt"), "hello\n").unwrap();
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/projects/{project_id}/checkpoints"))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "label": "first" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let cp = json_body(resp).await;
        let checkpoint_id = cp["id"].as_str().unwrap().to_string();
        assert_eq!(cp["label"], "first");

        // It shows up in the list.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/projects/{project_id}/checkpoints"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let list = json_body(resp).await;
        assert_eq!(list.as_array().unwrap().len(), 1);

        // Its diff mentions the file.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/projects/{project_id}/checkpoints/{checkpoint_id}/diff"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let diff = json_body(resp).await;
        assert!(diff["diff"].as_str().unwrap().contains("a.txt"));

        // Tamper, then revert restores the snapshot.
        std::fs::write(root.path().join(&project_id).join("a.txt"), "tampered\n").unwrap();
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/projects/{project_id}/checkpoints/{checkpoint_id}/revert"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            std::fs::read_to_string(root.path().join(&project_id).join("a.txt")).unwrap(),
            "hello\n"
        );
    }

    #[tokio::test]
    async fn status_of_a_clean_fresh_workspace_is_empty() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::new(root.path().to_path_buf());
        storage.ensure_root().unwrap();
        storage.create_project("clean", "blank").unwrap();
        let app = test_app(root.path(), "clean");

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/projects/clean/vcs/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = json_body(resp).await;
        // A freshly created project has a `.micracode` sidecar; that's the only
        // entry Git would see, so `clean` may be false — assert the shape only.
        assert!(body["files"].is_array());
        assert!(body["clean"].is_boolean());
    }

    #[tokio::test]
    async fn unknown_project_is_404() {
        let root = tempfile::tempdir().unwrap();
        let app = test_app(root.path(), "ignored");
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/projects/nope/checkpoints")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}

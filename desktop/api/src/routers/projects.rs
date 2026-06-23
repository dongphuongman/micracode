//! Project CRUD + hydration endpoints — a faithful port of `routers/projects.py`.

use std::io::Write;

use axum::extract::{Path, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::{json, Value};

use super::AppState;
use crate::error::ApiError;
use crate::schemas::{
    CreateProjectRequest, ProjectRecord, ProjectWithRootPath, PromptRecord, SnapshotRecord,
    UpdateProjectFileRequest,
};
use crate::storage::Storage;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects", get(list_projects).post(create_project))
        .route(
            "/projects/{project_id}",
            get(get_project).delete(delete_project),
        )
        .route(
            "/projects/{project_id}/files",
            get(get_project_files).put(put_project_file),
        )
        .route("/projects/{project_id}/download", get(download_project_zip))
        .route("/projects/{project_id}/prompts", get(get_project_prompts))
        .route(
            "/projects/{project_id}/prompts/pop-assistant",
            post(pop_last_assistant_prompt),
        )
        .route(
            "/projects/{project_id}/snapshots",
            get(list_project_snapshots),
        )
        .route(
            "/projects/{project_id}/snapshots/{snapshot_id}/restore",
            post(restore_project_snapshot),
        )
        .route(
            "/projects/{project_id}/snapshots/{snapshot_id}",
            axum::routing::delete(delete_project_snapshot),
        )
}

/// Mirror `_normalize_rel_path`.
fn normalize_rel_path(raw: &str) -> String {
    raw.replace('\\', "/")
        .trim()
        .trim_start_matches('/')
        .to_string()
}

/// Mirror `_reject_sidecar_path`.
fn reject_sidecar_path(rel: &str) -> Result<(), ApiError> {
    if rel == ".micracode" || rel.starts_with(".micracode/") {
        return Err(ApiError::BadRequest("cannot write under .micracode".to_string()));
    }
    Ok(())
}

/// Ensure a project exists, returning 404 otherwise (matches the explicit
/// `storage.get_project(...) is None` checks in the Python routes).
fn require_project(storage: &Storage, project_id: &str) -> Result<(), ApiError> {
    match storage.get_project(project_id)? {
        Some(_) => Ok(()),
        None => Err(ApiError::NotFound("project not found".to_string())),
    }
}

async fn list_projects(State(state): State<AppState>) -> Json<Vec<ProjectRecord>> {
    Json(state.storage.list_projects())
}

async fn create_project(
    State(state): State<AppState>,
    Json(body): Json<CreateProjectRequest>,
) -> Result<(StatusCode, Json<ProjectRecord>), ApiError> {
    let record = state.storage.create_project(&body.name, &body.template)?;
    Ok((StatusCode::CREATED, Json(record)))
}

async fn get_project(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<ProjectWithRootPath>, ApiError> {
    let record = state
        .storage
        .get_project(&project_id)?
        .ok_or_else(|| ApiError::NotFound("project not found".to_string()))?;
    let root_path = state
        .storage
        .project_dir(&project_id)?
        .to_string_lossy()
        .to_string();
    Ok(Json(ProjectWithRootPath { record, root_path }))
}

async fn delete_project(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if state.storage.delete_project(&project_id)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound("project not found".to_string()))
    }
}

async fn get_project_files(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    require_project(&state.storage, &project_id)?;
    state.storage.ensure_next_preview_layout(&project_id)?;
    let tree = state.storage.read_tree(&project_id)?;
    Ok(Json(json!({ "tree": tree })))
}

async fn put_project_file(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    Json(body): Json<UpdateProjectFileRequest>,
) -> Result<StatusCode, ApiError> {
    require_project(&state.storage, &project_id)?;
    let rel = normalize_rel_path(&body.path);
    if rel.is_empty() {
        return Err(ApiError::BadRequest("path is empty".to_string()));
    }
    reject_sidecar_path(&rel)?;
    state.storage.write_file(&project_id, &rel, &body.content)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn download_project_zip(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<(HeaderMap, Vec<u8>), ApiError> {
    require_project(&state.storage, &project_id)?;
    let files = state.storage.collect_files_for_zip(&project_id)?;

    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut cursor);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (abs, rel) in files {
            let arcname = format!("{project_id}/{rel}");
            let bytes = std::fs::read(&abs)
                .map_err(|e| ApiError::Internal(format!("failed to read {rel}: {e}")))?;
            zip.start_file(arcname, options)
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            zip.write_all(&bytes)
                .map_err(|e| ApiError::Internal(e.to_string()))?;
        }
        zip.finish().map_err(|e| ApiError::Internal(e.to_string()))?;
    }

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/zip"));
    let disposition = format!("attachment; filename=\"{project_id}.zip\"");
    headers.insert(
        CONTENT_DISPOSITION,
        HeaderValue::from_str(&disposition).map_err(|e| ApiError::Internal(e.to_string()))?,
    );
    Ok((headers, cursor.into_inner()))
}

async fn get_project_prompts(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<Vec<PromptRecord>>, ApiError> {
    require_project(&state.storage, &project_id)?;
    Ok(Json(state.storage.read_prompts(&project_id)?))
}

#[derive(Serialize)]
struct PoppedResponse {
    popped: bool,
}

async fn pop_last_assistant_prompt(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<PoppedResponse>, ApiError> {
    require_project(&state.storage, &project_id)?;
    let dropped = state.storage.pop_last_assistant_prompt(&project_id)?;
    Ok(Json(PoppedResponse {
        popped: dropped.is_some(),
    }))
}

async fn list_project_snapshots(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<Vec<SnapshotRecord>>, ApiError> {
    require_project(&state.storage, &project_id)?;
    Ok(Json(state.storage.list_snapshots(&project_id)?))
}

async fn restore_project_snapshot(
    State(state): State<AppState>,
    Path((project_id, snapshot_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    require_project(&state.storage, &project_id)?;
    if state.storage.restore_snapshot(&project_id, &snapshot_id)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound("snapshot not found".to_string()))
    }
}

async fn delete_project_snapshot(
    State(state): State<AppState>,
    Path((project_id, snapshot_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    require_project(&state.storage, &project_id)?;
    if state.storage.delete_snapshot(&project_id, &snapshot_id)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound("snapshot not found".to_string()))
    }
}

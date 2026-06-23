//! Wire schemas, mirroring `micracode_core.schemas.project`. Field names match
//! the Python/Pydantic models so the JSON is byte-compatible with the TS client
//! types in `apps/web/src/lib/api/projects.ts`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A persisted project (`.micracode/project.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRecord {
    pub id: String,
    pub name: String,
    #[serde(default = "default_template")]
    pub template: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn default_template() -> String {
    "next".to_string()
}

/// `ProjectRecord` extended with the on-disk root path (for desktop clients).
#[derive(Debug, Clone, Serialize)]
pub struct ProjectWithRootPath {
    #[serde(flatten)]
    pub record: ProjectRecord,
    pub root_path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateProjectRequest {
    pub name: String,
    #[serde(default = "default_template")]
    pub template: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateProjectFileRequest {
    pub path: String,
    #[serde(default)]
    pub content: String,
}

pub type PromptRole = String;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptRecord {
    pub id: String,
    pub role: PromptRole,
    pub content: String,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotRecord {
    pub id: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub user_prompt: String,
    #[serde(default = "default_kind")]
    pub kind: String,
}

fn default_kind() -> String {
    "pre-turn".to_string()
}

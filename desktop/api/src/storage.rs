//! Local-filesystem storage for projects, generated code, and chat history.
//!
//! A faithful Rust port of `micracode_core.storage.Storage`. The on-disk layout
//! (`.micracode/project.json`, `prompts.jsonl`, `snapshots/<id>/files/`) is
//! identical, so this server and the Python service can share a data directory.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::schemas::{ProjectRecord, PromptRecord, SnapshotRecord};
use crate::starter::{starter_file, NEXT_STARTER_FILES};

pub static SLUG_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[a-z0-9][a-z0-9-]{0,62}$").unwrap());
pub static SNAPSHOT_ID_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[0-9]{8}T[0-9]{6}Z-[0-9a-f]{4}$").unwrap());
static NON_SLUG_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"[^a-z0-9]+").unwrap());

const SIDECAR_DIR: &str = ".micracode";
const PROJECT_FILE: &str = "project.json";
const PROMPTS_FILE: &str = "prompts.jsonl";
const SNAPSHOTS_DIR: &str = "snapshots";
const SNAPSHOT_FILES_DIR: &str = "files";
const SNAPSHOT_META_FILE: &str = "project.json";
#[allow(dead_code)] // used by create_snapshot (pending /generate port)
const SNAPSHOT_KEEP: usize = 20;
const DEV_SCRIPT: &str = "next dev --hostname 0.0.0.0 --port 3000";

pub const IGNORED_TOP_LEVEL: &[&str] = &[
    SIDECAR_DIR,
    "node_modules",
    ".git",
    ".next",
    ".turbo",
    "dist",
    ".cache",
];

fn is_ignored(name: &str) -> bool {
    IGNORED_TOP_LEVEL.contains(&name)
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid project id: {0}")]
    InvalidSlug(String),
    #[error("invalid snapshot id: {0}")]
    InvalidSnapshotId(String),
    #[error("{0}")]
    PathEscape(String),
    #[error("{0}")]
    Validation(String),
    #[error("not found")]
    NotFound,
}

type Result<T> = std::result::Result<T, StorageError>;

fn now() -> DateTime<Utc> {
    Utc::now()
}

pub fn slugify(name: &str) -> String {
    let cleaned = name.trim().to_lowercase();
    let cleaned = NON_SLUG_RE.replace_all(&cleaned, "-");
    let cleaned = cleaned.trim_matches('-');
    let cleaned: String = cleaned.chars().take(63).collect();
    match cleaned.chars().next() {
        Some(c) if c.is_ascii_alphanumeric() => cleaned,
        _ => String::new(),
    }
}

/// Resolve `rel` against a root, blocking absolute paths and `..` traversal.
fn normalize_rel(rel: &str) -> Result<PathBuf> {
    let p = Path::new(rel);
    let mut out = PathBuf::new();
    let mut depth = 0usize;
    for comp in p.components() {
        match comp {
            Component::Normal(c) => {
                out.push(c);
                depth += 1;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if depth == 0 {
                    return Err(StorageError::PathEscape(format!(
                        "path escapes project root: {rel:?}"
                    )));
                }
                out.pop();
                depth -= 1;
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(StorageError::PathEscape(format!(
                    "absolute paths are not allowed: {rel:?}"
                )));
            }
        }
    }
    Ok(out)
}

fn safe_join(root: &Path, rel: &str) -> Result<PathBuf> {
    Ok(root.join(normalize_rel(rel)?))
}

pub struct Storage {
    pub root: PathBuf,
    write_lock: Mutex<()>,
}

impl Storage {
    pub fn new(root: PathBuf) -> Self {
        // Best-effort canonicalisation; falls back to the raw path if it does
        // not exist yet (matching `Path.expanduser().resolve()` leniency).
        let root = root.canonicalize().unwrap_or(root);
        Storage {
            root,
            write_lock: Mutex::new(()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.write_lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn ensure_root(&self) -> Result<()> {
        fs::create_dir_all(&self.root)?;
        Ok(())
    }

    pub fn validate_slug(slug: &str) -> Result<()> {
        if SLUG_RE.is_match(slug) {
            Ok(())
        } else {
            Err(StorageError::InvalidSlug(slug.to_string()))
        }
    }

    pub fn project_dir(&self, slug: &str) -> Result<PathBuf> {
        Self::validate_slug(slug)?;
        Ok(self.root.join(slug))
    }

    fn sidecar_dir(&self, slug: &str) -> Result<PathBuf> {
        Ok(self.project_dir(slug)?.join(SIDECAR_DIR))
    }

    fn project_json_path(&self, slug: &str) -> Result<PathBuf> {
        Ok(self.sidecar_dir(slug)?.join(PROJECT_FILE))
    }

    pub fn unique_slug(&self, name: &str) -> String {
        let mut base = slugify(name);
        if base.is_empty() {
            base = format!("project-{}", &Uuid::new_v4().simple().to_string()[..8]);
        }
        let mut candidate = base.clone();
        let mut n = 2;
        while self.root.join(&candidate).exists() {
            let suffix = format!("-{n}");
            let keep = 63usize.saturating_sub(suffix.len());
            let head: String = base.chars().take(keep).collect();
            candidate = format!("{head}{suffix}");
            n += 1;
        }
        candidate
    }

    pub fn create_project(&self, name: &str, template: &str) -> Result<ProjectRecord> {
        self.ensure_root()?;
        let slug = self.unique_slug(name);
        let proj = self.root.join(&slug);
        let sidecar = proj.join(SIDECAR_DIR);
        fs::create_dir_all(&sidecar)?;

        let ts = now();
        let mut record = ProjectRecord {
            id: slug.clone(),
            name: name.trim().to_string(),
            template: template.to_string(),
            created_at: ts,
            updated_at: ts,
        };
        self.write_project_json(&slug, &record)?;
        // touch prompts.jsonl
        {
            let _g = self.lock();
            fs::write(sidecar.join(PROMPTS_FILE), b"")?;
        }

        if template == "next" {
            for (rel, content) in NEXT_STARTER_FILES {
                self.write_file(&slug, rel, content)?;
            }
            if let Some(refreshed) = self.try_read_project_json(&slug) {
                record = refreshed;
            }
        }

        Ok(record)
    }

    pub fn ensure_next_preview_layout(&self, slug: &str) -> Result<()> {
        let Some(rec) = self.try_read_project_json(slug) else {
            return Ok(());
        };
        if rec.template != "next" {
            return Ok(());
        }
        let proj = self.project_dir(slug)?;
        if !proj.exists() {
            return Ok(());
        }
        for (rel, content) in NEXT_STARTER_FILES {
            if safe_join(&proj, rel)?.is_file() {
                continue;
            }
            self.write_file(slug, rel, content)?;
        }
        self.ensure_package_json_dev_script(slug)?;
        self.ensure_starter_dependencies(slug)?;
        Ok(())
    }

    fn ensure_package_json_dev_script(&self, slug: &str) -> Result<()> {
        let pkg_path = safe_join(&self.project_dir(slug)?, "package.json")?;
        if !pkg_path.is_file() {
            return Ok(());
        }
        let Ok(raw) = fs::read_to_string(&pkg_path) else {
            return Ok(());
        };
        let Ok(Value::Object(mut data)) = serde_json::from_str::<Value>(&raw) else {
            return Ok(());
        };
        let scripts = match data.get_mut("scripts") {
            Some(Value::Object(s)) => s,
            _ => {
                data.insert("scripts".to_string(), Value::Object(Map::new()));
                data.get_mut("scripts").unwrap().as_object_mut().unwrap()
            }
        };
        if let Some(Value::String(dev)) = scripts.get("dev") {
            if !dev.trim().is_empty() {
                return Ok(());
            }
        }
        scripts.insert("dev".to_string(), Value::String(DEV_SCRIPT.to_string()));
        let out = serde_json::to_string_pretty(&Value::Object(data)).unwrap() + "\n";
        self.write_file(slug, "package.json", &out)?;
        Ok(())
    }

    fn ensure_starter_dependencies(&self, slug: &str) -> Result<()> {
        let pkg_path = safe_join(&self.project_dir(slug)?, "package.json")?;
        if !pkg_path.is_file() {
            return Ok(());
        }
        let Some(starter_raw) = starter_file("package.json") else {
            return Ok(());
        };
        let Ok(starter) = serde_json::from_str::<Value>(starter_raw) else {
            return Ok(());
        };
        let required_deps = starter.get("dependencies").and_then(Value::as_object);
        let required_dev = starter.get("devDependencies").and_then(Value::as_object);
        if required_deps.map_or(true, Map::is_empty) && required_dev.map_or(true, Map::is_empty) {
            return Ok(());
        }

        let Ok(raw) = fs::read_to_string(&pkg_path) else {
            return Ok(());
        };
        let Ok(Value::Object(mut data)) = serde_json::from_str::<Value>(&raw) else {
            return Ok(());
        };

        let mut changed = false;
        for (section, required) in [
            ("dependencies", required_deps),
            ("devDependencies", required_dev),
        ] {
            let Some(required) = required else { continue };
            if !matches!(data.get(section), Some(Value::Object(_))) {
                data.insert(section.to_string(), Value::Object(Map::new()));
            }
            let current = data.get_mut(section).unwrap().as_object_mut().unwrap();
            for (name, version) in required {
                if !current.contains_key(name) {
                    current.insert(name.clone(), version.clone());
                    changed = true;
                }
            }
        }

        if changed {
            let out = serde_json::to_string_pretty(&Value::Object(data)).unwrap() + "\n";
            self.write_file(slug, "package.json", &out)?;
        }
        Ok(())
    }

    pub fn list_projects(&self) -> Vec<ProjectRecord> {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut records: Vec<ProjectRecord> = Vec::new();
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if !ft.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if !SLUG_RE.is_match(&name) {
                continue;
            }
            if let Some(rec) = self.try_read_project_json(&name) {
                records.push(rec);
            }
        }
        records.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        records
    }

    pub fn get_project(&self, slug: &str) -> Result<Option<ProjectRecord>> {
        Self::validate_slug(slug)?;
        Ok(self.try_read_project_json(slug))
    }

    pub fn delete_project(&self, slug: &str) -> Result<bool> {
        Self::validate_slug(slug)?;
        let target = self.project_dir(slug)?;
        if !target.exists() {
            return Ok(false);
        }
        // Guard: refuse to delete anything outside the storage root.
        let resolved = target.canonicalize()?;
        if !resolved.starts_with(&self.root) {
            return Err(StorageError::Validation(
                "refusing to delete path outside storage root".to_string(),
            ));
        }
        fs::remove_dir_all(&resolved)?;
        Ok(true)
    }

    pub fn read_tree(&self, slug: &str) -> Result<Value> {
        let proj = self.project_dir(slug)?;
        if !proj.exists() {
            return Err(StorageError::NotFound);
        }
        Ok(Value::Object(walk_tree(&proj, true)))
    }

    pub fn write_file(&self, slug: &str, rel_path: &str, content: &str) -> Result<PathBuf> {
        let proj = self.project_dir(slug)?;
        if !proj.exists() {
            return Err(StorageError::NotFound);
        }
        let target = safe_join(&proj, rel_path)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        {
            let _g = self.lock();
            fs::write(&target, content)?;
        }
        self.touch_project(slug);
        Ok(target)
    }

    // --- prompts -----------------------------------------------------------

    pub fn read_prompts(&self, slug: &str) -> Result<Vec<PromptRecord>> {
        let path = self.sidecar_dir(slug)?.join(PROMPTS_FILE);
        let Ok(raw) = fs::read_to_string(&path) else {
            return Ok(Vec::new());
        };
        let mut records = Vec::new();
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(rec) = serde_json::from_str::<PromptRecord>(line) {
                records.push(rec);
            }
        }
        Ok(records)
    }

    pub fn pop_last_assistant_prompt(&self, slug: &str) -> Result<Option<PromptRecord>> {
        let path = self.sidecar_dir(slug)?.join(PROMPTS_FILE);
        let _g = self.lock();
        let Ok(raw) = fs::read_to_string(&path) else {
            return Ok(None);
        };
        let lines: Vec<&str> = raw.split_inclusive('\n').collect();
        let mut drop_idx: Option<usize> = None;
        let mut dropped: Option<PromptRecord> = None;
        for i in (0..lines.len()).rev() {
            let trimmed = lines[i].trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<PromptRecord>(trimmed) {
                Ok(rec) => {
                    if rec.role == "assistant" {
                        drop_idx = Some(i);
                        dropped = Some(rec);
                    }
                    break;
                }
                Err(_) => continue,
            }
        }

        let Some(idx) = drop_idx else {
            return Ok(None);
        };
        let remaining: String = lines
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != idx)
            .map(|(_, l)| *l)
            .collect();
        fs::write(&path, remaining)?;
        drop(_g);
        self.touch_project(slug);
        Ok(dropped)
    }

    // --- snapshots ---------------------------------------------------------

    fn snapshots_dir(&self, slug: &str) -> Result<PathBuf> {
        Ok(self.sidecar_dir(slug)?.join(SNAPSHOTS_DIR))
    }

    fn snapshot_dir(&self, slug: &str, snapshot_id: &str) -> Result<PathBuf> {
        if !SNAPSHOT_ID_RE.is_match(snapshot_id) {
            return Err(StorageError::InvalidSnapshotId(snapshot_id.to_string()));
        }
        Ok(self.snapshots_dir(slug)?.join(snapshot_id))
    }

    #[allow(dead_code)]
    fn new_snapshot_id(ts: DateTime<Utc>) -> String {
        let stamp = ts.format("%Y%m%dT%H%M%SZ");
        let suffix = &Uuid::new_v4().simple().to_string()[..4];
        format!("{stamp}-{suffix}")
    }

    /// Capture a pre-turn snapshot. Wired up by the (not-yet-ported)
    /// `/generate` endpoint; kept here so the storage layer is complete.
    #[allow(dead_code)]
    pub fn create_snapshot(&self, slug: &str, user_prompt: &str) -> Result<SnapshotRecord> {
        let proj = self.project_dir(slug)?;
        if !proj.exists() {
            return Err(StorageError::NotFound);
        }
        let created_at = now();
        let mut dest = None;
        for _ in 0..8 {
            let candidate = self.snapshot_dir(slug, &Self::new_snapshot_id(created_at))?;
            if !candidate.exists() {
                dest = Some(candidate);
                break;
            }
        }
        let dest = dest.ok_or_else(|| {
            StorageError::Validation("failed to allocate unique snapshot id".to_string())
        })?;
        let snapshot_id = dest.file_name().unwrap().to_string_lossy().to_string();

        let trimmed: String = user_prompt.chars().take(4000).collect();
        let record = SnapshotRecord {
            id: snapshot_id,
            created_at,
            user_prompt: trimmed,
            kind: "pre-turn".to_string(),
        };

        let files_dir = dest.join(SNAPSHOT_FILES_DIR);
        {
            let _g = self.lock();
            fs::create_dir_all(&files_dir)?;
            for entry in fs::read_dir(&proj)?.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if is_ignored(&name) {
                    continue;
                }
                let ft = entry.file_type()?;
                if ft.is_symlink() {
                    continue;
                }
                let target = files_dir.join(&name);
                if ft.is_dir() {
                    copy_dir_recursive(&entry.path(), &target, true)?;
                } else if ft.is_file() {
                    fs::copy(entry.path(), &target)?;
                }
            }
            let payload = serde_json::to_string_pretty(&record).unwrap();
            fs::write(dest.join(SNAPSHOT_META_FILE), payload)?;
        }

        self.prune_snapshots(slug);
        Ok(record)
    }

    pub fn list_snapshots(&self, slug: &str) -> Result<Vec<SnapshotRecord>> {
        let root = self.snapshots_dir(slug)?;
        let Ok(entries) = fs::read_dir(&root) else {
            return Ok(Vec::new());
        };
        let mut records = Vec::new();
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if !ft.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if !SNAPSHOT_ID_RE.is_match(&name) {
                continue;
            }
            let meta = entry.path().join(SNAPSHOT_META_FILE);
            if !meta.is_file() {
                continue;
            }
            if let Ok(raw) = fs::read_to_string(&meta) {
                if let Ok(rec) = serde_json::from_str::<SnapshotRecord>(&raw) {
                    records.push(rec);
                }
            }
        }
        records.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(records)
    }

    pub fn restore_snapshot(&self, slug: &str, snapshot_id: &str) -> Result<bool> {
        let proj = self.project_dir(slug)?;
        if !proj.exists() {
            return Err(StorageError::NotFound);
        }
        let snap_dir = self.snapshot_dir(slug, snapshot_id)?;
        let files_dir = snap_dir.join(SNAPSHOT_FILES_DIR);
        if !snap_dir.is_dir() || !files_dir.is_dir() {
            return Ok(false);
        }

        {
            let _g = self.lock();
            for entry in fs::read_dir(&proj)?.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if is_ignored(&name) {
                    continue;
                }
                let ft = entry.file_type()?;
                let path = entry.path();
                if ft.is_symlink() || ft.is_file() {
                    fs::remove_file(&path)?;
                } else if ft.is_dir() {
                    fs::remove_dir_all(&path)?;
                }
            }
            for entry in fs::read_dir(&files_dir)?.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if is_ignored(&name) {
                    continue;
                }
                let ft = entry.file_type()?;
                let target = proj.join(&name);
                if ft.is_dir() {
                    copy_dir_recursive(&entry.path(), &target, false)?;
                } else if ft.is_file() {
                    fs::copy(entry.path(), &target)?;
                }
            }
        }
        self.touch_project(slug);
        Ok(true)
    }

    pub fn delete_snapshot(&self, slug: &str, snapshot_id: &str) -> Result<bool> {
        let snap_dir = self.snapshot_dir(slug, snapshot_id)?;
        if !snap_dir.exists() {
            return Ok(false);
        }
        let resolved = snap_dir.canonicalize()?;
        let snaps_root = self.snapshots_dir(slug)?.canonicalize()?;
        if !resolved.starts_with(&snaps_root) {
            return Err(StorageError::Validation(
                "refusing to delete path outside snapshots root".to_string(),
            ));
        }
        let _g = self.lock();
        fs::remove_dir_all(&resolved)?;
        Ok(true)
    }

    #[allow(dead_code)]
    fn prune_snapshots(&self, slug: &str) {
        let Ok(records) = self.list_snapshots(slug) else {
            return;
        };
        if records.len() <= SNAPSHOT_KEEP {
            return;
        }
        for rec in &records[SNAPSHOT_KEEP..] {
            let _ = self.delete_snapshot(slug, &rec.id);
        }
    }

    // --- project.json helpers ----------------------------------------------

    fn write_project_json(&self, slug: &str, record: &ProjectRecord) -> Result<()> {
        let path = self.project_json_path(slug)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let payload = serde_json::to_string_pretty(record).unwrap();
        let _g = self.lock();
        fs::write(&path, payload)?;
        Ok(())
    }

    fn try_read_project_json(&self, slug: &str) -> Option<ProjectRecord> {
        let path = self.project_json_path(slug).ok()?;
        let raw = fs::read_to_string(&path).ok()?;
        serde_json::from_str(&raw).ok()
    }

    fn touch_project(&self, slug: &str) {
        if let Some(mut rec) = self.try_read_project_json(slug) {
            rec.updated_at = now();
            let _ = self.write_project_json(slug, &rec);
        }
    }

    // --- zip download ------------------------------------------------------

    /// Collect `(absolute_path, relative_path)` pairs for every file that
    /// should be included in a project download. Ignored top-level entries and
    /// symlinks are skipped (mirrors the Python `os.walk` logic).
    pub fn collect_files_for_zip(&self, slug: &str) -> Result<Vec<(PathBuf, String)>> {
        let proj = self.project_dir(slug)?;
        let mut out = Vec::new();
        collect_zip(&proj, "", true, &mut out)?;
        Ok(out)
    }
}

fn walk_tree(dir: &Path, is_root: bool) -> Map<String, Value> {
    let mut tree = Map::new();
    let Ok(read) = fs::read_dir(dir) else {
        return tree;
    };
    let mut entries: Vec<_> = read.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        if is_root && is_ignored(&name) {
            continue;
        }
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            let mut node = Map::new();
            node.insert(
                "directory".to_string(),
                Value::Object(walk_tree(&entry.path(), false)),
            );
            tree.insert(name, Value::Object(node));
        } else if ft.is_file() {
            match fs::read(entry.path()) {
                Ok(bytes) => match String::from_utf8(bytes) {
                    Ok(contents) => {
                        let mut file = Map::new();
                        let mut inner = Map::new();
                        inner.insert("contents".to_string(), Value::String(contents));
                        file.insert("file".to_string(), Value::Object(inner));
                        tree.insert(name, Value::Object(file));
                    }
                    Err(_) => continue, // non-UTF8: skip, as Python does
                },
                Err(_) => continue,
            }
        }
    }
    tree
}

fn collect_zip(
    dir: &Path,
    rel_prefix: &str,
    is_root: bool,
    out: &mut Vec<(PathBuf, String)>,
) -> std::result::Result<(), StorageError> {
    let read = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return Ok(()),
    };
    let mut entries: Vec<_> = read.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        if is_root && is_ignored(&name) {
            continue;
        }
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        let rel = if rel_prefix.is_empty() {
            name.clone()
        } else {
            format!("{rel_prefix}/{name}")
        };
        if ft.is_dir() {
            collect_zip(&entry.path(), &rel, false, out)?;
        } else if ft.is_file() {
            out.push((entry.path(), rel));
        }
    }
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path, skip_ignored: bool) -> std::result::Result<(), StorageError> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if skip_ignored && is_ignored(&name) {
            continue;
        }
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            continue;
        }
        let target = dst.join(&name);
        if ft.is_dir() {
            copy_dir_recursive(&entry.path(), &target, skip_ignored)?;
        } else if ft.is_file() {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

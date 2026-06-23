//! VCS service: bridges the local-Git checkpoint layer ([`core_vcs`]) to the
//! event-sourced core (PRD FR6).
//!
//! Capturing or reverting a checkpoint appends a domain event to the log, so
//! the change flows out over the single ordered push path (`/v1/events`) and the
//! UI refreshes — the same pattern the provider pump uses for session events
//! (PRD G3). Git's object database is the durable store for the snapshots
//! themselves; the event log only carries the notification + metadata.
//!
//! These calls do blocking Git I/O; callers run them on a blocking task.

use std::path::Path;

use core_orchestration::Engine;
use core_persistence::DomainEvent;
use core_vcs::{Checkpoint, VcsError, Workspace};
use serde_json::json;

/// Capture a checkpoint of `workspace` and record a `checkpoint.captured` event.
pub fn capture(
    engine: &Engine,
    workspace: &Path,
    project_id: Option<&str>,
    session_id: Option<&str>,
    label: &str,
) -> Result<Checkpoint, VcsError> {
    let checkpoint = Workspace::open_or_init(workspace)?.capture_checkpoint(label)?;
    append_captured(engine, project_id, session_id, &checkpoint);
    Ok(checkpoint)
}

/// Like [`capture`] but skips no-op snapshots — used for per-turn auto-capture
/// so turns that change nothing don't litter the checkpoint chain.
pub fn capture_if_changed(
    engine: &Engine,
    workspace: &Path,
    project_id: Option<&str>,
    session_id: Option<&str>,
    label: &str,
) -> Result<Option<Checkpoint>, VcsError> {
    let Some(checkpoint) =
        Workspace::open_or_init(workspace)?.capture_checkpoint_if_changed(label)?
    else {
        return Ok(None);
    };
    append_captured(engine, project_id, session_id, &checkpoint);
    Ok(Some(checkpoint))
}

/// Revert `workspace` to a checkpoint and record a `checkpoint.reverted` event.
pub fn revert(
    engine: &Engine,
    workspace: &Path,
    project_id: Option<&str>,
    checkpoint_id: &str,
) -> Result<(), VcsError> {
    Workspace::open_or_init(workspace)?.revert_to(checkpoint_id)?;
    let _ = engine.store().append(vec![DomainEvent::new(
        "checkpoint.reverted",
        json!({ "project_id": project_id, "checkpoint_id": checkpoint_id }),
    )]);
    Ok(())
}

fn append_captured(
    engine: &Engine,
    project_id: Option<&str>,
    session_id: Option<&str>,
    checkpoint: &Checkpoint,
) {
    // A failed append must not fail the capture itself; the store logs it.
    let _ = engine.store().append(vec![DomainEvent::new(
        "checkpoint.captured",
        json!({
            "project_id": project_id,
            "session_id": session_id,
            "checkpoint": checkpoint,
        }),
    )]);
}

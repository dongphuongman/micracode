//! Keyed registry of dev-server previews (PRD FR7).
//!
//! [`core_terminal::PreviewServer`] is the primitive — one dev server, scanned
//! for its port and watched. This manager owns one preview per project, keyed
//! by `project_id`. Starting a preview for a project that already has one
//! replaces it (a fresh dev server), so the UI's "restart preview" is just
//! another `start`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use core_terminal::{PreviewError, PreviewOptions, PreviewServer, PreviewStatus};

/// Tracks one live preview per project.
#[derive(Default)]
pub struct PreviewManager {
    previews: Mutex<HashMap<String, Arc<PreviewServer>>>,
}

impl PreviewManager {
    pub fn new() -> Self {
        PreviewManager::default()
    }

    /// Start (or restart) the preview for `project_id`. Any existing preview
    /// for the project is stopped first. Returns the initial status.
    pub async fn start(
        &self,
        project_id: String,
        opts: PreviewOptions,
    ) -> Result<PreviewStatus, PreviewError> {
        // Stop and drop any prior preview before spawning the replacement, but
        // never hold the lock across the await.
        if let Some(prev) = self.previews.lock().unwrap().remove(&project_id) {
            prev.stop();
        }

        let server = Arc::new(PreviewServer::spawn(opts).await?);
        let status = server.status();
        self.previews.lock().unwrap().insert(project_id, server);
        Ok(status)
    }

    /// Current status of a project's preview, if one is running.
    pub fn status(&self, project_id: &str) -> Option<PreviewStatus> {
        self.previews
            .lock()
            .unwrap()
            .get(project_id)
            .map(|s| s.status())
    }

    /// Stop and forget a project's preview. `false` if none was running.
    pub fn stop(&self, project_id: &str) -> bool {
        let Some(server) = self.previews.lock().unwrap().remove(project_id) else {
            return false;
        };
        server.stop();
        true
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::net::TcpListener;

    async fn wait_running(manager: &PreviewManager, project_id: &str) -> PreviewStatus {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(s @ PreviewStatus::Running { .. }) = manager.status(project_id) {
                    return s;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap_or_else(|_| manager.status(project_id).unwrap_or(PreviewStatus::Starting))
    }

    #[tokio::test]
    async fn start_reports_running_then_stop_forgets_it() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let dir = tempfile::tempdir().unwrap();
        let manager = PreviewManager::new();

        manager
            .start(
                "proj".to_string(),
                PreviewOptions {
                    workspace: dir.path().to_path_buf(),
                    command: "/bin/sh".to_string(),
                    args: vec!["-c".to_string(), "sleep 30".to_string()],
                    env: Vec::new(),
                    scan_host: "127.0.0.1".to_string(),
                    ports: vec![port],
                    url_host: "localhost".to_string(),
                },
            )
            .await
            .expect("start preview");

        let status = wait_running(&manager, "proj").await;
        assert!(matches!(status, PreviewStatus::Running { .. }), "got {status:?}");

        assert!(manager.stop("proj"));
        assert!(manager.status("proj").is_none());
        assert!(!manager.stop("proj"), "stopping twice is false");
    }
}

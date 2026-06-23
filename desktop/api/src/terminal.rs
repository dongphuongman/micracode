//! Keyed registry of PTY terminal sessions (PRD FR7).
//!
//! [`core_terminal::Terminal`] is the primitive — one PTY, one child. This
//! manager owns the many of them the UI opens, addressed by a local id (the
//! same split as [`ProviderManager`](crate::provider) over `CodexDriver`). The
//! `/v1/terminals` router drives it; output is streamed per-session over SSE.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::Arc;

use core_terminal::{Terminal, TerminalError, TerminalOptions};
use uuid::Uuid;

/// Tracks live terminals by local id.
#[derive(Default)]
pub struct TerminalManager {
    sessions: Mutex<HashMap<String, Arc<Terminal>>>,
}

impl TerminalManager {
    pub fn new() -> Self {
        TerminalManager::default()
    }

    /// Spawn a terminal and register it, returning its local id.
    pub fn start(&self, opts: TerminalOptions) -> Result<String, TerminalError> {
        let terminal = Arc::new(Terminal::spawn(opts)?);
        let id = Uuid::new_v4().to_string();
        self.sessions.lock().unwrap().insert(id.clone(), terminal);
        Ok(id)
    }

    /// The terminal for `id`, if it is still registered.
    pub fn get(&self, id: &str) -> Option<Arc<Terminal>> {
        self.sessions.lock().unwrap().get(id).cloned()
    }

    /// Ids of all live terminals.
    pub fn list(&self) -> Vec<String> {
        self.sessions.lock().unwrap().keys().cloned().collect()
    }

    /// Kill a terminal and drop it from the registry. `false` if id unknown.
    pub fn kill(&self, id: &str) -> bool {
        let Some(terminal) = self.sessions.lock().unwrap().remove(id) else {
            return false;
        };
        // Best-effort kill; dropping the Arc also reaps once the last ref goes.
        let _ = terminal.kill();
        true
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn start_get_list_and_kill() {
        let dir = tempfile::tempdir().unwrap();
        let manager = TerminalManager::new();

        let id = manager
            .start(TerminalOptions {
                workspace: dir.path().to_path_buf(),
                command: Some("/bin/sh".into()),
                args: vec!["-c".into(), "printf HELLO; sleep 5".into()],
                ..Default::default()
            })
            .expect("start");

        assert_eq!(manager.list(), vec![id.clone()]);

        // Output reaches a subscriber obtained through the manager.
        let term = manager.get(&id).expect("get");
        let mut rx = term.subscribe();
        let mut acc = String::new();
        for chunk in term.scrollback() {
            acc.push_str(&String::from_utf8_lossy(&chunk.bytes));
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            while !acc.contains("HELLO") {
                if let Ok(c) = rx.recv().await {
                    acc.push_str(&String::from_utf8_lossy(&c.bytes));
                } else {
                    break;
                }
            }
        })
        .await;
        assert!(acc.contains("HELLO"), "got: {acc:?}");

        assert!(manager.kill(&id));
        assert!(manager.get(&id).is_none());
        assert!(!manager.kill(&id), "killing twice is false");
    }
}

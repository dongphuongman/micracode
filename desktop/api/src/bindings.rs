//! TypeScript binding generation (PRD FR8).
//!
//! The Rust wire types are the single source of truth; this module's test
//! exports them to `apps/web/src/lib/api/generated/` via `ts-rs` so the web
//! client never hand-maintains a duplicate schema. Regenerate with:
//!
//! ```sh
//! cd desktop/api && cargo test --test-threads=1 export_bindings
//! ```
//!
//! (or run `scripts/gen-bindings.sh` from the repo root). CI re-runs this and
//! fails if the committed bindings are stale — see `scripts/check-bindings.sh`.

#[cfg(test)]
mod tests {
    use ts_rs::TS;

    /// Repo-relative output directory for the generated `.ts` files. Resolved
    /// from this crate's manifest dir so it works regardless of cwd.
    const OUT_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/api/generated");

    /// Every wire type the server emits. `export_all_to` also writes each type's
    /// transitive dependencies (e.g. `Thread` pulls in `Turn`/`Message`), so the
    /// roots listed here are enough; we still list the standalone enums
    /// explicitly so they're emitted even when nothing references them yet.
    #[test]
    fn export_bindings() {
        // Event-sourced command/event core (FR2).
        core_orchestration::Receipt::export_all_to(OUT_DIR).unwrap();
        core_orchestration::RuntimeReceipt::export_all_to(OUT_DIR).unwrap();
        core_persistence::StoredEvent::export_all_to(OUT_DIR).unwrap();
        crate::routers::commands::EventsResponse::export_all_to(OUT_DIR).unwrap();

        // Read-model projection (threads → turns → messages, FR2).
        core_projection::ThreadStatus::export_all_to(OUT_DIR).unwrap();
        core_projection::TurnStatus::export_all_to(OUT_DIR).unwrap();
        core_projection::Message::export_all_to(OUT_DIR).unwrap();
        core_projection::Turn::export_all_to(OUT_DIR).unwrap();
        core_projection::Thread::export_all_to(OUT_DIR).unwrap();
        core_projection::ThreadSummary::export_all_to(OUT_DIR).unwrap();

        // Local Git / checkpoints (FR6).
        core_vcs::ChangeKind::export_all_to(OUT_DIR).unwrap();
        core_vcs::FileChange::export_all_to(OUT_DIR).unwrap();
        core_vcs::Checkpoint::export_all_to(OUT_DIR).unwrap();
        crate::routers::vcs::StatusResponse::export_all_to(OUT_DIR).unwrap();
    }
}

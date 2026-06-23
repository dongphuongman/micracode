//! `GET /v1/receipts/stream` — Server-Sent Events of runtime completion
//! receipts (PRD FR3).
//!
//! Unlike `/v1/events`, receipts are *not* persisted: they are advisory idle
//! signals (a turn went quiescent, a checkpoint was captured) the UI can use to
//! know when background work has settled — e.g. to stop a spinner the instant a
//! turn's checkpoint lands rather than polling. The durable record is always the
//! event log. Because they aren't persisted there is no backlog to replay; this
//! stream carries only receipts emitted after the client connects.

use std::convert::Infallible;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::get;
use axum::Router;
use core_orchestration::RuntimeReceipt;
use futures::stream::{Stream, StreamExt};
use tokio_stream::wrappers::BroadcastStream;

use super::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/receipts/stream", get(stream_receipts))
}

async fn stream_receipts(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let receipts = BroadcastStream::new(state.provider.receipts().subscribe());
    let events = receipts.filter_map(|res| async move {
        // Drop lag errors: a client that fell behind on advisory signals just
        // resyncs from the durable event log, never from a receipt.
        let receipt: RuntimeReceipt = res.ok()?;
        Some(Ok(Event::default()
            .json_data(&receipt)
            .unwrap_or_else(|_| Event::default())))
    });
    Sse::new(events).keep_alive(KeepAlive::default())
}

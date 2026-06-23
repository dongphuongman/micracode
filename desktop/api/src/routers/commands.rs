//! `/v1/commands` + `/v1/events` — the HTTP transport over the event-sourced
//! core (PRD G6 / FR5). `POST /v1/commands` dispatches a command through the
//! [`Engine`](core_orchestration::Engine); `GET /v1/events?cursor=N` replays
//! the log from a cursor so the frontend can hydrate and poll for new events.

use std::convert::Infallible;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use core_orchestration::{Command, EngineError, Receipt};
use core_persistence::StoredEvent;
use futures::stream::{self, Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_stream::wrappers::BroadcastStream;

use super::AppState;

/// Incoming command from the frontend. `id` is the idempotency key.
#[derive(Debug, Deserialize)]
pub struct CommandRequest {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    #[serde(default)]
    pub cursor: u64,
}

/// Replay response: the events after the requested cursor, plus the new cursor
/// to resume from on the next poll.
#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(rename = "EventsPage")]
pub struct EventsResponse {
    pub events: Vec<StoredEvent>,
    #[ts(type = "number")]
    pub cursor: u64,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/commands", post(dispatch_command))
        .route("/events", get(list_events))
        .route("/events/stream", get(stream_events))
}

type ApiResult<T> = Result<Json<T>, (StatusCode, String)>;

fn server_error(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

async fn dispatch_command(
    State(state): State<AppState>,
    Json(req): Json<CommandRequest>,
) -> ApiResult<Receipt> {
    let receipt = state
        .engine
        .dispatch(Command {
            id: req.id,
            kind: req.kind,
            payload: req.payload,
        })
        .map_err(|e| match e {
            // A command that fails validation is a client error, not a 500.
            EngineError::Rejected(msg) => (StatusCode::BAD_REQUEST, msg.to_string()),
            other => server_error(other),
        })?;
    Ok(Json(receipt))
}

async fn list_events(
    State(state): State<AppState>,
    Query(q): Query<EventsQuery>,
) -> ApiResult<EventsResponse> {
    let events = state.engine.store().read_from(q.cursor).map_err(server_error)?;
    let cursor = events.last().map(|e| e.seq).unwrap_or(q.cursor);
    Ok(Json(EventsResponse { events, cursor }))
}

/// `GET /v1/events/stream?cursor=N` — Server-Sent Events. Replays the log from
/// `cursor`, then pushes new events live as they're appended. Subscribing to
/// the hot stream *before* replaying guarantees no event slips through the gap
/// between the two; events already covered by the replay are filtered out.
async fn stream_events(
    State(state): State<AppState>,
    Query(q): Query<EventsQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, (StatusCode, String)> {
    let store = state.engine.store();
    let live = BroadcastStream::new(store.subscribe());
    let backlog = store.read_from(q.cursor).map_err(server_error)?;
    let last_replayed = backlog.last().map(|e| e.seq).unwrap_or(q.cursor);

    let live = live.filter_map(move |res| async move {
        match res {
            Ok(ev) if ev.seq > last_replayed => Some(ev),
            _ => None, // drop lag errors and any event already in the backlog
        }
    });

    let events = stream::iter(backlog).chain(live).map(|ev: StoredEvent| {
        Ok(Event::default()
            .json_data(&ev)
            .unwrap_or_else(|_| Event::default()))
    });

    Ok(Sse::new(events).keep_alive(KeepAlive::default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use core_orchestration::Engine;
    use core_persistence::EventStore;
    use std::sync::Arc;
    use tower::ServiceExt; // for `oneshot`

    fn test_app_with_engine() -> (Router, Arc<Engine>) {
        let store = EventStore::open_in_memory().unwrap();
        let engine = Arc::new(Engine::new(store, crate::decider::decide));
        let state = AppState {
            storage: Arc::new(crate::storage::Storage::new(std::env::temp_dir())),
            config: Arc::new(crate::config::Config::from_env()),
            engine: Arc::clone(&engine),
            provider: Arc::new(crate::provider::ProviderManager::new(
                core_provider::CodexDriver::with_program("codex"),
                Arc::clone(&engine),
            )),
            projection: crate::projection::ProjectionHandle::spawn(Arc::clone(&engine)),
            terminals: Arc::new(crate::terminal::TerminalManager::new()),
            previews: Arc::new(crate::preview::PreviewManager::new()),
        };
        (super::router().with_state(state), engine)
    }

    fn test_app() -> Router {
        test_app_with_engine().0
    }

    /// Read the next SSE `data:` frame as JSON, with a timeout so a stuck
    /// stream fails the test instead of hanging.
    async fn next_sse_json(
        stream: &mut (impl Stream<Item = Result<axum::body::Bytes, axum::Error>> + Unpin),
        buf: &mut String,
    ) -> Value {
        loop {
            if let Some(end) = buf.find("\n\n") {
                let frame = buf[..end].to_string();
                buf.drain(..end + 2);
                for line in frame.lines() {
                    if let Some(data) = line.strip_prefix("data:") {
                        return serde_json::from_str(data.trim()).unwrap();
                    }
                }
                continue;
            }
            let chunk = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
                .await
                .expect("sse frame timed out")
                .expect("sse stream ended")
                .expect("sse chunk error");
            buf.push_str(std::str::from_utf8(&chunk).unwrap());
        }
    }

    async fn json_body(resp: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn post_command(id: &str, kind: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/commands")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "id": id,
                    "kind": kind,
                    "payload": { "session_id": "s1", "text": "hi" },
                })
                .to_string(),
            ))
            .unwrap()
    }

    #[tokio::test]
    async fn post_command_dispatches_and_get_events_replays_it() {
        let app = test_app();

        let resp = app
            .clone()
            .oneshot(post_command("c1", "send_turn"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let receipt = json_body(resp).await;
        assert_eq!(receipt["command_id"], "c1");
        assert_eq!(receipt["deduped"], false);
        assert_eq!(receipt["events"], serde_json::json!([1]));

        // Replay from cursor 0 sees the event the decider produced.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/events?cursor=0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let page = json_body(resp).await;
        assert_eq!(page["cursor"], 1);
        assert_eq!(page["events"][0]["seq"], 1);
        assert_eq!(page["events"][0]["kind"], "provider.user_turn");
        assert_eq!(page["events"][0]["payload"]["text"], "hi");
    }

    #[tokio::test]
    async fn an_invalid_command_is_rejected_with_400_and_appends_nothing() {
        let app = test_app();

        // Unknown kind → 400, no event recorded.
        let resp = app
            .clone()
            .oneshot(post_command("c1", "frobnicate"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/events?cursor=0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let page = json_body(resp).await;
        assert_eq!(page["events"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn reposting_same_command_id_is_idempotent_over_http() {
        let app = test_app();
        app.clone()
            .oneshot(post_command("dup", "send_turn"))
            .await
            .unwrap();

        let resp = app
            .clone()
            .oneshot(post_command("dup", "send_turn"))
            .await
            .unwrap();
        let receipt = json_body(resp).await;
        assert_eq!(receipt["deduped"], true);

        // Only one event in the log despite two POSTs.
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/events?cursor=0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let page = json_body(resp).await;
        assert_eq!(page["events"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn stream_replays_backlog_then_pushes_live_events() {
        let (app, engine) = test_app_with_engine();
        engine
            .dispatch(Command {
                id: "seed".into(),
                kind: "send_turn".into(),
                payload: serde_json::json!({ "session_id": "s1", "text": "warm" }),
            })
            .unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/events/stream?cursor=0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(ct.starts_with("text/event-stream"), "got {ct}");

        let mut body = resp.into_body().into_data_stream();
        let mut buf = String::new();

        // First frame: the replayed backlog event.
        let first = next_sse_json(&mut body, &mut buf).await;
        assert_eq!(first["seq"], 1);
        assert_eq!(first["kind"], "provider.user_turn");

        // A new dispatch arrives live on the same connection.
        engine
            .dispatch(Command {
                id: "live".into(),
                kind: "send_turn".into(),
                payload: serde_json::json!({ "session_id": "s1", "text": "yo" }),
            })
            .unwrap();
        let second = next_sse_json(&mut body, &mut buf).await;
        assert_eq!(second["seq"], 2);
        assert_eq!(second["kind"], "provider.user_turn");
        assert_eq!(second["payload"]["text"], "yo");
    }
}

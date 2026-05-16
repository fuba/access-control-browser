// `GET /events` — server-sent event stream of the activity feed. Subscribes
// to the broadcast channel and translates each event into an SSE `data:`
// line as JSON.

use std::convert::Infallible;

use axum::extract::State;
use axum::response::sse::{Event, Sse};
use futures::Stream;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::AppState;

pub async fn handler(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.events().subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|res| match res {
        Ok(ev) => Some(Ok(Event::default().json_data(ev).unwrap_or_default())),
        Err(_) => None,
    });
    Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}

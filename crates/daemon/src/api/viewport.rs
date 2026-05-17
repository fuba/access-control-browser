// `GET /sessions/:id/viewport` — bidirectional WebSocket.
//
//   server → client : binary JPEG frames (Page.startScreencast pump)
//   client → server : JSON input events (mouse / key / composition)
//
// Screencast is started lazily on first WS subscriber and stays running
// for the lifetime of the session.

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, warn};

use crate::browser::input::{self, Button, KeyEvent, MouseKind};
use crate::browser::screencast::{self, ScreencastConfig};
use crate::browser::session::Session;
use crate::AppState;

pub async fn handler(
    Path(id): Path<String>,
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
) -> Result<Response, StatusCode> {
    let session = state.get_session(&id).await.ok_or(StatusCode::NOT_FOUND)?;
    // Start screencast if it isn't already. Subsequent subscribers share
    // the broadcast channel.
    screencast::ensure_started(&session, ScreencastConfig::default())
        .await
        .map_err(|e| {
            warn!(error = ?e, "ensure_started failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(ws.on_upgrade(move |socket| pump(socket, session)))
}

async fn pump(socket: WebSocket, session: Arc<Session>) {
    let (mut sender, mut receiver) = socket.split();
    let mut frames = session.viewport_frames.subscribe();

    // Send the cached most-recent frame (if any) so the subscriber sees
    // something even when the page is static.
    if let Some(cached) = session.last_frame.read().await.clone() {
        let _ = sender.send(Message::Binary(cached)).await;
    }

    // server -> client: forward JPEG frames as binary WS messages.
    let forward = tokio::spawn(async move {
        loop {
            match frames.recv().await {
                Ok(bytes) => {
                    if sender.send(Message::Binary(bytes)).await.is_err() {
                        return;
                    }
                }
                Err(RecvError::Lagged(skipped)) => {
                    debug!(skipped, "viewport WS subscriber lagged behind broadcast");
                    continue;
                }
                Err(RecvError::Closed) => return,
            }
        }
    });

    // client -> server: parse JSON input events and dispatch to CDP.
    let s = session.clone();
    let inbox = tokio::spawn(async move {
        while let Some(msg) = receiver.next().await {
            let txt = match msg {
                Ok(Message::Text(t)) => t,
                Ok(Message::Binary(_)) => continue,
                Ok(Message::Close(_)) | Err(_) => return,
                Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => continue,
            };
            let ev: InputMsg = match serde_json::from_str(&txt) {
                Ok(v) => v,
                Err(e) => {
                    debug!(error = ?e, raw = %txt, "bad viewport input");
                    continue;
                }
            };
            if let Err(e) = dispatch(&s, ev).await {
                debug!(error = ?e, "viewport input dispatch failed");
            }
        }
    });

    tokio::select! {
        _ = forward => (),
        _ = inbox => (),
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum InputMsg {
    Mouse(MouseInput),
    Key(KeyInput),
    CompositionStart,
    CompositionUpdate(CompositionUpdate),
    CompositionEnd(CompositionEnd),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MouseInput {
    /// "down" | "up" | "move" | "wheel"
    r#type: String,
    x: f64,
    y: f64,
    /// "left" | "middle" | "right" | "none"
    #[serde(default)]
    button: Option<String>,
    #[serde(default = "default_click_count")]
    click_count: i64,
    #[serde(default)]
    modifiers: i64,
    /// For wheel events.
    #[serde(default)]
    delta_x: f64,
    #[serde(default)]
    delta_y: f64,
}

fn default_click_count() -> i64 {
    1
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyInput {
    /// "down" | "up"
    r#type: String,
    key: String,
    code: String,
    /// Text to insert if the key produces a character.
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    modifiers: i64,
    #[serde(default)]
    windows_virtual_key_code: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompositionUpdate {
    text: String,
    #[serde(default)]
    selection_start: Option<i64>,
    #[serde(default)]
    selection_end: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompositionEnd {
    text: String,
}

async fn dispatch(session: &Arc<Session>, msg: InputMsg) -> anyhow::Result<()> {
    match msg {
        InputMsg::Mouse(m) => {
            let kind = match m.r#type.as_str() {
                "down" => MouseKind::Pressed,
                "up" => MouseKind::Released,
                "move" => MouseKind::Moved,
                "wheel" => MouseKind::Wheel,
                other => {
                    return Err(anyhow::anyhow!("unknown mouse type {other}"));
                }
            };
            let button = match m.button.as_deref() {
                Some("left") | None => Button::Left,
                Some("right") => Button::Right,
                Some("middle") => Button::Middle,
                Some("none") => Button::None,
                Some(other) => {
                    return Err(anyhow::anyhow!("unknown mouse button {other}"));
                }
            };
            input::dispatch_mouse(
                &session.page,
                kind,
                m.x,
                m.y,
                button,
                m.click_count,
                m.modifiers,
            )
            .await?;
            // Note: wheel deltas (m.delta_x/y) are ignored in v0.2's mouse
            // implementation — CDP requires them on a separate path. Will
            // add when needed.
            let _ = (m.delta_x, m.delta_y);
        }
        InputMsg::Key(k) => {
            let down = match k.r#type.as_str() {
                "down" => true,
                "up" => false,
                other => return Err(anyhow::anyhow!("unknown key type {other}")),
            };
            input::dispatch_key(
                &session.page,
                KeyEvent {
                    kind_down: down,
                    key: k.key,
                    code: k.code,
                    text: k.text,
                    modifiers: k.modifiers,
                    windows_virtual_key_code: k.windows_virtual_key_code,
                },
            )
            .await?;
        }
        InputMsg::CompositionStart => {
            // Open the composition with empty text.
            input::ime_set_composition(&session.page, "", 0, 0).await?;
        }
        InputMsg::CompositionUpdate(u) => {
            let len = u.text.chars().count() as i64;
            let sel_start = u.selection_start.unwrap_or(len);
            let sel_end = u.selection_end.unwrap_or(len);
            input::ime_set_composition(&session.page, &u.text, sel_start, sel_end).await?;
        }
        InputMsg::CompositionEnd(e) => {
            // Insert the committed text. This also clears any open
            // composition state.
            input::insert_text(&session.page, &e.text).await?;
        }
    }
    Ok(())
}

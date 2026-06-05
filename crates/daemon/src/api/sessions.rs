// Session lifecycle endpoints. A session owns one chromiumoxide Page; the
// interceptor is installed on creation.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
use serde::Serialize;

use crate::browser::{interceptor, session::Session};
use crate::events::{now_unix, ActivityEvent};
use crate::AppState;

#[derive(Serialize)]
pub struct SessionCreated {
    pub id: String,
}

#[derive(Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub url: Option<String>,
    pub title: Option<String>,
    pub created_at: u64,
}

/// `GET /sessions` — list live sessions so the UI tab bar and
/// `acb-cli sessions` can discover and target them. Sorted by created_at
/// so tab order is stable.
pub async fn list(State(state): State<AppState>) -> Json<Vec<SessionSummary>> {
    let mut out = Vec::new();
    for s in state.list_sessions().await {
        out.push(SessionSummary {
            id: s.id.clone(),
            url: s.current_url.read().await.clone(),
            title: s.title.read().await.clone(),
            created_at: s.created_at,
        });
    }
    out.sort_by_key(|s| s.created_at);
    Json(out)
}

pub async fn create(
    State(state): State<AppState>,
) -> Result<Json<SessionCreated>, (StatusCode, String)> {
    let browser = match state.browser_clone().await {
        Some(b) => b,
        None => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "browser not ready".into(),
            ))
        }
    };
    // about:blank is a Chromium internal URL; it is not subject to our
    // allowlist because there is no network fetch.
    let page = browser
        .browser
        .new_page("about:blank")
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("new_page: {e}")))?;

    // Pin the rendered viewport to the configured size. Without this Chromium
    // renders at its own default window size, so screencast frame dimensions
    // and the UI's coordinate scaling (which uses the same configured
    // width/height) disagree and clicks land in the wrong place.
    let vp = state.policy().chromium.viewport;
    let metrics = SetDeviceMetricsOverrideParams::builder()
        .width(vp.width as i64)
        .height(vp.height as i64)
        .device_scale_factor(1.0)
        .mobile(false)
        .build()
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("metrics build: {e}"),
            )
        })?;
    page.execute(metrics).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("set viewport: {e}"),
        )
    })?;

    let id = format!("s_{}", uuid_like());
    let session = Session::new(id.clone(), page);
    interceptor::install(session.clone(), state.clone())
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("install: {e}")))?;
    crate::browser::injected::install_auto_inject(&session.page)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("inject: {e}")))?;
    state.put_session(id.clone(), session).await;
    let _ = state.events().send(ActivityEvent::SessionOpened {
        ts: now_unix(),
        session: id.clone(),
    });
    Ok(Json(SessionCreated { id }))
}

pub async fn delete(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<StatusCode, (StatusCode, String)> {
    match state.get_session(&id).await {
        Some(s) => {
            s.shutdown().await;
            // Best-effort close the page. chromiumoxide's Page::close takes
            // ownership; clone the inner handle (cheap Arc bump) so we keep
            // the session usable through the rest of the drop path.
            let _ = s.page.clone().close().await;
            state.drop_session(&id).await;
            let _ = state.events().send(ActivityEvent::SessionClosed {
                ts: now_unix(),
                session: id,
            });
            Ok(StatusCode::NO_CONTENT)
        }
        None => Err((StatusCode::NOT_FOUND, "no such session".into())),
    }
}

// Tiny non-secure unique id for sessions; not used for auth.
fn uuid_like() -> String {
    use rand::Rng;
    let n: u128 = rand::thread_rng().gen();
    format!("{n:032x}")
}

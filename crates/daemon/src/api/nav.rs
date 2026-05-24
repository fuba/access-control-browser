// `POST /sessions/:id/open` — navigate the session's page to a URL. We
// re-validate here (defense in depth: the agent never bypasses the
// validator just because the Fetch interceptor would).
//
// back / forward pre-validate the destination history entry's URL with
// validate_url and refuse (403) if it's no longer allowed. The Fetch
// interceptor alone is NOT sufficient here: Chromium can restore a history
// entry from its in-memory document cache before/instead of the network
// request we intercept, so a policy change between first-visit and
// going-back could otherwise leak a now-disallowed page. reload re-loads
// the (already-allowed) current page, so the interceptor suffices there.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chromiumoxide::cdp::browser_protocol::page::{
    GetNavigationHistoryParams, NavigateToHistoryEntryParams,
};
use serde::{Deserialize, Serialize};

use crate::AppState;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenReq {
    pub url: String,
}

#[derive(Serialize)]
pub struct OpenResp {
    pub url: String,
    pub rule: String,
}

pub async fn handler(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(req): Json<OpenReq>,
) -> Result<Json<OpenResp>, (StatusCode, String)> {
    let session = state
        .get_session(&id)
        .await
        .ok_or((StatusCode::NOT_FOUND, "no such session".into()))?;

    let policy = state.policy();
    let cls = acb_policy::url_validator::validate_url(&req.url, &policy)
        .map_err(|reason| (StatusCode::FORBIDDEN, format!("blocked: {reason}")))?;
    let acb_policy::url_validator::UrlClass::Allowed { rule_name } = cls;

    // Pre-populate current_url so any pre-paint subresource events in the
    // very early moments after `Page.navigate` have a sensible page to
    // inherit from. The frame-navigated handler will overwrite this with
    // the canonical post-redirect URL.
    *session.current_url.write().await = Some(req.url.clone());

    session
        .page
        .goto(&req.url)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("navigate: {e}")))?;

    Ok(Json(OpenResp {
        url: req.url,
        rule: rule_name,
    }))
}

/// Step one entry back in the page's navigation history (no-op at the
/// start). The history navigation re-fetches the entry, so the Fetch
/// interceptor re-applies the allowlist.
pub async fn back(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<StatusCode, (StatusCode, String)> {
    history_step(&state, &id, -1).await
}

/// Step one entry forward in the page's navigation history (no-op at the end).
pub async fn forward(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<StatusCode, (StatusCode, String)> {
    history_step(&state, &id, 1).await
}

/// Reload the current page. The reload is a fresh top-level Document load,
/// so the allowlist still binds via the interceptor.
pub async fn reload(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<StatusCode, (StatusCode, String)> {
    let session = state
        .get_session(&id)
        .await
        .ok_or((StatusCode::NOT_FOUND, "no such session".into()))?;
    session
        .page
        .clone()
        .reload()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("reload: {e}")))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn history_step(
    state: &AppState,
    id: &str,
    delta: i64,
) -> Result<StatusCode, (StatusCode, String)> {
    let session = state
        .get_session(id)
        .await
        .ok_or((StatusCode::NOT_FOUND, "no such session".into()))?;
    let hist = session
        .page
        .execute(GetNavigationHistoryParams::default())
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("history: {e}")))?;
    let target_index = hist.current_index + delta;
    if target_index < 0 || target_index as usize >= hist.entries.len() {
        // At the start/end of history — nothing to do.
        return Ok(StatusCode::NO_CONTENT);
    }
    let entry = &hist.entries[target_index as usize];

    // Pre-validate the destination URL. Unlike a fresh `open`, a history
    // navigation can be restored by Chromium from its in-memory document
    // cache *before* (or instead of) issuing the network request the Fetch
    // interceptor guards — so the interceptor is NOT a reliable chokepoint
    // for back/forward. We must gate here: if the entry's URL no longer
    // passes the allowlist (e.g. policy changed since it was first
    // visited), refuse the navigation entirely.
    let policy = state.policy();
    if acb_policy::url_validator::validate_url(&entry.url, &policy).is_err() {
        return Err((
            StatusCode::FORBIDDEN,
            format!("history entry no longer allowed: {}", entry.url),
        ));
    }

    let entry_id = entry.id;
    session
        .page
        .execute(NavigateToHistoryEntryParams { entry_id })
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("navigate-history: {e}")))?;
    Ok(StatusCode::NO_CONTENT)
}

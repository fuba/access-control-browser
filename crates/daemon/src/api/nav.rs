// `POST /sessions/:id/open` — navigate the session's page to a URL. We
// re-validate here (defense in depth: the agent never bypasses the
// validator just because the Fetch interceptor would).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
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

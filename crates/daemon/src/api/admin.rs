// Admin endpoints. /admin/reload triggers a manual reload of the policy
// file (the path the daemon was launched with). Used by `acb-cli reload`
// for scripts that prefer an explicit nudge over file-mtime detection.

use std::path::PathBuf;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Serialize;

use crate::AppState;

#[derive(Serialize)]
pub struct ReloadResp {
    pub etag: String,
}

pub async fn reload(
    State(state): State<AppState>,
) -> Result<Json<ReloadResp>, (StatusCode, String)> {
    let path = state.policy_path().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "daemon has no on-disk policy".into(),
    ))?;
    let etag = crate::reload::reload_now(&path, &state)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("reload: {e}")))?;
    Ok(Json(ReloadResp { etag }))
}

// Tiny extension that exposes the path for the admin handler.
pub fn _ensure_used(_p: PathBuf) {}

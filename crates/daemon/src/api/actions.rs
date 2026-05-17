// Element-action endpoints. Every action takes only a `ref` returned by
// the most recent snapshot; the agent cannot pass raw selectors or scripts.
// Stale refs return 410 Gone.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::browser::injected::call_helper;
use crate::snapshot::{parse_ref, take, SnapshotResponse};
use crate::AppState;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefBody {
    pub r#ref: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FillBody {
    pub r#ref: String,
    pub text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PressBody {
    pub r#ref: String,
    pub key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectBody {
    pub r#ref: String,
    pub value: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckBody {
    pub r#ref: String,
    pub checked: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub enum FindBody {
    Role { query: String },
    Text { query: String },
}

// 8 KiB ceiling on agent-supplied text; cheap insurance against pathological
// inputs reaching the page.
const MAX_TEXT_LEN: usize = 8 * 1024;

async fn resolve_or_gone(
    state: &AppState,
    id: &str,
    r: &str,
) -> Result<(std::sync::Arc<crate::browser::session::Session>, u64), (StatusCode, String)> {
    let session = state
        .get_session(id)
        .await
        .ok_or((StatusCode::NOT_FOUND, "no such session".into()))?;
    let acb_id = parse_ref(&session, r).ok_or((StatusCode::GONE, "stale or unknown ref".into()))?;
    Ok((session, acb_id))
}

pub async fn snapshot(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<SnapshotResponse>, (StatusCode, String)> {
    let session = state
        .get_session(&id)
        .await
        .ok_or((StatusCode::NOT_FOUND, "no such session".into()))?;
    let snap = take(&session, &state)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("snapshot: {e}")))?;
    Ok(Json(snap))
}

pub async fn click(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<RefBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let (s, acb_id) = resolve_or_gone(&state, &id, &body.r#ref).await?;
    let _ = call_helper(&s.page, "clickEl", &[json!(acb_id)])
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("click: {e}")))?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn fill(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<FillBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    if body.text.len() > MAX_TEXT_LEN {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, "text too large".into()));
    }
    let (s, acb_id) = resolve_or_gone(&state, &id, &body.r#ref).await?;
    let _ = call_helper(&s.page, "fillValue", &[json!(acb_id), json!(body.text)])
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("fill: {e}")))?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn type_text(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<FillBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    if body.text.len() > MAX_TEXT_LEN {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, "text too large".into()));
    }
    let (s, acb_id) = resolve_or_gone(&state, &id, &body.r#ref).await?;
    let _ = call_helper(&s.page, "typeText", &[json!(acb_id), json!(body.text)])
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("type: {e}")))?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn press(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<PressBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let (s, acb_id) = resolve_or_gone(&state, &id, &body.r#ref).await?;
    let _ = call_helper(&s.page, "pressKey", &[json!(acb_id), json!(body.key)])
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("press: {e}")))?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn hover(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<RefBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let (s, acb_id) = resolve_or_gone(&state, &id, &body.r#ref).await?;
    let _ = call_helper(&s.page, "hoverEl", &[json!(acb_id)])
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("hover: {e}")))?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn select(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<SelectBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let (s, acb_id) = resolve_or_gone(&state, &id, &body.r#ref).await?;
    let _ = call_helper(&s.page, "selectOption", &[json!(acb_id), json!(body.value)])
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("select: {e}")))?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn check(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<CheckBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let (s, acb_id) = resolve_or_gone(&state, &id, &body.r#ref).await?;
    let _ = call_helper(&s.page, "checkBox", &[json!(acb_id), json!(body.checked)])
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("check: {e}")))?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn find(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<FindBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let session = state
        .get_session(&id)
        .await
        .ok_or((StatusCode::NOT_FOUND, "no such session".into()))?;
    let current = session.current_url.read().await.clone().unwrap_or_default();
    let policy = state.policy();
    let classes: Vec<String> = acb_policy::element_policy::allowed_classes_for(&current, &policy)
        .into_iter()
        .map(|s| s.to_string())
        .collect();
    let result = match body {
        FindBody::Role { query } => {
            call_helper(&session.page, "findRole", &[json!(query), json!(classes)]).await
        }
        FindBody::Text { query } => {
            call_helper(&session.page, "findText", &[json!(query), json!(classes)]).await
        }
    }
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("find: {e}")))?;
    Ok(Json(result))
}

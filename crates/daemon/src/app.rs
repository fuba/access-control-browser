// HTTP router assembly. /healthz is unauthenticated; everything else
// requires a valid bearer token.

use axum::{middleware, routing::*, Router};
use tower_http::set_header::SetResponseHeaderLayer;

use crate::api::{actions, admin, config_route, healthz, nav, sessions, sse, viewport};
use crate::auth;
use crate::AppState;

pub fn router(state: AppState) -> Router {
    // Authenticated subtree.
    let protected = Router::new()
        .route("/config", get(config_route::handler))
        .route("/sessions", post(sessions::create))
        .route("/sessions/:id", delete(sessions::delete))
        .route("/sessions/:id/open", post(nav::handler))
        .route("/sessions/:id/snapshot", post(actions::snapshot))
        .route("/sessions/:id/click", post(actions::click))
        .route("/sessions/:id/fill", post(actions::fill))
        .route("/sessions/:id/type", post(actions::type_text))
        .route("/sessions/:id/press", post(actions::press))
        .route("/sessions/:id/hover", post(actions::hover))
        .route("/sessions/:id/select", post(actions::select))
        .route("/sessions/:id/check", post(actions::check))
        .route("/sessions/:id/find", post(actions::find))
        .route("/sessions/:id/viewport", get(viewport::handler))
        .route("/events", get(sse::handler))
        .route("/admin/reload", post(admin::reload))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_bearer,
        ));

    Router::new()
        .route("/healthz", get(healthz::handler))
        .merge(protected)
        // UI is unauthenticated (static asset). The page itself reads the
        // token from `?token=` and adds it to its API calls.
        .fallback(get(crate::ui_static::any_path))
        // Defense-in-depth: clamp framing and referrers on every response.
        .layer(SetResponseHeaderLayer::if_not_present(
            axum::http::header::HeaderName::from_static("x-frame-options"),
            axum::http::HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            axum::http::header::HeaderName::from_static("referrer-policy"),
            axum::http::HeaderValue::from_static("no-referrer"),
        ))
        .with_state(state)
}

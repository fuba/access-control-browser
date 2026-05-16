// HTTP router assembly. /healthz is unauthenticated; everything else
// requires a valid bearer token.

use axum::{middleware, routing::*, Router};
use tower_http::set_header::SetResponseHeaderLayer;

use crate::api::{config_route, healthz, nav, sessions, sse};
use crate::auth;
use crate::AppState;

pub fn router(state: AppState) -> Router {
    // Authenticated subtree.
    let protected = Router::new()
        .route("/config", get(config_route::handler))
        .route("/sessions", post(sessions::create))
        .route("/sessions/:id", delete(sessions::delete))
        .route("/sessions/:id/open", post(nav::handler))
        .route("/events", get(sse::handler))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_bearer,
        ));

    Router::new()
        .route("/healthz", get(healthz::handler))
        .merge(protected)
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

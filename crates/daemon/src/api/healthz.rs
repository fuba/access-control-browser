// Unauthenticated health probe. Carries an `x-acb: 1` header so
// single-instance detection can distinguish our daemon from another service
// that happens to be bound to the same port.

use axum::http::{HeaderMap, HeaderValue, StatusCode};

pub async fn handler() -> (StatusCode, HeaderMap, &'static str) {
    let mut headers = HeaderMap::new();
    headers.insert("x-acb", HeaderValue::from_static("1"));
    (StatusCode::OK, headers, "ok")
}

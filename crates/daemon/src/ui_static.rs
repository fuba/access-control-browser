// Serve the embedded UI (Next.js static export at ui/out/) at `/`.
//
// CSP is set on every UI response so the bundle cannot be reused to load
// external scripts even if it somehow shipped with one.

use axum::body::Body;
use axum::extract::Path;
use axum::http::{header, HeaderValue, Response, StatusCode, Uri};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../../ui/out/"]
struct UiAssets;

fn build_response(path: &str) -> Response<Body> {
    let lookup = if path.is_empty() || path == "/" {
        "index.html".to_string()
    } else {
        path.trim_start_matches('/').to_string()
    };

    // First try the exact path, then `<path>/index.html` (Next.js
    // trailing-slash output), then fall back to top-level index.html so
    // client-side routes still load.
    let candidates = [
        lookup.clone(),
        format!("{}/index.html", lookup.trim_end_matches('/')),
        "index.html".to_string(),
    ];
    for c in candidates {
        if let Some(file) = UiAssets::get(&c) {
            let mime = mime_guess::from_path(&c).first_or_octet_stream();
            return Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime.as_ref())
                .header(
                    header::CONTENT_SECURITY_POLICY,
                    HeaderValue::from_static(CSP),
                )
                .header(
                    "Permissions-Policy",
                    HeaderValue::from_static(
                        "geolocation=(), microphone=(), camera=(), midi=(), payment=()",
                    ),
                )
                .body(Body::from(file.data.into_owned()))
                .unwrap();
        }
    }
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::from("not found"))
        .unwrap()
}

const CSP: &str = "default-src 'self'; img-src 'self' data: blob:; \
connect-src 'self'; script-src 'self' 'unsafe-inline'; \
style-src 'self' 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'";

pub async fn root() -> Response<Body> {
    build_response("/")
}

pub async fn any_path(uri: Uri) -> Response<Body> {
    build_response(uri.path())
}

pub async fn named(Path(path): Path<String>) -> Response<Body> {
    build_response(&format!("/{path}"))
}

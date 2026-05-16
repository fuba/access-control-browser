//! Tests for `decide_request`: top-level vs sub-resource policy.

use acb_policy::{
    load::load_policy,
    request_policy::{decide_request, RequestDecision, RequestKind},
    url_validator::BlockReason,
};

fn fixture(inherit: bool) -> acb_policy::CompiledPolicy {
    let yaml = format!(
        r#"
server:
  bind: "127.0.0.1"
  port: 39100
  log_file: "./logs/access-control-browser.log"
  log_rotation: "daily"

chromium:
  binary: null
  user_data_dir: "./var/profile"
  viewport: {{ width: 1280, height: 800 }}
  screencast: {{ format: "jpeg", quality: 70, max_fps: 8 }}

resource_policy:
  subresources_inherit_page: {inherit}
  always_block_schemes: ["javascript", "data", "file", "chrome", "about", "blob", "ws", "wss"]
  bypass_service_worker: true

rules:
  - name: "site-a"
    match: {{ kind: "fqdn", host: "site-a.example", subdomains: false }}
    allowed_classes: []
  - name: "cdn-b"
    match: {{ kind: "fqdn", host: "cdn-b.example", subdomains: false }}
    allowed_classes: []
"#
    );
    load_policy(&yaml).expect("fixture compiles")
}

fn allowed(d: &RequestDecision) -> bool {
    matches!(d, RequestDecision::Allow)
}

#[test]
fn top_level_allowed() {
    let cfg = fixture(true);
    let d = decide_request("https://site-a.example/", RequestKind::TopLevelDocument, None, &cfg);
    assert!(allowed(&d));
}

#[test]
fn top_level_blocked() {
    let cfg = fixture(true);
    let d = decide_request("https://evil.example/", RequestKind::TopLevelDocument, None, &cfg);
    assert!(!allowed(&d));
}

#[test]
fn subresource_inherits_when_page_allowed() {
    // CDN does NOT need its own rule when inheritance is on.
    let cfg = fixture(true);
    let d = decide_request(
        "https://untrusted-cdn.example/x.js",
        RequestKind::Subresource,
        Some("https://site-a.example/page"),
        &cfg,
    );
    assert!(allowed(&d));
}

#[test]
fn subresource_blocked_when_page_disallowed() {
    let cfg = fixture(true);
    let d = decide_request(
        "https://cdn-b.example/x.js",
        RequestKind::Subresource,
        Some("https://evil.example/"),
        &cfg,
    );
    assert!(!allowed(&d));
}

#[test]
fn subresource_strict_mode_requires_own_match() {
    let cfg = fixture(false);
    // Page allowed, but subresource's own URL doesn't match any rule.
    let d = decide_request(
        "https://untrusted-cdn.example/x.js",
        RequestKind::Subresource,
        Some("https://site-a.example/"),
        &cfg,
    );
    assert!(!allowed(&d));
    // Subresource own URL matches CDN rule -> allowed.
    let d = decide_request(
        "https://cdn-b.example/x.js",
        RequestKind::Subresource,
        Some("https://site-a.example/"),
        &cfg,
    );
    assert!(allowed(&d));
}

#[test]
fn javascript_subresource_always_blocked() {
    let cfg = fixture(true);
    let d = decide_request(
        "javascript:alert(1)",
        RequestKind::Subresource,
        Some("https://site-a.example/"),
        &cfg,
    );
    match d {
        RequestDecision::Block(BlockReason::SchemeBlocked(_)) => {}
        other => panic!("expected SchemeBlocked, got {other:?}"),
    }
}

#[test]
fn data_subresource_always_blocked() {
    let cfg = fixture(true);
    let d = decide_request(
        "data:text/javascript,alert(1)",
        RequestKind::Subresource,
        Some("https://site-a.example/"),
        &cfg,
    );
    match d {
        RequestDecision::Block(BlockReason::SchemeBlocked(_)) => {}
        other => panic!("expected SchemeBlocked, got {other:?}"),
    }
}

#[test]
fn subframe_document_treated_like_subresource() {
    let cfg = fixture(true);
    let d = decide_request(
        "https://untrusted-cdn.example/iframe.html",
        RequestKind::SubFrameDocument,
        Some("https://site-a.example/"),
        &cfg,
    );
    assert!(allowed(&d));
}

#[test]
fn inherit_page_none_fails_closed() {
    // If we don't know which page this subresource belongs to, block.
    // Fail-closed: the alternative would be to let an unauthenticated
    // subresource ride into an unknown context.
    let cfg = fixture(true);
    let d = decide_request(
        "https://cdn-b.example/x.js",
        RequestKind::Subresource,
        None,
        &cfg,
    );
    assert!(!allowed(&d));
}

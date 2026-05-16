// The daemon embeds the Next.js static export. `GET /` must return the
// HTML shell with the hardened CSP and X-Frame-Options.

mod support;

use support::{fixture_policy, TestDaemon};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn root_serves_html_with_csp() {
    let policy = fixture_policy(r#"  []"#);
    let d = TestDaemon::spawn(policy).await;
    let res = d.client().get(format!("{}/", d.base)).send().await.unwrap();
    assert!(res.status().is_success(), "got {}", res.status());
    let csp = res
        .headers()
        .get("content-security-policy")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        csp.contains("default-src 'self'"),
        "CSP must restrict default-src; got {csp:?}"
    );
    let xf = res
        .headers()
        .get("x-frame-options")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert_eq!(xf, "DENY", "X-Frame-Options must be DENY");
    let body = res.text().await.unwrap();
    assert!(body.contains("access-control-browser"), "title missing");
    d.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sse_accepts_query_token() {
    // SSE clients (EventSource) can't add a header; the auth middleware
    // must fall back to `?token=`. We use a short read timeout because
    // SSE keeps the connection open indefinitely; we only need to see
    // that the headers came back with a 2xx status.
    let policy = fixture_policy(r#"  []"#);
    let d = TestDaemon::spawn(policy).await;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap();
    let url = format!("{}/events?token={}", d.base, d.token);
    let res = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        client.get(&url).send(),
    )
    .await
    .expect("headers within 3s")
    .expect("request ok");
    assert!(res.status().is_success(), "got {}", res.status());
    drop(res); // close the stream
    d.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sse_rejects_missing_query_token() {
    let policy = fixture_policy(r#"  []"#);
    let d = TestDaemon::spawn(policy).await;
    let res = d.client().get(format!("{}/events", d.base)).send().await.unwrap();
    assert_eq!(res.status(), 401);
    d.shutdown().await;
}

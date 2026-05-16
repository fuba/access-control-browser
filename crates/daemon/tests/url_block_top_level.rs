// Real Chromium + real HTTP server: verify the daemon refuses to open a
// URL that does not match the allowlist, and accepts one that does.

mod support;

use std::collections::HashMap;

use serde_json::json;

use support::{fixture_policy, spawn_static, TestDaemon};

fn fixtures() -> HashMap<String, Vec<u8>> {
    let mut m = HashMap::new();
    m.insert(
        "/allowed/index.html".into(),
        b"<!doctype html><html><head><title>ok</title></head><body><p class=\"ok\">hi</p></body></html>".to_vec(),
    );
    m.insert(
        "/blocked/index.html".into(),
        b"<!doctype html><html><body>blocked</body></html>".to_vec(),
    );
    m
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn allowed_url_opens() {
    let (base, shutdown) = spawn_static(fixtures()).await;
    let regex_pattern = format!("^{}/allowed/.*$", regex_escape(&base));
    let policy = fixture_policy(&format!(
        r#"  - name: "test-allow"
    match: {{ kind: "regex", pattern: '{regex_pattern}' }}
    allowed_classes: ["ok"]"#
    ));
    let d = TestDaemon::spawn(policy).await;

    let sess: serde_json::Value = d
        .client()
        .post(format!("{}/sessions", d.base))
        .bearer_auth(&d.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = sess["id"].as_str().unwrap();

    let url = format!("{base}/allowed/index.html");
    let res = d
        .client()
        .post(format!("{}/sessions/{}/open", d.base, id))
        .bearer_auth(&d.token)
        .json(&json!({"url": url}))
        .send()
        .await
        .unwrap();
    assert!(res.status().is_success(), "got {}", res.status());
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["rule"], "test-allow");

    let _ = shutdown.send(());
    d.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blocked_url_rejected() {
    let (base, shutdown) = spawn_static(fixtures()).await;
    let regex_pattern = format!("^{}/allowed/.*$", regex_escape(&base));
    let policy = fixture_policy(&format!(
        r#"  - name: "test-allow"
    match: {{ kind: "regex", pattern: '{regex_pattern}' }}
    allowed_classes: ["ok"]"#
    ));
    let d = TestDaemon::spawn(policy).await;

    let sess: serde_json::Value = d
        .client()
        .post(format!("{}/sessions", d.base))
        .bearer_auth(&d.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = sess["id"].as_str().unwrap();

    let bad = format!("{base}/blocked/index.html");
    let res = d
        .client()
        .post(format!("{}/sessions/{}/open", d.base, id))
        .bearer_auth(&d.token)
        .json(&json!({"url": bad}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 403, "blocked URL must be 403");

    let _ = shutdown.send(());
    d.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn javascript_scheme_rejected_at_open() {
    let policy = fixture_policy(r#"  []"#);
    let d = TestDaemon::spawn(policy).await;
    let sess: serde_json::Value = d
        .client()
        .post(format!("{}/sessions", d.base))
        .bearer_auth(&d.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = sess["id"].as_str().unwrap();
    let res = d
        .client()
        .post(format!("{}/sessions/{}/open", d.base, id))
        .bearer_auth(&d.token)
        .json(&json!({"url": "javascript:alert(1)"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 403);
    d.shutdown().await;
}

fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        if "\\.+*?()|[]{}^$".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

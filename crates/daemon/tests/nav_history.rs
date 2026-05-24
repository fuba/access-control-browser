// Back / forward / reload drive the page's navigation history.

mod support;

use std::collections::HashMap;
use std::time::Duration;

use serde_json::json;

use support::{fixture_policy, spawn_static, TestDaemon};

fn files() -> HashMap<String, Vec<u8>> {
    let mut m = HashMap::new();
    m.insert(
        "/page/a.html".into(),
        b"<!doctype html><html><head><title>A</title></head><body>aaa</body></html>".to_vec(),
    );
    m.insert(
        "/page/b.html".into(),
        b"<!doctype html><html><head><title>B</title></head><body>bbb</body></html>".to_vec(),
    );
    m
}

async fn current_url(d: &TestDaemon, id: &str) -> String {
    d.running
        .state
        .get_session(id)
        .await
        .unwrap()
        .current_url
        .read()
        .await
        .clone()
        .unwrap_or_default()
}

/// Poll current_url until it contains `needle` or times out.
async fn wait_url(d: &TestDaemon, id: &str, needle: &str) -> bool {
    for _ in 0..40 {
        if current_url(d, id).await.contains(needle) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn back_forward_reload() {
    let (base, shutdown) = spawn_static(files()).await;
    let regex = format!("^{}/page/.*$", regex_escape(&base));
    let policy = fixture_policy(&format!(
        r#"  - name: "p"
    match: {{ kind: "regex", pattern: '{regex}' }}
    allowed_classes: []"#
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
    let id = sess["id"].as_str().unwrap().to_string();

    let open = |path: &str| {
        let url = format!("{base}/page/{path}");
        let d = &d;
        let id = id.clone();
        async move {
            d.client()
                .post(format!("{}/sessions/{}/open", d.base, id))
                .bearer_auth(&d.token)
                .json(&json!({ "url": url }))
                .send()
                .await
                .unwrap();
        }
    };
    open("a.html").await;
    assert!(wait_url(&d, &id, "/page/a.html").await, "A loaded");
    open("b.html").await;
    assert!(wait_url(&d, &id, "/page/b.html").await, "B loaded");

    // Back -> A
    d.client()
        .post(format!("{}/sessions/{}/back", d.base, id))
        .bearer_auth(&d.token)
        .send()
        .await
        .unwrap();
    assert!(wait_url(&d, &id, "/page/a.html").await, "back -> A");

    // Forward -> B
    d.client()
        .post(format!("{}/sessions/{}/forward", d.base, id))
        .bearer_auth(&d.token)
        .send()
        .await
        .unwrap();
    assert!(wait_url(&d, &id, "/page/b.html").await, "forward -> B");

    // Reload -> still B, succeeds (204)
    let r = d
        .client()
        .post(format!("{}/sessions/{}/reload", d.base, id))
        .bearer_auth(&d.token)
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "reload ok: {}", r.status());
    assert!(
        wait_url(&d, &id, "/page/b.html").await,
        "still B after reload"
    );

    let _ = shutdown.send(());
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

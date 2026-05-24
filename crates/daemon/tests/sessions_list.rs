// GET /sessions lists live sessions with their url/title/created_at.

mod support;

use std::collections::HashMap;

use serde_json::json;

use support::{fixture_policy, spawn_static, TestDaemon};

fn files() -> HashMap<String, Vec<u8>> {
    let mut m = HashMap::new();
    m.insert(
        "/page/a.html".into(),
        b"<!doctype html><html><head><title>Page A</title></head><body>a</body></html>".to_vec(),
    );
    m
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lists_sessions_with_url() {
    let (base, shutdown) = spawn_static(files()).await;
    let regex = format!("^{}/page/.*$", regex_escape(&base));
    let policy = fixture_policy(&format!(
        r#"  - name: "p"
    match: {{ kind: "regex", pattern: '{regex}' }}
    allowed_classes: []"#
    ));
    let d = TestDaemon::spawn(policy).await;

    // Initially empty.
    let v: serde_json::Value = d
        .client()
        .get(format!("{}/sessions", d.base))
        .bearer_auth(&d.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v.as_array().unwrap().len(), 0, "no sessions yet");

    // Create two sessions; navigate one of them.
    let mk = || async {
        let r: serde_json::Value = d
            .client()
            .post(format!("{}/sessions", d.base))
            .bearer_auth(&d.token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        r["id"].as_str().unwrap().to_string()
    };
    let s1 = mk().await;
    let _s2 = mk().await;
    d.client()
        .post(format!("{}/sessions/{}/open", d.base, s1))
        .bearer_auth(&d.token)
        .json(&json!({"url": format!("{base}/page/a.html")}))
        .send()
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;

    let v: serde_json::Value = d
        .client()
        .get(format!("{}/sessions", d.base))
        .bearer_auth(&d.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 2, "two sessions listed");
    // The navigated session should report its URL (and eventually title).
    let s1_entry = arr
        .iter()
        .find(|e| e["id"] == json!(s1))
        .expect("s1 listed");
    assert!(
        s1_entry["url"]
            .as_str()
            .unwrap_or("")
            .contains("/page/a.html"),
        "s1 url should reflect navigation, got {:?}",
        s1_entry["url"]
    );
    assert!(s1_entry["created_at"].as_u64().unwrap() > 0);

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

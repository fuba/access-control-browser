// Going back to a URL that the policy no longer allows is blocked by the
// Fetch interceptor — back/forward are NOT a bypass of the allowlist.

mod support;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
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
async fn back_to_disallowed_is_blocked() {
    let (base, shutdown) = spawn_static(files()).await;
    let regex_all = format!("^{}/page/.*$", regex_escape(&base));
    // Start with a policy allowing both A and B.
    let policy = fixture_policy(&format!(
        r#"  - name: "all"
    match: {{ kind: "regex", pattern: '{regex_all}' }}
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

    // Swap the policy so only B is allowed now (A no longer matches).
    let regex_b_only = format!("^{}/page/b\\.html$", regex_escape(&base));
    let new_policy = build_policy(&regex_b_only);
    d.running.state.swap_policy(Arc::new(new_policy));

    // Back -> A must be refused: the destination history entry's URL no
    // longer passes the allowlist, so the handler returns 403 and never
    // navigates (history nav can't rely on the Fetch interceptor — Chromium
    // may restore the doc from cache before the network request). This is
    // deterministic and load-insensitive (no polling / event races).
    let res = d
        .client()
        .post(format!("{}/sessions/{}/back", d.base, id))
        .bearer_auth(&d.token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        403,
        "back to a now-disallowed history entry must be refused"
    );

    // The page never left B, and current_url certainly is not A.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let session = d.running.state.get_session(&id).await.unwrap();
    let body: serde_json::Value = session
        .page
        .execute(
            EvaluateParams::builder()
                .expression("document.body ? document.body.textContent : ''")
                .return_by_value(true)
                .build()
                .unwrap(),
        )
        .await
        .unwrap()
        .result
        .result
        .value
        .clone()
        .unwrap_or(serde_json::Value::Null);
    assert!(
        !body.as_str().unwrap_or("").contains("aaa"),
        "disallowed page A must not be rendered"
    );
    assert!(!current_url(&d, &id).await.contains("/page/a.html"));

    let _ = shutdown.send(());
    d.shutdown().await;
}

// Build a CompiledPolicy with a single regex rule (used for the hot-swap).
fn build_policy(regex: &str) -> acb_policy::CompiledPolicy {
    let yaml = format!(
        r#"
server: {{ bind: "127.0.0.1", port: 39100, log_file: "./l.log", log_rotation: "never" }}
chromium: {{ binary: null, user_data_dir: "./p", viewport: {{ width: 1024, height: 768 }}, screencast: {{ format: "jpeg", quality: 60, max_fps: 8 }} }}
resource_policy: {{ subresources_inherit_page: true, always_block_schemes: ["javascript","data","file"], bypass_service_worker: true }}
rules:
  - name: "b-only"
    match: {{ kind: "regex", pattern: '{regex}' }}
    allowed_classes: []
"#
    );
    acb_policy::load::load_policy(&yaml).expect("policy compiles")
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

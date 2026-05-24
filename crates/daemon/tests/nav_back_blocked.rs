// Going back to a URL that the policy no longer allows is blocked by the
// Fetch interceptor — back/forward are NOT a bypass of the allowlist.

mod support;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use acb_daemon::events::ActivityEvent;
use serde_json::json;
use tokio::time::timeout;

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

    let mut feed = d.running.state.events().subscribe();

    // Back -> A should be blocked by the interceptor.
    d.client()
        .post(format!("{}/sessions/{}/back", d.base, id))
        .bearer_auth(&d.token)
        .send()
        .await
        .unwrap();

    // Generous timeout: the full suite runs many Chromium instances in
    // parallel, so the back-nav → Fetch block → event cycle can be slow.
    let blocked = timeout(Duration::from_secs(15), async {
        loop {
            match feed.recv().await {
                Ok(ActivityEvent::Blocked { url, .. }) if url.contains("/page/a.html") => {
                    return true
                }
                Ok(_) => continue,
                Err(_) => return false,
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(
        blocked,
        "back to now-disallowed A must emit a Blocked event"
    );

    // The page must NOT have become A.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !current_url(&d, &id).await.contains("/page/a.html"),
        "page must not navigate to the disallowed A"
    );

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

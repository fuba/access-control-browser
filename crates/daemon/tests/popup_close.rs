// A page calling `window.open()` to a non-allowlisted URL must have its
// popup auto-closed by the Target.targetCreated handler. We assert by
// subscribing to the broadcast channel and waiting for a `blocked` event of
// kind "popup".

mod support;

use std::collections::HashMap;
use std::time::Duration;

use acb_daemon::events::ActivityEvent;
use serde_json::json;
use tokio::time::timeout;

use support::{fixture_policy, spawn_static, TestDaemon};

fn page_files() -> HashMap<String, Vec<u8>> {
    let mut m = HashMap::new();
    // Open via inline script so we don't need user-gesture for window.open.
    m.insert(
        "/page/index.html".into(),
        b"<!doctype html><html><body><script>window.open('https://evil.example/spy.html', '_blank');</script></body></html>".to_vec(),
    );
    m
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disallowed_popup_is_closed() {
    let (page_base, page_shutdown) = spawn_static(page_files()).await;
    let regex_page = format!("^{}/page/.*$", regex_escape(&page_base));
    let policy = fixture_policy(&format!(
        r#"  - name: "page-allow"
    match: {{ kind: "regex", pattern: '{regex_page}' }}
    allowed_classes: []"#
    ));
    let d = TestDaemon::spawn(policy).await;
    let mut rx = d.running.state.events().subscribe();

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
    d.client()
        .post(format!("{}/sessions/{}/open", d.base, id))
        .bearer_auth(&d.token)
        .json(&json!({"url": format!("{page_base}/page/index.html")}))
        .send()
        .await
        .unwrap();

    // Within a few seconds we should see a popup-block event.
    let mut popup_blocked = false;
    let deadline = timeout(Duration::from_secs(5), async {
        loop {
            match rx.recv().await {
                Ok(ActivityEvent::Blocked { kind, .. }) if kind == "popup" => {
                    return true;
                }
                Ok(_) => continue,
                Err(_) => return false,
            }
        }
    })
    .await;
    if let Ok(true) = deadline {
        popup_blocked = true;
    }
    assert!(popup_blocked, "popup should have been blocked and an event emitted");

    let _ = page_shutdown.send(());
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

// Opening a page emits a `session_url` activity event carrying the URL
// (and eventually a title), so the UI location bar / tab labels update live.

mod support;

use std::collections::HashMap;
use std::time::Duration;

use acb_daemon::events::ActivityEvent;
use serde_json::json;
use tokio::time::timeout;

use support::{fixture_policy, spawn_static, TestDaemon};

fn files() -> HashMap<String, Vec<u8>> {
    let mut m = HashMap::new();
    m.insert(
        "/page/index.html".into(),
        b"<!doctype html><html><head><title>Hello Title</title></head><body>hi</body></html>"
            .to_vec(),
    );
    m
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_emits_session_url() {
    let (base, shutdown) = spawn_static(files()).await;
    let regex = format!("^{}/page/.*$", regex_escape(&base));
    let policy = fixture_policy(&format!(
        r#"  - name: "p"
    match: {{ kind: "regex", pattern: '{regex}' }}
    allowed_classes: []"#
    ));
    let d = TestDaemon::spawn(policy).await;
    let mut feed = d.running.state.events().subscribe();

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
    let url = format!("{base}/page/index.html");
    d.client()
        .post(format!("{}/sessions/{}/open", d.base, id))
        .bearer_auth(&d.token)
        .json(&json!({"url": url}))
        .send()
        .await
        .unwrap();

    // Expect a session_url event for our URL within a few seconds, and a
    // non-empty title at least once (titles arrive via targetInfoChanged
    // after the page parses <title>).
    let got = timeout(Duration::from_secs(6), async {
        let mut saw_url = false;
        let mut saw_title = false;
        loop {
            match feed.recv().await {
                Ok(ActivityEvent::SessionUrl { url: u, title, .. }) => {
                    if u.contains("/page/index.html") {
                        saw_url = true;
                        if title.as_deref().map(|t| !t.is_empty()).unwrap_or(false) {
                            saw_title = true;
                        }
                    }
                    if saw_url && saw_title {
                        return true;
                    }
                }
                Ok(_) => continue,
                Err(_) => return saw_url, // channel closed; url alone is enough
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(got, "expected a session_url event for the opened URL");

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

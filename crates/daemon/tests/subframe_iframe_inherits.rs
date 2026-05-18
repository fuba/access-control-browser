// An allowed page may embed iframes pointing to non-allowlisted origins
// (ads, captchas, analytics, social-widget panels). Per the security
// model these should inherit from the parent page's allowance — only the
// top-level navigation needs to satisfy the URL allowlist.
//
// Before the fix, `Fetch.requestPaused` for an iframe's Document load
// was classified as TopLevelDocument and ran through validate_url,
// which blocked the iframe and broke pages like yahoo.co.jp that depend
// on many cross-origin iframes for their UI / scripts to work.

mod support;

use std::collections::HashMap;
use std::time::Duration;

use acb_daemon::events::ActivityEvent;
use serde_json::json;
use tokio::time::timeout;

use support::{fixture_policy, spawn_static, TestDaemon};

fn page_files(iframe_origin: &str) -> HashMap<String, Vec<u8>> {
    let mut m = HashMap::new();
    m.insert(
        "/page/index.html".into(),
        format!(
            r#"<!doctype html><html><body>
<iframe id=fr src="{iframe_origin}/widget/inner.html"></iframe>
</body></html>"#
        )
        .into_bytes(),
    );
    m
}

fn iframe_files() -> HashMap<String, Vec<u8>> {
    let mut m = HashMap::new();
    m.insert(
        "/widget/inner.html".into(),
        b"<!doctype html><html><body><p id=marker>iframe-ok</p></body></html>".to_vec(),
    );
    m
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cross_origin_iframe_is_inherited() {
    // Iframe origin — not in the allowlist on its own.
    let (iframe_base, iframe_shutdown) = spawn_static(iframe_files()).await;
    // Page origin — in the allowlist.
    let (page_base, page_shutdown) = spawn_static(page_files(&iframe_base)).await;
    let regex_page = format!("^{}/page/.*$", regex_escape(&page_base));
    let policy = fixture_policy(&format!(
        r#"  - name: "page-only"
    match: {{ kind: "regex", pattern: '{regex_page}' }}
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
    let id = sess["id"].as_str().unwrap();
    let iframe_url = format!("{iframe_base}/widget/inner.html");
    d.client()
        .post(format!("{}/sessions/{}/open", d.base, id))
        .bearer_auth(&d.token)
        .json(&json!({"url": format!("{page_base}/page/index.html")}))
        .send()
        .await
        .unwrap();

    // Collect activity for ~1.2s. If sub-frame inheritance is broken, a
    // Blocked event with the iframe URL will appear; if the fix works,
    // we should see no Blocked event for that URL.
    let target = iframe_url.clone();
    let blocked_iframe = timeout(Duration::from_millis(1200), async move {
        loop {
            match feed.recv().await {
                Ok(ActivityEvent::Blocked { url, .. }) if url == target => return true,
                Ok(_) => continue,
                Err(_) => return false,
            }
        }
    })
    .await
    .unwrap_or(false);

    assert!(
        !blocked_iframe,
        "expected the iframe at {iframe_url} to inherit from the page; it was blocked"
    );

    let _ = iframe_shutdown.send(());
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

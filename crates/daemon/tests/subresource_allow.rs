// Subresource inheritance: an allowed page must be able to load CSS/JS from
// any origin (CDN), as long as it inherits from its allowed top-level URL.

mod support;

use std::collections::HashMap;

use serde_json::json;

use support::{fixture_policy, spawn_static, TestDaemon};

fn page_origin_files(cdn_url_in_page: &str) -> HashMap<String, Vec<u8>> {
    let mut m = HashMap::new();
    m.insert(
        "/page/index.html".into(),
        format!(
            r#"<!doctype html><html><head>
<link rel="stylesheet" href="{cdn_url_in_page}/cdn/style.css">
<title>p</title></head><body><p id="x">hi</p></body></html>"#
        )
        .into_bytes(),
    );
    m
}

fn cdn_origin_files() -> HashMap<String, Vec<u8>> {
    let mut m = HashMap::new();
    m.insert(
        "/cdn/style.css".into(),
        b"#x{color:rgb(7, 11, 13)}".to_vec(),
    );
    m
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cross_origin_subresource_is_inherited() {
    // 1) CDN origin — never explicitly allowed; its only ticket to load is
    //    inheritance from the page's allowlist.
    let (cdn_base, cdn_shutdown) = spawn_static(cdn_origin_files()).await;
    // 2) Page origin — allowed.
    let (page_base, page_shutdown) = spawn_static(page_origin_files(&cdn_base)).await;

    let regex_page = format!("^{}/page/.*$", regex_escape(&page_base));
    let policy = fixture_policy(&format!(
        r#"  - name: "page-allow"
    match: {{ kind: "regex", pattern: '{regex_page}' }}
    allowed_classes: ["x"]"#
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
    let url = format!("{page_base}/page/index.html");
    let res = d
        .client()
        .post(format!("{}/sessions/{}/open", d.base, id))
        .bearer_auth(&d.token)
        .json(&json!({"url": url}))
        .send()
        .await
        .unwrap();
    assert!(res.status().is_success());

    // Give the page a beat to actually paint the CSS rule, then evaluate
    // computed color to prove the CSS request was allowed and applied.
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;

    let session = d.running.state.get_session(id).await.expect("session");
    let computed: chromiumoxide::cdp::js_protocol::runtime::EvaluateReturns = session
        .page
        .execute(
            chromiumoxide::cdp::js_protocol::runtime::EvaluateParams::builder()
                .expression("getComputedStyle(document.getElementById('x')).color")
                .return_by_value(true)
                .build()
                .unwrap(),
        )
        .await
        .unwrap()
        .result
        .clone();
    let color = computed
        .result
        .value
        .as_ref()
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    assert_eq!(color, "rgb(7, 11, 13)", "CSS should have been applied");

    let _ = cdn_shutdown.send(());
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

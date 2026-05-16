// Subtree element filter end-to-end. A page with two siblings — one whose
// classList contains an allowed class, one that does not — must produce a
// snapshot containing only the allowed subtree's elements; click on a ref
// outside that subtree is impossible because no such ref is allocated.

mod support;

use std::collections::HashMap;

use serde_json::json;

use support::{fixture_policy, spawn_static, TestDaemon};

fn files() -> HashMap<String, Vec<u8>> {
    let mut m = HashMap::new();
    m.insert(
        "/page/index.html".into(),
        br#"<!doctype html><html><body>
            <div class="agent-allowed">
                <button id="b1">visible</button>
                <span>child-text</span>
            </div>
            <div class="secret">
                <button id="b2">hidden-button</button>
                <p>hidden-text</p>
            </div>
        </body></html>"#
            .to_vec(),
    );
    m
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_contains_only_allowed_subtree() {
    let (page_base, page_shutdown) = spawn_static(files()).await;
    let regex_page = format!("^{}/page/.*$", regex_escape(&page_base));
    let policy = fixture_policy(&format!(
        r#"  - name: "page-allow"
    match: {{ kind: "regex", pattern: '{regex_page}' }}
    allowed_classes: ["agent-allowed"]"#
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
    d.client()
        .post(format!("{}/sessions/{}/open", d.base, id))
        .bearer_auth(&d.token)
        .json(&json!({"url": format!("{page_base}/page/index.html")}))
        .send()
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    let snap: serde_json::Value = d
        .client()
        .post(format!("{}/sessions/{}/snapshot", d.base, id))
        .bearer_auth(&d.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let refs = snap["refs"].as_array().expect("refs");
    let all_text: String = refs
        .iter()
        .map(|r| r["text"].as_str().unwrap_or("").to_string())
        .collect::<Vec<_>>()
        .join("|");
    assert!(
        all_text.contains("child-text") || all_text.contains("visible"),
        "expected allowed subtree text; got {all_text:?}"
    );
    assert!(
        !all_text.contains("hidden-button") && !all_text.contains("hidden-text"),
        "non-allowed subtree must not appear; got {all_text:?}"
    );

    let _ = page_shutdown.send(());
    d.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_ref_is_410() {
    let (page_base, page_shutdown) = spawn_static(files()).await;
    let regex_page = format!("^{}/page/.*$", regex_escape(&page_base));
    let policy = fixture_policy(&format!(
        r#"  - name: "page-allow"
    match: {{ kind: "regex", pattern: '{regex_page}' }}
    allowed_classes: ["agent-allowed"]"#
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
    d.client()
        .post(format!("{}/sessions/{}/open", d.base, id))
        .bearer_auth(&d.token)
        .json(&json!({"url": format!("{page_base}/page/index.html")}))
        .send()
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    // Take snapshot to bump generation.
    let _ = d
        .client()
        .post(format!("{}/sessions/{}/snapshot", d.base, id))
        .bearer_auth(&d.token)
        .send()
        .await
        .unwrap();

    let res = d
        .client()
        .post(format!("{}/sessions/{}/click", d.base, id))
        .bearer_auth(&d.token)
        .json(&json!({"ref": "@e999"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 410);

    let _ = page_shutdown.send(());
    d.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_ref_from_previous_generation_is_410() {
    let (page_base, page_shutdown) = spawn_static(files()).await;
    let regex_page = format!("^{}/page/.*$", regex_escape(&page_base));
    let policy = fixture_policy(&format!(
        r#"  - name: "page-allow"
    match: {{ kind: "regex", pattern: '{regex_page}' }}
    allowed_classes: ["agent-allowed"]"#
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
    d.client()
        .post(format!("{}/sessions/{}/open", d.base, id))
        .bearer_auth(&d.token)
        .json(&json!({"url": format!("{page_base}/page/index.html")}))
        .send()
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    let snap1: serde_json::Value = d
        .client()
        .post(format!("{}/sessions/{}/snapshot", d.base, id))
        .bearer_auth(&d.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let r1 = snap1["refs"][0]["ref"].as_str().unwrap().to_string();

    // Take a new snapshot — generation bumps; r1 should no longer resolve.
    let _ = d
        .client()
        .post(format!("{}/sessions/{}/snapshot", d.base, id))
        .bearer_auth(&d.token)
        .send()
        .await
        .unwrap();

    let res = d
        .client()
        .post(format!("{}/sessions/{}/click", d.base, id))
        .bearer_auth(&d.token)
        .json(&json!({"ref": r1}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 410, "old-generation ref must be 410");

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

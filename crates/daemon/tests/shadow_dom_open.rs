// The helper traverses open shadow roots via composed parent chains, so
// elements inside an open shadow DOM that carry an allowed class (or
// descend from one) appear in the snapshot.

mod support;

use std::collections::HashMap;

use serde_json::json;

use support::{fixture_policy, spawn_static, TestDaemon};

fn files() -> HashMap<String, Vec<u8>> {
    let mut m = HashMap::new();
    m.insert(
        "/page/index.html".into(),
        br#"<!doctype html><html><body>
            <my-widget></my-widget>
            <script>
              const tpl = document.createElement('template');
              tpl.innerHTML = '<div class="agent-allowed"><button id="shadow-btn">shadow-hi</button></div>';
              class MyWidget extends HTMLElement {
                connectedCallback() {
                  const sr = this.attachShadow({ mode: 'open' });
                  sr.appendChild(tpl.content.cloneNode(true));
                }
              }
              customElements.define('my-widget', MyWidget);
            </script>
        </body></html>"#.to_vec(),
    );
    m
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_shadow_root_subtree_is_visible() {
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
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;

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
    let texts: String = refs
        .iter()
        .map(|r| r["text"].as_str().unwrap_or("").to_string())
        .collect::<Vec<_>>()
        .join("|");
    assert!(
        texts.contains("shadow-hi"),
        "shadow-DOM child should appear in snapshot; got {texts:?}"
    );

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

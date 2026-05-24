// A wheel event sent over the viewport WS actually scrolls the page.

mod support;

use std::collections::HashMap;
use std::time::Duration;

use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
use futures::SinkExt;
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;

use support::{fixture_policy, spawn_static, TestDaemon};

fn files() -> HashMap<String, Vec<u8>> {
    let mut m = HashMap::new();
    // A page taller than the viewport so it can scroll.
    m.insert(
        "/page/tall.html".into(),
        b"<!doctype html><html><body style='margin:0'>\
          <div style='height:5000px;background:linear-gradient(#fff,#000)'>tall</div>\
          </body></html>"
            .to_vec(),
    );
    m
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wheel_scrolls_page() {
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
    d.client()
        .post(format!("{}/sessions/{}/open", d.base, id))
        .bearer_auth(&d.token)
        .json(&json!({"url": format!("{base}/page/tall.html")}))
        .send()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;

    let ws_url = format!(
        "ws://{}/sessions/{}/viewport?token={}",
        d.running.addr, id, d.token
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("ws connect");
    // Send several wheel events to accumulate scroll.
    for _ in 0..5 {
        ws.send(Message::Text(
            json!({"kind":"mouse","type":"wheel","x":200,"y":200,"delta_x":0,"delta_y":400})
                .to_string(),
        ))
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
    }
    tokio::time::sleep(Duration::from_millis(400)).await;

    let session = d.running.state.get_session(&id).await.unwrap();
    let scroll_y: serde_json::Value = session
        .page
        .execute(
            EvaluateParams::builder()
                .expression("window.scrollY")
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
    let y = scroll_y.as_f64().unwrap_or(0.0);
    assert!(y > 0.0, "wheel should have scrolled the page, scrollY={y}");

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

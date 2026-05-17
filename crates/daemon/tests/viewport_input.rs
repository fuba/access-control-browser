// /sessions/:id/viewport WS: screencast → frames, input → CDP.
//
// We exercise three things end-to-end against a real Chromium:
//   1. A JPEG frame arrives on the WS within a few seconds (screencast
//      pump is running).
//   2. A mouse click at the input field's coordinates focuses it.
//   3. An IME composition_update + composition_end actually inserts the
//      Japanese text into the input.

mod support;

use std::collections::HashMap;
use std::time::Duration;

use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
use futures::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;

use support::{fixture_policy, spawn_static, TestDaemon};

fn fixtures() -> HashMap<String, Vec<u8>> {
    let mut m = HashMap::new();
    // Place the input at a known absolute position so the test can click
    // at deterministic coordinates.
    m.insert(
        "/page/index.html".into(),
        br#"<!doctype html><html><body style="margin:0">
            <input id="x" class="agent-allowed"
                style="position:absolute; left:50px; top:50px; width:240px; height:36px; font-size:20px;">
        </body></html>"#
            .to_vec(),
    );
    m
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn screencast_frame_arrives() {
    let (base, page_shutdown) = spawn_static(fixtures()).await;
    let regex_page = format!("^{}/page/.*$", regex_escape(&base));
    let policy = fixture_policy(&format!(
        r#"  - name: "page-allow"
    match: {{ kind: "regex", pattern: '{regex_page}' }}
    allowed_classes: ["agent-allowed"]"#
    ));
    let d = TestDaemon::spawn(policy).await;

    let id = open_session(&d, &format!("{base}/page/index.html")).await;
    let ws_url = format!(
        "ws://{}/sessions/{}/viewport?token={}",
        d.running.addr, id, d.token
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("ws connect");

    // We should see at least one JPEG within 5s.
    let got = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(msg) = ws.next().await {
            if let Ok(Message::Binary(b)) = msg {
                if !b.is_empty() {
                    return true;
                }
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(got, "expected at least one screencast frame in 5s");

    let _ = page_shutdown.send(());
    d.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mouse_click_focuses_input() {
    let (base, page_shutdown) = spawn_static(fixtures()).await;
    let regex_page = format!("^{}/page/.*$", regex_escape(&base));
    let policy = fixture_policy(&format!(
        r#"  - name: "page-allow"
    match: {{ kind: "regex", pattern: '{regex_page}' }}
    allowed_classes: ["agent-allowed"]"#
    ));
    let d = TestDaemon::spawn(policy).await;

    let id = open_session(&d, &format!("{base}/page/index.html")).await;
    let ws_url = format!(
        "ws://{}/sessions/{}/viewport?token={}",
        d.running.addr, id, d.token
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.expect("ws");

    // Wait until at least one frame has been pumped so the page is fully
    // rendered. (Avoids racing the input's bounding rect.)
    wait_for_first_frame(&mut ws).await;

    // Click in the middle of the input: x=170, y=68 (input is at 50,50
    // with 240x36).
    send(
        &mut ws,
        json!({"kind":"mouse","type":"move","x":170,"y":68}),
    )
    .await;
    send(
        &mut ws,
        json!({"kind":"mouse","type":"down","x":170,"y":68,"button":"left"}),
    )
    .await;
    send(
        &mut ws,
        json!({"kind":"mouse","type":"up","x":170,"y":68,"button":"left"}),
    )
    .await;

    // Give Chromium a beat to settle focus.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let session = d.running.state.get_session(&id).await.unwrap();
    let focused: serde_json::Value = session
        .page
        .execute(
            EvaluateParams::builder()
                .expression("document.activeElement && document.activeElement.id")
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
    assert_eq!(focused.as_str(), Some("x"), "click should focus #x");

    let _ = page_shutdown.send(());
    d.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn japanese_ime_inserts_text() {
    let (base, page_shutdown) = spawn_static(fixtures()).await;
    let regex_page = format!("^{}/page/.*$", regex_escape(&base));
    let policy = fixture_policy(&format!(
        r#"  - name: "page-allow"
    match: {{ kind: "regex", pattern: '{regex_page}' }}
    allowed_classes: ["agent-allowed"]"#
    ));
    let d = TestDaemon::spawn(policy).await;
    let id = open_session(&d, &format!("{base}/page/index.html")).await;
    let ws_url = format!(
        "ws://{}/sessions/{}/viewport?token={}",
        d.running.addr, id, d.token
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.expect("ws");
    wait_for_first_frame(&mut ws).await;

    // Click to focus the input.
    send(
        &mut ws,
        json!({"kind":"mouse","type":"down","x":170,"y":68,"button":"left"}),
    )
    .await;
    send(
        &mut ws,
        json!({"kind":"mouse","type":"up","x":170,"y":68,"button":"left"}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Simulate a Japanese IME composition + commit of "こんにちは".
    send(&mut ws, json!({"kind":"composition_start"})).await;
    send(&mut ws, json!({"kind":"composition_update","text":"こ"})).await;
    send(&mut ws, json!({"kind":"composition_update","text":"こん"})).await;
    send(
        &mut ws,
        json!({"kind":"composition_update","text":"こんにちは"}),
    )
    .await;
    send(
        &mut ws,
        json!({"kind":"composition_end","text":"こんにちは"}),
    )
    .await;

    tokio::time::sleep(Duration::from_millis(400)).await;
    let session = d.running.state.get_session(&id).await.unwrap();
    let value: serde_json::Value = session
        .page
        .execute(
            EvaluateParams::builder()
                .expression("document.getElementById('x').value")
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
    assert_eq!(
        value.as_str(),
        Some("こんにちは"),
        "IME commit should land in input"
    );

    let _ = page_shutdown.send(());
    d.shutdown().await;
}

async fn open_session(d: &TestDaemon, url: &str) -> String {
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
        .json(&json!({ "url": url }))
        .send()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    id
}

async fn send<S>(ws: &mut S, body: serde_json::Value)
where
    S: futures::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    ws.send(Message::Text(body.to_string()))
        .await
        .expect("ws send");
}

async fn wait_for_first_frame<S>(ws: &mut S)
where
    S: futures::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(Ok(Message::Binary(b))) = ws.next().await {
            if !b.is_empty() {
                return;
            }
        }
    })
    .await;
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

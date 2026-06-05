// The default capture backend is Page.startScreencast (event-driven). This
// verifies it works end-to-end through the daemon: an animated page that
// repaints every frame pushes a stream of frames to a WS subscriber, not the
// single frame a static page would emit.
//
// (The raw-CDP frame rate is measured separately in
// examples/screencast_spike.rs — ~60fps. Here we only assert the daemon's
// screencast path forwards a healthy stream, capped by the configured fps.)

mod support;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use acb_policy::CompiledPolicy;
use futures::StreamExt;
use tokio_tungstenite::tungstenite::Message;

use support::{spawn_static, TestDaemon};

fn fixtures() -> HashMap<String, Vec<u8>> {
    let mut m = HashMap::new();
    // An animated page: requestAnimationFrame repaints a changing background
    // every frame, so Chromium composites continuously and screencast emits.
    m.insert(
        "/page/index.html".into(),
        br#"<!doctype html><html><body style="margin:0">
            <h1 id="h" style="font:60px sans-serif">frame 0</h1>
            <script>
              let n = 0;
              function tick() {
                n++;
                document.body.style.background =
                  'rgb(' + (n % 255) + ',' + ((n * 7) % 255) + ',60)';
                document.getElementById('h').textContent = 'frame ' + n;
                requestAnimationFrame(tick);
              }
              requestAnimationFrame(tick);
            </script>
        </body></html>"#
            .to_vec(),
    );
    m
}

fn policy(page_regex: &str) -> Arc<CompiledPolicy> {
    let yaml = format!(
        r#"
server:
  bind: "127.0.0.1"
  port: 39100
  log_file: "./logs/test.log"
  log_rotation: "never"
chromium:
  binary: null
  user_data_dir: "./var/test-profile"
  viewport: {{ width: 1024, height: 768 }}
  screencast: {{ format: "jpeg", quality: 60, max_fps: 30 }}
resource_policy:
  subresources_inherit_page: true
  always_block_schemes: ["javascript", "data", "file", "chrome", "about", "blob", "ws", "wss"]
  bypass_service_worker: true
rules:
  - name: "page-allow"
    match: {{ kind: "regex", pattern: '{page_regex}' }}
"#
    );
    Arc::new(acb_policy::load::load_policy(&yaml).expect("test policy"))
}

fn regex_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if "\\^$.|?*+()[]{}".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
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
        .json(&serde_json::json!({ "url": url }))
        .send()
        .await
        .unwrap();
    id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn animated_page_streams_many_frames() {
    // Default mode is screencast; be explicit so a stray env from the shell
    // doesn't flip us to poll.
    std::env::set_var("ACB_CAPTURE_MODE", "screencast");

    let (base, page_shutdown) = spawn_static(fixtures()).await;
    let regex_page = format!("^{}/page/.*$", regex_escape(&base));
    let d = TestDaemon::spawn(policy(&regex_page)).await;

    let id = open_session(&d, &format!("{base}/page/index.html")).await;
    let ws_url = format!(
        "ws://{}/sessions/{}/viewport?token={}",
        d.running.addr, id, d.token
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("ws connect");

    // Count frames over ~1.5s. An animated page must push a stream; the old
    // 6fps poll floor and a static page would both give far fewer.
    let mut count = 0usize;
    let _ = tokio::time::timeout(Duration::from_millis(1500), async {
        while let Some(msg) = ws.next().await {
            if let Ok(Message::Binary(b)) = msg {
                if !b.is_empty() {
                    count += 1;
                }
            }
        }
    })
    .await;

    assert!(
        count >= 10,
        "animated page should stream many frames via screencast; got {count}"
    );

    let _ = page_shutdown.send(());
    d.shutdown().await;
}

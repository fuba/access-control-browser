// Two screencast-throughput behaviours, end-to-end against a real Chromium:
//
//   1. WebP frames — when the operator configures `screencast.format: webp`,
//      the bytes on the wire are actually WebP (RIFF....WEBP), not JPEG.
//   2. Static-page dedupe — once a page stops repainting, the pump stops
//      re-broadcasting byte-identical frames, so a fresh subscriber sees a
//      first frame and then the stream goes quiet instead of pushing the
//      same image at full frame rate forever.

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
    // A deliberately static page: plain text, no animation, nothing
    // focusable (so no blinking caret to keep repainting).
    m.insert(
        "/page/index.html".into(),
        br#"<!doctype html><html><body style="margin:0;background:#fff">
            <h1 style="font:40px sans-serif;color:#123">static content</h1>
            <p style="font:20px sans-serif">nothing here repaints.</p>
        </body></html>"#
            .to_vec(),
    );
    m
}

/// Like support::fixture_policy but with `screencast.format: webp` and a
/// generous active fps so the no-dedupe baseline would be obviously large.
fn webp_policy(page_regex: &str) -> Arc<CompiledPolicy> {
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
  screencast: {{ format: "webp", quality: 70, max_fps: 12 }}
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

fn is_webp(b: &[u8]) -> bool {
    b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP"
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webp_frames_and_static_dedupe() {
    // WebP + adaptive-fps + dedupe live on the poll backend (the event-driven
    // screencast backend only encodes JPEG). Exercise the poll path here.
    // Safe to set process-wide: this is the only test in this binary.
    std::env::set_var("ACB_CAPTURE_MODE", "poll");

    let (base, page_shutdown) = spawn_static(fixtures()).await;
    let regex_page = format!("^{}/page/.*$", regex_escape(&base));
    let d = TestDaemon::spawn(webp_policy(&regex_page)).await;

    let id = open_session(&d, &format!("{base}/page/index.html")).await;
    // Let the page finish its initial paints before we subscribe, so the
    // dedupe has something stable to settle on.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let ws_url = format!(
        "ws://{}/sessions/{}/viewport?token={}",
        d.running.addr, id, d.token
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("ws connect");

    // Collect frames for a window. The first frame (cached) must be WebP.
    // We then watch how many *more* frames arrive once the page is static.
    let mut first: Option<Vec<u8>> = None;
    let mut total = 0usize;
    let deadline = Duration::from_millis(2500);
    let _ = tokio::time::timeout(deadline, async {
        while let Some(msg) = ws.next().await {
            if let Ok(Message::Binary(b)) = msg {
                if b.is_empty() {
                    continue;
                }
                if first.is_none() {
                    first = Some(b.clone());
                }
                total += 1;
            }
        }
    })
    .await;

    let first = first.expect("expected at least one frame");
    assert!(
        is_webp(&first),
        "frame should be WebP (RIFF/WEBP magic); got {:?}",
        &first[..first.len().min(16)]
    );

    // Without dedupe, a static page at 12fps over 2.5s would push ~30
    // identical frames. With dedupe the stream goes quiet after the page
    // settles, so we expect only a small handful (the cached frame plus a
    // few late repaints). Allow generous slack but well below the no-dedupe
    // baseline.
    assert!(
        total <= 8,
        "static page should stop resending identical frames; got {total}"
    );

    let _ = page_shutdown.send(());
    d.shutdown().await;
}

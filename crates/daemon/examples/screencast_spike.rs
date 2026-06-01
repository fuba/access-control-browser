// B2 spike: can we drive viewport frames in new-headless Chromium WITHOUT
// falling back to a blind `Page.captureScreenshot` poll?
//
// Two candidate mechanisms are measured against an animated page (a
// requestAnimationFrame loop that repaints every frame):
//
//   1. Page.startScreencast + Page.screencastFrame events.
//      The repo comment claims these never fire in Chromium 147 + new
//      headless. This reproduces (or refutes) that, empirically.
//
//   2. HeadlessExperimental.beginFrame { screenshot }.
//      Drives the compositor by hand and returns a screenshot synchronized
//      to that frame — a way to stay headless yet pull frames on demand.
//      May require begin-frame-control launch flags; set SPIKE_BFC=1 to add
//      `--enable-begin-frame-control` + `--run-all-compositor-stages-before-draw`.
//
// Usage:
//   cargo run --example screencast_spike            # default flags
//   SPIKE_BFC=1 cargo run --example screencast_spike # with begin-frame-control
//
// Output is a verdict line per mechanism: frame count, and for beginFrame the
// average round-trip latency and how many frames reported damage.

use std::time::{Duration, Instant};

use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::headless_experimental::{
    BeginFrameParams, ScreenshotParams, ScreenshotParamsFormat,
};
use chromiumoxide::cdp::browser_protocol::page::{
    EventScreencastFrame, ScreencastFrameAckParams, StartScreencastFormat, StartScreencastParams,
};
use futures::StreamExt;

// An animated page: repaint a changing background every frame and bump the
// title, so every composited frame genuinely differs.
const ANIMATED_HTML: &str = r#"<!doctype html><html><body style="margin:0">
<h1 id="h" style="font:60px sans-serif">frame 0</h1>
<script>
let n = 0;
function tick() {
  n++;
  document.body.style.background = 'rgb(' + (n % 255) + ',' + ((n*7) % 255) + ',60)';
  document.getElementById('h').textContent = 'frame ' + n;
  requestAnimationFrame(tick);
}
requestAnimationFrame(tick);
</script></body></html>"#;

// A static page: paints once, then nothing repaints.
const STATIC_HTML: &str = r#"<!doctype html><html><body style="margin:0;background:#fff">
<h1 style="font:60px sans-serif;color:#123">static</h1></body></html>"#;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let bfc = std::env::var("SPIKE_BFC").ok().as_deref() == Some("1");
    let tmp = tempfile::tempdir()?;
    let mut builder = BrowserConfig::builder()
        .new_headless_mode()
        .user_data_dir(tmp.path())
        .arg("no-sandbox")
        .arg("window-size=1024,768");
    if bfc {
        // These are the flags Chrome historically required for
        // HeadlessExperimental.beginFrame to actually drive the compositor.
        builder = builder
            .arg("enable-begin-frame-control")
            .arg("run-all-compositor-stages-before-draw")
            .arg("disable-new-content-rendering-timeout");
    }
    let cfg = builder
        .build()
        .map_err(|e| anyhow::anyhow!("config: {e}"))?;
    println!("launching new-headless chromium (SPIKE_BFC={})", bfc);

    let (browser, mut handler) = Browser::launch(cfg).await?;
    let _handle = tokio::spawn(async move { while (handler.next().await).is_some() {} });

    let data_url = format!("data:text/html,{}", urlencoding_min(ANIMATED_HTML));
    let page = browser.new_page(&data_url).await?;
    page.wait_for_navigation().await?;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // ---- Mechanism 1: Page.startScreencast + screencastFrame events ------
    {
        let mut stream = page.event_listener::<EventScreencastFrame>().await?;
        page.execute(
            StartScreencastParams::builder()
                .format(StartScreencastFormat::Jpeg)
                .quality(60)
                .every_nth_frame(1)
                .build(),
        )
        .await?;

        let mut count = 0u32;
        let started = Instant::now();
        let _ = tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(frame) = stream.next().await {
                count += 1;
                // Must ack or Chromium stops sending further frames.
                let _ = page
                    .execute(
                        ScreencastFrameAckParams::builder()
                            .session_id(frame.session_id)
                            .build()
                            .unwrap(),
                    )
                    .await;
            }
        })
        .await;
        let secs = started.elapsed().as_secs_f64();
        println!(
            "[startScreencast]  frames={count}  ({:.1} fps over {:.1}s)  => {}",
            count as f64 / secs,
            secs,
            if count == 0 {
                "DEAD (events never fired) — confirms the repo comment"
            } else {
                "ALIVE — events fired!"
            }
        );
    }

    // ---- Mechanism 1b: startScreencast on a STATIC page -----------------
    // Hypothesis for why the repo concluded screencast was "dead": it is
    // damage-driven, so a static page emits the initial paint then goes
    // quiet. Tested on about:blank / a static fixture that looks like zero
    // frames forever.
    {
        let static_url = format!("data:text/html,{}", urlencoding_min(STATIC_HTML));
        page.goto(&static_url).await?;
        page.wait_for_navigation().await?;
        tokio::time::sleep(Duration::from_millis(300)).await;

        let mut stream = page.event_listener::<EventScreencastFrame>().await?;
        page.execute(
            StartScreencastParams::builder()
                .format(StartScreencastFormat::Jpeg)
                .quality(60)
                .every_nth_frame(1)
                .build(),
        )
        .await?;
        let mut count = 0u32;
        let started = Instant::now();
        let _ = tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(frame) = stream.next().await {
                count += 1;
                let _ = page
                    .execute(
                        ScreencastFrameAckParams::builder()
                            .session_id(frame.session_id)
                            .build()
                            .unwrap(),
                    )
                    .await;
            }
        })
        .await;
        println!(
            "[screencast/static] frames={count} over {:.1}s  => few/no frames is CORRECT (damage-driven); explains the 'never fires' misread",
            started.elapsed().as_secs_f64(),
        );
        let _ = page
            .execute(chromiumoxide::cdp::browser_protocol::page::StopScreencastParams::default())
            .await;
        // Restore the animated page for the beginFrame test below.
        page.goto(&data_url).await?;
        page.wait_for_navigation().await?;
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    // ---- Mechanism 2: HeadlessExperimental.beginFrame { screenshot } -----
    {
        let target_fps = 20u64;
        let frame_interval = Duration::from_millis(1000 / target_fps);
        let mut ok = 0u32;
        let mut damaged = 0u32;
        let mut err: Option<String> = None;
        let mut total_latency = Duration::ZERO;
        let started = Instant::now();
        let deadline = started + Duration::from_secs(3);

        while Instant::now() < deadline {
            let t0 = Instant::now();
            let params = BeginFrameParams::builder()
                .screenshot(
                    ScreenshotParams::builder()
                        .format(ScreenshotParamsFormat::Jpeg)
                        .quality(60)
                        .build(),
                )
                .build();
            match page.execute(params).await {
                Ok(resp) => {
                    total_latency += t0.elapsed();
                    if resp.result.has_damage {
                        damaged += 1;
                    }
                    if resp.result.screenshot_data.is_some() {
                        ok += 1;
                    }
                }
                Err(e) => {
                    err = Some(format!("{e}"));
                    break;
                }
            }
            tokio::time::sleep(frame_interval).await;
        }
        let secs = started.elapsed().as_secs_f64();
        match err {
            Some(e) => println!(
                "[beginFrame]       ERROR after {ok} ok frames: {e}\n                   => beginFrame not usable in this mode (try SPIKE_BFC=1)"
            ),
            None => {
                let avg_ms = if ok > 0 {
                    total_latency.as_secs_f64() * 1000.0 / ok as f64
                } else {
                    0.0
                };
                println!(
                    "[beginFrame]       screenshots={ok}  damaged={damaged}  ({:.1} fps over {:.1}s, avg {:.1}ms/frame)  => {}",
                    ok as f64 / secs,
                    secs,
                    avg_ms,
                    if ok == 0 {
                        "no screenshot data returned"
                    } else {
                        "WORKS — frames pulled headless without a blind poll"
                    }
                );
            }
        }
    }

    Ok(())
}

// Minimal percent-encoding for the data: URL. Encodes the characters that
// would otherwise break a `data:text/html,...` URL. Good enough for a spike;
// not a general-purpose encoder.
fn urlencoding_min(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

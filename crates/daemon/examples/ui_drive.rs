// One-shot helper: drive the acb-daemon UI from a separate Chromium,
// type a URL into the location bar, click Open, wait for the viewport,
// and screenshot the whole UI. Useful for "is the UI actually rendering
// the live viewport?" smoke checks that go through the *web bundle*, not
// the daemon API alone.
//
// Usage:
//   UI_URL='http://127.0.0.1:39151/?token=XXX' \
//     cargo run --release --example ui_drive -- /tmp/ui.png https://www.yahoo.co.jp/

use std::time::Duration;

use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotParams;
use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
use futures::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let ui_url = std::env::var("UI_URL").expect("UI_URL=http://...?token=...");
    let out_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/ui-shot.png".to_string());
    let target = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "https://www.yahoo.co.jp/".to_string());

    let tmp = tempfile::tempdir()?;
    let cfg = BrowserConfig::builder()
        .new_headless_mode()
        .user_data_dir(tmp.path())
        .arg("--no-sandbox")
        .arg("--window-size=1400,1000")
        .build()
        .map_err(|e| anyhow::anyhow!("config: {e}"))?;
    let (browser, mut handler) = Browser::launch(cfg).await?;
    tokio::spawn(async move { while (handler.next().await).is_some() {} });

    let page = browser.new_page(&ui_url).await?;
    page.wait_for_navigation().await?;
    tokio::time::sleep(Duration::from_millis(1200)).await;

    // Fill the location bar via the prototype value-setter (so React's
    // synthetic onChange fires), then click the Open button.
    let js = format!(
        r#"(() => {{
            const input = document.querySelector('.topbar input');
            const btn = document.querySelector('.topbar button');
            if (!input || !btn) return 'no-topbar';
            const setter = Object.getOwnPropertyDescriptor(
              HTMLInputElement.prototype, 'value'
            ).set;
            setter.call(input, {target:?});
            input.dispatchEvent(new Event('input', {{ bubbles: true }}));
            // Give React a tick to enable the button, then click.
            return new Promise((res) => setTimeout(() => {{
                btn.click();
                res('clicked');
            }}, 200));
        }})()"#
    );
    let r = page
        .execute(
            EvaluateParams::builder()
                .expression(js)
                .return_by_value(true)
                .await_promise(true)
                .build()
                .map_err(|e| anyhow::anyhow!("evaluate: {e}"))?,
        )
        .await?;
    println!(
        "ui-submit: {:?}",
        r.result.result.value.as_ref().and_then(|v| v.as_str())
    );

    // Wait for screencast frames to start flowing.
    tokio::time::sleep(Duration::from_secs(8)).await;

    // Print the session id the UI created so we can probe it from the
    // outside after ui_drive exits.
    let sid = page
        .execute(
            EvaluateParams::builder()
                .expression(
                    r#"document.querySelector('[data-acb-sid]')?.getAttribute('data-acb-sid') || '<unknown>'"#,
                )
                .return_by_value(true)
                .build()
                .map_err(|e| anyhow::anyhow!("evaluate sid: {e}"))?,
        )
        .await?;
    println!(
        "session-id: {}",
        sid.result
            .result
            .value
            .as_ref()
            .and_then(|v| v.as_str())
            .unwrap_or("?")
    );

    // Inspect the live-viewport <img> element to see whether the blob
    // decoded into a real bitmap or stayed blank.
    let inspect = page
        .execute(
            EvaluateParams::builder()
                .expression(
                    r#"(() => {
                        const img = document.querySelector('img[alt="live viewport"]');
                        if (!img) return JSON.stringify({error: 'no-img'});
                        return JSON.stringify({
                            src_prefix: (img.src || '').slice(0, 80),
                            complete: img.complete,
                            naturalWidth: img.naturalWidth,
                            naturalHeight: img.naturalHeight,
                            currentSrc_prefix: (img.currentSrc || '').slice(0, 80),
                            offsetWidth: img.offsetWidth,
                            offsetHeight: img.offsetHeight,
                            clientWidth: img.clientWidth,
                            clientHeight: img.clientHeight
                        });
                    })()"#,
                )
                .return_by_value(true)
                .build()
                .map_err(|e| anyhow::anyhow!("evaluate: {e}"))?,
        )
        .await?;
    println!(
        "img-inspect: {}",
        inspect
            .result
            .result
            .value
            .as_ref()
            .and_then(|v| v.as_str())
            .unwrap_or("<no-value>")
    );

    // Sample 5 pixels from the actual <img> bitmap to find out whether
    // the *image data* is white, or whether something is occluding it.
    let pixels = page
        .execute(
            EvaluateParams::builder()
                .expression(
                    r#"(() => {
                        const img = document.querySelector('img[alt="live viewport"]');
                        if (!img || !img.complete || !img.naturalWidth) {
                            return JSON.stringify({error: 'img-not-ready'});
                        }
                        const c = document.createElement('canvas');
                        c.width = img.naturalWidth;
                        c.height = img.naturalHeight;
                        const ctx = c.getContext('2d');
                        ctx.drawImage(img, 0, 0);
                        const pts = [
                            [10, 10],
                            [img.naturalWidth/2|0, img.naturalHeight/2|0],
                            [img.naturalWidth-10, img.naturalHeight-10],
                            [100, 100],
                            [400, 200],
                        ];
                        const result = pts.map(([x, y]) => {
                            const d = ctx.getImageData(x, y, 1, 1).data;
                            return `(${x},${y})=[${d[0]},${d[1]},${d[2]}]`;
                        });
                        // Also dump computed style of overlays to see if
                        // something blacks/whites out the image.
                        const ov = document.querySelector(
                            'div[style*="cursor: crosshair"]'
                        );
                        const ta = document.querySelector('textarea');
                        const ovStyle = ov && getComputedStyle(ov);
                        const taStyle = ta && getComputedStyle(ta);
                        return JSON.stringify({
                            pixels: result,
                            overlayBg: ovStyle && ovStyle.backgroundColor,
                            overlayOpacity: ovStyle && ovStyle.opacity,
                            textareaBg: taStyle && taStyle.backgroundColor,
                            textareaOpacity: taStyle && taStyle.opacity,
                        });
                    })()"#,
                )
                .return_by_value(true)
                .build()
                .map_err(|e| anyhow::anyhow!("evaluate: {e}"))?,
        )
        .await?;
    println!(
        "pixel-probe: {}",
        pixels
            .result
            .result
            .value
            .as_ref()
            .and_then(|v| v.as_str())
            .unwrap_or("<no-value>")
    );

    // Fetch the current blob URL and base64 it so we can inspect what
    // the *UI* actually rendered (vs. what the WS sent on a separate
    // subscribe).
    let dump = page
        .execute(
            EvaluateParams::builder()
                .expression(
                    r#"(async () => {
                        const img = document.querySelector('img[alt="live viewport"]');
                        try {
                            const res = await fetch(img.src);
                            const buf = await res.arrayBuffer();
                            const bytes = new Uint8Array(buf);
                            let s = '';
                            for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]);
                            return JSON.stringify({ok: true, len: bytes.length, b64: btoa(s), srcLen: img.src.length});
                        } catch (e) {
                            return JSON.stringify({ok: false, err: String(e), src: img.src});
                        }
                    })()"#,
                )
                .return_by_value(true)
                .await_promise(true)
                .build()
                .map_err(|e| anyhow::anyhow!("evaluate dump: {e}"))?,
        )
        .await?;
    println!(
        "ui-dump-raw: {}",
        dump.result
            .result
            .value
            .as_ref()
            .and_then(|v| v.as_str())
            .unwrap_or("<no-value>")
            .chars()
            .take(200)
            .collect::<String>()
    );
    if let Some(v) = dump.result.result.value.as_ref().and_then(|v| v.as_str()) {
        let parsed: serde_json::Value = serde_json::from_str(v).unwrap_or_default();
        if let Some(b64) = parsed["b64"].as_str() {
            use base64::engine::general_purpose::STANDARD as B64;
            use base64::Engine as _;
            if let Ok(bytes) = B64.decode(b64) {
                std::fs::write("/tmp/ui-frame-dump.jpg", &bytes)?;
                println!(
                    "ui-frame-dump: {} bytes -> /tmp/ui-frame-dump.jpg",
                    bytes.len()
                );
            }
        }
    }

    let shot = page
        .execute(CaptureScreenshotParams {
            format: Some(chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat::Png),
            quality: None,
            clip: None,
            from_surface: Some(true),
            capture_beyond_viewport: Some(false),
            optimize_for_speed: Some(false),
        })
        .await?;
    let b64: &str = shot.data.as_ref();
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine as _;
    let bytes = B64.decode(b64)?;
    std::fs::write(&out_path, bytes)?;
    println!(
        "wrote {} ({} bytes)",
        out_path,
        std::fs::metadata(&out_path)?.len()
    );
    Ok(())
}

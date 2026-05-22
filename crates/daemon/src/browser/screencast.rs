// Live viewport frame producer.
//
// We tried `Page.startScreencast` first (it's the obvious CDP API), but
// in Chromium 147 + new-headless the screencastFrame events never fire
// even though startScreencast returns success. Falling back to a periodic
// `Page.captureScreenshot` poll: more CPU per frame, but reliable across
// Chromium versions and headless modes.
//
// Idempotent: first WS subscriber for a session starts the pump;
// subsequent subscribers share the broadcast channel and the cached
// most-recent frame.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use chromiumoxide::cdp::browser_protocol::page::{
    CaptureScreenshotFormat, CaptureScreenshotParams, StartScreencastFormat,
};
use tracing::warn;

use crate::browser::session::Session;

#[derive(Debug, Clone)]
pub struct ScreencastConfig {
    pub format: StartScreencastFormat,
    pub quality: i64,
    pub max_fps: u8,
    pub max_width: Option<i64>,
    pub max_height: Option<i64>,
}

impl Default for ScreencastConfig {
    fn default() -> Self {
        Self {
            format: StartScreencastFormat::Jpeg,
            quality: 60,
            max_fps: 6,
            max_width: None,
            max_height: None,
        }
    }
}

pub async fn ensure_started(session: &Arc<Session>, cfg: ScreencastConfig) -> Result<()> {
    {
        let mut started = session.screencast_started.write().await;
        if *started {
            return Ok(());
        }
        *started = true;
    }

    let format = match cfg.format {
        StartScreencastFormat::Jpeg => CaptureScreenshotFormat::Jpeg,
        StartScreencastFormat::Png => CaptureScreenshotFormat::Png,
    };
    let interval = Duration::from_millis(1000u64 / cfg.max_fps.max(1) as u64);
    let s = session.clone();
    let task = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;

            // Skip capturing if nobody is watching AND there's a cached
            // frame — saves CPU on idle sessions.
            let any_subscribers = s.viewport_frames.receiver_count() > 0;
            if !any_subscribers && s.last_frame.read().await.is_some() {
                continue;
            }

            let params = CaptureScreenshotParams {
                format: Some(format.clone()),
                quality: Some(cfg.quality),
                clip: None,
                from_surface: Some(true),
                capture_beyond_viewport: Some(false),
                optimize_for_speed: Some(true),
            };
            let res = match s.page.execute(params).await {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = ?e, "captureScreenshot failed; pump backing off");
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
            };
            let b64: &str = res.data.as_ref();
            let bytes = match B64.decode(b64) {
                Ok(b) => b,
                Err(e) => {
                    warn!(error = ?e, "frame base64 decode failed");
                    continue;
                }
            };
            // Drop "compositor-transient" blanks. Chromium's
            // captureScreenshot occasionally returns a tiny uniform
            // image (~3.6 KB JPEG of solid white) when called between a
            // navigation's compositor swap. Without this filter, a
            // blank that lands as the LAST capture before the WS
            // subscriber count drops sticks in last_frame and gets sent
            // first to the next subscriber, leaving the UI white even
            // though the page is in fact rendered.
            //
            // 4 KB is just above the empirically observed pure-uniform
            // size (~3.6 KB) and well below any page with even one
            // visible widget. Tests use trivial fixture pages whose
            // JPEGs sit near this threshold, so we don't go higher.
            if bytes.len() < 4_000 {
                continue;
            }
            *s.last_frame.write().await = Some(bytes.clone());
            let _ = s.viewport_frames.send(bytes);
        }
    });
    session.push_task(task).await;
    Ok(())
}

// Live viewport frame producer.
//
// Two capture backends, selected by the `ACB_CAPTURE_MODE` env var:
//
//   * "screencast" (DEFAULT) — Page.startScreencast + Page.screencastFrame.
//     Chromium pushes a frame every time the page composites a new one, so
//     this is event-driven: up to ~60fps while the page animates, and zero
//     work while it is static (damage-driven). Measured at 60fps in
//     Chromium 147 + new-headless — see examples/screencast_spike.rs. An
//     earlier code comment claimed these events "never fire" in new
//     headless; the spike shows that was a misread of correct damage-driven
//     behaviour on a *static* test page (which emits one frame then goes
//     quiet). startScreencast only encodes JPEG/PNG, so a `webp` config maps
//     to JPEG here.
//
//   * "poll" — periodic Page.captureScreenshot. The portable fallback: more
//     CPU per frame and capped at the configured fps, but works even if a
//     future Chromium regresses screencast. Supports WebP. Frames are
//     deduped (hash) and the cadence adapts (active fps while changing,
//     idle fps once static) to keep it cheap.
//
// Both backends feed the same downstream: a hash-dedupe + blank filter, the
// `last_frame` cache (so a fresh WS subscriber gets the current image even
// when the page is static and nothing new will be pushed), and the
// `viewport_frames` broadcast that the WS handler forwards.
//
// Idempotent: the first WS subscriber for a session starts the pump;
// subsequent subscribers share the broadcast channel and the cache.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use chromiumoxide::cdp::browser_protocol::page::{
    CaptureScreenshotFormat, CaptureScreenshotParams, EventScreencastFrame,
    ScreencastFrameAckParams, StartScreencastFormat, StartScreencastParams,
};
use chromiumoxide::Page;
use futures::StreamExt;
use tracing::{info, warn};
use xxhash_rust::xxh3::xxh3_64;

use crate::browser::session::Session;

/// After this many consecutive byte-identical frames the poll backend treats
/// the page as static and slows the capture cadence to `idle_fps`.
const IDLE_AFTER_UNCHANGED: u32 = 3;

/// Frames smaller than this are treated as compositor-transient blanks and
/// dropped. Chromium's capture occasionally returns a tiny uniform image
/// (~3.6 KB JPEG of solid white) mid-navigation; if such a blank lands in
/// `last_frame` it would be the first thing a new subscriber sees, leaving
/// the UI white even though the page is rendered. 4 KB is just above the
/// observed pure-uniform size and below any page with a visible widget.
const MIN_FRAME_BYTES: usize = 4_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    Screencast,
    Poll,
}

impl CaptureMode {
    /// Read from `ACB_CAPTURE_MODE`; defaults to the high-throughput
    /// event-driven screencast backend.
    fn from_env() -> Self {
        Self::parse(std::env::var("ACB_CAPTURE_MODE").ok().as_deref())
    }

    /// Parse a capture-mode string. An unrecognized value is loud (it's
    /// almost certainly a typo) but non-fatal — we fall back to the default
    /// rather than refuse to serve the viewport.
    fn parse(v: Option<&str>) -> Self {
        match v {
            Some("poll") => CaptureMode::Poll,
            Some("screencast") | Some("") | None => CaptureMode::Screencast,
            Some(other) => {
                warn!(mode = other, "unknown ACB_CAPTURE_MODE; using screencast");
                CaptureMode::Screencast
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScreencastConfig {
    pub format: CaptureScreenshotFormat,
    pub quality: i64,
    /// Poll: capture rate while the page is repainting. Screencast: an upper
    /// bound on how often frames are forwarded (Chromium may produce more;
    /// we still ack every one but only broadcast at this cadence to bound
    /// bandwidth on slow links).
    pub active_fps: u8,
    /// Poll only: capture rate once the page has gone visually static.
    pub idle_fps: u8,
    pub max_width: Option<i64>,
    pub max_height: Option<i64>,
}

impl Default for ScreencastConfig {
    fn default() -> Self {
        Self {
            format: CaptureScreenshotFormat::Jpeg,
            quality: 60,
            active_fps: 8,
            idle_fps: 2,
            max_width: None,
            max_height: None,
        }
    }
}

impl ScreencastConfig {
    /// Build from the operator's `config.yaml` `chromium.screencast` block so
    /// the live viewport honours the configured format / quality / fps
    /// instead of a hardcoded default.
    pub fn from_policy(c: &acb_policy::ScreencastConfig) -> Self {
        let format = match c.format {
            acb_policy::ScreencastFormat::Jpeg => CaptureScreenshotFormat::Jpeg,
            acb_policy::ScreencastFormat::Png => CaptureScreenshotFormat::Png,
            acb_policy::ScreencastFormat::Webp => CaptureScreenshotFormat::Webp,
        };
        let active_fps = c.max_fps.max(1);
        Self {
            format,
            quality: c.quality as i64,
            active_fps,
            // Never idle faster than we tick when active; cap at 2fps so a
            // static page barely registers on the CPU.
            idle_fps: active_fps.min(2),
            max_width: None,
            max_height: None,
        }
    }
}

/// Poll cadence given how many identical frames we've seen in a row.
fn frame_delay(cfg: &ScreencastConfig, unchanged_streak: u32) -> Duration {
    let fps = if unchanged_streak >= IDLE_AFTER_UNCHANGED {
        cfg.idle_fps
    } else {
        cfg.active_fps
    };
    Duration::from_millis(1000u64 / fps.max(1) as u64)
}

/// Ack a screencast frame (so Chromium keeps producing) and base64-decode its
/// payload. Returns None on decode failure; the frame is still acked.
async fn ack_and_decode(page: &Page, frame: &EventScreencastFrame) -> Option<Vec<u8>> {
    let _ = page
        .execute(ScreencastFrameAckParams::new(frame.session_id))
        .await;
    let b64: &str = frame.data.as_ref();
    match B64.decode(b64) {
        Ok(b) => Some(b),
        Err(e) => {
            warn!(error = ?e, "screencast frame base64 decode failed");
            None
        }
    }
}

/// Apply the blank filter + hash dedupe, then cache and broadcast the frame.
/// Returns true if the frame was actually published (new + non-blank).
async fn publish_frame(s: &Arc<Session>, bytes: Vec<u8>, last_hash: &mut Option<u64>) -> bool {
    // Drop transient blanks (see MIN_FRAME_BYTES) — but never the very first
    // frame: a legitimately tiny page would otherwise stay black forever
    // because nothing larger will ever follow on a static page.
    if bytes.len() < MIN_FRAME_BYTES && last_hash.is_some() {
        return false;
    }
    let hash = xxh3_64(&bytes);
    if *last_hash == Some(hash) {
        return false;
    }
    *last_hash = Some(hash);
    *s.last_frame.write().await = Some(bytes.clone());
    let _ = s.viewport_frames.send(bytes);
    true
}

pub async fn ensure_started(session: &Arc<Session>, cfg: ScreencastConfig) -> Result<()> {
    {
        let mut started = session.screencast_started.write().await;
        if *started {
            return Ok(());
        }
        *started = true;
    }

    let mode = CaptureMode::from_env();
    match mode {
        CaptureMode::Screencast => start_screencast(session, cfg).await?,
        CaptureMode::Poll => start_poll(session, cfg).await,
    }
    Ok(())
}

/// Event-driven backend: Chromium pushes a frame on every composite.
async fn start_screencast(session: &Arc<Session>, cfg: ScreencastConfig) -> Result<()> {
    // startScreencast can't encode WebP; fall back to JPEG and say so once.
    let sc_format = match cfg.format {
        CaptureScreenshotFormat::Png => StartScreencastFormat::Png,
        CaptureScreenshotFormat::Webp => {
            info!("screencast backend doesn't support webp; using jpeg (set ACB_CAPTURE_MODE=poll for webp)");
            StartScreencastFormat::Jpeg
        }
        _ => StartScreencastFormat::Jpeg,
    };

    // Subscribe BEFORE starting so we never miss the first frame.
    let mut stream = session
        .page
        .event_listener::<EventScreencastFrame>()
        .await?;
    let mut params = StartScreencastParams::builder()
        .format(sc_format)
        .quality(cfg.quality)
        .every_nth_frame(1);
    if let Some(w) = cfg.max_width {
        params = params.max_width(w);
    }
    if let Some(h) = cfg.max_height {
        params = params.max_height(h);
    }
    session.page.execute(params.build()).await?;

    // Forward frames at up to active_fps to bound bandwidth. This is a
    // trailing-edge rate limiter: every frame is acked immediately (or
    // Chromium stops producing), but only the NEWEST frame in each interval
    // is forwarded — and it is always eventually forwarded, so the final
    // frame of a burst is never dropped (no stale viewport).
    let min_interval = Duration::from_millis(1000u64 / cfg.active_fps.max(1) as u64);
    let s = session.clone();
    let task = tokio::spawn(async move {
        let mut last_hash: Option<u64> = None;
        let mut last_sent = Instant::now()
            .checked_sub(min_interval)
            .unwrap_or_else(Instant::now);
        // Newest decoded-but-not-yet-forwarded frame.
        let mut pending: Option<Vec<u8>> = None;
        loop {
            if let Some(bytes) = pending.take() {
                let since = last_sent.elapsed();
                if since < min_interval {
                    // Slot not open yet. Wait for it, but keep accepting
                    // newer frames meanwhile (newest wins).
                    pending = Some(bytes);
                    tokio::select! {
                        _ = tokio::time::sleep(min_interval - since) => {}
                        next = stream.next() => match next {
                            Some(frame) => {
                                if let Some(b) = ack_and_decode(&s.page, &frame).await {
                                    pending = Some(b);
                                }
                            }
                            None => break,
                        },
                    }
                    continue;
                }
                // Slot open: forward the newest pending frame.
                if publish_frame(&s, bytes, &mut last_hash).await {
                    last_sent = Instant::now();
                }
                continue;
            }
            // Nothing pending: block for the next frame.
            match stream.next().await {
                Some(frame) => {
                    if let Some(b) = ack_and_decode(&s.page, &frame).await {
                        pending = Some(b);
                    }
                }
                None => break,
            }
        }
    });
    session.push_task(task).await;
    Ok(())
}

/// Portable fallback: periodic captureScreenshot with dedupe + adaptive fps.
async fn start_poll(session: &Arc<Session>, cfg: ScreencastConfig) {
    let s = session.clone();
    let task = tokio::spawn(async move {
        let mut last_hash: Option<u64> = None;
        let mut unchanged_streak: u32 = 0;
        loop {
            tokio::time::sleep(frame_delay(&cfg, unchanged_streak)).await;

            // Skip capturing if nobody is watching AND there's a cached
            // frame — saves CPU on idle sessions. Let the cadence stretch
            // toward idle while we wait.
            let any_subscribers = s.viewport_frames.receiver_count() > 0;
            if !any_subscribers && s.last_frame.read().await.is_some() {
                unchanged_streak = unchanged_streak.saturating_add(1);
                continue;
            }

            let params = CaptureScreenshotParams {
                format: Some(cfg.format.clone()),
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
            // Unchanged (or blank) frames don't get published; slow the
            // cadence toward idle so a static page costs almost nothing.
            if publish_frame(&s, bytes, &mut last_hash).await {
                unchanged_streak = 0;
            } else {
                unchanged_streak = unchanged_streak.saturating_add(1);
            }
        }
    });
    session.push_task(task).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ScreencastConfig {
        ScreencastConfig {
            active_fps: 10,
            idle_fps: 2,
            ..Default::default()
        }
    }

    #[test]
    fn active_cadence_while_changing() {
        assert_eq!(frame_delay(&cfg(), 0), Duration::from_millis(100));
        assert_eq!(
            frame_delay(&cfg(), IDLE_AFTER_UNCHANGED - 1),
            Duration::from_millis(100)
        );
    }

    #[test]
    fn idle_cadence_once_static() {
        assert_eq!(
            frame_delay(&cfg(), IDLE_AFTER_UNCHANGED),
            Duration::from_millis(500)
        );
        assert_eq!(frame_delay(&cfg(), 999), Duration::from_millis(500));
    }

    #[test]
    fn from_policy_maps_format_and_derives_idle_fps() {
        let pol = acb_policy::ScreencastConfig {
            format: acb_policy::ScreencastFormat::Webp,
            quality: 70,
            max_fps: 8,
        };
        let c = ScreencastConfig::from_policy(&pol);
        assert!(matches!(c.format, CaptureScreenshotFormat::Webp));
        assert_eq!(c.quality, 70);
        assert_eq!(c.active_fps, 8);
        assert_eq!(c.idle_fps, 2);
    }

    #[test]
    fn from_policy_idle_never_exceeds_active() {
        let pol = acb_policy::ScreencastConfig {
            format: acb_policy::ScreencastFormat::Jpeg,
            quality: 60,
            max_fps: 1,
        };
        let c = ScreencastConfig::from_policy(&pol);
        assert_eq!(c.active_fps, 1);
        assert_eq!(c.idle_fps, 1);
    }

    #[test]
    fn capture_mode_parsing() {
        assert_eq!(CaptureMode::parse(None), CaptureMode::Screencast);
        assert_eq!(CaptureMode::parse(Some("")), CaptureMode::Screencast);
        assert_eq!(
            CaptureMode::parse(Some("screencast")),
            CaptureMode::Screencast
        );
        assert_eq!(CaptureMode::parse(Some("poll")), CaptureMode::Poll);
        // Unknown values fall back to the default rather than erroring.
        assert_eq!(CaptureMode::parse(Some("bogus")), CaptureMode::Screencast);
    }
}

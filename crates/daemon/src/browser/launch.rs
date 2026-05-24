// Launch Chromium under chromiumoxide and keep its handler task running for
// the lifetime of the daemon.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chromiumoxide::browser::{Browser, BrowserConfig};
use futures::StreamExt;
use tracing::warn;

#[derive(Clone)]
pub struct BrowserHandle {
    pub browser: Arc<Browser>,
}

pub async fn launch(
    binary: Option<PathBuf>,
    user_data_dir: Option<PathBuf>,
    headless: bool,
) -> Result<BrowserHandle> {
    let mut builder = BrowserConfig::builder();
    if let Some(b) = binary {
        builder = builder.chrome_executable(b);
    }
    if let Some(d) = user_data_dir {
        builder = builder.user_data_dir(d);
    }
    // Hardening flags. WebRTC and WebTransport disabled so they cannot
    // bypass our Fetch interceptor; background networking off to keep
    // request flow predictable; no-first-run / no-default-browser-check
    // so headless launches are quiet.
    //
    // IMPORTANT: chromiumoxide 0.9's `arg()` prepends `--` itself
    // (the string becomes the flag key and is rendered as `--{key}`).
    // Passing a leading `--` here produces a malformed `----no-sandbox`
    // that Chromium silently ignores — which on hosts without
    // unprivileged user namespaces (CI runners under AppArmor, default
    // Docker containers) makes Chromium abort with "No usable sandbox".
    // So these are written WITHOUT the leading dashes.
    let args = [
        // BackForwardCache disabled so back/forward always re-fetch the
        // document and the Fetch interceptor re-applies the allowlist —
        // otherwise a bfcache restore would bypass policy on history nav.
        "disable-features=WebRTC,WebTransport,SharedArrayBuffer,BackForwardCache",
        "disable-background-networking",
        "disable-component-update",
        "no-first-run",
        "no-default-browser-check",
        "disable-default-apps",
        "disable-dev-shm-usage",
        // Required when running as root or inside an unprivileged container
        // without user namespaces.
        "no-sandbox",
    ];
    for a in args {
        builder = builder.arg(a);
    }
    if headless {
        // "New" headless is the modern compositor-backed mode and is the
        // only one with reliable Page.startScreencast in recent Chromium.
        builder = builder.new_headless_mode();
    } else {
        builder = builder.with_head();
    }
    builder = builder
        .request_timeout(Duration::from_secs(30))
        // chromiumoxide's default is 20s. CI runners under D-Bus contention
        // sometimes need longer to settle Chromium's startup before
        // exposing the WebSocket URL. Doubling it cheaply removes a class
        // of flaky CI failures.
        .launch_timeout(Duration::from_secs(60));

    let config = builder
        .build()
        .map_err(|e| anyhow::anyhow!("invalid BrowserConfig: {e}"))?;

    let (browser, mut handler) = Browser::launch(config)
        .await
        .context("failed to launch chromium")?;

    // The chromiumoxide handler future must be polled for events to flow.
    // Spawn it; it ends when the browser closes.
    tokio::spawn(async move {
        while let Some(ev) = handler.next().await {
            if let Err(e) = ev {
                warn!(error = ?e, "chromium handler error");
            }
        }
    });

    Ok(BrowserHandle {
        browser: Arc::new(browser),
    })
}

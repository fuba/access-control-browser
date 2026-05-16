// Hot reload of the policy file. Spawns a notify watcher (with a polling
// fallback for bind-mounted volumes on hosts where inotify doesn't
// propagate) and atomically swaps the AppState's ArcSwap<CompiledPolicy>
// on every successful load. Parse errors keep the previous policy in
// effect and emit a `policy.reload_failed` activity event.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use notify::Watcher;
use notify_debouncer_full::new_debouncer;
use tracing::{error, info};

use crate::events::{now_unix, ActivityEvent};
use crate::AppState;

/// Spawn the watcher. `path` is the policy file. `poll_ms = 0` means use
/// inotify; nonzero forces polling. Returns a guard task you should hold
/// for the lifetime of the daemon.
pub fn spawn(path: PathBuf, poll_ms: u64, state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let debouncer_timeout = Duration::from_millis(300);
        let mut debouncer = match new_debouncer(debouncer_timeout, None, move |res| {
            let _ = tx.blocking_send(res);
        }) {
            Ok(d) => d,
            Err(e) => {
                error!(error = ?e, "failed to start notify debouncer");
                return;
            }
        };
        if poll_ms == 0 {
            if let Err(e) = debouncer
                .watcher()
                .watch(&path, notify::RecursiveMode::NonRecursive)
            {
                error!(error = ?e, path = ?path, "failed to watch policy file (falling back to poll loop)");
                // Fall back to a manual polling loop.
                poll_loop(path, 1000, state).await;
                return;
            }
            info!(path = ?path, "policy hot-reload watcher active (inotify)");
            while let Some(res) = rx.recv().await {
                if let Ok(events) = res {
                    for e in events {
                        for p in &e.paths {
                            if p == &path {
                                reload_once(&path, &state).await;
                            }
                        }
                    }
                }
            }
        } else {
            info!(path = ?path, poll_ms, "policy hot-reload watcher active (poll)");
            poll_loop(path, poll_ms, state).await;
        }
    })
}

async fn poll_loop(path: PathBuf, poll_ms: u64, state: AppState) {
    let mut last = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    let interval = Duration::from_millis(poll_ms);
    loop {
        tokio::time::sleep(interval).await;
        let cur = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if cur != last {
            last = cur;
            reload_once(&path, &state).await;
        }
    }
}

async fn reload_once(path: &PathBuf, state: &AppState) {
    let yaml = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            let _ = state.events().send(ActivityEvent::PolicyReloadFailed {
                ts: now_unix(),
                error: format!("read {}: {e}", path.display()),
            });
            return;
        }
    };
    match acb_policy::load::load_policy(&yaml) {
        Ok(new_policy) => {
            let etag = new_policy.etag.clone();
            state.swap_policy(Arc::new(new_policy));
            info!(etag, "policy reloaded");
            let _ = state.events().send(ActivityEvent::PolicyReloaded {
                ts: now_unix(),
                etag,
            });
        }
        Err(e) => {
            error!(error = %e, "policy reload failed, keeping previous");
            let _ = state.events().send(ActivityEvent::PolicyReloadFailed {
                ts: now_unix(),
                error: e.to_string(),
            });
        }
    }
}

/// Force a single reload. Returns the resulting etag on success.
pub async fn reload_now(path: &PathBuf, state: &AppState) -> anyhow::Result<String> {
    let yaml = std::fs::read_to_string(path)?;
    let new_policy = acb_policy::load::load_policy(&yaml)?;
    let etag = new_policy.etag.clone();
    state.swap_policy(Arc::new(new_policy));
    let _ = state.events().send(ActivityEvent::PolicyReloaded {
        ts: now_unix(),
        etag: etag.clone(),
    });
    Ok(etag)
}

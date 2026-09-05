// Hot reload of the policy file. Spawns a notify watcher (with a polling
// fallback for bind-mounted volumes on hosts where inotify doesn't
// propagate) and atomically swaps the AppState's ArcSwap<CompiledPolicy>
// on every successful load. Parse errors, signature failures and revision
// rollbacks all keep the previous policy in effect and emit a
// `policy.reload_failed` activity event.
//
// Both `config.yaml` and `config.yaml.sig` are watched: a signed edit lands
// as two writes, and whichever arrives last must trigger the load that sees
// the consistent pair.

use std::path::{Path, PathBuf};
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
        if poll_ms != 0 {
            info!(path = ?path, poll_ms, "policy hot-reload watcher active (poll)");
            poll_loop(path, poll_ms, state).await;
            return;
        }

        // Watch the parent DIRECTORY and match events by file name, rather
        // than watching the file itself. Two bugs this fixes:
        //   * notify reports event paths in absolute form, so the old
        //     `event_path == path` check never matched when the daemon was
        //     launched with a relative `--config config.yaml`, and inotify
        //     hot-reload silently never fired.
        //   * editors (and our own tooling) save via write-temp + rename,
        //     which swaps the file's inode; a watch on the file itself is
        //     lost on the swap, while a watch on the directory still sees the
        //     rename and we match the replacement by name.
        let target = match path.canonicalize() {
            Ok(p) => p,
            Err(e) => {
                error!(error = ?e, path = ?path, "cannot resolve policy path; falling back to poll loop");
                poll_loop(path, 1000, state).await;
                return;
            }
        };
        let sig_name = crate::policy_file::sig_path(&target)
            .file_name()
            .map(|n| n.to_owned());
        let (dir, file_name) = match (target.parent(), target.file_name()) {
            (Some(d), Some(n)) => (d.to_path_buf(), n.to_owned()),
            _ => {
                error!(path = ?target, "policy path has no parent/file name; falling back to poll loop");
                poll_loop(path, 1000, state).await;
                return;
            }
        };

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
        if let Err(e) = debouncer
            .watcher()
            .watch(&dir, notify::RecursiveMode::NonRecursive)
        {
            error!(error = ?e, dir = ?dir, "failed to watch policy directory (falling back to poll loop)");
            poll_loop(path, 1000, state).await;
            return;
        }
        info!(path = ?target, "policy hot-reload watcher active (inotify, dir-watch)");
        while let Some(res) = rx.recv().await {
            if let Ok(events) = res {
                let touched = events.iter().any(|e| {
                    e.paths.iter().any(|p| {
                        let n = p.file_name();
                        n == Some(file_name.as_os_str())
                            || (n.is_some() && n == sig_name.as_deref())
                    })
                });
                if touched {
                    reload_once(&path, &state).await;
                }
            }
        }
    })
}

async fn poll_loop(path: PathBuf, poll_ms: u64, state: AppState) {
    let sig = crate::policy_file::sig_path(&path);
    let stamp = |p: &Path, s: &Path| {
        (
            std::fs::metadata(p).and_then(|m| m.modified()).ok(),
            std::fs::metadata(s).and_then(|m| m.modified()).ok(),
        )
    };
    let mut last = stamp(&path, &sig);
    let interval = Duration::from_millis(poll_ms);
    loop {
        tokio::time::sleep(interval).await;
        let cur = stamp(&path, &sig);
        if cur != last {
            last = cur;
            reload_once(&path, &state).await;
        }
    }
}

async fn reload_once(path: &Path, state: &AppState) {
    if let Err(e) = reload_now(path, state).await {
        error!(error = %format!("{e:#}"), "policy reload failed, keeping previous");
        let _ = state.events().send(ActivityEvent::PolicyReloadFailed {
            ts: now_unix(),
            error: format!("{e:#}"),
        });
    }
}

/// Force a single reload: read, verify (when verify keys are set), compile,
/// swap. Returns the resulting etag on success; on any failure the previous
/// policy stays in effect.
pub async fn reload_now(path: &Path, state: &AppState) -> anyhow::Result<String> {
    let keys = state.verify_keys();
    let current_revision = state.policy().revision;
    let loaded = crate::policy_file::load(path, keys.as_ref(), Some(current_revision))?;
    let etag = loaded.policy.etag.clone();
    let revision = loaded.policy.revision;
    let signer = loaded.signer.map(|s| s.fingerprint);
    state.swap_policy(Arc::new(loaded.policy));
    info!(etag, revision, signer = ?signer, "policy reloaded");
    let _ = state.events().send(ActivityEvent::PolicyReloaded {
        ts: now_unix(),
        etag: etag.clone(),
        revision,
        signer,
    });
    Ok(etag)
}

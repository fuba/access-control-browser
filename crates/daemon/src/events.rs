// Activity feed events. Broadcast to any number of SSE subscribers and to the
// daemon's structured log. Each event is small JSON; consumers do their own
// formatting.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActivityEvent {
    Navigated {
        ts: u64,
        session: String,
        url: String,
        rule: String,
    },
    Blocked {
        ts: u64,
        session: String,
        url: String,
        reason: String,
        kind: String,
    },
    PolicyReloaded {
        ts: u64,
        etag: String,
    },
    PolicyReloadFailed {
        ts: u64,
        error: String,
    },
    SessionOpened {
        ts: u64,
        session: String,
    },
    SessionClosed {
        ts: u64,
        session: String,
    },
    /// Live URL/title change for a session (top-level navigation, in-page
    /// SPA route change, or title update). Drives the UI location bar and
    /// tab labels without polling. Distinct from `Navigated`, which only
    /// fires on allowlist-passing top-level document loads and carries the
    /// matched rule.
    SessionUrl {
        ts: u64,
        session: String,
        url: String,
        title: Option<String>,
    },
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

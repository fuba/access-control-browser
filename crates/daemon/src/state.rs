// Shared mutable state for the daemon. Three concerns live here:
//   - the current policy (hot-swappable via ArcSwap so the request
//     interceptor always reads the live version)
//   - the bearer token expected by the auth middleware
//   - the browser handle and per-session map (sessions created in M2+)
//   - a broadcast channel for the activity feed (SSE consumers in M2+)

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use tokio::sync::{broadcast, RwLock};

use acb_policy::CompiledPolicy;

use crate::browser::launch::BrowserHandle;
use crate::browser::session::Session;
use crate::events::ActivityEvent;

#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    pub policy: ArcSwap<CompiledPolicy>,
    pub token: String,
    pub browser: RwLock<Option<BrowserHandle>>,
    pub sessions: RwLock<HashMap<String, Arc<Session>>>,
    pub events: broadcast::Sender<ActivityEvent>,
}

impl AppState {
    pub fn new(policy: Arc<CompiledPolicy>, token: String) -> Self {
        let (tx, _) = broadcast::channel(256);
        Self {
            inner: Arc::new(Inner {
                policy: ArcSwap::new(policy),
                token,
                browser: RwLock::new(None),
                sessions: RwLock::new(HashMap::new()),
                events: tx,
            }),
        }
    }

    pub fn policy(&self) -> Arc<CompiledPolicy> {
        self.inner.policy.load_full()
    }

    pub fn swap_policy(&self, new_policy: Arc<CompiledPolicy>) {
        self.inner.policy.store(new_policy);
    }

    pub fn token(&self) -> &str {
        &self.inner.token
    }

    pub async fn attach_browser(&self, handle: BrowserHandle) {
        *self.inner.browser.write().await = Some(handle);
    }

    pub async fn browser_clone(&self) -> Option<BrowserHandle> {
        self.inner.browser.read().await.clone()
    }

    pub fn events(&self) -> broadcast::Sender<ActivityEvent> {
        self.inner.events.clone()
    }

    pub async fn put_session(&self, id: String, session: Arc<Session>) {
        self.inner.sessions.write().await.insert(id, session);
    }

    pub async fn get_session(&self, id: &str) -> Option<Arc<Session>> {
        self.inner.sessions.read().await.get(id).cloned()
    }

    pub async fn drop_session(&self, id: &str) -> bool {
        self.inner.sessions.write().await.remove(id).is_some()
    }
}

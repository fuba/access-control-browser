// A Session = one chromiumoxide Page plus all the state the daemon tracks
// for it.

use std::sync::Arc;

use chromiumoxide::Page;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

use crate::snapshot::ref_table::RefTable;

pub struct Session {
    pub id: String,
    pub page: Page,
    /// Top-level page URL, updated on `Page.frameNavigated`. Used by the
    /// request interceptor to apply the inherit-page subresource rule.
    pub current_url: RwLock<Option<String>>,
    /// @eN ref allocator and stale-generation tracker.
    pub ref_table: RefTable,
    /// Background tasks (Fetch interceptor, frame tracker). Kept here so
    /// dropping the session aborts them.
    pub _tasks: RwLock<Vec<JoinHandle<()>>>,
}

impl Session {
    pub fn new(id: String, page: Page) -> Arc<Self> {
        Arc::new(Self {
            id,
            page,
            current_url: RwLock::new(None),
            ref_table: RefTable::new(),
            _tasks: RwLock::new(Vec::new()),
        })
    }

    pub async fn push_task(self: &Arc<Self>, handle: JoinHandle<()>) {
        self._tasks.write().await.push(handle);
    }

    pub async fn shutdown(&self) {
        let tasks = std::mem::take(&mut *self._tasks.write().await);
        for t in tasks {
            t.abort();
        }
    }
}

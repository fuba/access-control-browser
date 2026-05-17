// A Session = one chromiumoxide Page plus all the state the daemon tracks
// for it.

use std::sync::Arc;

use chromiumoxide::Page;
use tokio::sync::{broadcast, RwLock};
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
    /// Broadcast of decoded screencast frames (JPEG bytes). WS subscribers
    /// on `/sessions/:id/viewport` read from this; producers are the
    /// `Page.screencastFrame` event listener started on demand.
    pub viewport_frames: broadcast::Sender<Vec<u8>>,
    /// Cache of the most recent frame so a fresh WS subscriber gets
    /// something to render immediately. Chromium's screencast won't
    /// re-emit a frame until the compositor next repaints, which on a
    /// static page may be never.
    pub last_frame: RwLock<Option<Vec<u8>>>,
    /// Set to true once the screencast pump is started, so we don't
    /// double-start it when multiple WS subscribers connect.
    pub screencast_started: RwLock<bool>,
    /// Background tasks (Fetch interceptor, frame tracker, screencast
    /// pump). Kept here so dropping the session aborts them.
    pub _tasks: RwLock<Vec<JoinHandle<()>>>,
}

impl Session {
    pub fn new(id: String, page: Page) -> Arc<Self> {
        let (tx, _) = broadcast::channel(32);
        Arc::new(Self {
            id,
            page,
            current_url: RwLock::new(None),
            ref_table: RefTable::new(),
            viewport_frames: tx,
            last_frame: RwLock::new(None),
            screencast_started: RwLock::new(false),
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

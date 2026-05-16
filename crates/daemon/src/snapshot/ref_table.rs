// `@eN` ref allocation table. Per-session, monotonic across snapshots so an
// old ref's generation can be detected even after its number has elapsed.
// Stale refs (entry.generation != current_generation) are rejected with
// 410 Gone by the action handlers.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, RwLock};

use regex::Regex;

fn ref_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^@e[0-9]+$").unwrap())
}

pub fn is_ref(s: &str) -> bool {
    ref_regex().is_match(s)
}

#[derive(Debug, Clone, Copy)]
pub struct RefEntry {
    pub generation: u64,
    pub acb_id: u64,
}

#[derive(Default)]
pub struct RefTable {
    generation: AtomicU64,
    next_num: AtomicU64,
    map: RwLock<HashMap<String, RefEntry>>,
}

impl RefTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn current_generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    pub fn next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Allocate a new monotonic ref for the current generation. Returns the
    /// ref string (e.g. "@e42").
    pub fn allocate(&self, acb_id: u64) -> String {
        let g = self.current_generation();
        let n = self.next_num.fetch_add(1, Ordering::SeqCst) + 1;
        let key = format!("@e{n}");
        self.map.write().unwrap().insert(
            key.clone(),
            RefEntry {
                generation: g,
                acb_id,
            },
        );
        key
    }

    pub fn get(&self, key: &str) -> Option<RefEntry> {
        self.map.read().unwrap().get(key).copied()
    }
}

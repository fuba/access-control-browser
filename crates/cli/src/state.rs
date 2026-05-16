// Tiny on-disk state file so successive CLI invocations share a session.
// Lives in $XDG_RUNTIME_DIR (or /tmp) alongside the daemon's token file.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CliState {
    pub session_id: Option<String>,
}

pub fn state_path() -> PathBuf {
    if let Ok(d) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(d).join("access-control-browser.cli.json");
    }
    PathBuf::from("/tmp/access-control-browser.cli.json")
}

pub fn load() -> Result<CliState> {
    let p = state_path();
    if !p.exists() {
        return Ok(CliState::default());
    }
    let s = std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
    Ok(serde_json::from_str(&s).unwrap_or_default())
}

pub fn save(s: &CliState) -> Result<()> {
    let p = state_path();
    let body = serde_json::to_string(s)?;
    std::fs::write(&p, body).with_context(|| format!("write {}", p.display()))?;
    Ok(())
}

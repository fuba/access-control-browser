// Thin HTTP client wrapper. Reads the daemon's token from a file (or env)
// and presents a small surface for the subcommand modules.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use reqwest::{Client, RequestBuilder};
use serde::Serialize;

pub struct Daemon {
    pub base: String,
    pub token: String,
    pub client: Client,
}

impl Daemon {
    pub fn connect(base: String, token_file: Option<PathBuf>) -> Result<Self> {
        let token = read_token(token_file)?;
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()?;
        Ok(Self {
            base,
            token,
            client,
        })
    }

    pub fn get(&self, path: &str) -> RequestBuilder {
        self.client
            .get(format!("{}{}", self.base, path))
            .bearer_auth(&self.token)
    }

    pub fn post(&self, path: &str) -> RequestBuilder {
        self.client
            .post(format!("{}{}", self.base, path))
            .bearer_auth(&self.token)
    }

    pub async fn post_json<T: Serialize>(&self, path: &str, body: &T) -> Result<reqwest::Response> {
        let res = self.post(path).json(body).send().await?;
        Ok(res)
    }

    pub fn delete(&self, path: &str) -> RequestBuilder {
        self.client
            .delete(format!("{}{}", self.base, path))
            .bearer_auth(&self.token)
    }
}

pub fn default_token_path() -> PathBuf {
    if let Ok(d) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(d).join("access-control-browser.token");
    }
    PathBuf::from("/tmp/access-control-browser.token")
}

fn read_token(override_path: Option<PathBuf>) -> Result<String> {
    if let Ok(t) = std::env::var("ACB_TOKEN") {
        return Ok(t);
    }
    let p = override_path.unwrap_or_else(default_token_path);
    let s = std::fs::read_to_string(&p)
        .with_context(|| format!("read token file {} (is the daemon running?)", p.display()))?;
    let line = s
        .lines()
        .next()
        .ok_or_else(|| anyhow!("empty token file"))?;
    Ok(line.trim().to_string())
}

// acb-cli — the operator's command-line surface for the access-controlled
// browser. Each subcommand either runs locally (validate) or talks to the
// daemon over its localhost HTTP API using the bearer token the daemon
// wrote to disk on startup.

mod client;
mod state;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};

use client::Daemon;

#[derive(Parser)]
#[command(name = "acb-cli", version, about = "Access-controlled browser CLI")]
struct Cli {
    /// Daemon base URL.
    #[arg(long, default_value = "http://127.0.0.1:39100", env = "ACB_BASE")]
    base: String,
    /// Path to the daemon's bearer token file.
    #[arg(long, env = "ACB_TOKEN_FILE")]
    token_file: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Check whether a URL is allowed by the policy file (no daemon needed).
    Validate {
        url: String,
        #[arg(long, short, default_value = "./config.yaml", env = "ACB_CONFIG")]
        config: PathBuf,
    },
    /// Ping the daemon.
    Status,
    /// Print the redacted config summary.
    Config,
    /// Reload the daemon's policy file.
    Reload,
    /// Open a URL in a session (creates one if there's no current session).
    Open { url: String },
    /// List accessible elements in the current page.
    Snapshot,
    /// Click an element by ref.
    Click { r#ref: String },
    /// Fill a text field by ref.
    Fill { r#ref: String, text: String },
    /// Type text into a field (appends).
    Type { r#ref: String, text: String },
    /// Dispatch a key press on a ref.
    Press { r#ref: String, key: String },
    /// Hover the mouse over a ref.
    Hover { r#ref: String },
    /// Select an <option> in a <select> by value.
    Select { r#ref: String, value: String },
    /// Set checked/unchecked on a checkbox or radio.
    Check { r#ref: String, checked: bool },
    /// Find accessible elements by ARIA role or visible text.
    Find {
        #[arg(value_parser = ["role","text"])]
        kind: String,
        query: String,
    },
    /// Close the current session.
    Close,
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    match rt.block_on(run()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}

async fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Validate { url, config } => validate(url, config),
        Cmd::Status => status(cli.base, cli.token_file).await,
        Cmd::Config => config_cmd(cli.base, cli.token_file).await,
        Cmd::Reload => reload_cmd(cli.base, cli.token_file).await,
        Cmd::Open { url } => open_cmd(cli.base, cli.token_file, url).await,
        Cmd::Snapshot => snapshot_cmd(cli.base, cli.token_file).await,
        Cmd::Click { r#ref } => action_ref(cli.base, cli.token_file, "click", r#ref).await,
        Cmd::Fill { r#ref, text } => {
            action_ref_text(cli.base, cli.token_file, "fill", r#ref, text).await
        }
        Cmd::Type { r#ref, text } => {
            action_ref_text(cli.base, cli.token_file, "type", r#ref, text).await
        }
        Cmd::Press { r#ref, key } => press_cmd(cli.base, cli.token_file, r#ref, key).await,
        Cmd::Hover { r#ref } => action_ref(cli.base, cli.token_file, "hover", r#ref).await,
        Cmd::Select { r#ref, value } => select_cmd(cli.base, cli.token_file, r#ref, value).await,
        Cmd::Check { r#ref, checked } => check_cmd(cli.base, cli.token_file, r#ref, checked).await,
        Cmd::Find { kind, query } => find_cmd(cli.base, cli.token_file, kind, query).await,
        Cmd::Close => close_cmd(cli.base, cli.token_file).await,
    }
}

fn validate(url: String, config: PathBuf) -> Result<ExitCode> {
    let yaml = std::fs::read_to_string(&config)
        .with_context(|| format!("failed to read {}", config.display()))?;
    let cfg = acb_policy::load::load_policy(&yaml)
        .with_context(|| format!("invalid policy in {}", config.display()))?;
    match acb_policy::url_validator::validate_url(&url, &cfg) {
        Ok(acb_policy::url_validator::UrlClass::Allowed { rule_name }) => {
            println!("ALLOWED rule={rule_name}");
            Ok(ExitCode::SUCCESS)
        }
        Err(reason) => {
            eprintln!("BLOCKED {reason}");
            Ok(ExitCode::from(1))
        }
    }
}

async fn status(base: String, token_file: Option<PathBuf>) -> Result<ExitCode> {
    // Health probe is unauthenticated; auth is only needed to confirm it's
    // *our* daemon (x-acb header).
    let client = reqwest::Client::new();
    let res = client.get(format!("{base}/healthz")).send().await?;
    if !res.status().is_success() || res.headers().get("x-acb").map(|v| v.as_bytes()) != Some(b"1")
    {
        eprintln!("daemon not responding at {base}");
        return Ok(ExitCode::from(1));
    }
    // Then config to confirm token works.
    if Daemon::connect(base.clone(), token_file).is_ok() {
        println!("OK {base}");
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!("daemon up but token unreadable");
        Ok(ExitCode::from(1))
    }
}

async fn config_cmd(base: String, token_file: Option<PathBuf>) -> Result<ExitCode> {
    let d = Daemon::connect(base, token_file)?;
    let v: Value = d.get("/config").send().await?.json().await?;
    println!("{}", serde_json::to_string_pretty(&v)?);
    Ok(ExitCode::SUCCESS)
}

async fn reload_cmd(base: String, token_file: Option<PathBuf>) -> Result<ExitCode> {
    let d = Daemon::connect(base, token_file)?;
    let res = d.post("/admin/reload").send().await?;
    if !res.status().is_success() {
        eprintln!("reload failed: {}", res.text().await.unwrap_or_default());
        return Ok(ExitCode::from(1));
    }
    let v: Value = res.json().await?;
    println!("reloaded etag={}", v["etag"].as_str().unwrap_or("?"));
    Ok(ExitCode::SUCCESS)
}

async fn ensure_session(d: &Daemon) -> Result<String> {
    let mut s = state::load()?;
    if let Some(id) = &s.session_id {
        // Confirm session still exists by attempting a tiny no-op snapshot
        // call. If it 404s, allocate a new one.
        let res = d.post(&format!("/sessions/{id}/snapshot")).send().await?;
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            // fallthrough to create
        } else {
            return Ok(id.clone());
        }
    }
    let v: Value = d.post("/sessions").send().await?.json().await?;
    let id = v["id"]
        .as_str()
        .ok_or_else(|| anyhow!("no id in /sessions response"))?
        .to_string();
    s.session_id = Some(id.clone());
    state::save(&s)?;
    Ok(id)
}

async fn open_cmd(base: String, token_file: Option<PathBuf>, url: String) -> Result<ExitCode> {
    let d = Daemon::connect(base, token_file)?;
    let id = ensure_session(&d).await?;
    let res = d
        .post_json(&format!("/sessions/{id}/open"), &json!({"url": url}))
        .await?;
    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        eprintln!("open failed: {status} {body}");
        return Ok(ExitCode::from(1));
    }
    let v: Value = res.json().await?;
    println!(
        "OK session={id} rule={} url={}",
        v["rule"].as_str().unwrap_or("?"),
        v["url"].as_str().unwrap_or("?")
    );
    Ok(ExitCode::SUCCESS)
}

async fn snapshot_cmd(base: String, token_file: Option<PathBuf>) -> Result<ExitCode> {
    let d = Daemon::connect(base, token_file)?;
    let id = ensure_session(&d).await?;
    let res = d.post(&format!("/sessions/{id}/snapshot")).send().await?;
    if !res.status().is_success() {
        eprintln!("snapshot failed: {}", res.text().await.unwrap_or_default());
        return Ok(ExitCode::from(1));
    }
    let v: Value = res.json().await?;
    let refs = v["refs"].as_array().cloned().unwrap_or_default();
    println!("gen={} refs={}", v["generation"], refs.len());
    for r in &refs {
        println!(
            "{} {} {}",
            r["ref"].as_str().unwrap_or(""),
            r["role"]
                .as_str()
                .unwrap_or(r["tag"].as_str().unwrap_or("")),
            r["text"].as_str().unwrap_or("")
        );
    }
    Ok(ExitCode::SUCCESS)
}

async fn action_ref(
    base: String,
    token_file: Option<PathBuf>,
    verb: &str,
    r#ref: String,
) -> Result<ExitCode> {
    let d = Daemon::connect(base, token_file)?;
    let id = ensure_session(&d).await?;
    let res = d
        .post_json(&format!("/sessions/{id}/{verb}"), &json!({"ref": r#ref}))
        .await?;
    exit_from(res).await
}

async fn action_ref_text(
    base: String,
    token_file: Option<PathBuf>,
    verb: &str,
    r#ref: String,
    text: String,
) -> Result<ExitCode> {
    let d = Daemon::connect(base, token_file)?;
    let id = ensure_session(&d).await?;
    let res = d
        .post_json(
            &format!("/sessions/{id}/{verb}"),
            &json!({"ref": r#ref, "text": text}),
        )
        .await?;
    exit_from(res).await
}

async fn press_cmd(
    base: String,
    token_file: Option<PathBuf>,
    r#ref: String,
    key: String,
) -> Result<ExitCode> {
    let d = Daemon::connect(base, token_file)?;
    let id = ensure_session(&d).await?;
    let res = d
        .post_json(
            &format!("/sessions/{id}/press"),
            &json!({"ref": r#ref, "key": key}),
        )
        .await?;
    exit_from(res).await
}

async fn select_cmd(
    base: String,
    token_file: Option<PathBuf>,
    r#ref: String,
    value: String,
) -> Result<ExitCode> {
    let d = Daemon::connect(base, token_file)?;
    let id = ensure_session(&d).await?;
    let res = d
        .post_json(
            &format!("/sessions/{id}/select"),
            &json!({"ref": r#ref, "value": value}),
        )
        .await?;
    exit_from(res).await
}

async fn check_cmd(
    base: String,
    token_file: Option<PathBuf>,
    r#ref: String,
    checked: bool,
) -> Result<ExitCode> {
    let d = Daemon::connect(base, token_file)?;
    let id = ensure_session(&d).await?;
    let res = d
        .post_json(
            &format!("/sessions/{id}/check"),
            &json!({"ref": r#ref, "checked": checked}),
        )
        .await?;
    exit_from(res).await
}

async fn find_cmd(
    base: String,
    token_file: Option<PathBuf>,
    kind: String,
    query: String,
) -> Result<ExitCode> {
    let d = Daemon::connect(base, token_file)?;
    let id = ensure_session(&d).await?;
    let body = json!({ "kind": kind, "query": query });
    let res = d.post_json(&format!("/sessions/{id}/find"), &body).await?;
    if !res.status().is_success() {
        eprintln!("find failed: {}", res.text().await.unwrap_or_default());
        return Ok(ExitCode::from(1));
    }
    let v: Value = res.json().await?;
    println!("{}", serde_json::to_string_pretty(&v)?);
    Ok(ExitCode::SUCCESS)
}

async fn close_cmd(base: String, token_file: Option<PathBuf>) -> Result<ExitCode> {
    let d = Daemon::connect(base, token_file)?;
    let s = state::load()?;
    let id = match s.session_id {
        Some(id) => id,
        None => {
            println!("no session to close");
            return Ok(ExitCode::SUCCESS);
        }
    };
    let res = d.delete(&format!("/sessions/{id}")).send().await?;
    let mut s = state::load()?;
    s.session_id = None;
    state::save(&s)?;
    if res.status().is_success() {
        println!("closed {id}");
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!("close returned {}", res.status());
        Ok(ExitCode::from(1))
    }
}

async fn exit_from(res: reqwest::Response) -> Result<ExitCode> {
    let st = res.status();
    if st.is_success() {
        Ok(ExitCode::SUCCESS)
    } else {
        let body = res.text().await.unwrap_or_default();
        eprintln!("{st}: {body}");
        Ok(ExitCode::from(1))
    }
}

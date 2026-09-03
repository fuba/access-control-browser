// acb-cli — the operator's command-line surface for the access-controlled
// browser. Each subcommand either runs locally (validate) or talks to the
// daemon over its localhost HTTP API using the bearer token the daemon
// wrote to disk on startup.

mod client;
mod state;

use std::path::PathBuf;
use std::process::ExitCode;

use acb_cli::{edit, sign};
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
    /// Operate on a specific session id (one-shot override; beats the
    /// `use` pin and the last-used session). Discover ids with
    /// `acb-cli sessions`.
    #[arg(long, global = true, env = "ACB_SESSION")]
    session: Option<String>,
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
    /// Edit config.yaml in $EDITOR, validate it, and save it back
    /// (visudo-style). Invalid policy is rejected. With --signing-key the
    /// save also bumps `revision:` and re-signs (required once the policy has
    /// a config.yaml.sig).
    EditConfig {
        #[arg(long, short, default_value = "./config.yaml", env = "ACB_CONFIG")]
        config: PathBuf,
        /// Key for `ssh-keygen -Y sign` (private key file, or the .pub of a
        /// key held by ssh-agent / a security key). Required once the policy
        /// has a config.yaml.sig.
        #[arg(long, env = "ACB_SIGNING_KEY")]
        signing_key: Option<PathBuf>,
    },
    /// Sign config.yaml as-is into config.yaml.sig with `ssh-keygen -Y sign`,
    /// for a daemon started with --verify-key. Does not touch the policy.
    SignConfig {
        #[arg(long, short, default_value = "./config.yaml", env = "ACB_CONFIG")]
        config: PathBuf,
        /// Key for `ssh-keygen -Y sign -f`.
        #[arg(long, env = "ACB_SIGNING_KEY")]
        signing_key: PathBuf,
    },
    /// Check config.yaml.sig against a verify-key file, exactly as the daemon
    /// does at startup and on reload (no daemon needed).
    VerifyConfig {
        #[arg(long, short, default_value = "./config.yaml", env = "ACB_CONFIG")]
        config: PathBuf,
        /// File of trusted OpenSSH public keys (what the daemon gets as
        /// --verify-key).
        #[arg(long, env = "ACB_VERIFY_KEY")]
        verify_key: PathBuf,
    },
    /// Ping the daemon.
    Status,
    /// Print the redacted config summary.
    Config,
    /// Reload the daemon's policy file.
    Reload,
    /// List live sessions (id / url / title), marking the pinned one.
    Sessions,
    /// Pin a session so subsequent commands target it (e.g. the tab a
    /// human opened in the web UI). Discover ids with `acb-cli sessions`.
    Use { sid: String },
    /// Clear the pinned session.
    Unuse,
    /// Open a URL in a session (creates one if there's no current session).
    Open { url: String },
    /// Go back in the session's history.
    Back,
    /// Go forward in the session's history.
    Forward,
    /// Reload the current page.
    ReloadPage,
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
    let base = cli.base;
    let tf = cli.token_file;
    let sess = cli.session;
    match cli.cmd {
        Cmd::Validate { url, config } => validate(url, config),
        Cmd::EditConfig {
            config,
            signing_key,
        } => edit_config_cmd(base, tf, config, signing_key).await,
        Cmd::SignConfig {
            config,
            signing_key,
        } => sign_config_cmd(config, signing_key),
        Cmd::VerifyConfig { config, verify_key } => verify_config_cmd(config, verify_key),
        Cmd::Status => status(base, tf).await,
        Cmd::Config => config_cmd(base, tf).await,
        Cmd::Reload => reload_cmd(base, tf).await,
        Cmd::Sessions => sessions_cmd(base, tf).await,
        Cmd::Use { sid } => use_cmd(base, tf, sid).await,
        Cmd::Unuse => unuse_cmd().await,
        Cmd::Open { url } => open_cmd(base, tf, sess, url).await,
        Cmd::Back => action_session(base, tf, sess, "back").await,
        Cmd::Forward => action_session(base, tf, sess, "forward").await,
        Cmd::ReloadPage => action_session(base, tf, sess, "reload").await,
        Cmd::Snapshot => snapshot_cmd(base, tf, sess).await,
        Cmd::Click { r#ref } => action_ref(base, tf, sess, "click", r#ref).await,
        Cmd::Fill { r#ref, text } => action_ref_text(base, tf, sess, "fill", r#ref, text).await,
        Cmd::Type { r#ref, text } => action_ref_text(base, tf, sess, "type", r#ref, text).await,
        Cmd::Press { r#ref, key } => press_cmd(base, tf, sess, r#ref, key).await,
        Cmd::Hover { r#ref } => action_ref(base, tf, sess, "hover", r#ref).await,
        Cmd::Select { r#ref, value } => select_cmd(base, tf, sess, r#ref, value).await,
        Cmd::Check { r#ref, checked } => check_cmd(base, tf, sess, r#ref, checked).await,
        Cmd::Find { kind, query } => find_cmd(base, tf, sess, kind, query).await,
        Cmd::Close => close_cmd(base, tf, sess).await,
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

async fn edit_config_cmd(
    base: String,
    token_file: Option<PathBuf>,
    config: PathBuf,
    signing_key: Option<PathBuf>,
) -> Result<ExitCode> {
    let opts = edit::EditOptions { signing_key };
    match edit::edit(&config, &opts)? {
        edit::EditStatus::Unchanged => {
            println!("no changes");
            return Ok(ExitCode::SUCCESS);
        }
        edit::EditStatus::Saved {
            signed_revision: Some(rev),
        } => println!(
            "saved {} (revision {rev}, signed into {})",
            config.display(),
            sign::sig_path(&config).display()
        ),
        edit::EditStatus::Saved {
            signed_revision: None,
        } => println!("saved {}", config.display()),
    }
    // Best-effort hot-reload nudge; the daemon's file watcher also catches it.
    match Daemon::connect(base, token_file) {
        Ok(d) => match d.post("/admin/reload").send().await {
            Ok(res) if res.status().is_success() => {
                let v: Value = res.json().await.unwrap_or_default();
                println!("reloaded etag={}", v["etag"].as_str().unwrap_or("?"));
            }
            _ => println!("(saved; daemon will hot-reload on file change)"),
        },
        Err(_) => println!("(daemon not reachable; it will hot-reload on file change)"),
    }
    Ok(ExitCode::SUCCESS)
}

fn sign_config_cmd(config: PathBuf, signing_key: PathBuf) -> Result<ExitCode> {
    let text = std::fs::read_to_string(&config)
        .with_context(|| format!("failed to read {}", config.display()))?;
    // Sign only what the daemon would accept; a broken file is caught here
    // rather than as a reload failure later.
    acb_policy::load::load_policy(&text)
        .with_context(|| format!("invalid policy in {}", config.display()))?;
    if sign::current_revision(&text).is_none() {
        eprintln!(
            "warning: {} has no top-level `revision:`; without it the daemon cannot tell an \
             old signed policy from a new one (no rollback protection). `acb-cli edit-config` \
             adds and bumps it automatically.",
            config.display()
        );
    }
    let sig = sign::sign(&config, &signing_key)?;
    println!("signed {} -> {}", config.display(), sig.display());
    Ok(ExitCode::SUCCESS)
}

fn verify_config_cmd(config: PathBuf, verify_key: PathBuf) -> Result<ExitCode> {
    match sign::verify(&config, &verify_key) {
        Ok((signer, revision)) => {
            println!(
                "OK {} revision={revision} signer={} {}",
                config.display(),
                signer.fingerprint,
                signer.comment
            );
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            eprintln!("FAIL {}: {e:#}", config.display());
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

/// Is this session id still live on the daemon? Probes with a no-op
/// snapshot call (404 == gone).
async fn session_alive(d: &Daemon, id: &str) -> bool {
    match d.post(&format!("/sessions/{id}/snapshot")).send().await {
        Ok(res) => res.status() != reqwest::StatusCode::NOT_FOUND,
        Err(_) => false,
    }
}

/// Resolve which session a command should target. Order (first live wins):
///   1. `--session` flag (one-shot override)
///   2. `acb-cli use <sid>` pin
///   3. last-used session in local state
///   4. create a new session
///
/// Explicit-only model: we never auto-follow a daemon "active" session.
async fn resolve_session(d: &Daemon, flag: &Option<String>) -> Result<String> {
    if let Some(id) = flag {
        if session_alive(d, id).await {
            return Ok(id.clone());
        }
        return Err(anyhow!("--session {id} is not a live session"));
    }
    let mut s = state::load()?;
    if let Some(id) = s.pinned_session_id.clone() {
        if session_alive(d, &id).await {
            return Ok(id);
        }
    }
    if let Some(id) = s.session_id.clone() {
        if session_alive(d, &id).await {
            return Ok(id);
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

async fn sessions_cmd(base: String, token_file: Option<PathBuf>) -> Result<ExitCode> {
    let d = Daemon::connect(base, token_file)?;
    let v: Value = d.get("/sessions").send().await?.json().await?;
    let pinned = state::load()?.pinned_session_id;
    let arr = v.as_array().cloned().unwrap_or_default();
    if arr.is_empty() {
        println!("(no sessions)");
        return Ok(ExitCode::SUCCESS);
    }
    for s in &arr {
        let id = s["id"].as_str().unwrap_or("?");
        let mark = if Some(id.to_string()) == pinned {
            "*"
        } else {
            " "
        };
        println!(
            "{mark} {id}  {}  {}",
            s["title"].as_str().unwrap_or(""),
            s["url"].as_str().unwrap_or("")
        );
    }
    Ok(ExitCode::SUCCESS)
}

async fn use_cmd(base: String, token_file: Option<PathBuf>, sid: String) -> Result<ExitCode> {
    // Verify the session exists before pinning, so a typo fails loudly.
    let d = Daemon::connect(base, token_file)?;
    if !session_alive(&d, &sid).await {
        eprintln!("no such session: {sid} (try `acb-cli sessions`)");
        return Ok(ExitCode::from(1));
    }
    let mut s = state::load()?;
    s.pinned_session_id = Some(sid.clone());
    state::save(&s)?;
    println!("pinned {sid}");
    Ok(ExitCode::SUCCESS)
}

async fn unuse_cmd() -> Result<ExitCode> {
    let mut s = state::load()?;
    s.pinned_session_id = None;
    state::save(&s)?;
    println!("unpinned");
    Ok(ExitCode::SUCCESS)
}

/// Session-level POST with no body (back / forward / reload).
async fn action_session(
    base: String,
    token_file: Option<PathBuf>,
    sess: Option<String>,
    verb: &str,
) -> Result<ExitCode> {
    let d = Daemon::connect(base, token_file)?;
    let id = resolve_session(&d, &sess).await?;
    let res = d.post(&format!("/sessions/{id}/{verb}")).send().await?;
    exit_from(res).await
}

async fn open_cmd(
    base: String,
    token_file: Option<PathBuf>,
    sess: Option<String>,
    url: String,
) -> Result<ExitCode> {
    let d = Daemon::connect(base, token_file)?;
    let id = resolve_session(&d, &sess).await?;
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

async fn snapshot_cmd(
    base: String,
    token_file: Option<PathBuf>,
    sess: Option<String>,
) -> Result<ExitCode> {
    let d = Daemon::connect(base, token_file)?;
    let id = resolve_session(&d, &sess).await?;
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
    sess: Option<String>,
    verb: &str,
    r#ref: String,
) -> Result<ExitCode> {
    let d = Daemon::connect(base, token_file)?;
    let id = resolve_session(&d, &sess).await?;
    let res = d
        .post_json(&format!("/sessions/{id}/{verb}"), &json!({"ref": r#ref}))
        .await?;
    exit_from(res).await
}

async fn action_ref_text(
    base: String,
    token_file: Option<PathBuf>,
    sess: Option<String>,
    verb: &str,
    r#ref: String,
    text: String,
) -> Result<ExitCode> {
    let d = Daemon::connect(base, token_file)?;
    let id = resolve_session(&d, &sess).await?;
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
    sess: Option<String>,
    r#ref: String,
    key: String,
) -> Result<ExitCode> {
    let d = Daemon::connect(base, token_file)?;
    let id = resolve_session(&d, &sess).await?;
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
    sess: Option<String>,
    r#ref: String,
    value: String,
) -> Result<ExitCode> {
    let d = Daemon::connect(base, token_file)?;
    let id = resolve_session(&d, &sess).await?;
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
    sess: Option<String>,
    r#ref: String,
    checked: bool,
) -> Result<ExitCode> {
    let d = Daemon::connect(base, token_file)?;
    let id = resolve_session(&d, &sess).await?;
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
    sess: Option<String>,
    kind: String,
    query: String,
) -> Result<ExitCode> {
    let d = Daemon::connect(base, token_file)?;
    let id = resolve_session(&d, &sess).await?;
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

async fn close_cmd(
    base: String,
    token_file: Option<PathBuf>,
    sess: Option<String>,
) -> Result<ExitCode> {
    let d = Daemon::connect(base, token_file)?;
    // Resolve which session to close: explicit flag > pin > last-used.
    // (Don't create one just to close it.)
    let id = match &sess {
        Some(id) => id.clone(),
        None => {
            let s = state::load()?;
            match s.pinned_session_id.or(s.session_id) {
                Some(id) => id,
                None => {
                    println!("no session to close");
                    return Ok(ExitCode::SUCCESS);
                }
            }
        }
    };
    let res = d.delete(&format!("/sessions/{id}")).send().await?;
    // Clear any local pointers that referenced the closed session.
    let mut s = state::load()?;
    if s.session_id.as_deref() == Some(id.as_str()) {
        s.session_id = None;
    }
    if s.pinned_session_id.as_deref() == Some(id.as_str()) {
        s.pinned_session_id = None;
    }
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

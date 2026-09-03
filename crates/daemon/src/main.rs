// acb-daemon entry point.
//
// Loads ./config.yaml (or --config), generates a fresh bearer token, writes
// it to the runtime token path, launches Chromium, and starts the HTTP
// server. Logs to a rolling file; mirrors to stderr when --foreground is
// set.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "acb-daemon",
    version,
    about = "Access-controlled browser daemon"
)]
struct Cli {
    /// Path to the policy YAML file.
    #[arg(long, default_value = "./config.yaml", env = "ACB_CONFIG")]
    config: PathBuf,
    /// Override the bind address (e.g. 0.0.0.0). Defaults to the config's
    /// server.bind.
    #[arg(long, env = "ACB_BIND")]
    bind: Option<String>,
    /// Override the bind port. Defaults to the config's server.port.
    #[arg(long, env = "ACB_PORT")]
    port: Option<u16>,
    /// Run in foreground (don't detach); mirror logs to stderr. In Docker
    /// this should be the default.
    #[arg(long)]
    foreground: bool,
    /// Run Chromium in headless mode. Required for CI / Docker.
    #[arg(long, env = "ACB_HEADLESS")]
    headless: bool,
    /// Path to write the bearer token. Default: ${XDG_RUNTIME_DIR}/acb.token
    /// or /tmp/acb.token.
    #[arg(long, env = "ACB_TOKEN_FILE")]
    token_file: Option<PathBuf>,
    /// Require the policy to be signed. Path to a file of OpenSSH public
    /// keys (one per line, `id_*.pub` / `authorized_keys` shape) allowed to
    /// sign it; `<config>.sig` must then be a valid `ssh-keygen -Y sign -n
    /// acb-policy` signature over the exact file bytes, at startup and on
    /// every reload. See docs/usage.md "Signing the policy file".
    #[arg(long, env = "ACB_VERIFY_KEY")]
    verify_key: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Trusted signing keys, if the operator turned verification on. Parsed
    // before the policy so a bad key file is reported as such rather than as
    // a signature failure.
    let verify_keys = match &cli.verify_key {
        Some(p) => {
            let text = std::fs::read_to_string(p)
                .with_context(|| format!("read verify key {}", p.display()))?;
            Some(Arc::new(
                acb_policy::sig::VerifyKeys::parse(&text)
                    .with_context(|| format!("invalid verify key {}", p.display()))?,
            ))
        }
        None => None,
    };

    // `policy_file::load` resolves config-internal relative paths against
    // the config file's parent. This lets the operator place
    // /etc/acb/config.yaml outside the daemon's CWD (e.g. outside an
    // LLM-agent sandbox) and still use relative log_file / user_data_dir
    // paths that land alongside the config rather than inside the agent's
    // writable area. With verify keys set, an unsigned or badly signed
    // policy is fatal here: the daemon never starts on a policy it would
    // refuse to hot-reload.
    let loaded = acb_daemon::policy_file::load(&cli.config, verify_keys.as_ref(), None)?;
    let policy = Arc::new(loaded.policy);
    let startup_signer = loaded.signer;

    // Initialize logging. File appender always; stderr only in foreground.
    let log_path = std::path::Path::new(&policy.server.log_file);
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let file_appender = tracing_appender::rolling::daily(
        log_path.parent().unwrap_or(std::path::Path::new(".")),
        log_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "acb-daemon.log".into()),
    );
    let (nb, _guard) = tracing_appender::non_blocking(file_appender);
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // Foreground and background both write to the rolling file. Foreground
    // ANSI mirroring is a TODO; for now both modes go to the same writer
    // sans ANSI so log files don't contain escape sequences.
    let _ = cli.foreground;
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(nb)
        .with_ansi(false)
        .init();
    tracing::info!(
        config = %cli.config.display(),
        bind = %policy.server.bind,
        port = policy.server.port,
        revision = policy.revision,
        signature_required = verify_keys.is_some(),
        signer = ?startup_signer.as_ref().map(|s| &s.fingerprint),
        "acb-daemon starting",
    );
    if let Some(keys) = &verify_keys {
        tracing::info!(
            trusted_keys = ?keys.fingerprints(),
            "policy signature verification is ON: on-disk edits need config.yaml.sig"
        );
    }

    let bind = cli
        .bind
        .clone()
        .unwrap_or_else(|| policy.server.bind.clone());
    let port = cli.port.unwrap_or(policy.server.port);
    let addr: std::net::SocketAddr = format!("{bind}:{port}").parse().context("invalid bind")?;

    let token_path = cli.token_file.clone().unwrap_or_else(default_token_path);
    let token = acb_daemon::auth::generate_token();

    let user_data_dir = Some(PathBuf::from(&policy.chromium.user_data_dir));
    let poll_ms = policy.server.config_poll_ms;
    let running = acb_daemon::start(acb_daemon::StartConfig {
        bind: addr,
        policy,
        token,
        token_path: Some(token_path.clone()),
        chrome_binary: None,
        headless: cli.headless,
        user_data_dir,
    })
    .await?;
    running.state.set_policy_path(cli.config.clone()).await;
    if let Some(keys) = verify_keys {
        running.state.set_verify_keys(keys);
    }

    let _reload_handle =
        acb_daemon::reload::spawn(cli.config.clone(), poll_ms, running.state.clone());

    tracing::info!(addr = %running.addr, token_file = %token_path.display(), "acb-daemon listening");

    // Wait for Ctrl+C.
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("acb-daemon shutting down");
    let _ = running.shutdown.send(());
    let _ = running.join.await;
    Ok(())
}

fn default_token_path() -> PathBuf {
    if let Ok(d) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(d).join("access-control-browser.token");
    }
    PathBuf::from("/tmp/access-control-browser.token")
}

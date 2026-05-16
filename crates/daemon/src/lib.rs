// acb-daemon library facade. The binary in main.rs is a thin wrapper around
// `run_with` so integration tests can spawn the daemon in-process.

pub mod app;
pub mod auth;
pub mod browser;
pub mod events;
pub mod reload;
pub mod snapshot;
pub mod state;
pub mod ui_static;

pub mod api {
    pub mod actions;
    pub mod admin;
    pub mod config_route;
    pub mod healthz;
    pub mod nav;
    pub mod sessions;
    pub mod sse;
}

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tokio::net::TcpListener;

use acb_policy::CompiledPolicy;

pub use state::AppState;

#[derive(Debug, Clone)]
pub struct StartConfig {
    pub bind: std::net::SocketAddr,
    pub policy: Arc<CompiledPolicy>,
    pub token: String,
    /// Where to write the token file (caller may delete after read).
    pub token_path: Option<PathBuf>,
    pub chrome_binary: Option<PathBuf>,
    /// Headless mode for tests / CI. Default false (visible). In Docker
    /// runtime, set to true.
    pub headless: bool,
    /// Override Chromium's user-data-dir. Tests pass a unique tempdir to
    /// avoid the SingletonLock collision when running in parallel.
    pub user_data_dir: Option<PathBuf>,
}

pub struct RunningDaemon {
    pub addr: std::net::SocketAddr,
    pub state: AppState,
    pub shutdown: tokio::sync::oneshot::Sender<()>,
    pub join: tokio::task::JoinHandle<()>,
}

pub async fn start(cfg: StartConfig) -> Result<RunningDaemon> {
    let listener = TcpListener::bind(cfg.bind).await?;
    let addr = listener.local_addr()?;

    let state = AppState::new(cfg.policy.clone(), cfg.token.clone());
    if let Some(p) = &cfg.token_path {
        auth::write_token_file(p, &cfg.token)?;
    }

    // Launch Chromium. If this fails (no binary, sandbox, etc.) the daemon
    // refuses to start — we never silently degrade.
    let browser = browser::launch::launch(
        cfg.chrome_binary.clone(),
        cfg.user_data_dir.clone(),
        cfg.headless,
    )
    .await?;
    state.attach_browser(browser).await;

    let app = app::router(state.clone());

    let (tx, rx) = tokio::sync::oneshot::channel();
    let join = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .await;
    });

    Ok(RunningDaemon {
        addr,
        state,
        shutdown: tx,
        join,
    })
}

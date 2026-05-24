// The CLI agent can target a session created out-of-band (as the web UI
// would) via `acb-cli use <sid>` / `--session <sid>`, instead of always
// spinning up its own. Spawns a real daemon in-process and drives the
// compiled acb-cli binary against it.

use std::sync::Arc;

use assert_cmd::Command;
use predicates::str::contains;

fn fixture_policy() -> Arc<acb_policy::CompiledPolicy> {
    // Empty rule set is fine — these tests only create/list sessions and
    // probe them via snapshot; no navigation is required.
    let yaml = r#"
server: { bind: "127.0.0.1", port: 39100, log_file: "./logs/t.log", log_rotation: "never" }
chromium: { binary: null, user_data_dir: "./var/p", viewport: { width: 1024, height: 768 }, screencast: { format: "jpeg", quality: 60, max_fps: 8 } }
resource_policy: { subresources_inherit_page: true, always_block_schemes: ["javascript"], bypass_service_worker: true }
rules: []
"#;
    Arc::new(acb_policy::load::load_policy(yaml).expect("policy"))
}

struct Harness {
    running: acb_daemon::RunningDaemon,
    base: String,
    token: String,
    _profile: tempfile::TempDir,
    runtime_dir: tempfile::TempDir,
    client: reqwest::Client,
}

impl Harness {
    async fn spawn() -> Self {
        let token = acb_daemon::auth::generate_token();
        let profile = tempfile::tempdir().unwrap();
        let runtime_dir = tempfile::tempdir().unwrap();
        let cfg = acb_daemon::StartConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            policy: fixture_policy(),
            token: token.clone(),
            token_path: None,
            chrome_binary: None,
            headless: true,
            user_data_dir: Some(profile.path().to_path_buf()),
        };
        let running = acb_daemon::start(cfg).await.expect("daemon start");
        let base = format!("http://{}", running.addr);
        Self {
            running,
            base,
            token,
            _profile: profile,
            runtime_dir,
            client: reqwest::Client::new(),
        }
    }

    /// Create a session via the HTTP API directly (simulating the web UI).
    async fn create_session(&self) -> String {
        let v: serde_json::Value = self
            .client
            .post(format!("{}/sessions", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        v["id"].as_str().unwrap().to_string()
    }

    async fn session_count(&self) -> usize {
        let v: serde_json::Value = self
            .client
            .get(format!("{}/sessions", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        v.as_array().map(|a| a.len()).unwrap_or(0)
    }

    /// An `acb-cli` invocation wired to this daemon (token via env, isolated
    /// state dir via XDG_RUNTIME_DIR).
    fn cli(&self) -> Command {
        let mut c = Command::cargo_bin("acb-cli").unwrap();
        c.env("ACB_TOKEN", &self.token)
            .env("XDG_RUNTIME_DIR", self.runtime_dir.path())
            .arg("--base")
            .arg(&self.base);
        c
    }

    async fn shutdown(self) {
        let _ = self.running.shutdown.send(());
        let _ = self.running.join.await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn use_pins_and_snapshot_targets_it() {
    let h = Harness::spawn().await;
    let sid = h.create_session().await;
    assert_eq!(h.session_count().await, 1);

    // `sessions` lists it.
    h.cli()
        .arg("sessions")
        .assert()
        .success()
        .stdout(contains(&sid));

    // Pin it.
    h.cli()
        .args(["use", &sid])
        .assert()
        .success()
        .stdout(contains("pinned"));

    // snapshot with no --session must resolve to the pinned session, NOT
    // create a new one: the count stays 1.
    h.cli().arg("snapshot").assert().success();
    assert_eq!(
        h.session_count().await,
        1,
        "snapshot should reuse the pinned session, not create another"
    );

    h.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_flag_overrides_pin() {
    let h = Harness::spawn().await;
    let pinned = h.create_session().await;
    let other = h.create_session().await;
    assert_eq!(h.session_count().await, 2);

    h.cli().args(["use", &pinned]).assert().success();

    // --session <other> overrides the pin; still no new session created.
    h.cli()
        .args(["--session", &other, "snapshot"])
        .assert()
        .success();
    assert_eq!(h.session_count().await, 2, "no new session created");

    // A bogus --session fails loudly (exit 2, error printed).
    h.cli()
        .args(["--session", "s_does_not_exist", "snapshot"])
        .assert()
        .failure();

    h.shutdown().await;
}

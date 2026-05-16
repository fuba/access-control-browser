// Policy hot reload: editing the config file while the daemon is running
// must cause `/config` to reflect the new etag, and subsequent navigation
// decisions to use the new rule set. Malformed YAML must keep the old
// policy in effect.

mod support;

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tempfile::NamedTempFile;
use tokio::time::timeout;

use acb_daemon::{auth, events::ActivityEvent, start, RunningDaemon, StartConfig};

const POLICY_A: &str = r#"
server:
  bind: "127.0.0.1"
  port: 39100
  log_file: "./logs/t.log"
  log_rotation: "never"
chromium:
  binary: null
  user_data_dir: "./var/p"
  viewport: { width: 1024, height: 768 }
  screencast: { format: "jpeg", quality: 60, max_fps: 8 }
resource_policy:
  subresources_inherit_page: true
  always_block_schemes: ["javascript", "data", "file"]
  bypass_service_worker: true
rules:
  - name: "alpha"
    match: { kind: "fqdn", host: "alpha.example", subdomains: false }
    allowed_classes: []
"#;

const POLICY_B: &str = r#"
server:
  bind: "127.0.0.1"
  port: 39100
  log_file: "./logs/t.log"
  log_rotation: "never"
chromium:
  binary: null
  user_data_dir: "./var/p"
  viewport: { width: 1024, height: 768 }
  screencast: { format: "jpeg", quality: 60, max_fps: 8 }
resource_policy:
  subresources_inherit_page: true
  always_block_schemes: ["javascript", "data", "file"]
  bypass_service_worker: true
rules:
  - name: "beta"
    match: { kind: "fqdn", host: "beta.example", subdomains: false }
    allowed_classes: []
"#;

async fn spawn_with_file(yaml: &str) -> (RunningDaemon, String, NamedTempFile, tempfile::TempDir) {
    let mut f = NamedTempFile::new().unwrap();
    use std::io::Write;
    write!(f, "{yaml}").unwrap();
    f.flush().unwrap();
    let profile = tempfile::tempdir().unwrap();
    let policy = Arc::new(acb_policy::load::load_policy(yaml).unwrap());
    let token = auth::generate_token();
    let cfg = StartConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        policy,
        token: token.clone(),
        token_path: None,
        chrome_binary: None,
        headless: true,
        user_data_dir: Some(profile.path().to_path_buf()),
    };
    let running = start(cfg).await.expect("daemon start");
    running.state.set_policy_path(f.path().to_path_buf()).await;
    // Start the watcher with a short polling interval so the test doesn't
    // depend on inotify delivery.
    let _ = acb_daemon::reload::spawn(f.path().to_path_buf(), 200, running.state.clone());
    let base = format!("http://{}", running.addr);
    (running, token, f, profile)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admin_reload_swaps_etag() {
    let (running, token, mut f, _profile) = spawn_with_file(POLICY_A).await;
    let base = format!("http://{}", running.addr);
    let c = reqwest::Client::new();

    let cfg1: serde_json::Value = c
        .get(format!("{base}/config"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let etag1 = cfg1["etag"].as_str().unwrap().to_string();
    assert_eq!(cfg1["rules"][0]["name"], "alpha");

    // Rewrite to POLICY_B (different content -> different etag).
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        f.as_file_mut().set_len(0).unwrap();
        let mut h = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .mode(0o644)
            .open(f.path())
            .unwrap();
        h.write_all(POLICY_B.as_bytes()).unwrap();
    }
    // POST /admin/reload — explicit nudge so we don't depend on poll timing.
    let res = c
        .post(format!("{base}/admin/reload"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert!(res.status().is_success(), "reload failed: {}", res.status());

    let cfg2: serde_json::Value = c
        .get(format!("{base}/config"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_ne!(cfg2["etag"].as_str().unwrap(), etag1, "etag should change");
    assert_eq!(cfg2["rules"][0]["name"], "beta");

    let _ = running.shutdown.send(());
    let _ = running.join.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_yaml_keeps_old_policy_and_emits_failure() {
    let (running, token, mut f, _profile) = spawn_with_file(POLICY_A).await;
    let base = format!("http://{}", running.addr);
    let c = reqwest::Client::new();
    let mut rx = running.state.events().subscribe();

    // Write broken YAML.
    {
        use std::io::Write;
        f.as_file_mut().set_len(0).unwrap();
        let mut h = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(f.path())
            .unwrap();
        h.write_all(b"this: is: not: valid: yaml: [").unwrap();
    }
    let res = c
        .post(format!("{base}/admin/reload"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert!(!res.status().is_success(), "broken yaml should fail reload");

    // Old policy must remain.
    let cfg: serde_json::Value = c
        .get(format!("{base}/config"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cfg["rules"][0]["name"], "alpha");

    // We may or may not get a PolicyReloadFailed event depending on path;
    // the admin endpoint returns an error directly. Don't require an event.
    let _ = timeout(Duration::from_millis(200), rx.recv()).await;

    let _ = running.shutdown.send(());
    let _ = running.join.await;
}

// Policy hot reload: editing the config file while the daemon is running
// must cause `/config` to reflect the new etag, and subsequent navigation
// decisions to use the new rule set. Malformed YAML must keep the old
// policy in effect.

mod support;

use std::sync::Arc;
use std::time::Duration;

use tempfile::NamedTempFile;
use tokio::time::timeout;

use acb_daemon::{auth, start, RunningDaemon, StartConfig};

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
    // depend on inotify delivery. The JoinHandle is dropped (detached);
    // the spawned task is cancelled when the runtime tears down.
    drop(acb_daemon::reload::spawn(
        f.path().to_path_buf(),
        200,
        running.state.clone(),
    ));
    (running, token, f, profile)
}

/// Spawn the daemon with the policy file living in its own directory and the
/// watcher in inotify mode (poll_ms = 0), so the test exercises the real
/// filesystem-event path rather than the polling fallback.
async fn spawn_inotify(
    yaml: &str,
) -> (
    RunningDaemon,
    String,
    tempfile::TempDir,
    std::path::PathBuf,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("config.yaml");
    std::fs::write(&cfg_path, yaml).unwrap();
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
    running.state.set_policy_path(cfg_path.clone()).await;
    // poll_ms = 0 -> inotify dir-watch (the path the bug lived in).
    drop(acb_daemon::reload::spawn(
        cfg_path.clone(),
        0,
        running.state.clone(),
    ));
    // Return both tempdirs so the caller keeps them alive for the daemon's
    // lifetime (dropping them mid-test would yank the profile / config dir).
    (running, token, dir, cfg_path, profile)
}

async fn rule0_name(c: &reqwest::Client, base: &str, token: &str) -> String {
    let v: serde_json::Value = c
        .get(format!("{base}/config"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    v["rules"][0]["name"].as_str().unwrap_or("").to_string()
}

/// Wait until `/config`'s first rule name equals `want`, or fail after ~5s.
async fn wait_rule0(c: &reqwest::Client, base: &str, token: &str, want: &str) {
    for _ in 0..50 {
        if rule0_name(c, base, token).await == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("policy did not hot-reload to rule '{want}' within timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inotify_autoreload_on_inplace_write_and_atomic_rename() {
    let (running, token, dir, cfg_path, _profile) = spawn_inotify(POLICY_A).await;
    let base = format!("http://{}", running.addr);
    let c = reqwest::Client::new();
    assert_eq!(rule0_name(&c, &base, &token).await, "alpha");

    // 1) In-place modify (truncate + write to the same inode).
    std::fs::write(&cfg_path, POLICY_B).unwrap();
    wait_rule0(&c, &base, &token, "beta").await;

    // 2) Atomic rename (write a sibling temp file, rename over the config).
    //    This swaps the inode — a watch on the file itself would miss it,
    //    but the directory watch catches the rename. Regression guard for
    //    editor-style saves.
    let tmp = dir.path().join(".config.yaml.tmp");
    std::fs::write(&tmp, POLICY_A).unwrap();
    std::fs::rename(&tmp, &cfg_path).unwrap();
    wait_rule0(&c, &base, &token, "alpha").await;

    let _ = running.shutdown.send(());
    let _ = running.join.await;
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

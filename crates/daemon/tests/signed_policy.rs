// Signed-policy mode: with verify keys set, the daemon must accept only a
// policy whose `config.yaml.sig` is a valid `acb-policy` sshsig by a
// trusted key, and must refuse a revision rollback. An unsigned or badly
// signed edit keeps the previous policy in effect and surfaces as
// `policy.reload_failed`, exactly like malformed YAML does.
//
// Signing here is done in-process with the `ssh-key` crate so the test is
// deterministic and independent of an installed OpenSSH; interop with the
// real `ssh-keygen -Y sign` is covered in acb-policy's tests.

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ssh_key::{Algorithm, HashAlg, LineEnding, PrivateKey};
use tokio::time::timeout;

use acb_daemon::events::ActivityEvent;
use acb_daemon::{auth, start, RunningDaemon, StartConfig};
use acb_policy::sig::{VerifyKeys, NAMESPACE};

fn policy(revision: u64, rule: &str) -> String {
    format!(
        r#"revision: {revision}
server:
  bind: "127.0.0.1"
  port: 39100
  log_file: "./logs/t.log"
  log_rotation: "never"
chromium:
  binary: null
  user_data_dir: "./var/p"
  viewport: {{ width: 1024, height: 768 }}
  screencast: {{ format: "jpeg", quality: 60, max_fps: 8 }}
resource_policy:
  subresources_inherit_page: true
  always_block_schemes: ["javascript", "data", "file"]
  bypass_service_worker: true
rules:
  - name: "{rule}"
    match: {{ kind: "fqdn", host: "{rule}.example", subdomains: false }}
    allowed_classes: []
"#
    )
}

struct Signer(PrivateKey);

impl Signer {
    fn new() -> Self {
        Self(PrivateKey::random(&mut rand_core::OsRng, Algorithm::Ed25519).unwrap())
    }
    fn verify_keys(&self) -> Arc<VerifyKeys> {
        Arc::new(VerifyKeys::parse(&self.0.public_key().to_openssh().unwrap()).unwrap())
    }
    /// Write `yaml` and a matching signature (signature first, like the CLI).
    fn install(&self, cfg: &Path, yaml: &str) {
        let pem = self
            .0
            .sign(NAMESPACE, HashAlg::Sha512, yaml.as_bytes())
            .unwrap()
            .to_pem(LineEnding::LF)
            .unwrap();
        std::fs::write(sig_path(cfg), pem).unwrap();
        std::fs::write(cfg, yaml).unwrap();
    }
}

fn sig_path(cfg: &Path) -> PathBuf {
    acb_daemon::policy_file::sig_path(cfg)
}

async fn spawn_signed(
    signer: &Signer,
    yaml: &str,
) -> (
    RunningDaemon,
    String,
    tempfile::TempDir,
    PathBuf,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.yaml");
    signer.install(&cfg, yaml);
    let keys = signer.verify_keys();

    // Startup goes through the same verified loader as the daemon binary.
    let loaded = acb_daemon::policy_file::load(&cfg, Some(&keys), None).expect("startup load");
    let profile = tempfile::tempdir().unwrap();
    let token = auth::generate_token();
    let running = start(StartConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        policy: Arc::new(loaded.policy),
        token: token.clone(),
        token_path: None,
        chrome_binary: None,
        headless: true,
        user_data_dir: Some(profile.path().to_path_buf()),
    })
    .await
    .expect("daemon start");
    running.state.set_policy_path(cfg.clone()).await;
    running.state.set_verify_keys(keys);
    drop(acb_daemon::reload::spawn(
        cfg.clone(),
        100,
        running.state.clone(),
    ));
    (running, token, dir, cfg, profile)
}

async fn config_json(c: &reqwest::Client, base: &str, token: &str) -> serde_json::Value {
    c.get(format!("{base}/config"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn wait_rule0(c: &reqwest::Client, base: &str, token: &str, want: &str) {
    for _ in 0..50 {
        if config_json(c, base, token).await["rules"][0]["name"] == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("policy did not hot-reload to rule '{want}' within timeout");
}

async fn wait_reload_failed(
    rx: &mut tokio::sync::broadcast::Receiver<ActivityEvent>,
    needle: &str,
) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match timeout(remaining, rx.recv()).await {
            Ok(Ok(ActivityEvent::PolicyReloadFailed { error, .. })) if error.contains(needle) => {
                return error
            }
            Ok(Ok(_)) => continue,
            Ok(Err(_)) => continue,
            Err(_) => panic!("no policy.reload_failed containing {needle:?} within 5s"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_refuses_unsigned_or_tampered_policy() {
    let signer = Signer::new();
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.yaml");
    let keys = signer.verify_keys();

    // No .sig at all.
    std::fs::write(&cfg, policy(1, "alpha")).unwrap();
    let err = acb_daemon::policy_file::load(&cfg, Some(&keys), None).unwrap_err();
    assert!(format!("{err:#}").contains("read signature"), "{err:#}");

    // Valid signature, then the file is edited underneath it.
    signer.install(&cfg, &policy(1, "alpha"));
    acb_daemon::policy_file::load(&cfg, Some(&keys), None).expect("signed policy loads");
    std::fs::write(&cfg, policy(1, "evil")).unwrap();
    let err = acb_daemon::policy_file::load(&cfg, Some(&keys), None).unwrap_err();
    assert!(format!("{err:#}").contains("does not match"), "{err:#}");

    // Signed by a key the daemon does not trust.
    Signer::new().install(&cfg, &policy(1, "evil"));
    let err = acb_daemon::policy_file::load(&cfg, Some(&keys), None).unwrap_err();
    assert!(format!("{err:#}").contains("not trusted"), "{err:#}");

    // Without verify keys the same file loads (unsigned mode is unchanged).
    acb_daemon::policy_file::load(&cfg, None, None).expect("unsigned mode");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hot_reload_accepts_only_signed_edits_and_refuses_rollback() {
    let signer = Signer::new();
    let (running, token, _dir, cfg, _profile) = spawn_signed(&signer, &policy(5, "alpha")).await;
    let base = format!("http://{}", running.addr);
    let c = reqwest::Client::new();
    let mut rx = running.state.events().subscribe();

    let v = config_json(&c, &base, &token).await;
    assert_eq!(v["rules"][0]["name"], "alpha");
    assert_eq!(v["revision"], 5);
    assert_eq!(v["signature_required"], true);

    // 1) The agent rewrites the file without a signature: refused, old
    //    policy stays, failure is observable on the event feed.
    std::fs::write(&cfg, policy(6, "evil")).unwrap();
    let err = wait_reload_failed(&mut rx, "does not match").await;
    assert!(err.contains("config.yaml.sig"), "{err}");
    assert_eq!(
        config_json(&c, &base, &token).await["rules"][0]["name"],
        "alpha"
    );

    // /admin/reload takes the same verified path.
    let res = c
        .post(format!("{base}/admin/reload"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
    assert!(res.text().await.unwrap().contains("does not match"));

    // 2) The operator signs a newer revision: accepted.
    signer.install(&cfg, &policy(6, "beta"));
    wait_rule0(&c, &base, &token, "beta").await;
    let v = config_json(&c, &base, &token).await;
    assert_eq!(v["revision"], 6);

    // 3) Replaying an older, validly signed policy: refused.
    signer.install(&cfg, &policy(5, "alpha"));
    let err = wait_reload_failed(&mut rx, "refusing rollback").await;
    assert!(err.contains("5") && err.contains("6"), "{err}");
    assert_eq!(
        config_json(&c, &base, &token).await["rules"][0]["name"],
        "beta"
    );

    // 4) Same revision, re-signed: accepted (a re-save is not a rollback).
    signer.install(&cfg, &policy(6, "gamma"));
    wait_rule0(&c, &base, &token, "gamma").await;

    let _ = running.shutdown.send(());
    let _ = running.join.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reloaded_event_names_the_signer() {
    let signer = Signer::new();
    let (running, token, _dir, cfg, _profile) = spawn_signed(&signer, &policy(1, "alpha")).await;
    let base = format!("http://{}", running.addr);
    let c = reqwest::Client::new();
    let mut rx = running.state.events().subscribe();

    signer.install(&cfg, &policy(2, "beta"));
    wait_rule0(&c, &base, &token, "beta").await;

    let want = signer.verify_keys().fingerprints()[0].clone();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match timeout(remaining, rx.recv()).await {
            Ok(Ok(ActivityEvent::PolicyReloaded {
                revision, signer, ..
            })) => {
                assert_eq!(revision, 2);
                assert_eq!(signer.as_deref(), Some(want.as_str()));
                break;
            }
            Ok(_) => continue,
            Err(_) => panic!("no policy.reloaded event"),
        }
    }

    let _ = running.shutdown.send(());
    let _ = running.join.await;
}

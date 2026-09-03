//! End-to-end tests for the signing commands: `sign-config`, `verify-config`
//! and the signing path of `edit-config`. They drive the real `ssh-keygen`
//! (the production signer) and skip when it is not installed.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as Proc;

use assert_cmd::Command;
use predicates::prelude::*;

const VALID: &str = r#"
server:
  bind: "127.0.0.1"
  port: 39100
  log_file: "./x.log"
  log_rotation: "daily"
chromium:
  binary: null
  user_data_dir: "./p"
  viewport: { width: 1280, height: 800 }
  screencast: { format: "jpeg", quality: 70, max_fps: 8 }
resource_policy:
  subresources_inherit_page: true
  always_block_schemes: ["javascript"]
  bypass_service_worker: true
rules: []
"#;

fn have_ssh_keygen() -> bool {
    Proc::new("ssh-keygen").arg("-V").output().is_ok()
}

/// Generate an unencrypted ed25519 key; returns (private key path, public key path).
fn keygen(dir: &Path, name: &str) -> (PathBuf, PathBuf) {
    let key = dir.join(name);
    let st = Proc::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "operator", "-f"])
        .arg(&key)
        .status()
        .unwrap();
    assert!(st.success());
    let pubkey = key.with_extension("pub");
    (key, pubkey)
}

fn acb() -> Command {
    let mut c = Command::cargo_bin("acb-cli").unwrap();
    c.env_remove("VISUAL")
        .env_remove("ACB_SIGNING_KEY")
        .env_remove("ACB_VERIFY_KEY")
        .env("ACB_BASE", "http://127.0.0.1:39199");
    c
}

#[test]
fn sign_then_verify_roundtrip_and_tamper_detection() {
    if !have_ssh_keygen() {
        eprintln!("ssh-keygen not installed; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.yaml");
    fs::write(&cfg, format!("revision: 4\n{VALID}")).unwrap();
    let (key, pubkey) = keygen(dir.path(), "k");

    acb()
        .args(["sign-config", "-c"])
        .arg(&cfg)
        .arg("--signing-key")
        .arg(&key)
        .assert()
        .success()
        .stdout(predicate::str::contains("signed"));
    let sig = dir.path().join("config.yaml.sig");
    assert!(fs::read_to_string(&sig)
        .unwrap()
        .starts_with("-----BEGIN SSH SIGNATURE-----"));

    acb()
        .args(["verify-config", "-c"])
        .arg(&cfg)
        .arg("--verify-key")
        .arg(&pubkey)
        .assert()
        .success()
        .stdout(predicate::str::contains("OK").and(predicate::str::contains("revision=4")));

    // Re-signing must not stop on ssh-keygen's "overwrite?" prompt.
    acb()
        .args(["sign-config", "-c"])
        .arg(&cfg)
        .arg("--signing-key")
        .arg(&key)
        .assert()
        .success();

    // An agent editing the file after the fact: the signature no longer matches.
    fs::write(
        &cfg,
        format!("revision: 4\n{}", VALID.replace("rules: []", "rules: []\n")),
    )
    .unwrap();
    acb()
        .args(["verify-config", "-c"])
        .arg(&cfg)
        .arg("--verify-key")
        .arg(&pubkey)
        .assert()
        .code(1)
        .stderr(predicate::str::contains("does not match"));

    // A key the daemon does not trust.
    let (_, other_pub) = keygen(dir.path(), "other");
    fs::write(&cfg, format!("revision: 4\n{VALID}")).unwrap();
    acb()
        .args(["verify-config", "-c"])
        .arg(&cfg)
        .arg("--verify-key")
        .arg(&other_pub)
        .assert()
        .code(1)
        .stderr(predicate::str::contains("not trusted"));
}

#[test]
fn sign_config_refuses_an_invalid_policy() {
    if !have_ssh_keygen() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.yaml");
    fs::write(&cfg, "rules: []\nbogus: 1\n").unwrap();
    let (key, _) = keygen(dir.path(), "k");
    acb()
        .args(["sign-config", "-c"])
        .arg(&cfg)
        .arg("--signing-key")
        .arg(&key)
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid policy"));
    assert!(!dir.path().join("config.yaml.sig").exists());
}

#[test]
fn edit_config_with_signing_key_bumps_revision_and_resigns() {
    if !have_ssh_keygen() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.yaml");
    fs::write(&cfg, format!("revision: 9\n{VALID}")).unwrap();
    let (key, pubkey) = keygen(dir.path(), "k");

    let new_src = dir.path().join("new.yaml");
    fs::write(
        &new_src,
        format!(
            "revision: 9\n{}",
            VALID.replace("port: 39100", "port: 39101")
        ),
    )
    .unwrap();

    acb()
        .env("EDITOR", format!("cp {}", new_src.display()))
        .args(["edit-config", "-c"])
        .arg(&cfg)
        .arg("--signing-key")
        .arg(&key)
        .assert()
        .success()
        .stdout(predicate::str::contains("revision 10"));

    let saved = fs::read_to_string(&cfg).unwrap();
    assert!(saved.starts_with("revision: 10\n"), "{saved}");
    assert!(saved.contains("port: 39101"));

    // The signature installed alongside covers exactly the saved bytes.
    acb()
        .args(["verify-config", "-c"])
        .arg(&cfg)
        .arg("--verify-key")
        .arg(&pubkey)
        .assert()
        .success()
        .stdout(predicate::str::contains("revision=10"));
}

#[test]
fn edit_config_inserts_revision_when_absent() {
    if !have_ssh_keygen() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.yaml");
    fs::write(&cfg, VALID).unwrap();
    let (key, _) = keygen(dir.path(), "k");
    let new_src = dir.path().join("new.yaml");
    fs::write(&new_src, VALID.replace("port: 39100", "port: 39102")).unwrap();

    acb()
        .env("EDITOR", format!("cp {}", new_src.display()))
        .env("ACB_SIGNING_KEY", &key)
        .args(["edit-config", "-c"])
        .arg(&cfg)
        .assert()
        .success()
        .stdout(predicate::str::contains("revision 1"));
    assert!(fs::read_to_string(&cfg)
        .unwrap()
        .starts_with("revision: 1\n"));
    assert!(dir.path().join("config.yaml.sig").exists());
}

#[test]
fn edit_config_refuses_to_unsign_a_signed_policy() {
    if !have_ssh_keygen() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.yaml");
    fs::write(&cfg, format!("revision: 2\n{VALID}")).unwrap();
    let (key, _) = keygen(dir.path(), "k");
    acb()
        .args(["sign-config", "-c"])
        .arg(&cfg)
        .arg("--signing-key")
        .arg(&key)
        .assert()
        .success();
    let before_cfg = fs::read(&cfg).unwrap();
    let before_sig = fs::read(dir.path().join("config.yaml.sig")).unwrap();

    let new_src = dir.path().join("new.yaml");
    fs::write(
        &new_src,
        format!("revision: 2\n{}", VALID.replace("port: 39100", "port: 1")),
    )
    .unwrap();

    // No --signing-key: must not write an unsigned edit over a signed policy.
    acb()
        .env("EDITOR", format!("cp {}", new_src.display()))
        .args(["edit-config", "-c"])
        .arg(&cfg)
        .assert()
        .failure()
        .stderr(predicate::str::contains("signing key"));

    assert_eq!(fs::read(&cfg).unwrap(), before_cfg);
    assert_eq!(
        fs::read(dir.path().join("config.yaml.sig")).unwrap(),
        before_sig
    );
}

#[test]
fn edit_config_signing_failure_leaves_files_untouched() {
    if !have_ssh_keygen() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.yaml");
    fs::write(&cfg, format!("revision: 2\n{VALID}")).unwrap();
    let before = fs::read(&cfg).unwrap();
    let new_src = dir.path().join("new.yaml");
    fs::write(
        &new_src,
        format!("revision: 2\n{}", VALID.replace("port: 39100", "port: 1")),
    )
    .unwrap();

    // A key path that does not exist: ssh-keygen fails, nothing is installed.
    acb()
        .env("EDITOR", format!("cp {}", new_src.display()))
        .args(["edit-config", "-c"])
        .arg(&cfg)
        .arg("--signing-key")
        .arg(dir.path().join("no-such-key"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("ssh-keygen"));
    assert_eq!(fs::read(&cfg).unwrap(), before);
    assert!(!dir.path().join("config.yaml.sig").exists());
}

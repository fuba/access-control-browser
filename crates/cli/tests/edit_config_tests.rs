//! End-to-end tests for `acb-cli edit-config` against an *unprotected* file
//! (the path that needs no sudo). The editor is faked with `cp <src>` so the
//! flow runs for real: stage -> "edit" -> validate -> write back. The
//! privileged install path is covered by the unit tests in `protect::specs`
//! and verified manually, since CI cannot grant root.

use std::fs;
use std::path::Path;

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

/// `acb-cli edit-config -c <cfg>` with `EDITOR="cp <src>"`, so the fake
/// editor overwrites the staged file with `src`. Reload is pointed at a dead
/// port so the best-effort nudge fails instantly without touching a real
/// daemon.
fn run_edit(cfg: &Path, editor_src: &Path) -> assert_cmd::assert::Assert {
    Command::cargo_bin("acb-cli")
        .unwrap()
        .env_remove("VISUAL")
        .env("EDITOR", format!("cp {}", editor_src.display()))
        .env("ACB_BASE", "http://127.0.0.1:39199")
        .args(["edit-config", "-c", &cfg.to_string_lossy()])
        .assert()
}

#[test]
fn saves_a_valid_edit() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.yaml");
    fs::write(&cfg, VALID).unwrap();

    let new_src = dir.path().join("new.yaml");
    fs::write(&new_src, VALID.replace("port: 39100", "port: 39101")).unwrap();

    run_edit(&cfg, &new_src)
        .success()
        .stdout(predicate::str::contains("saved"));

    assert!(fs::read_to_string(&cfg).unwrap().contains("port: 39101"));
}

#[test]
fn rejects_invalid_edit_and_keeps_original() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.yaml");
    fs::write(&cfg, VALID).unwrap();

    // Unknown top-level field -> strict loader rejects it.
    let bad_src = dir.path().join("bad.yaml");
    fs::write(&bad_src, format!("{VALID}\nbogus: 1\n")).unwrap();

    run_edit(&cfg, &bad_src)
        .failure()
        .stderr(predicate::str::contains("INVALID"));

    // Original is untouched: still the unedited valid policy.
    let after = fs::read_to_string(&cfg).unwrap();
    assert_eq!(after, VALID);
    assert!(!after.contains("bogus"));
}

#[test]
fn reports_no_changes_when_editor_is_a_noop() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.yaml");
    fs::write(&cfg, VALID).unwrap();

    Command::cargo_bin("acb-cli")
        .unwrap()
        .env_remove("VISUAL")
        .env("EDITOR", "true")
        .env("ACB_BASE", "http://127.0.0.1:39199")
        .args(["edit-config", "-c", &cfg.to_string_lossy()])
        .assert()
        .success()
        .stdout(predicate::str::contains("no changes"));

    assert_eq!(fs::read_to_string(&cfg).unwrap(), VALID);
}

//! Integration test for `acb-cli validate`. Real binary, real YAML on disk.

use std::io::Write;

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;
use tempfile::NamedTempFile;

fn config_file() -> NamedTempFile {
    let mut f = NamedTempFile::new().expect("tempfile");
    write!(
        f,
        r#"
server:
  bind: "127.0.0.1"
  port: 39100
  log_file: "./logs/acb.log"
  log_rotation: "daily"
chromium:
  binary: null
  user_data_dir: "./p"
  viewport: {{ width: 1280, height: 800 }}
  screencast: {{ format: "jpeg", quality: 70, max_fps: 8 }}
resource_policy:
  subresources_inherit_page: true
  always_block_schemes: ["javascript", "data", "file"]
  bypass_service_worker: true
rules:
  - name: "github"
    match: {{ kind: "fqdn", host: "github.com", subdomains: false }}
    allowed_classes: []
"#
    )
    .unwrap();
    f.flush().unwrap();
    f
}

fn cli() -> Command {
    Command::cargo_bin("acb-cli").unwrap()
}

#[test]
fn allowed_url_exits_zero() {
    let f = config_file();
    cli()
        .args(["validate", "https://github.com/foo", "--config"])
        .arg(f.path())
        .assert()
        .success()
        .stdout(contains("ALLOWED").and(contains("github")));
}

#[test]
fn blocked_url_exits_one() {
    let f = config_file();
    cli()
        .args(["validate", "https://evil.example/", "--config"])
        .arg(f.path())
        .assert()
        .failure()
        .code(1)
        .stderr(contains("BLOCKED"));
}

#[test]
fn javascript_scheme_blocked() {
    let f = config_file();
    cli()
        .args(["validate", "javascript:alert(1)", "--config"])
        .arg(f.path())
        .assert()
        .failure()
        .code(1)
        .stderr(contains("BLOCKED"));
}

#[test]
fn missing_config_exits_two() {
    cli()
        .args(["validate", "https://x/", "--config", "/nonexistent.yaml"])
        .assert()
        .failure()
        .code(2)
        .stderr(contains("failed to read"));
}

#[test]
fn malformed_config_exits_two() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(f, "not: a: valid: yaml: structure: [").unwrap();
    f.flush().unwrap();
    cli()
        .args(["validate", "https://x/", "--config"])
        .arg(f.path())
        .assert()
        .failure()
        .code(2)
        .stderr(contains("invalid policy"));
}

//! Tests for `load_policy`: schema strictness and validation.

use acb_policy::load::{load_policy, LoadError};

fn minimal_yaml() -> String {
    r#"
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
"#
    .to_string()
}

#[test]
fn minimal_config_loads() {
    let cfg = load_policy(&minimal_yaml()).expect("minimal config");
    assert_eq!(cfg.server.port, 39100);
    assert!(cfg.rules.is_empty());
}

#[test]
fn unknown_top_level_field_rejected() {
    let mut yaml = minimal_yaml();
    yaml.push_str("\nfoo: bar\n");
    let err = load_policy(&yaml).expect_err("unknown field must reject");
    assert!(matches!(err, LoadError::Parse(_)), "got {err:?}");
}

#[test]
fn duplicate_rule_names_rejected() {
    let yaml = r#"
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
  always_block_schemes: []
  bypass_service_worker: true
rules:
  - name: "dup"
    match: { kind: "fqdn", host: "a.example", subdomains: false }
    allowed_classes: []
  - name: "dup"
    match: { kind: "fqdn", host: "b.example", subdomains: false }
    allowed_classes: []
"#;
    let err = load_policy(yaml).expect_err("duplicate names must reject");
    assert!(matches!(err, LoadError::DuplicateRule(_)), "got {err:?}");
}

#[test]
fn invalid_regex_rejected() {
    let yaml = r#"
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
  always_block_schemes: []
  bypass_service_worker: true
rules:
  - name: "bad"
    match: { kind: "regex", pattern: "([" }
    allowed_classes: []
"#;
    let err = load_policy(yaml).expect_err("invalid regex must reject");
    assert!(matches!(err, LoadError::InvalidRegex { .. }), "got {err:?}");
}

#[test]
fn invalid_cidr_rejected() {
    let yaml = r#"
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
  always_block_schemes: []
  bypass_service_worker: true
rules:
  - name: "bad-cidr"
    match: { kind: "ip_cidr", cidr: "999.0.0.0/8" }
    allowed_classes: []
"#;
    let err = load_policy(yaml).expect_err("invalid cidr must reject");
    assert!(matches!(err, LoadError::InvalidCidr { .. }), "got {err:?}");
}

#[test]
fn port_zero_rejected() {
    let mut yaml = minimal_yaml();
    yaml = yaml.replace("port: 39100", "port: 0");
    let err = load_policy(&yaml).expect_err("port=0 must reject");
    assert!(
        matches!(err, LoadError::InvalidServer { .. }),
        "got {err:?}"
    );
}

#[test]
fn loaded_etag_is_stable() {
    let cfg1 = load_policy(&minimal_yaml()).unwrap();
    let cfg2 = load_policy(&minimal_yaml()).unwrap();
    assert_eq!(cfg1.etag, cfg2.etag);
}

#[test]
fn loaded_etag_changes_on_content_change() {
    let cfg1 = load_policy(&minimal_yaml()).unwrap();
    let yaml2 = minimal_yaml().replace("port: 39100", "port: 39101");
    let cfg2 = load_policy(&yaml2).unwrap();
    assert_ne!(cfg1.etag, cfg2.etag);
}

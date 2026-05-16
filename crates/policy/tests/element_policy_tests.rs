//! Tests for `allowed_classes_for`: subtree-allowed mode element scoping.

use acb_policy::{element_policy::allowed_classes_for, load::load_policy};

fn fixture() -> acb_policy::CompiledPolicy {
    let yaml = r#"
server:
  bind: "127.0.0.1"
  port: 39100
  log_file: "./logs/access-control-browser.log"
  log_rotation: "daily"

chromium:
  binary: null
  user_data_dir: "./var/profile"
  viewport: { width: 1280, height: 800 }
  screencast: { format: "jpeg", quality: 70, max_fps: 8 }

resource_policy:
  subresources_inherit_page: true
  always_block_schemes: ["javascript", "data", "file"]
  bypass_service_worker: true

rules:
  - name: "github"
    match: { kind: "fqdn", host: "github.com", subdomains: false }
    allowed_classes: ["js-issue-row", "Box-row"]
  - name: "wiki"
    match: { kind: "fqdn", host: "wikipedia.org", subdomains: true }
    allowed_classes: []
"#;
    load_policy(yaml).expect("fixture compiles")
}

#[test]
fn classes_for_matching_rule() {
    let cfg = fixture();
    let classes = allowed_classes_for("https://github.com/foo", &cfg);
    assert_eq!(classes, vec!["js-issue-row", "Box-row"]);
}

#[test]
fn classes_for_subdomain_rule() {
    let cfg = fixture();
    let classes = allowed_classes_for("https://en.wikipedia.org/wiki/Foo", &cfg);
    // Empty allowed_classes = page loads but nothing is interactable.
    assert!(classes.is_empty());
}

#[test]
fn classes_for_unknown_url() {
    let cfg = fixture();
    let classes = allowed_classes_for("https://random.example/", &cfg);
    assert!(classes.is_empty());
}

#[test]
fn classes_for_malformed_url() {
    let cfg = fixture();
    let classes = allowed_classes_for("not a url", &cfg);
    assert!(classes.is_empty());
}

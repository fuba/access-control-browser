//! `load_policy_with_base` resolves relative paths in `log_file` and
//! `user_data_dir` against the supplied base directory (typically the
//! config file's parent). Absolute paths pass through unchanged.

use std::path::Path;

use acb_policy::load::load_policy_with_base;

fn yaml_with(log_file: &str, user_data_dir: &str) -> String {
    format!(
        r#"
server:
  bind: "127.0.0.1"
  port: 39100
  log_file: "{log_file}"
  log_rotation: "never"
chromium:
  binary: null
  user_data_dir: "{user_data_dir}"
  viewport: {{ width: 1024, height: 768 }}
  screencast: {{ format: "jpeg", quality: 60, max_fps: 8 }}
resource_policy:
  subresources_inherit_page: true
  always_block_schemes: ["javascript"]
  bypass_service_worker: true
rules: []
"#
    )
}

#[test]
fn relative_dot_slash_resolves_against_base() {
    let cfg = load_policy_with_base(
        &yaml_with("./logs/x.log", "./var/profile"),
        Some(Path::new("/etc/acb")),
    )
    .expect("compile");
    assert_eq!(cfg.server.log_file, "/etc/acb/logs/x.log");
    assert_eq!(cfg.chromium.user_data_dir, "/etc/acb/var/profile");
}

#[test]
fn bare_relative_resolves_against_base() {
    let cfg = load_policy_with_base(
        &yaml_with("logs/y.log", "var/profile"),
        Some(Path::new("/srv/acb")),
    )
    .expect("compile");
    assert_eq!(cfg.server.log_file, "/srv/acb/logs/y.log");
    assert_eq!(cfg.chromium.user_data_dir, "/srv/acb/var/profile");
}

#[test]
fn absolute_paths_pass_through() {
    let cfg = load_policy_with_base(
        &yaml_with("/var/log/acb.log", "/var/lib/acb/profile"),
        Some(Path::new("/etc/acb")),
    )
    .expect("compile");
    assert_eq!(cfg.server.log_file, "/var/log/acb.log");
    assert_eq!(cfg.chromium.user_data_dir, "/var/lib/acb/profile");
}

#[test]
fn no_base_keeps_strings_verbatim() {
    // Backwards-compat: callers that don't supply a base get the YAML
    // string as written.
    let cfg = load_policy_with_base(&yaml_with("./logs/z.log", "./p"), None).expect("compile");
    assert_eq!(cfg.server.log_file, "./logs/z.log");
    assert_eq!(cfg.chromium.user_data_dir, "./p");
}

#[test]
fn parent_traversal_in_relative_is_preserved() {
    // ../shared/logs is a legitimate way to share a directory between
    // two configs; we don't normalize it away.
    let cfg = load_policy_with_base(
        &yaml_with("../shared/logs/x.log", "../shared/profile"),
        Some(Path::new("/etc/acb")),
    )
    .expect("compile");
    assert_eq!(cfg.server.log_file, "/etc/acb/../shared/logs/x.log");
    assert_eq!(cfg.chromium.user_data_dir, "/etc/acb/../shared/profile");
}

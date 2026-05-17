//! Tests for `validate_url`: the single decision point for top-level URL access.

use acb_policy::{
    load::load_policy,
    url_validator::{validate_url, BlockReason, UrlClass},
};

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
  always_block_schemes: ["javascript", "data", "file", "chrome", "about", "blob", "ws", "wss"]
  bypass_service_worker: true

rules:
  - name: "github-exact"
    match: { kind: "fqdn", host: "github.com", subdomains: false }
    allowed_classes: ["Box-row"]
  - name: "wikipedia-any"
    match: { kind: "fqdn", host: "wikipedia.org", subdomains: true }
    allowed_classes: ["mw-parser-output"]
  - name: "internal-docs"
    match: { kind: "regex", pattern: "^https://docs\\.internal\\.example\\.com/(api|guide)(/.*)?$" }
    allowed_classes: ["doc-content"]
  - name: "lan-grafana"
    match:
      kind: "ip_cidr"
      cidr: "10.0.0.0/8"
      ports: [3000]
      schemes: ["http", "https"]
    allowed_classes: ["dashboard-panel"]
  - name: "lan-v6"
    match:
      kind: "ip_cidr"
      cidr: "fd00::/8"
    allowed_classes: ["panel"]
"#;
    load_policy(yaml).expect("fixture policy compiles")
}

fn assert_allowed(url: &str, expected_rule: &str, cfg: &acb_policy::CompiledPolicy) {
    match validate_url(url, cfg) {
        Ok(UrlClass::Allowed { rule_name }) => assert_eq!(rule_name, expected_rule, "url={url}"),
        Err(e) => panic!("expected allow for {url}, got block: {e:?}"),
    }
}

fn assert_blocked(
    url: &str,
    cfg: &acb_policy::CompiledPolicy,
    predicate: impl FnOnce(&BlockReason) -> bool,
) {
    match validate_url(url, cfg) {
        Ok(_) => panic!("expected block for {url}, got allow"),
        Err(reason) => assert!(
            predicate(&reason),
            "wrong block reason for {url}: {reason:?}"
        ),
    }
}

#[test]
fn fqdn_exact_match() {
    let cfg = fixture();
    assert_allowed("https://github.com/", "github-exact", &cfg);
    assert_allowed("https://github.com/foo/bar", "github-exact", &cfg);
}

#[test]
fn fqdn_exact_rejects_subdomain() {
    let cfg = fixture();
    assert_blocked("https://api.github.com/users", &cfg, |r| {
        matches!(r, BlockReason::NoMatchingRule)
    });
}

#[test]
fn fqdn_subdomain_wildcard() {
    let cfg = fixture();
    assert_allowed("https://en.wikipedia.org/wiki/Foo", "wikipedia-any", &cfg);
    assert_allowed("https://wikipedia.org/", "wikipedia-any", &cfg);
    assert_allowed("https://ja.m.wikipedia.org/wiki/Bar", "wikipedia-any", &cfg);
}

#[test]
fn fqdn_subdomain_wildcard_rejects_lookalikes() {
    let cfg = fixture();
    // Naive substring matchers would let this through; ensure suffix-with-dot only.
    assert_blocked("https://evilwikipedia.org/", &cfg, |r| {
        matches!(r, BlockReason::NoMatchingRule)
    });
    assert_blocked("https://wikipedia.org.attacker.example/", &cfg, |r| {
        matches!(r, BlockReason::NoMatchingRule)
    });
}

#[test]
fn regex_rule_matches_anchored() {
    let cfg = fixture();
    assert_allowed(
        "https://docs.internal.example.com/api/v1",
        "internal-docs",
        &cfg,
    );
    assert_allowed(
        "https://docs.internal.example.com/guide",
        "internal-docs",
        &cfg,
    );
}

#[test]
fn regex_rule_rejects_other_paths() {
    let cfg = fixture();
    assert_blocked(
        "https://docs.internal.example.com/private/secrets",
        &cfg,
        |r| matches!(r, BlockReason::NoMatchingRule),
    );
}

#[test]
fn ip_cidr_matches_with_port() {
    let cfg = fixture();
    assert_allowed("http://10.1.2.3:3000/d/abc", "lan-grafana", &cfg);
    assert_allowed("https://10.255.0.1:3000/", "lan-grafana", &cfg);
}

#[test]
fn ip_cidr_rejects_wrong_port() {
    let cfg = fixture();
    assert_blocked("http://10.1.2.3:8080/", &cfg, |r| {
        matches!(r, BlockReason::NoMatchingRule)
    });
}

#[test]
fn ip_cidr_v6() {
    let cfg = fixture();
    assert_allowed("http://[fd00::1]/", "lan-v6", &cfg);
}

#[test]
fn ip_cidr_does_not_match_dns_name() {
    let cfg = fixture();
    // Host is a name, not a literal — IP CIDR rule must not match.
    assert_blocked("http://internal.example/foo", &cfg, |r| {
        matches!(r, BlockReason::NoMatchingRule)
    });
}

#[test]
fn javascript_scheme_blocked() {
    let cfg = fixture();
    assert_blocked("javascript:alert(1)", &cfg, |r| {
        matches!(r, BlockReason::SchemeBlocked(_))
    });
}

#[test]
fn data_url_blocked() {
    let cfg = fixture();
    assert_blocked("data:text/html,<script>x</script>", &cfg, |r| {
        matches!(r, BlockReason::SchemeBlocked(_))
    });
}

#[test]
fn file_scheme_blocked() {
    let cfg = fixture();
    assert_blocked("file:///etc/passwd", &cfg, |r| {
        matches!(r, BlockReason::SchemeBlocked(_))
    });
}

#[test]
fn chrome_scheme_blocked() {
    let cfg = fixture();
    assert_blocked("chrome://settings/", &cfg, |r| {
        matches!(r, BlockReason::SchemeBlocked(_))
    });
}

#[test]
fn ftp_scheme_blocked_even_if_not_in_deny_list() {
    // Only http/https beyond the deny list, even if scheme isn't explicitly denied.
    let cfg = fixture();
    assert_blocked("ftp://example.com/", &cfg, |r| {
        matches!(r, BlockReason::SchemeBlocked(_))
    });
}

#[test]
fn malformed_url() {
    let cfg = fixture();
    assert_blocked("not a url", &cfg, |r| {
        matches!(r, BlockReason::MalformedUrl(_))
    });
    assert_blocked("://broken", &cfg, |r| {
        matches!(r, BlockReason::MalformedUrl(_))
    });
}

#[test]
fn no_matching_rule() {
    let cfg = fixture();
    assert_blocked("https://random.example.org/", &cfg, |r| {
        matches!(r, BlockReason::NoMatchingRule)
    });
}

#[test]
fn scheme_is_case_insensitive_for_deny() {
    let cfg = fixture();
    assert_blocked("JavaScript:alert(1)", &cfg, |r| {
        matches!(r, BlockReason::SchemeBlocked(_))
    });
}

#[test]
fn host_is_case_insensitive() {
    let cfg = fixture();
    assert_allowed("https://GITHUB.com/", "github-exact", &cfg);
    assert_allowed("https://EN.Wikipedia.ORG/", "wikipedia-any", &cfg);
}

#[test]
fn userinfo_bait_rejected() {
    // The host of `http://github.com:80@evil.example/` is `evil.example`,
    // not `github.com`. A naive reader might mis-assess the URL; reject
    // userinfo outright as MalformedUrl so neither side of the `@` can be
    // confused.
    let cfg = fixture();
    assert_blocked("http://github.com:80@evil.example/", &cfg, |r| {
        matches!(r, BlockReason::MalformedUrl(_))
    });
    // Reverse direction: allowlisted host AFTER the `@` — still rejected,
    // because we treat any userinfo as suspicious for an allowlist context.
    assert_blocked("http://attacker.example:80@github.com/", &cfg, |r| {
        matches!(r, BlockReason::MalformedUrl(_))
    });
}

#[test]
fn default_port_treated_as_known_default() {
    // `port_or_known_default` returns 80 for http / 443 for https when port
    // is omitted. The IP CIDR rule's port filter must therefore reject
    // schemes whose default port is not in the list.
    let cfg = fixture();
    // No port -> default 80 / 443, neither in [3000].
    assert_blocked("http://10.1.2.3/", &cfg, |r| {
        matches!(r, BlockReason::NoMatchingRule)
    });
    assert_blocked("https://10.1.2.3/", &cfg, |r| {
        matches!(r, BlockReason::NoMatchingRule)
    });
}

#[test]
fn ipv4_mapped_ipv6_does_not_match_ipv4_cidr() {
    // `::ffff:10.1.2.3` is the IPv4-mapped IPv6 form of 10.1.2.3. CIDR rules
    // are family-specific in `ipnet::IpNet`; do not let the mapped form
    // sneak past a 10.0.0.0/8 (v4) rule.
    let cfg = fixture();
    assert_blocked("http://[::ffff:10.1.2.3]:3000/", &cfg, |r| {
        matches!(r, BlockReason::NoMatchingRule)
    });
}

#[test]
fn upper_case_scheme_allowed_after_normalization() {
    // RFC 3986: scheme is case-insensitive. `Url::parse` lowercases it for
    // us, so http/HTTP are equivalent at the validator boundary.
    let cfg = fixture();
    assert_allowed("HTTPS://github.com/", "github-exact", &cfg);
}

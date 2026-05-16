// The injected helper is fingerprinted at startup; the hash is surfaced via
// `/config`. The CI gate compares this hash to the checked-in
// `crates/injected-js/dist/snapshot-helper.js`.

mod support;

use support::{fixture_policy, TestDaemon};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_exposes_helper_sha256() {
    let policy = fixture_policy(r#"  []"#);
    let d = TestDaemon::spawn(policy).await;
    let cfg: serde_json::Value = d
        .client()
        .get(format!("{}/config", d.base))
        .bearer_auth(&d.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let hash = cfg["helper_sha256"].as_str().expect("hash field");
    assert_eq!(hash.len(), 64, "SHA-256 hex should be 64 chars");
    assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    // Stable: re-fetching returns same value.
    let cfg2: serde_json::Value = d
        .client()
        .get(format!("{}/config", d.base))
        .bearer_auth(&d.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cfg2["helper_sha256"], hash);
    d.shutdown().await;
}

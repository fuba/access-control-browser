// Bearer-token enforcement on the protected subtree.

mod support;

use support::{fixture_policy, TestDaemon};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn healthz_is_unauthenticated() {
    let policy = fixture_policy(r#"  []"#);
    let d = TestDaemon::spawn(policy).await;
    let res = d
        .client()
        .get(format!("{}/healthz", d.base))
        .send()
        .await
        .unwrap();
    assert!(
        res.status().is_success(),
        "healthz should be 200, got {}",
        res.status()
    );
    assert_eq!(res.headers().get("x-acb").unwrap(), "1");
    d.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_requires_token() {
    let policy = fixture_policy(r#"  []"#);
    let d = TestDaemon::spawn(policy).await;
    let res = d
        .client()
        .get(format!("{}/config", d.base))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401, "no token must be 401");
    d.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_token_rejected() {
    let policy = fixture_policy(r#"  []"#);
    let d = TestDaemon::spawn(policy).await;
    let res = d
        .client()
        .get(format!("{}/config", d.base))
        .bearer_auth("wrong-token")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
    d.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn correct_token_passes() {
    let policy = fixture_policy(r#"  []"#);
    let d = TestDaemon::spawn(policy).await;
    let res = d
        .client()
        .get(format!("{}/config", d.base))
        .bearer_auth(&d.token)
        .send()
        .await
        .unwrap();
    assert!(res.status().is_success(), "got {}", res.status());
    let json: serde_json::Value = res.json().await.unwrap();
    assert!(json["etag"].is_string());
    d.shutdown().await;
}

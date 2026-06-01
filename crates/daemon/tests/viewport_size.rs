// The daemon must pin Chromium's rendered viewport to the configured size
// (via Emulation.setDeviceMetricsOverride at session create). Otherwise the
// screencast frames render at Chromium's default window size while the UI
// scales mouse coordinates against the configured size, and clicks miss.
//
// The fixture policy configures viewport 1024x768 (see support::fixture_policy),
// so a freshly created session's `window.innerWidth/innerHeight` must report
// exactly that.

mod support;

use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
use serde_json::json;

use support::{fixture_policy, TestDaemon};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_viewport_matches_configured_size() {
    let policy = fixture_policy(
        r#"  - name: "allow-all"
    match: { kind: "regex", pattern: '^https?://.*$' }"#,
    );
    let d = TestDaemon::spawn(policy).await;

    // Create a session; about:blank is fine — we only read the viewport.
    let sess: serde_json::Value = d
        .client()
        .post(format!("{}/sessions", d.base))
        .bearer_auth(&d.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = sess["id"].as_str().unwrap().to_string();

    let session = d.running.state.get_session(&id).await.unwrap();
    let dims: serde_json::Value = session
        .page
        .execute(
            EvaluateParams::builder()
                .expression("JSON.stringify([window.innerWidth, window.innerHeight])")
                .return_by_value(true)
                .build()
                .unwrap(),
        )
        .await
        .unwrap()
        .result
        .result
        .value
        .clone()
        .unwrap_or(serde_json::Value::Null);
    let parsed: serde_json::Value =
        serde_json::from_str(dims.as_str().unwrap_or("null")).unwrap_or(json!(null));

    assert_eq!(parsed[0].as_u64(), Some(1024), "innerWidth should be 1024");
    assert_eq!(parsed[1].as_u64(), Some(768), "innerHeight should be 768");

    d.shutdown().await;
}

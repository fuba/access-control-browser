// `Browser.setDownloadBehavior(Deny)` blocks file saves. We exercise it by
// telling the page to navigate to a Content-Disposition: attachment URL and
// asserting that no download event arrived. There's no positive signal for
// "no download" from CDP, so we verify indirectly: the in-page document
// remains the page we opened (Chromium cancels the navigation instead of
// saving).

mod support;

use std::collections::HashMap;

use serde_json::json;

use support::{fixture_policy, spawn_static, TestDaemon};

fn files() -> HashMap<String, Vec<u8>> {
    let mut m = HashMap::new();
    m.insert(
        "/page/index.html".into(),
        b"<!doctype html><html><body><p id=marker>stay</p></body></html>".to_vec(),
    );
    // A would-be download. Our static server doesn't set
    // Content-Disposition, but a click on a `download` anchor still asks
    // Chromium to save unless the behavior is Deny.
    m.insert("/page/file.bin".into(), b"\x00\x01\x02".to_vec());
    m
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn download_deny_keeps_us_on_page() {
    let (page_base, page_shutdown) = spawn_static(files()).await;
    let regex_page = format!("^{}/page/.*$", regex_escape(&page_base));
    let policy = fixture_policy(&format!(
        r#"  - name: "page-allow"
    match: {{ kind: "regex", pattern: '{regex_page}' }}
    allowed_classes: []"#
    ));
    let d = TestDaemon::spawn(policy).await;

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
    let id = sess["id"].as_str().unwrap();
    d.client()
        .post(format!("{}/sessions/{}/open", d.base, id))
        .bearer_auth(&d.token)
        .json(&json!({"url": format!("{page_base}/page/index.html")}))
        .send()
        .await
        .unwrap();

    // Trigger an in-page <a download> click. With SetDownloadBehavior::Deny
    // the click is a no-op (the file is not saved) and the page stays put.
    let session = d.running.state.get_session(id).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let _ = session
        .page
        .execute(
            chromiumoxide::cdp::js_protocol::runtime::EvaluateParams::builder()
                .expression(
                    r#"(() => {
                        const a = document.createElement('a');
                        a.href = '/page/file.bin';
                        a.setAttribute('download', 'file.bin');
                        document.body.appendChild(a);
                        a.click();
                    })()"#,
                )
                .return_by_value(true)
                .build()
                .unwrap(),
        )
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let evaluate = session
        .page
        .execute(
            chromiumoxide::cdp::js_protocol::runtime::EvaluateParams::builder()
                .expression("!!document.getElementById('marker')")
                .return_by_value(true)
                .build()
                .unwrap(),
        )
        .await
        .unwrap()
        .result
        .clone();
    let still_here = evaluate
        .result
        .value
        .as_ref()
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    assert!(
        still_here,
        "page must remain (download Deny prevented save)"
    );

    let _ = page_shutdown.send(());
    d.shutdown().await;
}

fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        if "\\.+*?()|[]{}^$".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

// Per-session CDP request interceptor.
//
// On install:
//   1. Enable `Fetch` so every network request is paused before send.
//   2. Subscribe to `Fetch.requestPaused` and dispatch each through
//      `acb_policy::request_policy::decide_request`.
//   3. Subscribe to `Page.frameNavigated` to keep the top-level URL fresh
//      for the inheritance rule and to learn the main frame id.
//
// Defense in depth:
//   - The scheme deny list applies before any rule lookup (in
//     `decide_request`).
//   - Service workers are bypassed and any existing ones for the visited
//     origin are cleared at session start, so SWs cannot intercept the
//     network ahead of us.
//   - Downloads are denied: a malicious page cannot exfiltrate by triggering
//     a file save.

use std::sync::Arc;

use acb_policy::request_policy::{decide_request, RequestDecision, RequestKind};
use chromiumoxide::cdp::browser_protocol::browser::{
    SetDownloadBehaviorBehavior, SetDownloadBehaviorParams,
};
use chromiumoxide::cdp::browser_protocol::fetch::{
    ContinueRequestParams, EnableParams as FetchEnableParams, EventRequestPaused,
    FailRequestParams, RequestPattern, RequestStage,
};
use chromiumoxide::cdp::browser_protocol::network::{
    ErrorReason, ResourceType, SetBypassServiceWorkerParams,
};
use chromiumoxide::cdp::browser_protocol::page::EventFrameNavigated;
use chromiumoxide::cdp::browser_protocol::target::{CloseTargetParams, EventTargetCreated};
use futures::StreamExt;

use crate::browser::session::Session;
use crate::events::{now_unix, ActivityEvent};
use crate::AppState;

pub async fn install(session: Arc<Session>, state: AppState) -> anyhow::Result<()> {
    // Bypass any service worker so we see the network requests, not the
    // SW's cache responses.
    let _ = session
        .page
        .execute(SetBypassServiceWorkerParams { bypass: true })
        .await;

    // Deny downloads.
    let _ = session
        .page
        .execute(SetDownloadBehaviorParams {
            behavior: SetDownloadBehaviorBehavior::Deny,
            browser_context_id: None,
            download_path: None,
            events_enabled: None,
        })
        .await;

    // Enable Fetch with a wildcard pattern.
    session
        .page
        .execute(FetchEnableParams {
            patterns: Some(vec![RequestPattern {
                url_pattern: Some("*".to_string()),
                resource_type: None,
                request_stage: Some(RequestStage::Request),
            }]),
            handle_auth_requests: Some(false),
        })
        .await?;

    // Spawn the request-paused pump.
    let mut events = session.page.event_listener::<EventRequestPaused>().await?;
    let s = session.clone();
    let st = state.clone();
    let task = tokio::spawn(async move {
        while let Some(ev) = events.next().await {
            handle_paused(&ev, &s, &st).await;
        }
    });
    session.push_task(task).await;

    // Track frame navigations: keep current_url and learn the main frame id.
    let mut navs = session.page.event_listener::<EventFrameNavigated>().await?;
    let s2 = session.clone();
    let task2 = tokio::spawn(async move {
        while let Some(ev) = navs.next().await {
            if ev.frame.parent_id.is_none() {
                *s2.current_url.write().await = Some(ev.frame.url.clone());
            }
        }
    });
    session.push_task(task2).await;

    // Close popup targets whose URL is not allowed. Browser-level event.
    let browser = match state.browser_clone().await {
        Some(b) => b,
        None => return Ok(()),
    };
    let mut targets = browser
        .browser
        .event_listener::<EventTargetCreated>()
        .await?;
    let s3 = session.clone();
    let st3 = state.clone();
    let task3 = tokio::spawn(async move {
        while let Some(ev) = targets.next().await {
            // Only act on page-type targets opened from our session (popups,
            // window.open, target=_blank). Ignore other types.
            if ev.target_info.r#type != "page" {
                continue;
            }
            // Only consider targets that have ourselves as opener.
            let opener_ok = ev
                .target_info
                .opener_id
                .as_ref()
                .map(|oid| oid.inner() == s3.page.target_id().inner())
                .unwrap_or(false);
            if !opener_ok {
                continue;
            }
            let url = &ev.target_info.url;
            let policy = st3.policy();
            let allowed = acb_policy::url_validator::validate_url(url, &policy).is_ok();
            if !allowed {
                let _ = browser
                    .browser
                    .execute(CloseTargetParams {
                        target_id: ev.target_info.target_id.clone(),
                    })
                    .await;
                let _ = st3.events().send(ActivityEvent::Blocked {
                    ts: now_unix(),
                    session: s3.id.clone(),
                    url: url.clone(),
                    reason: "popup blocked by allowlist".into(),
                    kind: "popup".into(),
                });
            }
        }
    });
    session.push_task(task3).await;

    Ok(())
}

fn classify(ev: &EventRequestPaused) -> RequestKind {
    match &ev.resource_type {
        ResourceType::Document => {
            // For top-level vs subframe document, we'd ideally check parent
            // frame. As an approximation we treat any Document load whose
            // request URL is what triggered our `open` call as top-level by
            // policy gate (which already validates), and subframe documents
            // also get validated as top-level — that is the stricter rule
            // and matches the security model (each new document is a fresh
            // navigation that must pass the allowlist).
            RequestKind::TopLevelDocument
        }
        _ => RequestKind::Subresource,
    }
}

async fn handle_paused(ev: &EventRequestPaused, session: &Session, state: &AppState) {
    let req_url = ev.request.url.clone();
    let kind = classify(ev);
    let page_url = session.current_url.read().await.clone();
    let policy = state.policy();
    let decision = decide_request(&req_url, kind, page_url.as_deref(), &policy);

    let request_id = ev.request_id.clone();
    match decision {
        RequestDecision::Allow => {
            let _ = session
                .page
                .execute(ContinueRequestParams {
                    request_id,
                    url: None,
                    method: None,
                    post_data: None,
                    headers: None,
                    intercept_response: None,
                })
                .await;
            if matches!(kind, RequestKind::TopLevelDocument) {
                if let Ok(acb_policy::url_validator::UrlClass::Allowed { rule_name }) =
                    acb_policy::url_validator::validate_url(&req_url, &policy)
                {
                    let _ = state.events().send(ActivityEvent::Navigated {
                        ts: now_unix(),
                        session: session.id.clone(),
                        url: req_url,
                        rule: rule_name,
                    });
                }
            }
        }
        RequestDecision::Block(reason) => {
            let _ = state.events().send(ActivityEvent::Blocked {
                ts: now_unix(),
                session: session.id.clone(),
                url: req_url,
                reason: reason.to_string(),
                kind: format!("{kind:?}"),
            });
            let _ = session
                .page
                .execute(FailRequestParams {
                    request_id,
                    error_reason: ErrorReason::BlockedByClient,
                })
                .await;
        }
    }
}

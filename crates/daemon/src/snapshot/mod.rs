// Per-session snapshot machinery. A snapshot is the policy-filtered list of
// accessible elements with their `@eN` refs. Each new snapshot bumps the
// session's generation and invalidates all previous refs.

pub mod ref_table;

use std::sync::Arc;

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

use crate::browser::injected::call_helper;
use crate::browser::session::Session;
use crate::AppState;

use self::ref_table::is_ref;

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotItem {
    pub r#ref: String,
    pub tag: String,
    pub role: Option<String>,
    pub name: Option<String>,
    pub text: String,
    pub href: Option<String>,
    pub rect: Option<Rect>,
    pub parent_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotResponse {
    pub generation: u64,
    pub refs: Vec<SnapshotItem>,
}

pub async fn take(session: &Arc<Session>, state: &AppState) -> Result<SnapshotResponse> {
    // Resolve the current top URL → which class set applies.
    let current = session.current_url.read().await.clone().unwrap_or_default();
    let policy = state.policy();
    let classes: Vec<String> = acb_policy::element_policy::allowed_classes_for(&current, &policy)
        .into_iter()
        .map(|s| s.to_string())
        .collect();

    let raw = call_helper(
        &session.page,
        "collect",
        &[Value::Array(
            classes.iter().cloned().map(Value::String).collect(),
        )],
    )
    .await?;

    let arr = raw.as_array().cloned().unwrap_or_default();

    // Bump generation; refs are monotonic across snapshots so stale ones
    // can be rejected even after their number has been reused.
    let gen = session.ref_table.next_generation();
    let mut items = Vec::with_capacity(arr.len());
    let mut acb_to_ref: std::collections::HashMap<u64, String> = Default::default();
    for v in &arr {
        let acb_id = v.get("acbId").and_then(|x| x.as_u64()).unwrap_or(0);
        if acb_id == 0 {
            continue;
        }
        let r = session.ref_table.allocate(acb_id);
        acb_to_ref.insert(acb_id, r.clone());
        items.push(SnapshotItem {
            r#ref: r,
            tag: v
                .get("tag")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            role: v.get("role").and_then(|x| x.as_str()).map(String::from),
            name: v.get("name").and_then(|x| x.as_str()).map(String::from),
            text: v
                .get("text")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            href: v.get("href").and_then(|x| x.as_str()).map(String::from),
            rect: v.get("rect").and_then(|x| {
                Some(Rect {
                    x: x.get("x")?.as_f64()?,
                    y: x.get("y")?.as_f64()?,
                    w: x.get("w")?.as_f64()?,
                    h: x.get("h")?.as_f64()?,
                })
            }),
            parent_ref: None,
        });
    }
    // Stitch parent_ref.
    for (i, v) in arr.iter().enumerate() {
        if let Some(p) = v.get("parent_acb_id").and_then(|x| x.as_u64()) {
            if let Some(pr) = acb_to_ref.get(&p) {
                items[i].parent_ref = Some(pr.clone());
            }
        }
    }
    Ok(SnapshotResponse {
        generation: gen,
        refs: items,
    })
}

pub fn validate_ref_format(r: &str) -> bool {
    is_ref(r)
}

pub fn parse_ref(session: &Session, r: &str) -> Option<u64> {
    if !validate_ref_format(r) {
        return None;
    }
    let entry = session.ref_table.get(r)?;
    let current_gen = session.ref_table.current_generation();
    if entry.generation != current_gen {
        return None;
    }
    Some(entry.acb_id)
}

// `GET /config` — read-only redacted policy summary. Used by the UI to
// configure its location-bar live indicator and by `acb-cli status` to print
// rule names. The full YAML is never exposed because it may include CIDR
// internals or regex patterns the operator considers sensitive; here we only
// return rule names and the etag.

use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::AppState;

#[derive(Serialize)]
pub struct ConfigSummary {
    etag: String,
    rules: Vec<RuleSummary>,
    always_block_schemes: Vec<String>,
    subresources_inherit_page: bool,
    helper_sha256: &'static str,
}

#[derive(Serialize)]
pub struct RuleSummary {
    name: String,
    kind: &'static str,
    allowed_classes: Vec<String>,
}

pub async fn handler(State(state): State<AppState>) -> Json<ConfigSummary> {
    let p = state.policy();
    let rules = p
        .rules
        .iter()
        .map(|r| RuleSummary {
            name: r.name.clone(),
            kind: match &r.match_ {
                acb_policy::CompiledMatch::Regex(_) => "regex",
                acb_policy::CompiledMatch::Fqdn { .. } => "fqdn",
                acb_policy::CompiledMatch::IpCidr { .. } => "ip_cidr",
            },
            allowed_classes: r.allowed_classes.clone(),
        })
        .collect();
    Json(ConfigSummary {
        etag: p.etag.clone(),
        rules,
        always_block_schemes: p.resource_policy.always_block_schemes.clone(),
        subresources_inherit_page: p.resource_policy.subresources_inherit_page,
        helper_sha256: crate::browser::injected::helper_sha256(),
    })
}

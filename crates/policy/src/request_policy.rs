// Request-level access decision used by the daemon's CDP `Fetch.requestPaused`
// handler. Splits requests into top-level navigation vs sub-frame document vs
// other sub-resource, and applies inheritance from the page's URL.

use url::Url;

use crate::url_rule::CompiledPolicy;
use crate::url_validator::{check_scheme, validate_url, BlockReason};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    TopLevelDocument,
    SubFrameDocument,
    Subresource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestDecision {
    Allow,
    Block(BlockReason),
}

pub fn decide_request(
    req_url: &str,
    kind: RequestKind,
    page_url: Option<&str>,
    cfg: &CompiledPolicy,
) -> RequestDecision {
    // Defense in depth: the scheme deny list applies to every request,
    // regardless of inheritance or context.
    match Url::parse(req_url) {
        Ok(u) => {
            if let Err(reason) = check_scheme(&u, cfg) {
                return RequestDecision::Block(reason);
            }
        }
        Err(e) => return RequestDecision::Block(BlockReason::MalformedUrl(e.to_string())),
    }

    match kind {
        RequestKind::TopLevelDocument => match validate_url(req_url, cfg) {
            Ok(_) => RequestDecision::Allow,
            Err(r) => RequestDecision::Block(r),
        },
        RequestKind::SubFrameDocument | RequestKind::Subresource => {
            if cfg.resource_policy.subresources_inherit_page {
                match page_url {
                    Some(p) => match validate_url(p, cfg) {
                        Ok(_) => RequestDecision::Allow,
                        Err(_) => RequestDecision::Block(BlockReason::NoMatchingRule),
                    },
                    None => RequestDecision::Block(BlockReason::NoMatchingRule),
                }
            } else {
                match validate_url(req_url, cfg) {
                    Ok(_) => RequestDecision::Allow,
                    Err(r) => RequestDecision::Block(r),
                }
            }
        }
    }
}

// URL validation — the single decision point for whether a top-level URL is
// allowed to be opened. Used by CLI, daemon `/open` handler, the request
// interceptor's top-level branch, and (mirrored) the UI's location-bar live
// indicator.
//
// Decision order:
//   1. Reject malformed URLs.
//   2. Reject schemes in `always_block_schemes`.
//   3. Reject any scheme outside http/https.
//   4. Walk rules; first match wins -> Allowed.
//   5. Otherwise NoMatchingRule.

use thiserror::Error;
use url::Url;

use crate::url_rule::{CompiledMatch, CompiledPolicy, CompiledRule};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlClass {
    Allowed { rule_name: String },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum BlockReason {
    #[error("scheme {0:?} is denied")]
    SchemeBlocked(String),
    #[error("no rule matched")]
    NoMatchingRule,
    #[error("malformed url: {0}")]
    MalformedUrl(String),
}

pub fn validate_url(raw_url: &str, cfg: &CompiledPolicy) -> Result<UrlClass, BlockReason> {
    let url = Url::parse(raw_url).map_err(|e| BlockReason::MalformedUrl(e.to_string()))?;
    check_scheme(&url, cfg)?;
    check_no_userinfo(&url)?;
    for rule in &cfg.rules {
        if rule_matches(&url, rule) {
            return Ok(UrlClass::Allowed {
                rule_name: rule.name.clone(),
            });
        }
    }
    Err(BlockReason::NoMatchingRule)
}

// URLs with userinfo are a classic allowlist-confusion vector: the host being
// matched lives after the `@`, but a reader may misread the URL. Reject them
// outright; legitimate web usage of userinfo in browser navigation is
// effectively zero.
pub(crate) fn check_no_userinfo(url: &Url) -> Result<(), BlockReason> {
    if !url.username().is_empty() || url.password().is_some() {
        return Err(BlockReason::MalformedUrl("userinfo not allowed".into()));
    }
    Ok(())
}

pub(crate) fn check_scheme(url: &Url, cfg: &CompiledPolicy) -> Result<(), BlockReason> {
    let scheme = url.scheme().to_ascii_lowercase();
    if cfg
        .resource_policy
        .always_block_schemes
        .iter()
        .any(|s| s.eq_ignore_ascii_case(&scheme))
    {
        return Err(BlockReason::SchemeBlocked(scheme));
    }
    if scheme != "http" && scheme != "https" {
        return Err(BlockReason::SchemeBlocked(scheme));
    }
    Ok(())
}

pub(crate) fn rule_matches(url: &Url, rule: &CompiledRule) -> bool {
    match &rule.match_ {
        CompiledMatch::Regex(re) => re.is_match(url.as_str()),
        CompiledMatch::Fqdn { host, subdomains } => match url.host_str() {
            None => false,
            Some(h) => {
                let h = h.to_ascii_lowercase();
                if h == *host {
                    return true;
                }
                if *subdomains {
                    // Must be a true subdomain: ends with ".<host>".
                    h.ends_with(&format!(".{host}"))
                } else {
                    false
                }
            }
        },
        CompiledMatch::IpCidr { net, ports, schemes } => {
            if !schemes.is_empty() {
                let s = url.scheme().to_ascii_lowercase();
                if !schemes.iter().any(|x| x == &s) {
                    return false;
                }
            }
            // Use the typed Host enum: host_str() keeps the brackets on IPv6
            // and would defeat IpAddr::from_str.
            let ip: std::net::IpAddr = match url.host() {
                Some(url::Host::Ipv4(v4)) => std::net::IpAddr::V4(v4),
                Some(url::Host::Ipv6(v6)) => std::net::IpAddr::V6(v6),
                Some(url::Host::Domain(_)) | None => return false,
            };
            if !net.contains(&ip) {
                return false;
            }
            if ports.is_empty() {
                return true;
            }
            match url.port_or_known_default() {
                Some(p) => ports.contains(&p),
                None => false,
            }
        }
    }
}

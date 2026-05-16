// Load and compile a `config.yaml` document into a `CompiledPolicy`.
//
// All validation that can fail happens here so that the rest of the daemon
// operates on a guaranteed-valid policy. The etag is the SHA-256 of the
// canonical-serialized raw config; identical input -> identical etag.

use std::collections::HashSet;

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::config::{PolicyConfig, RawRule, RuleMatch};
use crate::url_rule::{CompiledMatch, CompiledPolicy, CompiledRule};

// Defensive upper bounds for compiled regexes; rejects pathologically large
// or DoS-prone patterns at load time rather than at request time.
const REGEX_SIZE_LIMIT_BYTES: usize = 64 * 1024;
const REGEX_DFA_SIZE_LIMIT_BYTES: usize = 256 * 1024;

#[derive(Debug, Error)]
pub enum LoadError {
    #[error("yaml parse error: {0}")]
    Parse(#[from] serde_yaml::Error),

    #[error("duplicate rule name: {0}")]
    DuplicateRule(String),

    #[error("invalid regex in rule {rule}: {source}")]
    InvalidRegex {
        rule: String,
        #[source]
        source: regex::Error,
    },

    #[error("invalid CIDR in rule {rule}: {message}")]
    InvalidCidr { rule: String, message: String },

    #[error("invalid port {port} in rule {rule}")]
    InvalidPort { rule: String, port: u16 },

    #[error("invalid scheme {scheme:?} in rule {rule}")]
    InvalidScheme { rule: String, scheme: String },

    #[error("invalid server config: {reason}")]
    InvalidServer { reason: String },
}

pub fn load_policy(yaml: &str) -> Result<CompiledPolicy, LoadError> {
    let raw: PolicyConfig = serde_yaml::from_str(yaml)?;
    compile(raw, etag_of(yaml))
}

fn etag_of(yaml: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(yaml.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn compile(raw: PolicyConfig, etag: String) -> Result<CompiledPolicy, LoadError> {
    validate_server(&raw.server)?;

    let mut names = HashSet::new();
    let mut rules = Vec::with_capacity(raw.rules.len());
    for r in raw.rules {
        if !names.insert(r.name.clone()) {
            return Err(LoadError::DuplicateRule(r.name));
        }
        rules.push(compile_rule(r)?);
    }

    Ok(CompiledPolicy {
        server: raw.server,
        chromium: raw.chromium,
        resource_policy: raw.resource_policy,
        rules,
        etag,
    })
}

fn validate_server(s: &crate::config::ServerConfig) -> Result<(), LoadError> {
    if s.port == 0 {
        return Err(LoadError::InvalidServer {
            reason: "port must not be 0".into(),
        });
    }
    if s.bind.trim().is_empty() {
        return Err(LoadError::InvalidServer {
            reason: "bind address must not be empty".into(),
        });
    }
    Ok(())
}

fn compile_rule(r: RawRule) -> Result<CompiledRule, LoadError> {
    let match_ = match r.match_ {
        RuleMatch::Regex { pattern } => {
            let re = regex::RegexBuilder::new(&pattern)
                .size_limit(REGEX_SIZE_LIMIT_BYTES)
                .dfa_size_limit(REGEX_DFA_SIZE_LIMIT_BYTES)
                .build()
                .map_err(|e| LoadError::InvalidRegex {
                    rule: r.name.clone(),
                    source: e,
                })?;
            CompiledMatch::Regex(re)
        }
        RuleMatch::Fqdn { host, subdomains } => {
            let host = host.trim().to_ascii_lowercase();
            if host.is_empty() {
                return Err(LoadError::InvalidServer {
                    reason: format!("rule {}: fqdn host must not be empty", r.name),
                });
            }
            CompiledMatch::Fqdn { host, subdomains }
        }
        RuleMatch::IpCidr { cidr, ports, schemes } => {
            let net: ipnet::IpNet = cidr.parse().map_err(|e: ipnet::AddrParseError| {
                LoadError::InvalidCidr {
                    rule: r.name.clone(),
                    message: e.to_string(),
                }
            })?;
            for p in &ports {
                if *p == 0 {
                    return Err(LoadError::InvalidPort {
                        rule: r.name.clone(),
                        port: *p,
                    });
                }
            }
            for s in &schemes {
                let lower = s.to_ascii_lowercase();
                if lower != "http" && lower != "https" {
                    return Err(LoadError::InvalidScheme {
                        rule: r.name.clone(),
                        scheme: s.clone(),
                    });
                }
            }
            CompiledMatch::IpCidr {
                net,
                ports,
                schemes: schemes.into_iter().map(|s| s.to_ascii_lowercase()).collect(),
            }
        }
    };

    Ok(CompiledRule {
        name: r.name,
        match_,
        allowed_classes: r.allowed_classes,
    })
}


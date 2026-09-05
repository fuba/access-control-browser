// Compiled (post-validation) forms of the YAML rule schema. Compilation does
// the expensive work once: regex compile, CIDR parse, hostname lowercasing,
// scheme normalization. Everything downstream operates on these forms.

use crate::config::{ChromiumConfig, ResourcePolicy, ServerConfig};

#[derive(Debug, Clone)]
pub struct CompiledPolicy {
    pub server: ServerConfig,
    pub chromium: ChromiumConfig,
    pub resource_policy: ResourcePolicy,
    pub rules: Vec<CompiledRule>,
    pub etag: String,
    /// See `PolicyConfig::revision`.
    pub revision: u64,
}

#[derive(Debug, Clone)]
pub struct CompiledRule {
    pub name: String,
    pub match_: CompiledMatch,
    pub allowed_classes: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum CompiledMatch {
    Regex(regex::Regex),
    Fqdn {
        // Always stored lowercase.
        host: String,
        subdomains: bool,
    },
    IpCidr {
        net: ipnet::IpNet,
        // Empty = any port.
        ports: Vec<u16>,
        // Empty = any scheme (still subject to global scheme deny).
        schemes: Vec<String>,
    },
}

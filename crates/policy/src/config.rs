// Strict-schema deserialization of `config.yaml`. Unknown fields are rejected
// at every level to prevent silent typos that would otherwise weaken the
// policy.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PolicyConfig {
    /// Monotonic policy revision. Only meaningful for *signed* policies: a
    /// daemon running with `--verify-key` refuses to hot-reload a policy
    /// whose revision is lower than the one in effect, so an agent cannot
    /// re-install an older (validly signed) file. `acb-cli edit-config`
    /// bumps it automatically. Defaults to 0 (no replay protection).
    #[serde(default)]
    pub revision: u64,
    pub server: ServerConfig,
    pub chromium: ChromiumConfig,
    pub resource_policy: ResourcePolicy,
    pub rules: Vec<RawRule>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub bind: String,
    pub port: u16,
    pub log_file: String,
    pub log_rotation: LogRotation,
    #[serde(default)]
    pub config_poll_ms: u64,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogRotation {
    Daily,
    Hourly,
    Never,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ChromiumConfig {
    #[serde(default)]
    pub binary: Option<String>,
    pub user_data_dir: String,
    pub viewport: Viewport,
    pub screencast: ScreencastConfig,
    #[serde(default)]
    pub extra_args: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScreencastConfig {
    pub format: ScreencastFormat,
    pub quality: u8,
    pub max_fps: u8,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ScreencastFormat {
    Jpeg,
    Png,
    /// Recommended for the live viewport: same perceptual quality as JPEG at
    /// ~25-35% smaller payload, so fewer bytes per frame over the WebSocket.
    Webp,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ResourcePolicy {
    pub subresources_inherit_page: bool,
    pub always_block_schemes: Vec<String>,
    pub bypass_service_worker: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RawRule {
    pub name: String,
    #[serde(rename = "match")]
    pub match_: RuleMatch,
    #[serde(default)]
    pub allowed_classes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuleMatch {
    Regex {
        pattern: String,
    },
    Fqdn {
        host: String,
        #[serde(default)]
        subdomains: bool,
    },
    IpCidr {
        cidr: String,
        #[serde(default)]
        ports: Vec<u16>,
        #[serde(default)]
        schemes: Vec<String>,
    },
}

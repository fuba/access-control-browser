// Access-controlled browser policy crate.
// Pure logic: no I/O, no async. The single source of truth for what URLs and
// elements the agent is allowed to touch.

pub mod config;
pub mod element_policy;
pub mod load;
pub mod request_policy;
pub mod url_rule;
pub mod url_validator;

pub use config::{
    ChromiumConfig, LogRotation, PolicyConfig, ResourcePolicy, RuleMatch, ScreencastConfig,
    ScreencastFormat, ServerConfig, Viewport,
};
pub use url_rule::{CompiledMatch, CompiledPolicy, CompiledRule};

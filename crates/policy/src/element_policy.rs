// Subtree-allowed element scope.
//
// Returns the list of CSS class names that the agent is allowed to read or
// interact with on a given URL. Subtree semantics (an allowed-class element
// AND its descendants are accessible) are enforced inside the injected DOM
// helper; this function only resolves the class set.

use url::Url;

use crate::url_rule::CompiledPolicy;
use crate::url_validator::rule_matches;

pub fn allowed_classes_for<'a>(url: &str, cfg: &'a CompiledPolicy) -> Vec<&'a str> {
    let parsed = match Url::parse(url) {
        Ok(u) => u,
        Err(_) => return Vec::new(),
    };
    for rule in &cfg.rules {
        if rule_matches(&parsed, rule) {
            return rule.allowed_classes.iter().map(String::as_str).collect();
        }
    }
    Vec::new()
}

// The one place the daemon turns an on-disk policy into a CompiledPolicy.
// Startup, the hot-reload watcher and `POST /admin/reload` all go through
// `load`, so signature verification and the revision guard cannot be
// bypassed by taking a different path.
//
// With verify keys configured (`--verify-key`), the file is accepted only if
// `<config>.sig` is a valid `acb-policy` sshsig over its exact bytes by a
// trusted key, and its `revision` is not lower than the policy currently in
// effect. Without verify keys the file is loaded as-is (unsigned mode).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};

use acb_policy::sig::{check_revision, Signer, VerifyKeys, SIG_SUFFIX};
use acb_policy::CompiledPolicy;

/// `config.yaml` -> `config.yaml.sig` (what `ssh-keygen -Y sign` writes).
pub fn sig_path(config: &Path) -> PathBuf {
    let mut s = config.as_os_str().to_owned();
    s.push(SIG_SUFFIX);
    PathBuf::from(s)
}

/// A freshly loaded policy plus who signed it (None in unsigned mode).
#[derive(Debug)]
pub struct Loaded {
    pub policy: CompiledPolicy,
    pub signer: Option<Signer>,
}

/// Read, verify (if `keys` is set), and compile the policy at `path`.
/// `current_revision` is the revision of the policy in effect, used for the
/// rollback guard; pass `None` at startup.
pub fn load(
    path: &Path,
    keys: Option<&Arc<VerifyKeys>>,
    current_revision: Option<u64>,
) -> Result<Loaded> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;

    let signer = match keys {
        Some(keys) => {
            let sp = sig_path(path);
            let sig = std::fs::read_to_string(&sp).with_context(|| {
                format!(
                    "read signature {} (policy verification is on; sign with `acb-cli sign-config`)",
                    sp.display()
                )
            })?;
            Some(
                keys.verify(&bytes, &sig)
                    .map_err(|e| anyhow!("{}: {e}", sp.display()))?,
            )
        }
        None => None,
    };

    let yaml = String::from_utf8(bytes).context("policy is not UTF-8")?;
    let base = path.parent().unwrap_or(Path::new("."));
    let policy = acb_policy::load::load_policy_with_base(&yaml, Some(base))
        .with_context(|| format!("invalid policy in {}", path.display()))?;

    // The rollback guard only means something when the file is signed: an
    // unsigned file can be rewritten to any revision anyway.
    if keys.is_some() {
        if let Some(cur) = current_revision {
            check_revision(cur, policy.revision)?;
        }
    }
    Ok(Loaded { policy, signer })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sig_path_appends_suffix() {
        assert_eq!(
            sig_path(Path::new("/etc/acb/config.yaml")),
            PathBuf::from("/etc/acb/config.yaml.sig")
        );
        assert_eq!(
            sig_path(Path::new("config.yaml")),
            PathBuf::from("config.yaml.sig")
        );
    }
}

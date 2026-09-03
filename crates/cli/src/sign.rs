//! Operator-side signing of `config.yaml`.
//!
//! The signer is `ssh-keygen -Y sign` (OpenSSH >= 8.0, shipped on Linux,
//! macOS and Windows 11). Delegating to it — rather than signing in-process —
//! is the point: the key can be a passphrase-protected file, a key held by
//! an agent that asks for confirmation on every use (`ssh-add -c`), a FIDO2
//! security key that needs a touch, or a Secure-Enclave key behind Touch ID
//! via an agent such as Secretive. Every one of those is a human-presence
//! check that an autonomous agent running as the same user cannot pass, and
//! none of them needs root.
//!
//! The daemon side (`acb-daemon --verify-key`) verifies with the public key
//! only; see `acb_policy::sig`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use acb_policy::sig::{Signer, VerifyKeys, NAMESPACE, SIG_SUFFIX};

/// `config.yaml` -> `config.yaml.sig`.
pub fn sig_path(config: &Path) -> PathBuf {
    let mut s = config.as_os_str().to_owned();
    s.push(SIG_SUFFIX);
    PathBuf::from(s)
}

/// Sign the exact bytes of `config` with `signing_key` and install the
/// detached signature at `<config>.sig`. `signing_key` is whatever
/// `ssh-keygen -Y sign -f` accepts: a private key file, or a *public* key
/// file when the private half lives in an ssh-agent / security key.
///
/// Signing happens on a staged copy in a private temp dir, because
/// `ssh-keygen -Y sign` writes `<file>.sig` next to its input and stops to
/// ask before overwriting an existing one. The result is then moved into
/// place with a rename, so the daemon's watcher never sees a half-written
/// signature.
pub fn sign(config: &Path, signing_key: &Path) -> Result<PathBuf> {
    let bytes = std::fs::read(config).with_context(|| format!("read {}", config.display()))?;
    let out = sig_path(config);
    let pem = sign_bytes(&bytes, signing_key)?;
    install_sig(&out, &pem)?;
    Ok(out)
}

/// Run `ssh-keygen -Y sign` over `bytes` and return the PEM-armored sshsig.
pub fn sign_bytes(bytes: &[u8], signing_key: &Path) -> Result<String> {
    let dir = tempfile::tempdir().context("create signing staging dir")?;
    let staged = dir.path().join("config.yaml");
    std::fs::write(&staged, bytes).context("stage config for signing")?;

    // stdin/stdout/stderr are inherited on purpose: the key's passphrase
    // prompt, an agent's confirmation dialog or a security key's "touch me"
    // message must reach the human.
    let status = Command::new("ssh-keygen")
        .arg("-Y")
        .arg("sign")
        .arg("-f")
        .arg(signing_key)
        .arg("-n")
        .arg(NAMESPACE)
        .arg(&staged)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("run ssh-keygen (is OpenSSH installed and on PATH?)")?;
    if !status.success() {
        bail!("ssh-keygen -Y sign failed ({status})");
    }
    std::fs::read_to_string(sig_path(&staged)).context("read signature produced by ssh-keygen")
}

/// Atomically place `pem` at `out` (write a sibling temp file, then rename).
fn install_sig(out: &Path, pem: &str) -> Result<()> {
    let dir = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut tmp = tempfile::Builder::new()
        .prefix(".config.yaml.sig.")
        .tempfile_in(dir)
        .with_context(|| format!("create temp signature in {}", dir.display()))?;
    use std::io::Write;
    tmp.write_all(pem.as_bytes())
        .context("write temp signature")?;
    tmp.flush()?;
    tmp.persist(out)
        .map_err(|e| e.error)
        .with_context(|| format!("install signature at {}", out.display()))?;
    Ok(())
}

/// Offline check of `<config>.sig` against `verify_key`, exactly as the
/// daemon does it. Returns the signer and the policy's `revision`.
pub fn verify(config: &Path, verify_key: &Path) -> Result<(Signer, u64)> {
    let keys_text = std::fs::read_to_string(verify_key)
        .with_context(|| format!("read verify key {}", verify_key.display()))?;
    let keys = VerifyKeys::parse(&keys_text)
        .with_context(|| format!("invalid verify key {}", verify_key.display()))?;
    let bytes = std::fs::read(config).with_context(|| format!("read {}", config.display()))?;
    let sp = sig_path(config);
    let pem =
        std::fs::read_to_string(&sp).with_context(|| format!("read signature {}", sp.display()))?;
    let signer = keys
        .verify(&bytes, &pem)
        .map_err(|e| anyhow::anyhow!("{}: {e}", sp.display()))?;
    let yaml = String::from_utf8(bytes).context("policy is not UTF-8")?;
    let policy = acb_policy::load::load_policy(&yaml)
        .with_context(|| format!("invalid policy in {}", config.display()))?;
    Ok((signer, policy.revision))
}

/// The `revision:` value of a policy document, or `None` if the top-level
/// key is absent. Text-level (line-based) so the operator's formatting and
/// comments are untouched by [`bump_revision`].
pub fn current_revision(text: &str) -> Option<u64> {
    text.lines().find_map(|l| {
        let rest = l.strip_prefix("revision:")?;
        rest.split('#').next()?.trim().parse().ok()
    })
}

/// Return `text` with its top-level `revision:` incremented (or inserted as
/// `revision: 1` at the top when absent), plus the new value. Only the
/// number on that line changes; everything else is byte-identical.
pub fn bump_revision(text: &str) -> (String, u64) {
    if let Some(cur) = current_revision(text) {
        let next = cur.saturating_add(1);
        let mut out = String::with_capacity(text.len() + 4);
        let mut done = false;
        for line in text.split_inclusive('\n') {
            if !done && line.starts_with("revision:") {
                let (head, tail) = split_revision_line(line);
                out.push_str(&format!("{head}{next}{tail}"));
                done = true;
            } else {
                out.push_str(line);
            }
        }
        (out, next)
    } else {
        (format!("revision: 1\n{text}"), 1)
    }
}

/// Split `revision: 12   # note\n` into (`revision: `, `   # note\n`).
fn split_revision_line(line: &str) -> (String, String) {
    let after = &line["revision:".len()..];
    let ws = after.len() - after.trim_start().len();
    let digits = after[ws..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .count();
    (
        format!("revision:{}", &after[..ws]),
        after[ws + digits..].to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sig_path_appends_suffix() {
        assert_eq!(
            sig_path(Path::new("/x/config.yaml")),
            PathBuf::from("/x/config.yaml.sig")
        );
    }

    #[test]
    fn current_revision_parses_or_none() {
        assert_eq!(current_revision("revision: 7\nrules: []\n"), Some(7));
        assert_eq!(current_revision("revision: 7 # v7\nrules: []\n"), Some(7));
        assert_eq!(current_revision("rules: []\n"), None);
        // Indented (non-top-level) keys are not the policy revision.
        assert_eq!(current_revision("server:\n  revision: 3\n"), None);
    }

    #[test]
    fn bump_increments_in_place_keeping_everything_else() {
        let text = "# policy\nrevision: 41   # bumped by edit-config\nrules: []\n";
        let (out, n) = bump_revision(text);
        assert_eq!(n, 42);
        assert_eq!(
            out,
            "# policy\nrevision: 42   # bumped by edit-config\nrules: []\n"
        );
    }

    #[test]
    fn bump_inserts_when_absent() {
        let (out, n) = bump_revision("rules: []\n");
        assert_eq!(n, 1);
        assert_eq!(out, "revision: 1\nrules: []\n");
    }

    #[test]
    fn bump_only_touches_first_top_level_revision() {
        let text = "revision: 1\nrules:\n  - name: revision: 5\n";
        let (out, _) = bump_revision(text);
        assert_eq!(out, "revision: 2\nrules:\n  - name: revision: 5\n");
    }

    #[test]
    fn bumped_text_still_loads_with_expected_revision() {
        let yaml = r#"
server:
  bind: "127.0.0.1"
  port: 39100
  log_file: "./x.log"
  log_rotation: "daily"
chromium:
  binary: null
  user_data_dir: "./p"
  viewport: { width: 1280, height: 800 }
  screencast: { format: "jpeg", quality: 70, max_fps: 8 }
resource_policy:
  subresources_inherit_page: true
  always_block_schemes: ["javascript"]
  bypass_service_worker: true
rules: []
"#;
        let (b1, n1) = bump_revision(yaml);
        assert_eq!(acb_policy::load::load_policy(&b1).unwrap().revision, n1);
        let (b2, n2) = bump_revision(&b1);
        assert_eq!(n2, n1 + 1);
        assert_eq!(acb_policy::load::load_policy(&b2).unwrap().revision, n2);
    }
}

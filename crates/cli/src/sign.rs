//! Operator-side signing of `config.yaml`.
//!
//! Two signing backends produce the same **sshsig** envelope that
//! `acb-daemon --verify-key` checks (see `acb_policy::sig`):
//!
//! - [`SigningKey::SshKeygen`] delegates to `ssh-keygen -Y sign` (OpenSSH
//!   8.0 or newer, shipped on Linux, macOS and Windows 11). The key can be a
//!   passphrase-protected file, a key held by an agent that asks for
//!   confirmation on every use (`ssh-add -c`), or a FIDO2 security key that
//!   needs a touch.
//! - [`SigningKey::SecureEnclave`] (macOS only, zero install) signs with a
//!   P-256 key that lives in the Secure Enclave and is created with a
//!   user-presence access control, so every signature asks for Touch ID or
//!   the login password. See [`crate::se`].
//!
//! Every one of those is a human-presence check that an autonomous agent
//! running as the same user cannot pass, and none of them needs root.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;

use anyhow::{bail, Context, Result};
use ssh_key::public::{EcdsaPublicKey, KeyData};
use ssh_key::{HashAlg, PublicKey, SshSig};

use acb_policy::sig::{Signer, VerifyKeys, NAMESPACE, SIG_SUFFIX};

/// `config.yaml` -> `config.yaml.sig`.
pub fn sig_path(config: &Path) -> PathBuf {
    let mut s = config.as_os_str().to_owned();
    s.push(SIG_SUFFIX);
    PathBuf::from(s)
}

/// Prefix that selects the native Secure Enclave backend in
/// `--signing-key` / `ACB_SIGNING_KEY`: `secure-enclave` or
/// `secure-enclave:<label>`.
pub const SECURE_ENCLAVE_SCHEME: &str = "secure-enclave";

/// What `--signing-key` names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SigningKey {
    /// Anything `ssh-keygen -Y sign -f` accepts: a private key file, or the
    /// `.pub` of a key held by an ssh-agent / security key.
    SshKeygen(PathBuf),
    /// A Secure Enclave key created by `acb-cli keygen --backend
    /// secure-enclave`, identified by its label.
    SecureEnclave { label: String },
}

impl SigningKey {
    /// `secure-enclave` / `secure-enclave:<label>` select the native
    /// backend; everything else is a path for `ssh-keygen`.
    pub fn parse(s: &str) -> Self {
        if s == SECURE_ENCLAVE_SCHEME {
            return SigningKey::SecureEnclave {
                label: crate::se::DEFAULT_LABEL.to_string(),
            };
        }
        if let Some(label) = s.strip_prefix(&format!("{SECURE_ENCLAVE_SCHEME}:")) {
            let label = label.trim();
            return SigningKey::SecureEnclave {
                label: if label.is_empty() {
                    crate::se::DEFAULT_LABEL.to_string()
                } else {
                    label.to_string()
                },
            };
        }
        SigningKey::SshKeygen(PathBuf::from(s))
    }
}

impl FromStr for SigningKey {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(Self::parse(s))
    }
}

/// Sign the exact bytes of `config` with `key` and install the detached
/// signature at `<config>.sig`, moved into place with a rename so the
/// daemon's watcher never sees a half-written signature.
pub fn sign(config: &Path, key: &SigningKey) -> Result<PathBuf> {
    let bytes = std::fs::read(config).with_context(|| format!("read {}", config.display()))?;
    let out = sig_path(config);
    let pem = sign_bytes(&bytes, key)?;
    install_sig(&out, &pem)?;
    Ok(out)
}

/// Produce the PEM-armored sshsig over `bytes` with `key`.
pub fn sign_bytes(bytes: &[u8], key: &SigningKey) -> Result<String> {
    match key {
        SigningKey::SshKeygen(path) => sign_with_ssh_keygen(bytes, path),
        SigningKey::SecureEnclave { label } => sign_with_secure_enclave(bytes, label),
    }
}

/// Run `ssh-keygen -Y sign` over `bytes`.
///
/// Signing happens on a staged copy in a private temp dir, because
/// `ssh-keygen -Y sign` writes `<file>.sig` next to its input and stops to
/// ask before overwriting an existing one.
fn sign_with_ssh_keygen(bytes: &[u8], signing_key: &Path) -> Result<String> {
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

/// Sign with the Secure Enclave key labelled `label` and self-check the
/// result against the key's own public half before handing it out, so a
/// framework/encoding mismatch surfaces here as an error rather than as a
/// daemon that silently keeps the old policy.
fn sign_with_secure_enclave(bytes: &[u8], label: &str) -> Result<String> {
    let key = crate::se::open(label)?;
    let pubkey = key.public_key_sec1()?;
    eprintln!("signing policy with Secure Enclave key {label:?} (Touch ID / password prompt)...");
    let signed_data = SshSig::signed_data(NAMESPACE, HashAlg::Sha512, bytes)
        .context("build sshsig signed data")?;
    let der = key.sign_der(&signed_data)?;
    let pem = sshsig_from_p256_der(&pubkey, &der)?;

    let line = openssh_pubkey_p256(&pubkey, label)?;
    VerifyKeys::parse(&line)
        .context("self-check: encode public key")?
        .verify(bytes, &pem)
        .context("self-check of the Secure Enclave signature failed")?;
    Ok(pem)
}

/// Build an `ecdsa-sha2-nistp256` sshsig from a SEC1 uncompressed public
/// point (`04 || X || Y`) and a DER-encoded ECDSA signature over
/// SHA-256 of the sshsig signed-data blob — exactly what
/// `SecKeyCreateSignature(..., ECDSASignatureMessageX962SHA256, ...)`
/// returns. Pure, so it is unit-tested on every platform.
pub fn sshsig_from_p256_der(pubkey_sec1: &[u8], der_sig: &[u8]) -> Result<String> {
    let key_data = p256_key_data(pubkey_sec1)?;
    let p256_sig = p256::ecdsa::Signature::from_der(der_sig)
        .map_err(|e| anyhow::anyhow!("Secure Enclave returned a malformed DER signature: {e}"))?;
    let signature = ssh_key::Signature::try_from(&p256_sig).context("encode ECDSA signature")?;
    let sig =
        SshSig::new(key_data, NAMESPACE, HashAlg::Sha512, signature).context("assemble sshsig")?;
    sig.to_pem(ssh_key::LineEnding::LF)
        .context("PEM-encode sshsig")
}

/// The OpenSSH `ecdsa-sha2-nistp256 AAAA... <comment>` line for a SEC1
/// public point; this is what goes into the daemon's `--verify-key` file.
pub fn openssh_pubkey_p256(pubkey_sec1: &[u8], comment: &str) -> Result<String> {
    PublicKey::new(p256_key_data(pubkey_sec1)?, comment)
        .to_openssh()
        .context("encode OpenSSH public key")
}

fn p256_key_data(pubkey_sec1: &[u8]) -> Result<KeyData> {
    if pubkey_sec1.len() != 65 || pubkey_sec1[0] != 0x04 {
        bail!(
            "public key is not an uncompressed SEC1 P-256 point ({} bytes)",
            pubkey_sec1.len()
        );
    }
    let key = EcdsaPublicKey::from_sec1_bytes(pubkey_sec1)
        .map_err(|e| anyhow::anyhow!("public key is not a SEC1 P-256 point: {e}"))?;
    Ok(KeyData::Ecdsa(key))
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
    use p256::ecdsa::signature::Signer as _;

    #[test]
    fn sig_path_appends_suffix() {
        assert_eq!(
            sig_path(Path::new("/x/config.yaml")),
            PathBuf::from("/x/config.yaml.sig")
        );
    }

    #[test]
    fn signing_key_parse_selects_backend() {
        assert_eq!(
            SigningKey::parse("~/.ssh/acb_policy"),
            SigningKey::SshKeygen(PathBuf::from("~/.ssh/acb_policy"))
        );
        assert_eq!(
            SigningKey::parse("secure-enclave"),
            SigningKey::SecureEnclave {
                label: crate::se::DEFAULT_LABEL.into()
            }
        );
        assert_eq!(
            SigningKey::parse("secure-enclave:work"),
            SigningKey::SecureEnclave {
                label: "work".into()
            }
        );
        assert_eq!(
            SigningKey::parse("secure-enclave:"),
            SigningKey::SecureEnclave {
                label: crate::se::DEFAULT_LABEL.into()
            }
        );
        // A file that merely *contains* the word stays a path.
        assert_eq!(
            SigningKey::parse("keys/secure-enclave.pub"),
            SigningKey::SshKeygen(PathBuf::from("keys/secure-enclave.pub"))
        );
    }

    /// Model the Secure Enclave with a software P-256 key: what the
    /// framework hands back is a SEC1 public point and a DER signature over
    /// SHA-256 of the signed-data blob. The assembled sshsig must verify
    /// with the daemon's verifier from the OpenSSH line we print.
    #[test]
    fn p256_der_signature_becomes_a_verifiable_sshsig() {
        let sk = p256::ecdsa::SigningKey::random(&mut rand_core::OsRng);
        let vk = sk.verifying_key();
        let sec1 = vk.to_encoded_point(false).as_bytes().to_vec();
        assert_eq!(sec1.len(), 65);
        assert_eq!(sec1[0], 0x04);

        let policy = b"revision: 3\nrules: []\n";
        let signed_data = SshSig::signed_data(NAMESPACE, HashAlg::Sha512, policy).unwrap();
        let der: Vec<u8> = {
            let sig: p256::ecdsa::Signature = sk.sign(&signed_data);
            sig.to_der().as_bytes().to_vec()
        };

        let pem = sshsig_from_p256_der(&sec1, &der).unwrap();
        assert!(pem.starts_with("-----BEGIN SSH SIGNATURE-----"));

        let line = openssh_pubkey_p256(&sec1, "acb-policy").unwrap();
        assert!(line.starts_with("ecdsa-sha2-nistp256 "), "{line}");
        assert!(line.ends_with(" acb-policy"), "{line}");

        let keys = VerifyKeys::parse(&line).unwrap();
        let who = keys.verify(policy, &pem).unwrap();
        assert_eq!(who.comment, "acb-policy");
        assert!(keys.verify(b"revision: 4\nrules: []\n", &pem).is_err());
    }

    #[test]
    fn sshsig_assembly_rejects_bad_inputs() {
        let sk = p256::ecdsa::SigningKey::random(&mut rand_core::OsRng);
        let sec1 = sk
            .verifying_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec();
        assert!(sshsig_from_p256_der(&sec1, b"not der").is_err());
        assert!(openssh_pubkey_p256(&[1, 2, 3], "x").is_err());
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

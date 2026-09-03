//! Signature verification for the policy file.
//!
//! The policy is the agent's most powerful input, and on a developer machine
//! the agent usually shares a filesystem with it. Filesystem locks
//! (`protect-config`) turn "edit the policy" into "obtain root"; signatures
//! turn it into "use the operator's signing key", which can live behind a
//! human-presence check (Touch ID, Windows Hello, a FIDO2 touch, an
//! `ssh-add -c` confirmation) with no root involved at all.
//!
//! Format: **sshsig**, the detached-signature format of
//! `ssh-keygen -Y sign` (PROTOCOL.sshsig). It was chosen over a bespoke
//! envelope because OpenSSH ships on every target OS, the signer can be a
//! software key, an agent-held key, a FIDO2 `-sk` key or a Secure-Enclave
//! agent, and the operator can verify by hand with `ssh-keygen -Y verify`.
//!
//! The daemon holds only *public* keys. Verification therefore never needs a
//! secret, so it works unattended, at hot reload and inside a container.
//!
//! This module is pure: callers read the files and pass bytes in.

use std::fmt;

use ssh_key::{HashAlg, PublicKey, SshSig};

/// sshsig namespace bound into every policy signature. A signature made for
/// another purpose (e.g. a git commit, namespace `git`) with the same key
/// does not verify as a policy.
pub const NAMESPACE: &str = "acb-policy";

/// File-name suffix of the detached signature: `config.yaml` is signed into
/// `config.yaml.sig`, which is also what `ssh-keygen -Y sign` produces.
pub const SIG_SUFFIX: &str = ".sig";

#[derive(Debug)]
pub enum SigError {
    /// No usable public key in the verify-key text.
    NoKeys,
    /// A line of the verify-key text is not an OpenSSH public key.
    BadKey { line: usize, source: ssh_key::Error },
    /// The `.sig` file is not a PEM-armored sshsig.
    BadSignature(ssh_key::Error),
    /// The signature carries a different namespace.
    WrongNamespace(String),
    /// The signing key is not in the allowed set.
    UnknownSigner { fingerprint: String },
    /// The signature does not match the policy bytes.
    Invalid(ssh_key::Error),
}

impl fmt::Display for SigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SigError::NoKeys => write!(f, "verify-key file contains no public keys"),
            SigError::BadKey { line, source } => {
                write!(f, "verify-key line {line} is not an OpenSSH public key: {source}")
            }
            SigError::BadSignature(e) => write!(f, "signature file is not an sshsig: {e}"),
            SigError::WrongNamespace(ns) => write!(
                f,
                "signature namespace is {ns:?}, expected {NAMESPACE:?} (sign with `ssh-keygen -Y sign -n {NAMESPACE}`)"
            ),
            SigError::UnknownSigner { fingerprint } => {
                write!(f, "policy signed by a key that is not trusted: {fingerprint}")
            }
            SigError::Invalid(e) => write!(f, "policy signature does not match file contents: {e}"),
        }
    }
}

impl std::error::Error for SigError {}

/// The set of public keys allowed to sign the policy.
#[derive(Debug, Clone)]
pub struct VerifyKeys {
    keys: Vec<PublicKey>,
}

/// Which trusted key produced an accepted signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signer {
    /// SHA-256 fingerprint, `SHA256:...` as printed by `ssh-keygen -lf`.
    pub fingerprint: String,
    /// The comment field of the trusted key line, if any.
    pub comment: String,
}

impl VerifyKeys {
    /// Parse one or more OpenSSH public-key lines (the `id_*.pub` /
    /// `authorized_keys` shape). Blank lines and `#` comments are skipped.
    pub fn parse(text: &str) -> Result<Self, SigError> {
        let mut keys = Vec::new();
        for (i, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let key = PublicKey::from_openssh(line).map_err(|source| SigError::BadKey {
                line: i + 1,
                source,
            })?;
            keys.push(key);
        }
        if keys.is_empty() {
            return Err(SigError::NoKeys);
        }
        Ok(Self { keys })
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Fingerprints of every trusted key, for logs and `verify-config`.
    pub fn fingerprints(&self) -> Vec<String> {
        self.keys
            .iter()
            .map(|k| k.fingerprint(HashAlg::Sha256).to_string())
            .collect()
    }

    /// Verify that `sig_pem` is a valid `acb-policy` sshsig over exactly
    /// `policy_bytes`, made by one of the trusted keys.
    pub fn verify(&self, policy_bytes: &[u8], sig_pem: &str) -> Result<Signer, SigError> {
        let sig = SshSig::from_pem(sig_pem).map_err(SigError::BadSignature)?;
        if sig.namespace() != NAMESPACE {
            return Err(SigError::WrongNamespace(sig.namespace().to_string()));
        }
        // Match on the embedded key first so the error distinguishes "not a
        // trusted key" from "trusted key, wrong bytes".
        let Some(key) = self.keys.iter().find(|k| k.key_data() == sig.public_key()) else {
            let fp = ssh_key::public::PublicKey::from(sig.public_key().clone())
                .fingerprint(HashAlg::Sha256)
                .to_string();
            return Err(SigError::UnknownSigner { fingerprint: fp });
        };
        key.verify(NAMESPACE, policy_bytes, &sig)
            .map_err(SigError::Invalid)?;
        Ok(Signer {
            fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
            comment: key.comment().to_string(),
        })
    }
}

/// Why a hot reload was refused on revision grounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionRollback {
    pub current: u64,
    pub offered: u64,
}

impl fmt::Display for RevisionRollback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "policy revision {} is older than the revision in effect ({}); refusing rollback",
            self.offered, self.current
        )
    }
}

impl std::error::Error for RevisionRollback {}

/// Replay guard for signed policies: a newly offered policy must not carry a
/// lower `revision` than the one in effect. Equal is accepted (a re-save of
/// the same file, or an operator who does not use revisions at all).
pub fn check_revision(current: u64, offered: u64) -> Result<(), RevisionRollback> {
    if offered < current {
        Err(RevisionRollback { current, offered })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ssh_key::{Algorithm, LineEnding, PrivateKey};

    fn keypair(alg: Algorithm) -> (PrivateKey, String) {
        let sk = PrivateKey::random(&mut rand_core::OsRng, alg).unwrap();
        let pk_line = sk.public_key().to_openssh().unwrap();
        (sk, pk_line)
    }

    fn sign(sk: &PrivateKey, msg: &[u8], ns: &str) -> String {
        sk.sign(ns, HashAlg::Sha512, msg)
            .unwrap()
            .to_pem(LineEnding::LF)
            .unwrap()
    }

    #[test]
    fn ed25519_roundtrip_verifies() {
        let (sk, pk) = keypair(Algorithm::Ed25519);
        let keys = VerifyKeys::parse(&pk).unwrap();
        let msg = b"rules: []\n";
        let sig = sign(&sk, msg, NAMESPACE);
        let who = keys.verify(msg, &sig).unwrap();
        assert_eq!(who.fingerprint, keys.fingerprints()[0]);
    }

    #[test]
    fn p256_roundtrip_verifies() {
        let (sk, pk) = keypair(Algorithm::Ecdsa {
            curve: ssh_key::EcdsaCurve::NistP256,
        });
        let keys = VerifyKeys::parse(&pk).unwrap();
        let msg = b"rules: []\n";
        let sig = sign(&sk, msg, NAMESPACE);
        keys.verify(msg, &sig).unwrap();
    }

    #[test]
    fn tampered_bytes_are_rejected() {
        let (sk, pk) = keypair(Algorithm::Ed25519);
        let keys = VerifyKeys::parse(&pk).unwrap();
        let sig = sign(&sk, b"rules: []\n", NAMESPACE);
        let err = keys.verify(b"rules: [evil]\n", &sig).unwrap_err();
        assert!(matches!(err, SigError::Invalid(_)), "{err}");
    }

    #[test]
    fn wrong_namespace_is_rejected() {
        let (sk, pk) = keypair(Algorithm::Ed25519);
        let keys = VerifyKeys::parse(&pk).unwrap();
        let sig = sign(&sk, b"x", "git");
        let err = keys.verify(b"x", &sig).unwrap_err();
        assert!(
            matches!(err, SigError::WrongNamespace(ref ns) if ns == "git"),
            "{err}"
        );
    }

    #[test]
    fn untrusted_signer_is_rejected_even_with_valid_signature() {
        let (trusted_sk, trusted_pk) = keypair(Algorithm::Ed25519);
        let (evil_sk, _) = keypair(Algorithm::Ed25519);
        let keys = VerifyKeys::parse(&trusted_pk).unwrap();
        let ok = sign(&trusted_sk, b"x", NAMESPACE);
        keys.verify(b"x", &ok).unwrap();
        let bad = sign(&evil_sk, b"x", NAMESPACE);
        let err = keys.verify(b"x", &bad).unwrap_err();
        assert!(matches!(err, SigError::UnknownSigner { .. }), "{err}");
    }

    #[test]
    fn any_of_several_trusted_keys_may_sign() {
        let (a_sk, a_pk) = keypair(Algorithm::Ed25519);
        let (b_sk, b_pk) = keypair(Algorithm::Ed25519);
        let text = format!("# operator keys\n{a_pk}\n\n{b_pk}\n");
        let keys = VerifyKeys::parse(&text).unwrap();
        assert_eq!(keys.len(), 2);
        keys.verify(b"x", &sign(&a_sk, b"x", NAMESPACE)).unwrap();
        keys.verify(b"x", &sign(&b_sk, b"x", NAMESPACE)).unwrap();
    }

    #[test]
    fn parse_rejects_garbage_and_empty() {
        assert!(matches!(
            VerifyKeys::parse("# nothing\n\n"),
            Err(SigError::NoKeys)
        ));
        let err = VerifyKeys::parse("ssh-ed25519 notbase64\n").unwrap_err();
        assert!(matches!(err, SigError::BadKey { line: 1, .. }), "{err}");
    }

    #[test]
    fn garbage_signature_is_rejected() {
        let (_, pk) = keypair(Algorithm::Ed25519);
        let keys = VerifyKeys::parse(&pk).unwrap();
        assert!(matches!(
            keys.verify(b"x", "not a pem"),
            Err(SigError::BadSignature(_))
        ));
    }

    #[test]
    fn revision_guard_rejects_only_rollback() {
        assert!(check_revision(5, 6).is_ok());
        assert!(check_revision(5, 5).is_ok());
        assert!(check_revision(0, 0).is_ok());
        let err = check_revision(5, 4).unwrap_err();
        assert_eq!(
            err,
            RevisionRollback {
                current: 5,
                offered: 4
            }
        );
    }
}

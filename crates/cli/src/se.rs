//! Native macOS signing backend: a P-256 key in the Secure Enclave.
//!
//! Zero install — nothing beyond `acb-cli` itself — and the strongest gate
//! the platform offers: the key is created with
//! `kSecAccessControlUserPresence`, so *every* signature makes macOS ask for
//! Touch ID (or the login password). The private key never leaves the
//! enclave; there is no file an agent could copy, and no way to answer the
//! prompt from a shell.
//!
//! What this module talks to is Security.framework:
//!
//! - `SecKeyCreateRandomKey` with `kSecAttrTokenIDSecureEnclave`, stored
//!   permanently in the data-protection keychain under a label
//!   (`kSecAttrLabel`) derived from the operator-chosen key name.
//! - `SecItemCopyMatching` by that label to find it again.
//! - `SecKeyCreateSignature` with `ECDSASignatureMessageX962SHA256`, which
//!   is exactly the hash/curve OpenSSH's `ecdsa-sha2-nistp256` uses; the DER
//!   result is re-encoded as an sshsig by [`crate::sign`].
//!
//! Preconditions (documented in docs/usage.md): Apple Silicon or a T2 Mac
//! (no Secure Enclave in most VMs), and a code-signed `acb-cli` binary —
//! the Secure Enclave refuses unsigned callers. Apple Silicon's linker
//! ad-hoc-signs every binary, so a local `cargo build` normally satisfies
//! this; if not, `codesign -s - $(which acb-cli)`. Note that an ad-hoc
//! signature changes on every rebuild; keys created under one build stay
//! usable because access is scoped by the keychain access group, not the
//! code hash, but this needs confirming on real hardware.
//!
//! On other platforms every entry point returns an error explaining that
//! the backend is macOS-only.

use anyhow::Result;

/// Label used when `--signing-key secure-enclave` carries no explicit name.
pub const DEFAULT_LABEL: &str = "acb-policy";

/// `kSecAttrLabel` value for a key named `label`. Namespaced so
/// `acb-cli` never picks up an unrelated key with a short label.
pub fn keychain_label(label: &str) -> String {
    format!("access-control-browser policy signing key: {label}")
}

#[cfg(target_os = "macos")]
pub use imp::*;

#[cfg(not(target_os = "macos"))]
pub use stub::*;

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use anyhow::{anyhow, bail, Context};
    use security_framework::access_control::{ProtectionMode, SecAccessControl};
    use security_framework::item::{ItemSearchOptions, KeyClass, Limit, Reference, SearchResult};
    use security_framework::key::{Algorithm, GenerateKeyOptions, KeyType, SecKey, Token};
    use security_framework_sys::access_control::{
        kSecAccessControlPrivateKeyUsage, kSecAccessControlUserPresence,
    };

    /// A handle to a Secure Enclave private key (the enclave keeps the
    /// material; this is a reference).
    pub struct SeKey {
        label: String,
        private: SecKey,
    }

    impl std::fmt::Debug for SeKey {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("SeKey").field("label", &self.label).finish()
        }
    }

    /// Find the key named `label`, or fail with a hint to create it.
    pub fn open(label: &str) -> Result<SeKey> {
        find(label)?.ok_or_else(|| {
            anyhow!(
                "no Secure Enclave key named {label:?}; create one with \
                 `acb-cli keygen --backend secure-enclave --label {label}`"
            )
        })
    }

    /// Look the key up by its keychain label.
    pub fn find(label: &str) -> Result<Option<SeKey>> {
        let results = ItemSearchOptions::new()
            .key_class(KeyClass::private())
            .label(&keychain_label(label))
            .ignore_legacy_keychains()
            .load_refs(true)
            .limit(Limit::All)
            .search();
        let results = match results {
            Ok(r) => r,
            // errSecItemNotFound
            Err(e) if e.code() == -25300 => return Ok(None),
            Err(e) => return Err(anyhow!("keychain search failed: {e}")),
        };
        let mut keys = results.into_iter().filter_map(|r| match r {
            SearchResult::Ref(Reference::Key(k)) => Some(k),
            _ => None,
        });
        let Some(private) = keys.next() else {
            return Ok(None);
        };
        if keys.next().is_some() {
            bail!(
                "more than one Secure Enclave key is labelled {label:?}; delete the extras with \
                 `acb-cli keygen --backend secure-enclave --label {label} --delete` and recreate"
            );
        }
        Ok(Some(SeKey {
            label: label.to_string(),
            private,
        }))
    }

    /// Create the key named `label` if it does not exist. Returns the key
    /// and whether it was created by this call.
    pub fn ensure(label: &str) -> Result<(SeKey, bool)> {
        if let Some(k) = find(label)? {
            return Ok((k, false));
        }
        // PrivateKeyUsage is mandatory for Secure Enclave keys; UserPresence
        // is the whole point: Touch ID / password on every signature.
        let access = SecAccessControl::create_with_protection(
            Some(ProtectionMode::AccessibleWhenUnlockedThisDeviceOnly),
            kSecAccessControlPrivateKeyUsage | kSecAccessControlUserPresence,
        )
        .map_err(|e| anyhow!("create access control: {e}"))?;
        let mut opts = GenerateKeyOptions::default();
        opts.set_key_type(KeyType::ec_sec_prime_random())
            .set_size_in_bits(256)
            .set_label(keychain_label(label))
            .set_token(Token::SecureEnclave)
            .set_location(security_framework::item::Location::DataProtectionKeychain)
            .set_access_control(access);
        let private = SecKey::new(&opts).map_err(|e| {
            anyhow!(
                "Secure Enclave key generation failed: {e}\n\
                 hints: this needs a Mac with a Secure Enclave (Apple Silicon or T2; most VMs \
                 have none) and a code-signed acb-cli binary — try `codesign -s - $(which acb-cli)`"
            )
        })?;
        Ok((
            SeKey {
                label: label.to_string(),
                private,
            },
            true,
        ))
    }

    impl SeKey {
        pub fn label(&self) -> &str {
            &self.label
        }

        /// SEC1 uncompressed public point (`04 || X || Y`, 65 bytes).
        pub fn public_key_sec1(&self) -> Result<Vec<u8>> {
            let public = self
                .private
                .public_key()
                .ok_or_else(|| anyhow!("Secure Enclave key has no public half"))?;
            let data = public
                .external_representation()
                .ok_or_else(|| anyhow!("cannot export Secure Enclave public key"))?;
            let bytes = data.to_vec();
            if bytes.len() != 65 || bytes[0] != 0x04 {
                bail!(
                    "unexpected public key encoding from Security.framework ({} bytes)",
                    bytes.len()
                );
            }
            Ok(bytes)
        }

        /// OpenSSH public-key line for the daemon's `--verify-key` file.
        pub fn openssh_public_key(&self) -> Result<String> {
            crate::sign::openssh_pubkey_p256(&self.public_key_sec1()?, &self.label)
        }

        /// ECDSA-P256/SHA-256 signature (DER) over `data`. This is the call
        /// that triggers the Touch ID / password prompt.
        pub fn sign_der(&self, data: &[u8]) -> Result<Vec<u8>> {
            self.private
                .create_signature(Algorithm::ECDSASignatureMessageX962SHA256, data)
                .map_err(|e| {
                    anyhow!("Secure Enclave signing failed (prompt cancelled or denied?): {e}")
                })
        }

        /// Remove the key from the keychain. Irreversible: a policy signed
        /// with it can no longer be re-signed by this key.
        pub fn delete(self) -> Result<()> {
            self.private
                .delete()
                .with_context(|| format!("delete Secure Enclave key {:?}", self.label))
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod stub {
    use super::*;
    use anyhow::bail;

    /// Placeholder so call sites type-check on every platform.
    #[derive(Debug)]
    pub struct SeKey {
        never: std::convert::Infallible,
    }

    fn unsupported() -> anyhow::Error {
        anyhow::anyhow!(
            "the secure-enclave signing backend is only available on macOS; \
             use an ssh-keygen key (a FIDO2 `-sk` key, an agent-held key with `ssh-add -c`, \
             or a passphrase-protected key file)"
        )
    }

    pub fn open(_label: &str) -> Result<SeKey> {
        Err(unsupported())
    }

    pub fn find(_label: &str) -> Result<Option<SeKey>> {
        Err(unsupported())
    }

    pub fn ensure(_label: &str) -> Result<(SeKey, bool)> {
        Err(unsupported())
    }

    impl SeKey {
        pub fn label(&self) -> &str {
            match self.never {}
        }
        pub fn public_key_sec1(&self) -> Result<Vec<u8>> {
            match self.never {}
        }
        pub fn openssh_public_key(&self) -> Result<String> {
            match self.never {}
        }
        pub fn sign_der(&self, _data: &[u8]) -> Result<Vec<u8>> {
            match self.never {}
        }
        pub fn delete(self) -> Result<()> {
            bail!("unreachable")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keychain_label_is_namespaced() {
        assert_eq!(
            keychain_label("acb-policy"),
            "access-control-browser policy signing key: acb-policy"
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn non_macos_reports_unsupported() {
        let err = open("acb-policy").unwrap_err().to_string();
        assert!(err.contains("only available on macOS"), "{err}");
    }
}

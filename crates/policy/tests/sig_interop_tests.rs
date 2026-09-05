//! Interop with the real signer: a signature produced by `ssh-keygen -Y sign`
//! (the production path of `acb-cli sign-config`) must verify with the
//! in-tree verifier, and vice versa a tampered file must not. Skips when
//! `ssh-keygen` is not installed (CI runners have it).

use std::path::Path;
use std::process::Command;

use acb_policy::sig::{SigError, VerifyKeys, NAMESPACE};

fn have_ssh_keygen() -> bool {
    Command::new("ssh-keygen")
        .arg("-V")
        .output()
        .map(|_| true)
        .unwrap_or(false)
}

fn keygen(dir: &Path, name: &str, ty: &str) -> (String, String) {
    let key = dir.join(name);
    let st = Command::new("ssh-keygen")
        .args(["-q", "-t", ty, "-N", "", "-C", "acb-test", "-f"])
        .arg(&key)
        .status()
        .unwrap();
    assert!(st.success());
    let pubkey = std::fs::read_to_string(key.with_extension("pub")).unwrap();
    (key.to_string_lossy().into_owned(), pubkey)
}

fn ssh_sign(key: &str, file: &Path, namespace: &str) -> String {
    let mut sig = file.as_os_str().to_owned();
    sig.push(".sig");
    // `-Y sign` prompts before overwriting an existing .sig; clear it.
    let _ = std::fs::remove_file(&sig);
    let st = Command::new("ssh-keygen")
        .args(["-Y", "sign", "-f", key, "-n", namespace])
        .arg(file)
        .stdin(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(st.success(), "ssh-keygen -Y sign failed");
    std::fs::read_to_string(sig).unwrap()
}

#[test]
fn ssh_keygen_signature_verifies_for_ed25519_and_p256() {
    if !have_ssh_keygen() {
        eprintln!("ssh-keygen not installed; skipping interop test");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.yaml");
    std::fs::write(&cfg, "revision: 3\nrules: []\n").unwrap();

    for (name, ty) in [("k_ed", "ed25519"), ("k_ec", "ecdsa")] {
        let (key, pubkey) = keygen(dir.path(), name, ty);
        let keys = VerifyKeys::parse(&pubkey).unwrap();
        let sig = ssh_sign(&key, &cfg, NAMESPACE);

        let who = keys
            .verify(&std::fs::read(&cfg).unwrap(), &sig)
            .unwrap_or_else(|e| {
                panic!("{ty}: {e}; trusted={:?}; sig=\n{sig}", keys.fingerprints())
            });
        assert_eq!(who.comment, "acb-test");

        let err = keys.verify(b"revision: 99\nrules: []\n", &sig).unwrap_err();
        assert!(matches!(err, SigError::Invalid(_)), "{ty}: {err}");
    }
}

#[test]
fn ssh_keygen_signature_with_other_namespace_is_rejected() {
    if !have_ssh_keygen() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.yaml");
    std::fs::write(&cfg, "rules: []\n").unwrap();
    let (key, pubkey) = keygen(dir.path(), "k", "ed25519");
    let keys = VerifyKeys::parse(&pubkey).unwrap();
    let sig = ssh_sign(&key, &cfg, "file");
    let err = keys
        .verify(&std::fs::read(&cfg).unwrap(), &sig)
        .unwrap_err();
    assert!(matches!(err, SigError::WrongNamespace(_)), "{err}");
}

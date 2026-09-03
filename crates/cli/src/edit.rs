//! `acb-cli edit-config`: the visudo-style editing wrapper for `config.yaml`.
//!
//! The policy file is the agent's single most security-critical input: an
//! agent that can rewrite it can grant itself any URL or element. The wrapper
//! stages a copy, opens `$EDITOR`, validates the result with the daemon's own
//! loader, and — when a signing key is configured — bumps `revision:` and
//! signs the new policy before installing it (see [`crate::sign`]). Combined
//! with `acb-daemon --verify-key`, that makes the human's signing key, not
//! filesystem permissions, the thing that gates policy changes. No root.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

/// Outcome of [`edit`].
#[derive(Debug, PartialEq, Eq)]
pub enum EditStatus {
    /// The editor exited without changing the file.
    Unchanged,
    /// A validated new policy was written back.
    Saved {
        /// The `revision` the saved policy carries, when it was signed.
        signed_revision: Option<u64>,
    },
}

/// Signing behaviour of [`edit`].
#[derive(Debug, Clone, Default)]
pub struct EditOptions {
    /// The signing key (see [`crate::sign::SigningKey`]). When set, every
    /// save bumps `revision:` and installs a fresh `<config>.sig` before the
    /// policy itself.
    pub signing_key: Option<crate::sign::SigningKey>,
}

/// visudo-style edit: stage a copy, open `$EDITOR`, validate the result with
/// the real policy loader, sign it if a key is configured, and install it.
/// Invalid YAML is never installed; neither is an unsigned edit of a policy
/// that already has a signature.
pub fn edit(path: &Path, opts: &EditOptions) -> Result<EditStatus> {
    let original = std::fs::read_to_string(path).unwrap_or_default();
    let dir = tempfile::tempdir().context("create staging dir")?;
    let staged = dir.path().join("config.yaml");
    std::fs::write(&staged, &original).context("stage config copy")?;

    run_editor(&resolve_editor(), &staged)?;

    let mut edited = std::fs::read_to_string(&staged).context("read edited config")?;
    if edited == original {
        return Ok(EditStatus::Unchanged);
    }

    // A policy that is already signed must stay signed: writing an unsigned
    // edit would only make the daemon refuse it (and keep the old policy),
    // so refuse here instead, with the edits preserved.
    let sig = crate::sign::sig_path(path);
    if sig.exists() && opts.signing_key.is_none() {
        let recovery = keep_rejected(&edited);
        bail!(
            "{} is signed but no signing key was given; pass --signing-key or set ACB_SIGNING_KEY (your edits were kept at {})",
            sig.display(),
            recovery.display()
        );
    }

    let mut signed_revision = None;
    if opts.signing_key.is_some() {
        let (bumped, rev) = crate::sign::bump_revision(&edited);
        edited = bumped;
        signed_revision = Some(rev);
    }
    validate(&edited)?;

    // Sign before installing the policy so that, whichever of the two writes
    // the daemon's watcher sees last, the pair on disk is consistent by the
    // time it loads. A signing failure (passphrase cancelled, no touch on the
    // security key) leaves the old policy and signature untouched.
    if let Some(key) = &opts.signing_key {
        let pem = crate::sign::sign_bytes(edited.as_bytes(), key)?;
        std::fs::write(&sig, pem).with_context(|| format!("write {}", sig.display()))?;
    }

    install(path, &edited)?;
    Ok(EditStatus::Saved { signed_revision })
}

/// Write `text` to `path` via a sibling temp file + rename, so the daemon's
/// watcher never reads a half-written policy.
fn install(path: &Path, text: &str) -> Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut tmp = tempfile::Builder::new()
        .prefix(".config.yaml.")
        .tempfile_in(dir)
        .with_context(|| format!("create temp file in {}", dir.display()))?;
    use std::io::Write;
    tmp.write_all(text.as_bytes())
        .context("write staged policy")?;
    tmp.flush()?;
    tmp.persist(path)
        .map_err(|e| e.error)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Validate a policy document with the same loader the daemon uses. On
/// failure the rejected text is preserved so the operator does not lose work.
fn validate(yaml: &str) -> Result<()> {
    if let Err(e) = acb_policy::load::load_policy(yaml) {
        let recovery = keep_rejected(yaml);
        bail!(
            "edited config is INVALID, not saved: {e}\nyour edits were kept at {}",
            recovery.display()
        );
    }
    Ok(())
}

/// Park an edit we refuse to install where the operator can pick it up.
fn keep_rejected(yaml: &str) -> PathBuf {
    let recovery = std::env::temp_dir().join("acb-config.rejected.yaml");
    let _ = std::fs::write(&recovery, yaml);
    recovery
}

fn resolve_editor() -> String {
    for var in ["VISUAL", "EDITOR"] {
        if let Ok(v) = std::env::var(var) {
            if !v.trim().is_empty() {
                return v;
            }
        }
    }
    if cfg!(windows) {
        "notepad".to_string()
    } else {
        "vi".to_string()
    }
}

/// Split an `$EDITOR` string into program + args. Whitespace separates;
/// single/double quotes group. Deliberately *not* a shell — no command
/// substitution, no `;`/`&&`/`|` operators — so a hostile `$EDITOR` cannot
/// run arbitrary commands; it can only name a program and its arguments.
fn split_editor(editor: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut in_single, mut in_double, mut started) = (false, false, false);
    for c in editor.chars() {
        match c {
            '\'' if !in_double => {
                in_single = !in_single;
                started = true;
            }
            '"' if !in_single => {
                in_double = !in_double;
                started = true;
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            c => {
                cur.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push(cur);
    }
    out
}

/// Launch the editor on `file`, executing it directly (no shell) with the
/// filename as a trailing positional argument.
fn run_editor(editor: &str, file: &Path) -> Result<()> {
    let parts = split_editor(editor);
    let Some((prog, args)) = parts.split_first() else {
        bail!("empty editor command (set $EDITOR or $VISUAL)");
    };
    let status = Command::new(prog)
        .args(args)
        .arg(file)
        .status()
        .with_context(|| format!("launch editor: {prog}"))?;
    if !status.success() {
        bail!("editor exited with failure ({status})");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_rejects_unknown_field() {
        let yaml = "rules: []\nbogus: 1\n";
        assert!(validate(yaml).is_err());
    }

    #[test]
    fn resolve_editor_prefers_visual() {
        // Snapshot and restore to avoid cross-test interference.
        let saved = (std::env::var("VISUAL").ok(), std::env::var("EDITOR").ok());
        std::env::set_var("VISUAL", "my-visual");
        std::env::set_var("EDITOR", "my-editor");
        assert_eq!(resolve_editor(), "my-visual");
        match saved.0 {
            Some(v) => std::env::set_var("VISUAL", v),
            None => std::env::remove_var("VISUAL"),
        }
        match saved.1 {
            Some(v) => std::env::set_var("EDITOR", v),
            None => std::env::remove_var("EDITOR"),
        }
    }

    #[test]
    fn split_editor_handles_args_and_quotes() {
        assert_eq!(split_editor("vi"), vec!["vi"]);
        assert_eq!(split_editor("code --wait"), vec!["code", "--wait"]);
        assert_eq!(
            split_editor("\"/path with space/ed\" -x"),
            vec!["/path with space/ed", "-x"]
        );
    }

    #[test]
    fn split_editor_does_not_interpret_shell_operators() {
        // `;` / `&&` / `$(...)` are literal characters, not command chaining:
        // a hostile $EDITOR can only name a program + args, never inject.
        assert_eq!(split_editor("vi; rm -rf /"), vec!["vi;", "rm", "-rf", "/"]);
        assert_eq!(split_editor("$(touch pwned)"), vec!["$(touch", "pwned)"]);
    }

    #[test]
    fn install_replaces_file_atomically_and_keeps_content() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.yaml");
        std::fs::write(&p, "old").unwrap();
        install(&p, "new\n").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "new\n");
        // No stray temp files left behind.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name() != "config.yaml")
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }
}

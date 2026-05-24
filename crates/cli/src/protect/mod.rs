//! Operator-side protection for `config.yaml`.
//!
//! The policy file is the agent's single most security-critical input: an
//! agent that can rewrite it can grant itself any URL or element. We borrow
//! the `visudo` pattern — keep the file root-owned and immutable, and edit it
//! only through a wrapper that validates before installing. Modifying the
//! policy then requires the human's sudo/UAC password, which an autonomous
//! agent does not have.

mod specs;

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

pub use specs::{CommandSpec, Os};

/// Outcome of [`edit`].
#[derive(Debug, PartialEq, Eq)]
pub enum EditStatus {
    /// The editor exited without changing the file.
    Unchanged,
    /// A validated new policy was written back.
    Saved,
}

/// True if writing `path` needs elevated privileges (it is protected, or
/// otherwise not writable by us). A non-existent file is "not protected":
/// `edit` will create it in a normally-writable directory.
pub fn is_protected(path: &Path) -> bool {
    match std::fs::OpenOptions::new().write(true).open(path) {
        Ok(_) => false, // opened for write and closed without touching content
        Err(e) => e.kind() == std::io::ErrorKind::PermissionDenied,
    }
}

/// Make `path` root-owned and immutable.
pub fn protect(path: &Path) -> Result<()> {
    let abs = canonical(path)?;
    warn_if_parent_writable(&abs);
    run_specs(&specs::protect_specs(Os::current(), &abs))
}

/// Undo [`protect`], handing ownership back to the current user.
pub fn unprotect(path: &Path) -> Result<()> {
    let abs = canonical(path)?;
    run_specs(&specs::unprotect_specs(
        Os::current(),
        &abs,
        &current_user(),
    ))
}

/// visudo-style edit: stage a copy, open `$EDITOR`, validate the result with
/// the real policy loader, and install it back — through the privileged path
/// when the file is protected, or a plain write when it is not. Invalid YAML
/// is never installed.
pub fn edit(path: &Path) -> Result<EditStatus> {
    let original = std::fs::read_to_string(path).unwrap_or_default();
    let dir = tempfile::tempdir().context("create staging dir")?;
    let staged = dir.path().join("config.yaml");
    std::fs::write(&staged, &original).context("stage config copy")?;

    run_editor(&resolve_editor(), &staged)?;

    let edited = std::fs::read_to_string(&staged).context("read edited config")?;
    if edited == original {
        return Ok(EditStatus::Unchanged);
    }
    validate(&edited)?;

    if is_protected(path) {
        let abs = canonical(path)?;
        if let Err(e) = run_specs(&specs::install_specs(Os::current(), &abs, &staged)) {
            // A mid-sequence failure (e.g. sudo cancelled after the lock was
            // lifted) could leave the file unprotected. Best-effort re-lock,
            // then make the risk explicit rather than silently succeeding.
            let _ = run_specs(&specs::protect_specs(Os::current(), &abs));
            return Err(e.context(format!(
                "install failed — config may be left UNPROTECTED; re-run `acb-cli protect-config -c {}`",
                path.display()
            )));
        }
    } else {
        std::fs::write(path, &edited).with_context(|| format!("write {}", path.display()))?;
    }
    Ok(EditStatus::Saved)
}

/// Validate a policy document with the same loader the daemon uses. On
/// failure the rejected text is preserved so the operator does not lose work.
fn validate(yaml: &str) -> Result<()> {
    if let Err(e) = acb_policy::load::load_policy(yaml) {
        let recovery = std::env::temp_dir().join("acb-config.rejected.yaml");
        let _ = std::fs::write(&recovery, yaml);
        bail!(
            "edited config is INVALID, not saved: {e}\nyour edits were kept at {}",
            recovery.display()
        );
    }
    Ok(())
}

fn canonical(path: &Path) -> Result<std::path::PathBuf> {
    std::fs::canonicalize(path).with_context(|| format!("config not found: {}", path.display()))
}

fn current_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "root".to_string())
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

/// Warn (don't fail) when the directory holding the policy file is writable
/// by the current user. The immutable flag protects the file's bytes, but an
/// agent that can write the *directory* can still swap the entry or win a
/// race during the privileged write — so for full protection the file must
/// live where the agent cannot write. See docs/security-model.md §7.
fn warn_if_parent_writable(path: &Path) {
    if parent_dir_writable_by_us(path) {
        eprintln!(
            "warning: the directory holding {} is writable by your user, so an agent \
             running as you could replace or swap the file despite the lock. For full \
             protection keep config.yaml in a root-owned directory (e.g. \
             /etc/access-control-browser/). See docs/security-model.md §7.",
            path.display()
        );
    }
}

/// True if we can create a file in the policy file's parent directory.
/// Probes with an auto-removed temp file rather than guessing from mode bits.
fn parent_dir_writable_by_us(path: &Path) -> bool {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    tempfile::Builder::new()
        .prefix(".acb-probe-")
        .tempfile_in(&parent)
        .is_ok()
}

fn run_specs(specs: &[CommandSpec]) -> Result<()> {
    if specs.is_empty() {
        return Ok(());
    }
    if Os::current() == Os::Windows {
        run_elevated_windows(specs)
    } else {
        for sp in specs {
            run_unix(sp)?;
        }
        Ok(())
    }
}

fn run_unix(sp: &CommandSpec) -> Result<()> {
    let mut cmd = if sp.elevate {
        let mut c = Command::new("sudo");
        c.arg(&sp.program);
        c
    } else {
        Command::new(&sp.program)
    };
    cmd.args(&sp.args);
    let status = cmd
        .status()
        .with_context(|| format!("run {} (is it installed?)", sp.program))?;
    if !status.success() {
        bail!("`{}` failed ({status})", sp.program);
    }
    Ok(())
}

/// Quote a value as a PowerShell single-quoted literal: only `'` is special
/// (escaped by doubling). Used so dynamic paths can never break out of the
/// elevated command into code.
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Build the single elevated PowerShell command that runs every spec.
/// Each program/argument is embedded as a single-quoted literal, so paths
/// with spaces or metacharacters cannot inject — unlike the old `cmd /c`
/// chain. NOTE: the elevated execution path needs verification on a real
/// Windows host; the quoting is unit-tested here.
fn windows_elevated_command(specs: &[CommandSpec]) -> String {
    let body = std::iter::once("$ErrorActionPreference='Stop'".to_string())
        .chain(specs.iter().map(|sp| {
            std::iter::once(format!("& {}", ps_quote(&sp.program)))
                .chain(sp.args.iter().map(|a| ps_quote(a)))
                .collect::<Vec<_>>()
                .join(" ")
        }))
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "Start-Process -FilePath 'powershell' -Verb RunAs -Wait \
         -ArgumentList @('-NoProfile','-Command',{})",
        ps_quote(&body)
    )
}

/// Windows has no inline per-command elevation, so run every command in one
/// elevated PowerShell (single UAC prompt).
fn run_elevated_windows(specs: &[CommandSpec]) -> Result<()> {
    let cmd = windows_elevated_command(specs);
    let status = Command::new("powershell")
        .args(["-NoProfile", "-Command", &cmd])
        .status()
        .context("launch elevated powershell")?;
    if !status.success() {
        bail!("elevated command failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn unprotected_temp_file_is_not_protected() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "rules: []").unwrap();
        assert!(!is_protected(f.path()));
    }

    #[test]
    fn missing_file_is_not_protected() {
        let p = std::env::temp_dir().join("acb-does-not-exist-xyz.yaml");
        let _ = std::fs::remove_file(&p);
        assert!(!is_protected(&p));
    }

    // A read-only file we own already routes `edit` to the privileged path.
    // (Mirrors what `protect` achieves via chattr/ownership, without root.)
    #[cfg(unix)]
    #[test]
    fn read_only_file_is_detected_as_protected() {
        use std::os::unix::fs::PermissionsExt;
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::set_permissions(f.path(), std::fs::Permissions::from_mode(0o444)).unwrap();
        assert!(is_protected(f.path()));
    }

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
    fn ps_quote_escapes_single_quotes() {
        assert_eq!(ps_quote("plain"), "'plain'");
        assert_eq!(ps_quote("C:\\a b\\c.yaml"), "'C:\\a b\\c.yaml'");
        assert_eq!(ps_quote("x'y"), "'x''y'");
    }

    #[test]
    fn windows_command_keeps_spacey_path_in_one_literal() {
        let specs = specs::protect_specs(Os::Windows, Path::new("C:\\a b\\config.yaml"));
        let cmd = windows_elevated_command(&specs);
        assert!(cmd.contains("'C:\\a b\\config.yaml'"));
        assert!(cmd.contains("Start-Process"));
        assert!(cmd.contains("-Verb RunAs"));
        // The abandoned `cmd /c` chain must not reappear.
        assert!(!cmd.contains("/c "));
    }

    #[test]
    fn windows_command_neutralizes_injection_in_path() {
        let specs = specs::install_specs(
            Os::Windows,
            Path::new("C:\\a'; calc; '.yaml"),
            Path::new("C:\\tmp\\staged.yaml"),
        );
        let cmd = windows_elevated_command(&specs);
        // The quote/operators survive only as a doubled-quote literal.
        assert!(cmd.contains("''; calc; ''"));
    }

    #[test]
    fn parent_writable_for_tempdir_true_for_missing_false() {
        let dir = tempfile::tempdir().unwrap();
        assert!(parent_dir_writable_by_us(&dir.path().join("config.yaml")));
        // A non-existent parent directory is not writable.
        assert!(!parent_dir_writable_by_us(Path::new(
            "/acb-no-such-dir-xyz/config.yaml"
        )));
    }
}

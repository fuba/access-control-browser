//! Pure builders for the privileged file-protection commands.
//!
//! Kept free of I/O so the exact argv for every platform is unit-testable on
//! any host — CI runs on Linux but still checks the macOS/Windows command
//! shapes, because the builders take the target `Os` as a parameter rather
//! than reading it from `cfg!`.

use std::path::Path;

/// Operating-system family that decides which protection mechanism to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Linux,
    Macos,
    Windows,
}

impl Os {
    /// The OS this binary was compiled for.
    pub fn current() -> Os {
        if cfg!(target_os = "windows") {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::Macos
        } else {
            Os::Linux
        }
    }
}

/// One command in a protection sequence. `elevate` means it must run as
/// root (sudo) on Unix or under UAC on Windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub elevate: bool,
}

fn s(program: &str, args: Vec<&str>) -> CommandSpec {
    CommandSpec {
        program: program.to_string(),
        args: args.into_iter().map(String::from).collect(),
        elevate: true,
    }
}

fn p(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Commands that make `path` immutable and owned by root/Administrators so a
/// non-privileged agent cannot rewrite the policy. Ownership is the primary
/// lock (works even on filesystems that ignore the immutable flag); the
/// immutable flag is defense-in-depth.
pub fn protect_specs(os: Os, path: &Path) -> Vec<CommandSpec> {
    let f = p(path);
    match os {
        Os::Linux => vec![
            s("chown", vec!["root:root", f.as_str()]),
            s("chmod", vec!["0644", f.as_str()]),
            s("chattr", vec!["+i", f.as_str()]),
        ],
        Os::Macos => vec![
            s("chown", vec!["root:wheel", f.as_str()]),
            s("chmod", vec!["0644", f.as_str()]),
            s("chflags", vec!["schg", f.as_str()]),
        ],
        Os::Windows => vec![
            s(
                "icacls",
                vec![f.as_str(), "/setowner", "BUILTIN\\Administrators"],
            ),
            s(
                "icacls",
                vec![
                    f.as_str(),
                    "/inheritance:r",
                    "/grant:r",
                    "SYSTEM:(R,W)",
                    "/grant:r",
                    "BUILTIN\\Administrators:(F)",
                    "/grant:r",
                    "BUILTIN\\Users:(RX)",
                ],
            ),
        ],
    }
}

/// Reverse of [`protect_specs`]: clear the immutable flag and hand ownership
/// back to `owner` so the operator can edit the file normally again.
pub fn unprotect_specs(os: Os, path: &Path, owner: &str) -> Vec<CommandSpec> {
    let f = p(path);
    match os {
        Os::Linux => vec![
            s("chattr", vec!["-i", f.as_str()]),
            s("chown", vec![owner, f.as_str()]),
        ],
        Os::Macos => vec![
            s("chflags", vec!["noschg", f.as_str()]),
            s("chown", vec![owner, f.as_str()]),
        ],
        Os::Windows => vec![
            s("icacls", vec![f.as_str(), "/setowner", owner]),
            s("icacls", vec![f.as_str(), "/reset"]),
        ],
    }
}

/// Replace the contents of a *protected* file with `staged`, keeping the
/// protection intact: lift the lock, copy the validated content in as root,
/// then re-apply ownership and the immutable flag.
pub fn install_specs(os: Os, path: &Path, staged: &Path) -> Vec<CommandSpec> {
    let f = p(path);
    let t = p(staged);
    match os {
        Os::Linux => vec![
            s("chattr", vec!["-i", f.as_str()]),
            s("cp", vec![t.as_str(), f.as_str()]),
            s("chown", vec!["root:root", f.as_str()]),
            s("chmod", vec!["0644", f.as_str()]),
            s("chattr", vec!["+i", f.as_str()]),
        ],
        Os::Macos => vec![
            s("chflags", vec!["noschg", f.as_str()]),
            s("cp", vec![t.as_str(), f.as_str()]),
            s("chown", vec!["root:wheel", f.as_str()]),
            s("chmod", vec!["0644", f.as_str()]),
            s("chflags", vec!["schg", f.as_str()]),
        ],
        Os::Windows => {
            // After protect, Administrators has Full Control, so an elevated
            // Copy-Item can overwrite in place; re-assert owner/ACL afterward.
            // Copy-Item (not cmd `copy`) so the elevated runner invokes it
            // directly without a shell, and `-LiteralPath` disables globbing.
            let mut v = vec![s(
                "Copy-Item",
                vec![
                    "-LiteralPath",
                    t.as_str(),
                    "-Destination",
                    f.as_str(),
                    "-Force",
                ],
            )];
            v.extend(protect_specs(Os::Windows, path));
            v
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn argv(spec: &CommandSpec) -> Vec<String> {
        let mut v = vec![spec.program.clone()];
        v.extend(spec.args.clone());
        v
    }

    #[test]
    fn linux_protect_sets_root_and_immutable() {
        let specs = protect_specs(Os::Linux, Path::new("/etc/acb/config.yaml"));
        assert!(specs.iter().all(|s| s.elevate));
        assert_eq!(
            specs.iter().map(argv).collect::<Vec<_>>(),
            vec![
                vec!["chown", "root:root", "/etc/acb/config.yaml"],
                vec!["chmod", "0644", "/etc/acb/config.yaml"],
                vec!["chattr", "+i", "/etc/acb/config.yaml"],
            ]
        );
    }

    #[test]
    fn linux_unprotect_clears_immutable_and_restores_owner() {
        let specs = unprotect_specs(Os::Linux, Path::new("/etc/acb/config.yaml"), "alice");
        assert_eq!(
            specs.iter().map(argv).collect::<Vec<_>>(),
            vec![
                vec!["chattr", "-i", "/etc/acb/config.yaml"],
                vec!["chown", "alice", "/etc/acb/config.yaml"],
            ]
        );
    }

    #[test]
    fn linux_install_lifts_then_reapplies_lock() {
        let specs = install_specs(
            Os::Linux,
            Path::new("/etc/acb/config.yaml"),
            Path::new("/tmp/staged.yaml"),
        );
        let progs: Vec<&str> = specs.iter().map(|s| s.program.as_str()).collect();
        assert_eq!(progs, vec!["chattr", "cp", "chown", "chmod", "chattr"]);
        assert_eq!(
            specs.first().unwrap().args,
            vec!["-i", "/etc/acb/config.yaml"]
        );
        assert_eq!(
            specs.last().unwrap().args,
            vec!["+i", "/etc/acb/config.yaml"]
        );
        assert_eq!(
            specs[1].args,
            vec!["/tmp/staged.yaml", "/etc/acb/config.yaml"]
        );
    }

    #[test]
    fn macos_protect_uses_chflags_schg() {
        let specs = protect_specs(Os::Macos, Path::new("/etc/acb/config.yaml"));
        let last = specs.last().unwrap();
        assert_eq!(last.program, "chflags");
        assert_eq!(last.args, vec!["schg", "/etc/acb/config.yaml"]);
    }

    #[test]
    fn windows_protect_uses_icacls_owner_and_acl() {
        let specs = protect_specs(Os::Windows, Path::new("C:\\acb\\config.yaml"));
        assert_eq!(specs[0].program, "icacls");
        assert_eq!(
            specs[0].args,
            vec![
                "C:\\acb\\config.yaml",
                "/setowner",
                "BUILTIN\\Administrators"
            ]
        );
        // Second call breaks inheritance and denies the agent write access.
        assert_eq!(specs[1].args[1], "/inheritance:r");
        assert!(specs[1].args.iter().any(|a| a == "BUILTIN\\Users:(RX)"));
    }

    #[test]
    fn windows_install_copies_then_reprotects() {
        let specs = install_specs(
            Os::Windows,
            Path::new("C:\\acb\\config.yaml"),
            Path::new("C:\\tmp\\staged.yaml"),
        );
        assert_eq!(specs[0].program, "Copy-Item");
        assert_eq!(
            specs[0].args,
            vec![
                "-LiteralPath",
                "C:\\tmp\\staged.yaml",
                "-Destination",
                "C:\\acb\\config.yaml",
                "-Force"
            ]
        );
        // Followed by the protect sequence.
        assert_eq!(specs[1].program, "icacls");
    }
}

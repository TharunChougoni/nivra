//! Pure lexical write policy; no filesystem access or runtime enforcement.
//!
//! Paths and configured roots are relative to the same logical workspace.
//! Callers must ensure that workspace and all path components are symlink-free
//! (and cannot be replaced with symlinks during a write). Lexical normalization
//! cannot enforce filesystem containment. Reads are always allowed.
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OrchestratorPolicyConfig {
    pub strategies_dir: PathBuf,
    pub proposals_dir: PathBuf,
    pub notes_dir: PathBuf,
}

impl Default for OrchestratorPolicyConfig {
    fn default() -> Self {
        Self {
            strategies_dir: "strategies".into(),
            proposals_dir: "proposals".into(),
            notes_dir: "orchestrator_notes".into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyError {
    AbsolutePath,
    ParentTraversal,
    OutsideAllowedRoots,
    EmptyRoot,
}

impl std::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::AbsolutePath => "absolute paths are not allowed",
            Self::ParentTraversal => "parent traversal is not allowed",
            Self::OutsideAllowedRoots => "write is outside allowed roots",
            Self::EmptyRoot => "allowed root must not be empty",
        })
    }
}

impl std::error::Error for PolicyError {}

#[derive(Clone, Debug)]
pub struct OrchestratorWritePolicy {
    roots: [PathBuf; 3],
}

impl OrchestratorWritePolicy {
    /// Validate and lexically normalize configured roots without consulting disk.
    pub fn new(config: &OrchestratorPolicyConfig) -> Result<Self, PolicyError> {
        let roots = [
            normalize(&config.strategies_dir)?,
            normalize(&config.proposals_dir)?,
            normalize(&config.notes_dir)?,
        ];
        if roots.iter().any(|root| root.as_os_str().is_empty()) {
            return Err(PolicyError::EmptyRoot);
        }
        Ok(Self { roots })
    }

    /// Allow only a configured root or its descendants. All `..` components
    /// are rejected, including ones that would normalize back inside a root.
    pub fn check_write(&self, path: impl AsRef<Path>) -> Result<(), PolicyError> {
        let path = normalize(path.as_ref())?;
        if self.roots.iter().any(|root| path.starts_with(root)) {
            Ok(())
        } else {
            Err(PolicyError::OutsideAllowedRoots)
        }
    }

    /// Reads have no path restrictions under this policy.
    pub fn check_read(&self, _path: impl AsRef<Path>) -> Result<(), PolicyError> {
        Ok(())
    }
}

fn normalize(path: &Path) -> Result<PathBuf, PolicyError> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => return Err(PolicyError::AbsolutePath),
            Component::ParentDir => return Err(PolicyError::ParentTraversal),
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
        }
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_roots_allow_descendants_and_deny_escapes() {
        let config: OrchestratorPolicyConfig = serde_json::from_str("{}").unwrap();
        let policy = OrchestratorWritePolicy::new(&config).unwrap();
        for path in [
            "strategies",
            "strategies/new/spec.json",
            "./strategies//./spec.json",
            "proposals/idea.md",
            "orchestrator_notes/run/note.md",
        ] {
            assert_eq!(policy.check_write(path), Ok(()), "{path}");
        }
        for path in [
            "",
            ".",
            "notes/a",
            "strategies_old/a",
            "data/a",
            "Cargo.toml",
        ] {
            assert_eq!(
                policy.check_write(path),
                Err(PolicyError::OutsideAllowedRoots),
                "{path}"
            );
        }
        for path in [
            "../strategies/a",
            "strategies/../proposals/a",
            "strategies/a/../b",
        ] {
            assert_eq!(
                policy.check_write(path),
                Err(PolicyError::ParentTraversal),
                "{path}"
            );
        }
        for path in ["/strategies/a", "/etc/passwd"] {
            assert_eq!(policy.check_write(path), Err(PolicyError::AbsolutePath));
        }
        for path in ["/etc/passwd", "../data/a", "data/a", ""] {
            assert_eq!(policy.check_read(path), Ok(()));
        }
    }

    #[test]
    fn custom_roots_are_normalized_and_invalid_config_fails_closed() {
        let config: OrchestratorPolicyConfig = serde_json::from_str(
            r#"{"strategies_dir":"./lab//strategies/.","notes_dir":"own/notes"}"#,
        )
        .unwrap();
        let policy = OrchestratorWritePolicy::new(&config).unwrap();
        for path in ["lab/strategies/a", "own/notes/a", "proposals/a"] {
            assert_eq!(policy.check_write(path), Ok(()));
        }
        for path in [
            "strategies/a",
            "orchestrator_notes/a",
            "lab/strategies_other/a",
        ] {
            assert_eq!(
                policy.check_write(path),
                Err(PolicyError::OutsideAllowedRoots)
            );
        }
        for (root, error) in [
            ("", PolicyError::EmptyRoot),
            (".", PolicyError::EmptyRoot),
            ("/tmp/strategies", PolicyError::AbsolutePath),
            ("lab/../strategies", PolicyError::ParentTraversal),
        ] {
            let config = OrchestratorPolicyConfig {
                strategies_dir: root.into(),
                ..Default::default()
            };
            assert_eq!(OrchestratorWritePolicy::new(&config).unwrap_err(), error);
        }
    }
}

//! Private application paths: absolute, no `.`/`..`, no symlinked ancestor, and
//! outside repositories and other agents' namespaces.
use std::{
    fmt,
    path::{Component, Path},
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error;
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("private_path_invalid")
    }
}
impl std::error::Error for Error {}
pub fn file_path_ok(path: &Path) -> Result<(), Error> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(Error);
    }
    for ancestor in path.ancestors() {
        if ancestor.file_name().is_some_and(|n| {
            [".hermes", ".grok", ".codex", ".openclaw"]
                .iter()
                .any(|v| n == *v)
        }) || ancestor.join(".git").exists()
            || ancestor
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
        {
            return Err(Error);
        }
    }
    Ok(())
}

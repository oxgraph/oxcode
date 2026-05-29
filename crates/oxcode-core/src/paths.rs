/// Returns the project-local OxGraph database directory.
#[must_use]
pub fn database_dir(root: &Path) -> PathBuf {
    root.join(INDEX_DIR).join(DATABASE_DIR)
}

/// Returns the project-local index directory.
#[must_use]
pub fn index_dir(root: &Path) -> PathBuf {
    root.join(INDEX_DIR)
}

pub(crate) fn canonical_root(root: &Path) -> Result<PathBuf> {
    std::fs::canonicalize(root).map_err(|source| Error::fs(root, source))
}

/// Returns a stable forward-slash relative path.
#[must_use]
pub(crate) fn normalize_relative_path(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    normalize_path(relative)
}

/// Returns a stable forward-slash path.
#[must_use]
pub(crate) fn normalize_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Directory names skipped during source discovery (generated, dependency,
/// VCS, and the index store itself).
pub(crate) const SKIP_DIR_NAMES: &[&str] =
    &[".git", INDEX_DIR, "target", "node_modules", "vendor"];

/// Skips generated, dependency, VCS, and index storage paths.
pub(crate) fn should_skip_path(root: &Path, path: &Path) -> bool {
    let relative = path.strip_prefix(root).unwrap_or(path);
    relative.components().any(|component| {
        let part = component.as_os_str().to_string_lossy();
        SKIP_DIR_NAMES.contains(&part.as_ref())
    })
}
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Project-local index directory name.
pub const INDEX_DIR: &str = ".oxcode";
/// Native OxGraph database directory name inside [`INDEX_DIR`].
pub const DATABASE_DIR: &str = "index.oxgdb";

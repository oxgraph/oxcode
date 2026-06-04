//! Index manifest for content-digest change detection.
//!
//! After a successful index, a small manifest is written next to the database
//! recording a digest of every discovered source file's relative path and
//! content, plus the resulting [`IndexStats`] counts. Re-running the index
//! recomputes the digest and, when it matches and the database still exists,
//! returns the recorded stats without re-extracting, re-resolving, or rebuilding
//! the database. Any change to a file's content or to the set of files changes
//! the digest and forces a full re-index.

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
};

use oxcode_model::IndexStats;
use serde::{Deserialize, Serialize};

use crate::{
    error::{Error, Result},
    paths,
    scan::SourceFile,
};

/// Manifest format version; a mismatch forces a full re-index.
const MANIFEST_FORMAT: u32 = 1;

/// Manifest filename inside the `.oxcode` index directory.
const MANIFEST_FILE: &str = "manifest.json";

/// Persisted index manifest: a content digest plus the last run's stat counts.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Manifest {
    /// Manifest format version.
    pub(crate) format: u32,
    /// Digest over every discovered source file's relative path and content.
    pub(crate) digest: u64,
    /// Number of source files indexed.
    pub(crate) files: usize,
    /// Number of symbols stored.
    pub(crate) symbols: usize,
    /// Number of resolved edges stored.
    pub(crate) edges: usize,
    /// Number of unresolved references retained as diagnostics.
    pub(crate) unresolved_references: usize,
    /// Number of files ignored because no explicit extractor exists.
    pub(crate) skipped_unsupported_files: usize,
    /// Number of files that failed to read or parse.
    pub(crate) failed_files: usize,
    /// Number of files parsed with recoverable errors.
    pub(crate) partial_files: usize,
}

impl Manifest {
    /// Builds a manifest recording `digest` and the counts from `stats`.
    pub(crate) fn from_stats(digest: u64, stats: &IndexStats) -> Self {
        Self {
            format: MANIFEST_FORMAT,
            digest,
            files: stats.files,
            symbols: stats.symbols,
            edges: stats.edges,
            unresolved_references: stats.unresolved_references,
            skipped_unsupported_files: stats.skipped_unsupported_files,
            failed_files: stats.failed_files,
            partial_files: stats.partial_files,
        }
    }

    /// Returns whether this manifest is current for `digest`.
    pub(crate) fn matches(&self, digest: u64) -> bool {
        self.format == MANIFEST_FORMAT && self.digest == digest
    }

    /// Reconstructs the index stats this manifest recorded.
    pub(crate) fn into_stats(self, root: PathBuf, database: PathBuf) -> IndexStats {
        IndexStats {
            root,
            database,
            files: self.files,
            symbols: self.symbols,
            edges: self.edges,
            unresolved_references: self.unresolved_references,
            skipped_unsupported_files: self.skipped_unsupported_files,
            failed_files: self.failed_files,
            partial_files: self.partial_files,
        }
    }
}

/// Returns the manifest path inside the project's index directory.
fn manifest_path(root: &Path) -> PathBuf {
    paths::index_dir(root).join(MANIFEST_FILE)
}

/// Computes a digest over every discovered source file's relative path and
/// content, so any content or file-set change is detected.
///
/// # Errors
///
/// Returns [`Error::Fs`] when a source file cannot be read.
pub(crate) fn compute_digest(root: &Path, files: &[SourceFile], scope_token: u64) -> Result<u64> {
    let mut hasher = DefaultHasher::new();
    for file in files {
        paths::normalize_relative_path(root, &file.path).hash(&mut hasher);
        let content = std::fs::read(&file.path).map_err(|source| Error::Fs {
            path: file.path.clone(),
            source,
        })?;
        content.hash(&mut hasher);
    }
    scope_token.hash(&mut hasher);
    Ok(hasher.finish())
}

/// Hashes one file's content for the per-file extraction cache.
///
/// # Performance
///
/// This function is `O(bytes.len())`.
pub(crate) fn content_hash(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// Digest over every `Cargo.toml` under the root. A manifest change can alter
/// crate-derived qualified names without changing any source file, so it must
/// invalidate both the project digest and the extraction cache.
///
/// # Errors
///
/// Returns [`Error::Fs`] when a manifest cannot be read.
pub(crate) fn scope_token(root: &Path) -> Result<u64> {
    let mut manifests: Vec<PathBuf> = Vec::new();
    for entry in ignore::WalkBuilder::new(root)
        .standard_filters(true)
        .hidden(false)
        .build()
    {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
            && !paths::should_skip_path(root, path)
            && path.file_name().is_some_and(|name| name == "Cargo.toml")
        {
            manifests.push(path.to_path_buf());
        }
    }
    manifests.sort();
    let mut hasher = DefaultHasher::new();
    for path in manifests {
        paths::normalize_relative_path(root, &path).hash(&mut hasher);
        let content = std::fs::read(&path).map_err(|source| Error::Fs {
            path: path.clone(),
            source,
        })?;
        content.hash(&mut hasher);
    }
    Ok(hasher.finish())
}

/// Loads the manifest, returning `None` when it is absent or unreadable so the
/// caller falls back to a full re-index.
pub(crate) fn load(root: &Path) -> Option<Manifest> {
    let bytes = std::fs::read(manifest_path(root)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Writes the manifest atomically next to the database.
///
/// # Errors
///
/// Returns [`Error::Fs`] when the index directory cannot be created or the
/// manifest cannot be serialized or written.
pub(crate) fn store(root: &Path, manifest: &Manifest) -> Result<()> {
    let directory = paths::index_dir(root);
    std::fs::create_dir_all(&directory).map_err(|source| Error::Fs {
        path: directory.clone(),
        source,
    })?;
    let path = manifest_path(root);
    let bytes = serde_json::to_vec_pretty(manifest).map_err(|error| Error::Fs {
        path: path.clone(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, error),
    })?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, &bytes).map_err(|source| Error::Fs {
        path: temp.clone(),
        source,
    })?;
    std::fs::rename(&temp, &path).map_err(|source| Error::Fs {
        path: path.clone(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_is_stable_and_content_sensitive() {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path();
        let file_path = root.join("a.rs");
        std::fs::write(&file_path, b"fn alpha() {}").expect("write");
        let files = vec![SourceFile {
            path: file_path.clone(),
            recognized_unsupported: false,
        }];

        let first = compute_digest(root, &files, 0).expect("digest");
        let again = compute_digest(root, &files, 0).expect("digest");
        assert_eq!(first, again, "digest is stable for unchanged content");

        std::fs::write(&file_path, b"fn alpha() { run(); }").expect("write");
        let changed = compute_digest(root, &files, 0).expect("digest");
        assert_ne!(first, changed, "digest changes when content changes");
    }
}

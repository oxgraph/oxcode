//! Per-file extraction cache.
//!
//! After a successful index, every file's extraction IR is cached next to the
//! database keyed by its content hash. On re-index, a file whose content hash
//! still matches reuses its cached extraction instead of being re-parsed; only
//! changed files run the (expensive) tree-sitter extractor. The cache is scoped
//! by a `scope_token` over all `Cargo.toml` files, so a manifest change that can
//! alter crate-derived qualified names discards the whole cache.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use oxcode_model::Extraction;
use serde::{Deserialize, Serialize};

use crate::{
    error::{Error, Result},
    paths,
};

/// Cache format version; a mismatch discards the cache.
const CACHE_FORMAT: u32 = 1;

/// Extraction cache filename inside the `.oxcode` index directory.
const CACHE_FILE: &str = "extractions.json";

/// One cached file extraction.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct CacheEntry {
    /// Content hash this entry is valid for.
    pub(crate) hash: u64,
    /// Whether the parse was partial (so the diagnostic can be reconstructed).
    pub(crate) partial: bool,
    /// Cached per-file extraction IR.
    pub(crate) extraction: Extraction,
}

/// The persisted per-file extraction cache.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ExtractionCache {
    /// Cache format version.
    pub(crate) format: u32,
    /// Token over all `Cargo.toml` files; a change discards the cache.
    pub(crate) scope_token: u64,
    /// Per-file entries keyed by repository-relative path.
    pub(crate) files: BTreeMap<String, CacheEntry>,
}

impl ExtractionCache {
    /// Returns an empty cache for the given scope token.
    pub(crate) fn empty(scope_token: u64) -> Self {
        Self {
            format: CACHE_FORMAT,
            scope_token,
            files: BTreeMap::new(),
        }
    }

    /// Returns the cached entry for `relative_path` when its hash matches.
    pub(crate) fn lookup(&self, relative_path: &str, hash: u64) -> Option<&CacheEntry> {
        self.files
            .get(relative_path)
            .filter(|entry| entry.hash == hash)
    }
}

/// Returns the extraction cache path inside the project's index directory.
fn cache_path(root: &Path) -> PathBuf {
    paths::index_dir(root).join(CACHE_FILE)
}

/// Loads the cache, returning an empty cache when it is absent, unreadable, of a
/// different format, or scoped to a different set of manifests.
pub(crate) fn load(root: &Path, scope_token: u64) -> ExtractionCache {
    let cached = std::fs::read(cache_path(root))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<ExtractionCache>(&bytes).ok());
    match cached {
        Some(cache) if cache.format == CACHE_FORMAT && cache.scope_token == scope_token => cache,
        _ => ExtractionCache::empty(scope_token),
    }
}

/// Writes the cache atomically next to the database.
///
/// # Errors
///
/// Returns [`Error::Fs`] when the index directory cannot be created or the cache
/// cannot be serialized or written.
pub(crate) fn store(root: &Path, cache: &ExtractionCache) -> Result<()> {
    let directory = paths::index_dir(root);
    std::fs::create_dir_all(&directory).map_err(|source| Error::Fs {
        path: directory.clone(),
        source,
    })?;
    let path = cache_path(root);
    let bytes = serde_json::to_vec(cache).map_err(|error| Error::Fs {
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

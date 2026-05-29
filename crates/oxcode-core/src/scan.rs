//! File discovery for indexable source units.

use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

use crate::paths::should_skip_path;

/// One discovered source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceFile {
    /// Absolute path on disk.
    pub(crate) path: PathBuf,
    /// Whether the extension is a known source language with no extractor yet.
    pub(crate) recognized_unsupported: bool,
}

/// Discovers source files that should be considered by extractors.
pub(crate) fn discover_source_files(root: &Path) -> Vec<SourceFile> {
    let mut files = Vec::new();
    for entry in WalkBuilder::new(root)
        .standard_filters(true)
        .hidden(false)
        .build()
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let path = entry.path();
        if !entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
        {
            continue;
        }
        if should_skip_path(root, path) {
            continue;
        }
        files.push(SourceFile {
            path: path.to_path_buf(),
            recognized_unsupported: crate::extract::is_recognized_unsupported(path),
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    files
}

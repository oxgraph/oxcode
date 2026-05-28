//! Language extractor registry and extraction orchestration.

use std::path::Path;

use oxcode_model::{Extraction, LanguageSupport};

use crate::{
    error::{Error, Result},
    paths::normalize_relative_path,
    scan,
};

mod rust;

/// Input provided to one language extractor.
pub(crate) struct ExtractionInput<'a> {
    /// Absolute file path.
    pub(crate) path: &'a Path,
    /// Repository-relative path with forward separators.
    pub(crate) relative_path: String,
    /// Source bytes.
    pub(crate) source: Vec<u8>,
}

/// Compile-time language extraction extension point.
pub(crate) trait LanguageExtractor {
    /// Stable language ID.
    fn language_id(&self) -> oxcode_model::LanguageId;

    /// File extensions owned by this extractor.
    fn extensions(&self) -> &'static [&'static str];

    /// Tree-sitter language-pack parser name.
    fn parser_name(&self) -> &'static str;

    /// Extracts code facts from one source file.
    fn extract(&self, input: ExtractionInput<'_>) -> Result<Extraction>;

    /// Returns whether this extractor owns `path`.
    fn supports_path(&self, path: &Path) -> bool {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| self.extensions().contains(&extension))
    }
}

/// Per-file extraction output plus scan stats.
pub(crate) struct IndexInput {
    /// Per-file extractions.
    pub(crate) extractions: Vec<Extraction>,
    /// Unsupported known source files.
    pub(crate) skipped_unsupported_files: usize,
}

/// Extracts all supported source files under a root.
pub(crate) fn extract_project(root: &Path) -> Result<IndexInput> {
    let registry = registry();
    let mut extractions = Vec::new();
    let mut skipped_unsupported_files = 0_usize;

    for file in scan::discover_source_files(root) {
        let Some(extractor) = registry
            .iter()
            .find(|extractor| extractor.supports_path(&file.path))
        else {
            if file.recognized_unsupported {
                skipped_unsupported_files = skipped_unsupported_files.saturating_add(1);
            }
            continue;
        };

        let source = std::fs::read(&file.path).map_err(|source| Error::fs(&file.path, source))?;
        let relative_path = normalize_relative_path(root, &file.path);
        extractions.push(extractor.extract(ExtractionInput {
            path: &file.path,
            relative_path,
            source,
        })?);
    }

    extractions.sort_by(|left, right| left.file.path.cmp(&right.file.path));
    Ok(IndexInput {
        extractions,
        skipped_unsupported_files,
    })
}

/// Returns explicit extractor support.
pub(crate) fn language_support() -> Vec<LanguageSupport> {
    registry()
        .into_iter()
        .map(|extractor| LanguageSupport {
            language: extractor.language_id(),
            parser_available: tree_sitter_language_pack::get_parser(extractor.parser_name())
                .is_ok(),
            extractor_available: true,
        })
        .collect()
}

fn registry() -> Vec<Box<dyn LanguageExtractor>> {
    vec![Box::new(rust::RustExtractor)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_selects_rust_files() {
        let registry = registry();
        assert!(
            registry
                .iter()
                .any(|extractor| extractor.supports_path(Path::new("lib.rs")))
        );
        assert!(
            !registry
                .iter()
                .any(|extractor| extractor.supports_path(Path::new("lib.py")))
        );
    }
}

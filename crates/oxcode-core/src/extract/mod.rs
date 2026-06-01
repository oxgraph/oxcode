//! Language extractor registry and extraction orchestration.

use std::{collections::HashMap, path::Path, sync::OnceLock};

use oxcode_model::{Extraction, FileDiagnostic, FileParseStatus, LanguageSupport};

use crate::{
    error::{Error, Result},
    paths::normalize_relative_path,
    scan,
};

mod cargo;
mod cst;
mod rust;

/// Source-language file extensions oxcode recognizes, whether or not an
/// extractor exists yet. The subset that has an extractor is derived from the
/// [`Registry`]; a recognized extension with no extractor is reported as a
/// skipped unsupported file rather than silently ignored.
pub(crate) const RECOGNIZED_SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "c", "h", "cpp", "cc", "hpp",
];

/// Input provided to one language extractor.
pub(crate) struct ExtractionInput<'a> {
    /// Absolute file path.
    pub(crate) path: &'a Path,
    /// Repository-relative path with forward separators.
    pub(crate) relative_path: String,
    /// Source bytes.
    pub(crate) source: Vec<u8>,
}

/// Language extraction extension point.
///
/// Extractors are registered once in a process-global [`Registry`] keyed by
/// file extension (see [`registry`]); dispatch is therefore an extension-map
/// lookup, not a linear scan. Implementors are zero-cost stateless markers
/// today, but the `Send + Sync` bound keeps the door open for extractors that
/// cache parser/query state in the shared registry.
pub(crate) trait LanguageExtractor: Send + Sync {
    /// Stable language ID.
    fn language_id(&self) -> oxcode_model::LanguageId;

    /// File extensions owned by this extractor.
    fn extensions(&self) -> &'static [&'static str];

    /// Tree-sitter language-pack parser name.
    fn parser_name(&self) -> &'static str;

    /// Extracts code facts from one source file.
    fn extract(&self, input: ExtractionInput<'_>) -> Result<Extraction>;
}

/// Process-global set of language extractors, indexed by file extension.
pub(crate) struct Registry {
    extractors: Vec<Box<dyn LanguageExtractor>>,
    by_extension: HashMap<&'static str, usize>,
}

impl Registry {
    /// Builds the registry from the compiled-in extractor set.
    fn build() -> Self {
        let extractors: Vec<Box<dyn LanguageExtractor>> = vec![Box::new(rust::RustExtractor)];
        let mut by_extension = HashMap::new();
        for (index, extractor) in extractors.iter().enumerate() {
            for extension in extractor.extensions() {
                let previous = by_extension.insert(*extension, index);
                debug_assert!(
                    previous.is_none(),
                    "extension {extension:?} is claimed by more than one extractor"
                );
            }
        }
        Self {
            extractors,
            by_extension,
        }
    }

    /// Returns the extractor that owns `path`, if any.
    pub(crate) fn extractor_for(&self, path: &Path) -> Option<&dyn LanguageExtractor> {
        let extension = path.extension().and_then(|extension| extension.to_str())?;
        self.by_extension
            .get(extension)
            .map(|&index| self.extractors[index].as_ref())
    }

    /// Returns whether any extractor owns `extension`.
    pub(crate) fn supports_extension(&self, extension: &str) -> bool {
        self.by_extension.contains_key(extension)
    }

    /// Returns all registered extractors.
    pub(crate) fn extractors(&self) -> &[Box<dyn LanguageExtractor>] {
        &self.extractors
    }
}

/// Returns the shared, process-global extractor registry.
pub(crate) fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Registry::build)
}

/// Returns whether `path` is a recognized source file with no extractor yet.
pub(crate) fn is_recognized_unsupported(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            RECOGNIZED_SOURCE_EXTENSIONS.contains(&extension)
                && !registry().supports_extension(extension)
        })
}

/// Per-file extraction output plus scan stats.
pub(crate) struct IndexInput {
    /// Per-file extractions for files that produced symbols.
    pub(crate) extractions: Vec<Extraction>,
    /// Unsupported known source files.
    pub(crate) skipped_unsupported_files: usize,
    /// Per-file failures and partial parses, recorded rather than fatal.
    pub(crate) diagnostics: Vec<FileDiagnostic>,
}

/// Extracts all supported source files under a root.
///
/// A file that fails to read or whose extractor errors is recorded as a
/// [`FileDiagnostic`] and skipped; the rest of the project still indexes. Only
/// catastrophic failures (not per-file ones) propagate as `Err`.
pub(crate) fn extract_project(root: &Path) -> Result<IndexInput> {
    let registry = registry();
    let mut extractions = Vec::new();
    let mut skipped_unsupported_files = 0_usize;
    let mut diagnostics = Vec::new();

    for file in scan::discover_source_files(root) {
        let Some(extractor) = registry.extractor_for(&file.path) else {
            if file.recognized_unsupported {
                skipped_unsupported_files = skipped_unsupported_files.saturating_add(1);
            }
            continue;
        };

        let relative_path = normalize_relative_path(root, &file.path);
        let source = match std::fs::read(&file.path) {
            Ok(source) => source,
            Err(error) => {
                diagnostics.push(FileDiagnostic {
                    path: relative_path,
                    status: FileParseStatus::Failed,
                    message: Some(Error::fs(&file.path, error).to_string()),
                });
                continue;
            }
        };

        match extractor.extract(ExtractionInput {
            path: &file.path,
            relative_path: relative_path.clone(),
            source,
        }) {
            Ok(extraction) => {
                if extraction.parse_status == FileParseStatus::Partial {
                    diagnostics.push(FileDiagnostic {
                        path: relative_path,
                        status: FileParseStatus::Partial,
                        message: Some("recoverable parse errors; symbols are partial".to_string()),
                    });
                }
                extractions.push(extraction);
            }
            Err(error) => diagnostics.push(FileDiagnostic {
                path: relative_path,
                status: FileParseStatus::Failed,
                message: Some(error.to_string()),
            }),
        }
    }

    extractions.sort_by(|left, right| left.file.path.cmp(&right.file.path));
    Ok(IndexInput {
        extractions,
        skipped_unsupported_files,
        diagnostics,
    })
}

/// Returns explicit extractor support.
pub(crate) fn language_support() -> Vec<LanguageSupport> {
    registry()
        .extractors()
        .iter()
        .map(|extractor| LanguageSupport {
            language: extractor.language_id(),
            parser_available: tree_sitter_language_pack::get_parser(extractor.parser_name())
                .is_ok(),
            extractor_available: true,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_selects_rust_files() {
        assert!(registry().extractor_for(Path::new("lib.rs")).is_some());
        assert!(registry().extractor_for(Path::new("lib.py")).is_none());
    }

    #[test]
    fn registry_is_a_shared_singleton() {
        assert!(std::ptr::eq(registry(), registry()));
    }

    #[test]
    fn recognized_unsupported_excludes_supported_languages() {
        // Recognized source, no extractor yet -> unsupported.
        assert!(is_recognized_unsupported(Path::new("app.py")));
        // Recognized source with an extractor -> not unsupported.
        assert!(!is_recognized_unsupported(Path::new("lib.rs")));
        // Unknown extension -> not a recognized source at all.
        assert!(!is_recognized_unsupported(Path::new("notes.txt")));
    }
}

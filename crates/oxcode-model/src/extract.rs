//! Extraction and resolution data types produced before persistence.

use serde::{Deserialize, Serialize};

use crate::{EdgeKind, LanguageId, NodeKind, ReferenceKind, ResolutionKind, SourceSpan};

/// One source file accepted by an extractor.
//
// NOTE: extraction-side DTOs keep plain `String` keys/paths because they are
// internal, allocation-churny, and string-keyed in the resolver. The typed
// vocabulary (SymbolKey/QualifiedName/SourcePath/SymbolId/NodeKind) is adopted
// on the public read surface ([`crate::SymbolSummary`]/[`crate::CodeLocation`])
// where consumers benefit from it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceUnit {
    /// Repository-relative path with forward separators.
    pub path: String,
    /// Explicit extractor language.
    pub language: LanguageId,
}

/// A language-neutral reference target emitted by an extractor.
///
/// The extractor performs all language-specific normalization and hands the
/// resolver a fully structured target: a `::`-segmented [`Self::path`], an
/// optional receiver/path [`Self::qualifier`], and a typed [`Self::kind_hint`].
/// The resolver does no string surgery of its own.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ReferenceTarget {
    /// Raw spelling from the source file.
    pub raw: String,
    /// Normalized target path segments (already stripped of generics, raw
    /// markers, and language sigils such as `crate::`/`Self::`).
    pub path: Vec<String>,
    /// Receiver or path qualifier (e.g. a method's receiver type, or the path
    /// before the final segment).
    pub qualifier: Option<String>,
    /// Language-neutral target category.
    pub kind_hint: ReferenceKind,
}

impl ReferenceTarget {
    /// Returns the final path segment, if any.
    #[must_use]
    pub fn last_segment(&self) -> Option<&str> {
        self.path.last().map(String::as_str)
    }

    /// Returns the path joined with `::`.
    #[must_use]
    pub fn joined(&self) -> String {
        self.path.join("::")
    }
}

/// A symbol discovered before persistence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolNode {
    /// Stable key derived from file, kind, qualified name, and source span.
    pub stable_key: String,
    /// Simple display name.
    pub name: String,
    /// Qualified language-level name.
    pub qualified_name: String,
    /// Node kind.
    pub kind: NodeKind,
    /// Optional native syntax kind emitted by the extractor.
    pub raw_kind: Option<String>,
    /// Language that emitted the symbol.
    pub language: LanguageId,
    /// Repository-relative file path.
    pub file_path: String,
    /// Source span.
    pub span: SourceSpan,
    /// Compact declaration or header.
    pub signature: Option<String>,
    /// Documentation comments directly attached to the symbol.
    pub docstring: Option<String>,
    /// Bounded source excerpt for agent-facing context.
    pub source_preview: Option<String>,
}

/// A resolved symbolic edge.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SymbolEdge {
    /// Source stable key.
    pub source_key: String,
    /// Target stable key.
    pub target_key: String,
    /// Edge kind.
    pub kind: EdgeKind,
}

/// Source location for a resolved reference edge.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ReferenceSite {
    /// Repository-relative path where the reference expression appears.
    pub file_path: String,
    /// Source span of the reference expression.
    pub span: SourceSpan,
    /// Trimmed source expression text.
    pub text: String,
}

/// A resolved edge with optional source-reference context.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ResolvedEdge {
    /// Source stable key.
    pub source_key: String,
    /// Target stable key.
    pub target_key: String,
    /// Edge kind.
    pub kind: EdgeKind,
    /// How the target was chosen, so consumers can filter confident edges from
    /// best-effort ones.
    pub resolution: ResolutionKind,
    /// Source reference site for edges emitted from references.
    pub reference: Option<ReferenceSite>,
}

/// A reference that could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnresolvedReference {
    /// Source stable key.
    pub source_key: String,
    /// Referenced target emitted by the extractor.
    pub target: ReferenceTarget,
    /// Intended edge kind if resolution succeeds.
    pub kind: EdgeKind,
    /// Repository-relative file path.
    pub file_path: String,
    /// Source span.
    pub span: SourceSpan,
    /// Trimmed source expression text.
    pub text: String,
    /// Reason resolution failed, populated after indexing.
    pub reason: Option<String>,
}

string_enum! {
    /// Outcome of parsing one source file.
    pub enum FileParseStatus {
        /// Parsed cleanly with no error nodes.
        Ok => "ok",
        /// Parsed with recoverable error nodes; symbols are partial.
        Partial => "partial",
        /// Could not be read or parsed at all; no symbols emitted.
        Failed => "failed",
    }
    default = Ok;
}

/// A per-file diagnostic carried through indexing instead of aborting the run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiagnostic {
    /// Repository-relative file path.
    pub path: String,
    /// Parse outcome for the file.
    pub status: FileParseStatus,
    /// Human-readable detail, when available.
    pub message: Option<String>,
}

/// Extraction output for one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extraction {
    /// Indexed source file metadata.
    pub file: SourceUnit,
    /// Parse outcome for the file.
    pub parse_status: FileParseStatus,
    /// Discovered nodes.
    pub nodes: Vec<SymbolNode>,
    /// Already-resolved syntactic edges.
    pub edges: Vec<SymbolEdge>,
    /// References pending cross-file resolution.
    pub references: Vec<UnresolvedReference>,
}

/// Resolved project graph ready for OxGraph persistence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedIndex {
    /// Indexed source files.
    pub files: Vec<SourceUnit>,
    /// Deterministic symbol nodes.
    pub nodes: Vec<SymbolNode>,
    /// Resolved edges.
    pub edges: Vec<ResolvedEdge>,
    /// References that could not be resolved.
    pub unresolved: Vec<UnresolvedReference>,
}

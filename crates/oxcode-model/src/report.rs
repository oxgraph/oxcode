//! Agent-facing report types returned by navigation and query expansion.

use serde::{Deserialize, Serialize};

use crate::{
    CodeLocation, EdgeKind, GraphDirection, LanguageId, NodeKind, QualifiedName, SourcePath,
    SymbolId, SymbolKey,
};

/// Symbol details resolved from the OxGraph database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolSummary {
    /// OxGraph element ID.
    pub id: SymbolId,
    /// Stable symbol key.
    pub stable_key: SymbolKey,
    /// Simple display name.
    pub name: String,
    /// Qualified language-level name.
    pub qualified_name: QualifiedName,
    /// Stored symbol kind.
    pub kind: NodeKind,
    /// Extractor language.
    pub language: LanguageId,
    /// Definition source location.
    pub definition: CodeLocation,
    /// Compact declaration or header.
    pub signature: Option<String>,
    /// Documentation comments directly attached to the symbol.
    pub docstring: Option<String>,
    /// Bounded source excerpt for agent-facing context.
    pub source_preview: Option<String>,
}

/// One scored symbol search candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolSearchMatch {
    /// Higher values indicate a stronger match for the query.
    pub score: u32,
    /// Matching symbol.
    pub symbol: SymbolSummary,
}

/// Agent-friendly symbol discovery report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolSearchReport {
    /// Original search text.
    pub query: String,
    /// Maximum number of candidates requested.
    pub limit: usize,
    /// Ranked matching symbols.
    pub matches: Vec<SymbolSearchMatch>,
}

/// One relationship expanded into code-aware context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationshipSummary {
    /// OxGraph relation ID.
    pub relation_id: u64,
    /// Stored edge kind.
    pub kind: EdgeKind,
    /// Source symbol.
    pub source: SymbolSummary,
    /// Target symbol.
    pub target: SymbolSummary,
    /// Optional source-reference site.
    pub site: Option<CallSiteSummary>,
}

/// One symbol related to a task-oriented context entry point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelatedSymbol {
    /// Shortest discovered hop depth.
    pub depth: usize,
    /// Why this symbol is included.
    pub reason: String,
    /// Related symbol.
    pub symbol: SymbolSummary,
}

/// File-level summary for agent navigation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSummary {
    /// Repository-relative file path.
    pub path: SourcePath,
    /// Matching score for file search.
    pub score: u32,
    /// Number of indexed symbols in the file, excluding the file node itself.
    pub symbol_count: usize,
    /// Top symbols in source order.
    pub top_symbols: Vec<SymbolSummary>,
}

/// Agent-friendly file discovery report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSearchReport {
    /// Original search text.
    pub query: String,
    /// Maximum number of files requested.
    pub limit: usize,
    /// Ranked matching files.
    pub files: Vec<FileSummary>,
}

/// File contribution inside a task-oriented context report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextFileSummary {
    /// Repository-relative file path.
    pub path: SourcePath,
    /// Number of entry-point symbols in the file.
    pub matched_symbols: usize,
    /// Number of related symbols in the file.
    pub related_symbols: usize,
}

/// Deterministic task-oriented context report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextReport {
    /// Original task or question text.
    pub query: String,
    /// Compact deterministic summary of what was found.
    pub summary: String,
    /// Best matching symbols for the task.
    pub entry_points: Vec<SymbolSearchMatch>,
    /// Symbols adjacent to entry points.
    pub related_symbols: Vec<RelatedSymbol>,
    /// Relationships connecting entry points and adjacent symbols.
    pub relationships: Vec<RelationshipSummary>,
    /// Files represented by entry points and adjacent symbols.
    pub files: Vec<ContextFileSummary>,
}

/// A symbol discovered during a bounded graph walk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraversedSymbol {
    /// Shortest discovered hop depth.
    pub depth: usize,
    /// Symbol reached at that depth.
    pub symbol: SymbolSummary,
}

/// Location and expression text for one call relation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallSiteSummary {
    /// Source location of the call expression.
    pub location: CodeLocation,
    /// Trimmed source expression text.
    pub text: String,
}

/// One call relation expanded into code-aware context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallEdgeSummary {
    /// OxGraph relation ID.
    pub relation_id: u64,
    /// Hop depth at which this edge was traversed, or `None` for edges produced
    /// by flat query expansion (where no traversal occurred).
    pub depth: Option<usize>,
    /// Calling symbol.
    pub source: SymbolSummary,
    /// Called symbol.
    pub target: SymbolSummary,
    /// Call-site source context.
    pub call_site: Option<CallSiteSummary>,
}

/// One symbol description report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolReport {
    /// Original selector.
    pub selector: String,
    /// Matched symbol.
    pub symbol: SymbolSummary,
}

/// Agent-friendly bounded call graph report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallGraphReport {
    /// Original selector.
    pub selector: String,
    /// Seed symbol.
    pub seed: SymbolSummary,
    /// Traversal direction.
    pub direction: GraphDirection,
    /// Maximum hop depth.
    pub depth: usize,
    /// Maximum discovered symbol count.
    pub limit: usize,
    /// Reached symbols in traversal order, including the seed at depth 0.
    pub symbols: Vec<TraversedSymbol>,
    /// Traversed call edges carrying call-site metadata.
    pub edges: Vec<CallEdgeSummary>,
}

/// Expanded query report that maps raw OxGraph values to code context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpandedQueryReport {
    /// Expanded rows.
    pub rows: Vec<ExpandedQueryRow>,
}

/// One expanded query row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpandedQueryRow {
    /// Expanded values.
    pub values: Vec<ExpandedQueryValue>,
}

/// One expanded query value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpandedQueryValue {
    /// Raw compact value.
    pub raw: String,
    /// Symbol context for element values, when available.
    pub symbol: Option<SymbolSummary>,
    /// Call-edge context for relation values, when available.
    pub call_edge: Option<CallEdgeSummary>,
}

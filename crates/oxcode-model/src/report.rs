//! Agent-facing report types returned by navigation and query expansion.

use serde::{Deserialize, Serialize};

use crate::{
    CodeLocation, EdgeKind, GraphDirection, HyperedgeKind, LanguageId, NodeKind, ParticipantRole,
    QualifiedName, SourcePath, SymbolId, SymbolKey,
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

/// One symbol selected into a curated context report, with its lightweight
/// fields rendered exactly once. Its source lives in the owning [`ContextFile`],
/// never re-embedded per relationship.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderedSymbol {
    /// OxGraph element ID, referenced by every other section.
    pub id: SymbolId,
    /// Simple display name.
    pub name: String,
    /// Qualified language-level name.
    pub qualified_name: QualifiedName,
    /// Stored symbol kind.
    pub kind: NodeKind,
    /// Definition source location.
    pub definition: CodeLocation,
    /// Personalized-PageRank relevance score (higher is more central to the task).
    pub pagerank: f64,
    /// Compact declaration or header.
    pub signature: Option<String>,
}

/// One relationship between selected symbols, referencing them by id so a heavy
/// [`RenderedSymbol`] is never duplicated per edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextRelation {
    /// OxGraph relation ID.
    pub relation_id: u64,
    /// Stored edge kind.
    pub kind: EdgeKind,
    /// Source symbol id.
    pub source_id: SymbolId,
    /// Target symbol id.
    pub target_id: SymbolId,
    /// Optional source-reference site.
    pub site: Option<CallSiteSummary>,
}

/// One participant of a [`ContextHyperedge`]: the symbol, its role, and enough
/// identity (qualified name + kind) to read the relationship without
/// cross-referencing `ContextReport::symbols` — container participants
/// (crates/modules/files) are not listed there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextHyperedgeParticipant {
    /// Participant symbol id.
    pub id: SymbolId,
    /// Structural role this participant plays.
    pub role: ParticipantRole,
    /// Qualified language-level name (crate/module/type/method).
    pub qualified_name: QualifiedName,
    /// Stored node kind.
    pub kind: NodeKind,
}

/// One n-ary relationship (a trait impl group or container membership) touching
/// the selected symbols, referencing participants by id and carrying its
/// hypergraph-PageRank centrality as a unit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextHyperedge {
    /// OxGraph relation id.
    pub relation_id: u64,
    /// Kind of n-ary relationship.
    pub kind: HyperedgeKind,
    /// Roled participants of the hyperedge.
    pub participants: Vec<ContextHyperedgeParticipant>,
    /// Personalized hypergraph-PageRank score of this hyperedge as a unit.
    pub pagerank: f64,
}

/// One architecture-level dependency between the crate containers owning the
/// selected symbols (a lifted `DependsOn` edge): the source crate depends on the
/// target crate. Surfaced separately from symbol-level relationships.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextDependency {
    /// Depending container's node id.
    pub source_id: SymbolId,
    /// Depending container's qualified name (crate/module).
    pub source: QualifiedName,
    /// Depended-on container's node id.
    pub target_id: SymbolId,
    /// Depended-on container's qualified name (crate/module).
    pub target: QualifiedName,
}

/// One caller of an entry-point symbol, carried with enough identity to resolve
/// it on its own (callers are upstream of the selected neighbourhood, so they do
/// not appear in `ContextReport::symbols`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlastCaller {
    /// OxGraph element ID.
    pub id: SymbolId,
    /// Qualified language-level name.
    pub qualified_name: QualifiedName,
    /// Defining file path.
    pub path: SourcePath,
}

/// What depends on the entry-point symbols.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlastRadius {
    /// Symbols that call an entry point.
    pub callers: Vec<BlastCaller>,
    /// The subset of callers that live in test-like trees.
    pub tests: Vec<BlastCaller>,
}

/// One hop on the longest call chain among the selected symbols.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallFlowHop {
    /// Calling symbol id.
    pub from_id: SymbolId,
    /// Called symbol id.
    pub to_id: SymbolId,
    /// Trait symbol id when this hop crosses an `implements` edge (approximate
    /// dynamic dispatch).
    pub dynamic_dispatch: Option<SymbolId>,
}

/// One file's curated source, rendered once under the per-file byte budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextFile {
    /// Repository-relative file path.
    pub path: SourcePath,
    /// Selected symbol ids defined in this file.
    pub symbol_ids: Vec<SymbolId>,
    /// Line-numbered source skeleton (whole file if small, else merged slices
    /// around the selected symbols), or `None` when the source is unavailable.
    pub skeleton: Option<String>,
}

/// Output-size accounting for a curated context report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBudget {
    /// Characters of source skeleton actually emitted.
    pub total_chars: usize,
    /// The configured ceiling.
    pub max_total_chars: usize,
    /// Per-file skeleton character cap.
    pub per_file_cap: usize,
    /// Whether the budget cut off lower-ranked files.
    pub truncated: bool,
}

/// Deterministic, bounded, PageRank-curated task-oriented context report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextReport {
    /// Original task or question text.
    pub query: String,
    /// Compact deterministic summary of what was found.
    pub summary: String,
    /// Output-size budget accounting.
    pub budget: ContextBudget,
    /// Selected symbols ranked by relevance; heavy fields appear once here.
    pub symbols: Vec<RenderedSymbol>,
    /// Relationships among the selected symbols, referencing them by id.
    pub relationships: Vec<ContextRelation>,
    /// N-ary relationships (impl groups, container membership) touching the
    /// selected symbols, ranked by hypergraph PageRank — the architecture-altitude
    /// layer complementing the binary `relationships`.
    pub hyperedges: Vec<ContextHyperedge>,
    /// Crate-level dependencies of the selected symbols' crates (the "layer
    /// cake"), lifted from symbol references.
    pub dependencies: Vec<ContextDependency>,
    /// Callers and covering tests of the entry-point symbols.
    pub blast_radius: BlastRadius,
    /// The longest call chain among the selected symbols.
    pub call_flow: Vec<CallFlowHop>,
    /// Selected files with their rendered source skeletons.
    pub files: Vec<ContextFile>,
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

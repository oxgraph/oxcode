//! Agent-facing report types returned by navigation and query expansion.

use serde::{Deserialize, Serialize};

use crate::{CodeLocation, GraphDirection, LanguageId, NodeKind, QualifiedName, SymbolId, SymbolKey};

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

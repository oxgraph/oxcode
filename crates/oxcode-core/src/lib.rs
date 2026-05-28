//! OxGraph-native code indexing, query, and agent navigation facade.

use std::path::Path;

use oxgraph::db::{QueryLanguage, QueryResult};

pub use oxcode_model::*;
pub use oxgraph::db::{
    ElementId as OxElementId, QueryLanguage as OxQueryLanguage, QueryResult as OxQueryResult,
    QueryValue as OxQueryValue,
};

mod error;
mod extract;
mod format;
mod nav;
mod paths;
mod resolve;
mod scan;
mod store;

pub use crate::error::{Error, Result};
pub use crate::format::{
    format_call_graph_report, format_expanded_query_report, format_query_value,
    format_selector_matches, format_symbol_report,
};
pub use crate::paths::{DATABASE_DIR, INDEX_DIR, database_dir, index_dir};
pub use crate::resolve::resolve_extractions;

use crate::store::oxgraph::OxGraphStore;

/// Indexes one project root into a native OxGraph database.
pub fn index_project(root: impl AsRef<Path>) -> Result<IndexStats> {
    let root = paths::canonical_root(root.as_ref())?;
    let input = extract::extract_project(&root)?;
    let resolved = resolve::resolve_extractions(input.extractions)?;
    let database = store::oxgraph::rebuild_database(&root, &resolved)?;
    Ok(IndexStats {
        root,
        database,
        files: resolved.files.len(),
        symbols: resolved.nodes.len(),
        edges: resolved.edges.len(),
        unresolved_references: resolved.unresolved.len(),
        skipped_unsupported_files: input.skipped_unsupported_files,
    })
}

/// Prepares and executes one raw OxGraph DB query.
pub fn query_project(
    root: impl AsRef<Path>,
    language: QueryLanguage,
    query: &str,
) -> Result<QueryResult> {
    OxGraphStore::open(root)?.query(language, query)
}

/// Prepares one query and returns OxGraph's plan explanation.
pub fn explain_project(
    root: impl AsRef<Path>,
    language: QueryLanguage,
    query: &str,
) -> Result<String> {
    OxGraphStore::open(root)?.explain(language, query)
}

/// Returns project database status.
pub fn project_status(root: impl AsRef<Path>) -> Result<ProjectStatus> {
    store::oxgraph::project_status(root)
}

/// Returns explicit extractor support.
#[must_use]
pub fn language_support() -> Vec<LanguageSupport> {
    extract::language_support()
}

/// Parses a query language name.
pub fn parse_query_language(value: &str) -> std::result::Result<QueryLanguage, String> {
    match value {
        "oxql" => Ok(QueryLanguage::Oxql),
        "cypher" => Ok(QueryLanguage::Cypher),
        other => Err(format!(
            "unknown query language {other}; expected oxql or cypher"
        )),
    }
}

/// Parses an agent graph navigation direction.
pub fn parse_graph_direction(value: &str) -> std::result::Result<GraphDirection, String> {
    match value {
        "outgoing" => Ok(GraphDirection::Outgoing),
        "incoming" => Ok(GraphDirection::Incoming),
        "both" => Ok(GraphDirection::Both),
        other => Err(format!(
            "unknown graph direction {other}; expected outgoing, incoming, or both"
        )),
    }
}

/// Opened project index facade.
pub struct ProjectIndex {
    store: OxGraphStore,
}

impl ProjectIndex {
    /// Opens a project index rooted at `root`.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            store: OxGraphStore::open(root)?,
        })
    }

    /// Resolves one selector into matching symbols.
    pub fn resolve_selector(&self, selector: &str) -> Result<Vec<SymbolSummary>> {
        self.store
            .with_read(|read| nav::resolve_selector(read, selector))
    }

    /// Describes one selected symbol.
    pub fn describe_symbol(&self, selector: &str) -> Result<SymbolReport> {
        self.store
            .with_read(|read| nav::describe_symbol(read, selector))
    }

    /// Builds an agent-friendly call graph report for one selector.
    pub fn call_graph(
        &self,
        selector: &str,
        direction: GraphDirection,
        depth: usize,
        limit: usize,
    ) -> Result<CallGraphReport> {
        self.store
            .with_read(|read| nav::call_graph(read, selector, direction, depth, limit))
    }

    /// Executes and expands one query in the same read snapshot.
    pub fn query_expanded(
        &self,
        language: QueryLanguage,
        query: &str,
    ) -> Result<ExpandedQueryReport> {
        self.store
            .with_read(|read| nav::query_expanded(read, language, query))
    }
}

/// Resolves one selector into matching symbols.
pub fn resolve_selector(root: impl AsRef<Path>, selector: &str) -> Result<Vec<SymbolSummary>> {
    ProjectIndex::open(root)?.resolve_selector(selector)
}

/// Describes one selected symbol.
pub fn describe_symbol(root: impl AsRef<Path>, selector: &str) -> Result<SymbolReport> {
    ProjectIndex::open(root)?.describe_symbol(selector)
}

/// Builds an agent-friendly call graph report for one selector.
pub fn call_graph(
    root: impl AsRef<Path>,
    selector: &str,
    direction: GraphDirection,
    depth: usize,
    limit: usize,
) -> Result<CallGraphReport> {
    ProjectIndex::open(root)?.call_graph(selector, direction, depth, limit)
}

/// Executes and expands one query in the same read snapshot.
pub fn query_expanded_project(
    root: impl AsRef<Path>,
    language: QueryLanguage,
    query: &str,
) -> Result<ExpandedQueryReport> {
    ProjectIndex::open(root)?.query_expanded(language, query)
}

/// Expands a previously executed raw result by reopening the project index.
///
/// Prefer [`query_expanded_project`] when possible so raw IDs and hydrated code
/// context come from one read snapshot.
pub fn expand_query_result(
    root: impl AsRef<Path>,
    result: QueryResult,
) -> Result<ExpandedQueryReport> {
    ProjectIndex::open(root)?
        .store
        .with_read(|read| read.expand_query_result(&result))
}

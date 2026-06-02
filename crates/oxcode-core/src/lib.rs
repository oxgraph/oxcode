//! OxGraph-native code indexing, query, and agent navigation facade.

use std::path::Path;

pub use oxcode_model::*;
pub use oxgraph::db::{
    ElementId as OxElementId, QueryLanguage as OxQueryLanguage, QueryResult as OxQueryResult,
    QueryValue as OxQueryValue,
};
use oxgraph::db::{QueryLanguage, QueryResult};

mod error;
mod extract;
mod format;
mod paths;
mod resolve;
mod scan;
mod store;

use crate::store::oxgraph::{OxGraphStore, ReadSession};
pub use crate::{
    error::{Error, Result},
    format::{
        format_call_graph_report, format_context_report, format_expanded_query_report,
        format_file_search_report, format_query_value, format_selector_matches,
        format_selector_not_found, format_symbol_report, format_symbol_search_report,
    },
    paths::{DATABASE_DIR, INDEX_DIR, database_dir, index_dir},
    resolve::resolve_extractions,
};

/// Indexes one project root into a native OxGraph database.
pub fn index_project(root: impl AsRef<Path>) -> Result<IndexStats> {
    let root = paths::canonical_root(root.as_ref())?;
    let input = extract::extract_project(&root)?;
    let failed_files = input
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.status == FileParseStatus::Failed)
        .count();
    let partial_files = input
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.status == FileParseStatus::Partial)
        .count();
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
        failed_files,
        partial_files,
    })
}

/// Returns project database status, reporting `database_exists: false` rather
/// than erroring when no index has been built yet.
pub fn project_status(root: impl AsRef<Path>) -> Result<ProjectStatus> {
    store::oxgraph::project_status(root)
}

/// Returns explicit extractor support.
#[must_use]
pub fn language_support() -> Vec<LanguageSupport> {
    extract::language_support()
}

/// An opened project index: the single stateful entry point for all reads.
///
/// Opening resolves the database and its property-key schema once; every method
/// reuses that open handle, and [`ProjectIndex::with_session`] runs several
/// operations against one read snapshot so multi-step navigation stays
/// internally consistent.
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
        self.store.with_read(|read| read.resolve_selector(selector))
    }

    /// Searches indexed symbols with optional kind filters.
    pub fn search_symbols_filtered(
        &self,
        query: &str,
        limit: usize,
        kinds: &[NodeKind],
    ) -> Result<SymbolSearchReport> {
        self.store
            .with_read(|read| read.search_symbols_filtered(query, limit, kinds))
    }

    /// Searches indexed source files.
    pub fn search_files(&self, query: &str, limit: usize) -> Result<FileSearchReport> {
        self.store.with_read(|read| read.search_files(query, limit))
    }

    /// Builds deterministic task-oriented context.
    pub fn context(&self, query: &str, limit: usize, depth: usize) -> Result<ContextReport> {
        self.store
            .with_read(|read| read.context(query, limit, depth))
    }

    /// Describes one selected symbol.
    pub fn describe_symbol(&self, selector: &str) -> Result<SymbolReport> {
        self.store.with_read(|read| read.describe_symbol(selector))
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
            .with_read(|read| read.call_graph(selector, direction, depth, limit))
    }

    /// Executes one raw OxGraph database query.
    pub fn query(&self, language: QueryLanguage, query: &str) -> Result<QueryResult> {
        self.store.query(language, query)
    }

    /// Returns OxGraph's plan explanation for one query.
    pub fn explain(&self, language: QueryLanguage, query: &str) -> Result<String> {
        self.store.explain(language, query)
    }

    /// Executes and expands one query in a single read snapshot.
    pub fn query_expanded(
        &self,
        language: QueryLanguage,
        query: &str,
    ) -> Result<ExpandedQueryReport> {
        self.store
            .with_read(|read| read.query_expanded(language, query))
    }

    /// Runs several read operations against one shared read snapshot.
    pub fn with_session<T>(&self, f: impl FnOnce(&Session<'_>) -> Result<T>) -> Result<T> {
        self.store.with_read(|read| f(&Session { read }))
    }
}

/// A batch of read operations bound to a single read snapshot.
pub struct Session<'index> {
    read: &'index ReadSession<'index>,
}

impl Session<'_> {
    /// Resolves one selector into matching symbols.
    pub fn resolve_selector(&self, selector: &str) -> Result<Vec<SymbolSummary>> {
        self.read.resolve_selector(selector)
    }

    /// Searches indexed symbols with optional kind filters.
    pub fn search_symbols_filtered(
        &self,
        query: &str,
        limit: usize,
        kinds: &[NodeKind],
    ) -> Result<SymbolSearchReport> {
        self.read.search_symbols_filtered(query, limit, kinds)
    }

    /// Searches indexed source files.
    pub fn search_files(&self, query: &str, limit: usize) -> Result<FileSearchReport> {
        self.read.search_files(query, limit)
    }

    /// Builds deterministic task-oriented context.
    pub fn context(&self, query: &str, limit: usize, depth: usize) -> Result<ContextReport> {
        self.read.context(query, limit, depth)
    }

    /// Describes one selected symbol.
    pub fn describe_symbol(&self, selector: &str) -> Result<SymbolReport> {
        self.read.describe_symbol(selector)
    }

    /// Builds an agent-friendly call graph report for one selector.
    pub fn call_graph(
        &self,
        selector: &str,
        direction: GraphDirection,
        depth: usize,
        limit: usize,
    ) -> Result<CallGraphReport> {
        self.read.call_graph(selector, direction, depth, limit)
    }

    /// Executes one raw query against this snapshot.
    pub fn query(&self, language: QueryLanguage, query: &str) -> Result<QueryResult> {
        self.read.execute_query(language, query)
    }

    /// Executes and expands one query against this snapshot.
    pub fn query_expanded(
        &self,
        language: QueryLanguage,
        query: &str,
    ) -> Result<ExpandedQueryReport> {
        self.read.query_expanded(language, query)
    }

    /// Expands a previously executed raw result against this snapshot.
    pub fn expand(&self, result: &QueryResult) -> Result<ExpandedQueryReport> {
        self.read.expand_query_result(result)
    }
}

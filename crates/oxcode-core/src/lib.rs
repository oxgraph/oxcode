//! OxGraph-native code indexing, query, and agent navigation facade.

use std::path::Path;

pub use oxcode_model::*;
use oxgraph::db::QueryResult;
pub use oxgraph::db::{
    ElementId as OxElementId, QueryResult as OxQueryResult, QueryValue as OxQueryValue,
};

mod cache;
mod error;
mod extract;
mod format;
mod manifest;
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

/// Options for a graph navigation/traversal: direction from the seed, maximum
/// hop depth, and the maximum number of result rows.
#[derive(Debug, Clone, Copy)]
pub struct GraphWalk {
    /// Traversal direction from the seed.
    pub direction: GraphDirection,
    /// Maximum hop depth.
    pub depth: usize,
    /// Maximum number of rows to return.
    pub limit: usize,
}

/// A stage of the indexing pipeline, reported through the
/// [`index_project_with_progress`] callback as each phase begins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexStage {
    /// Discovering source files and hashing them into a content digest.
    Scan,
    /// Parsing sources into symbols and edges (tree-sitter extraction).
    Extract,
    /// Resolving cross-file references into graph edges.
    Resolve,
    /// Reconciling the resolved index into the OxGraph database.
    Store,
}

impl IndexStage {
    /// A short human-readable label for the stage, suitable for a progress
    /// message.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Scan => "scanning sources",
            Self::Extract => "extracting symbols",
            Self::Resolve => "resolving references",
            Self::Store => "reconciling database",
        }
    }
}

/// A single progress milestone emitted while indexing: the stage that is
/// starting and its position in the fixed sequence of [`IndexProgress::TOTAL`]
/// stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexProgress {
    /// The stage that is now beginning.
    pub stage: IndexStage,
    /// The 1-based position of this stage (monotonically increasing).
    pub step: u32,
    /// The total number of stages in a full index (always [`Self::TOTAL`]).
    pub total: u32,
}

impl IndexProgress {
    /// The fixed number of stages a full (non-no-op) index passes through.
    pub const TOTAL: u32 = 4;
}

/// Indexes one project root into a native OxGraph database.
///
/// Re-indexing an unchanged project is a near-instant no-op: a content digest
/// over every discovered source file is compared against the manifest written
/// by the previous run, and when it matches (and the database still exists) the
/// recorded stats are returned without re-extracting, re-resolving, or
/// rebuilding the database.
pub fn index_project(root: impl AsRef<Path>) -> Result<IndexStats> {
    index_project_with_progress(root, |_| {})
}

/// Indexes one project root into a native OxGraph database, reporting each
/// pipeline stage to `on_progress` as it begins.
///
/// Identical to [`index_project`] except that `on_progress` is invoked with an
/// [`IndexProgress`] at the start of each of the four heavy stages (`Scan →
/// Extract → Resolve → Store`). On the near-instant unchanged-digest path only
/// the `Scan` milestone fires before the cached stats are returned. The
/// callback runs synchronously on the calling thread.
pub fn index_project_with_progress(
    root: impl AsRef<Path>,
    mut on_progress: impl FnMut(IndexProgress),
) -> Result<IndexStats> {
    let stage = |stage: IndexStage, step: u32| IndexProgress {
        stage,
        step,
        total: IndexProgress::TOTAL,
    };

    on_progress(stage(IndexStage::Scan, 1));
    let root = paths::canonical_root(root.as_ref())?;
    let files = scan::discover_source_files(&root);
    let scope = manifest::scope_token(&root)?;
    let digest = manifest::compute_digest(&root, &files, scope)?;
    let database_path = paths::database_dir(&root);
    if database_path.exists()
        && let Some(existing) = manifest::load(&root)
        && existing.matches(digest)
    {
        return Ok(existing.into_stats(root, database_path));
    }

    on_progress(stage(IndexStage::Extract, 2));
    let cache = cache::load(&root, scope);
    let (input, next_cache) = extract::extract_project(&root, &cache)?;
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
    on_progress(stage(IndexStage::Resolve, 3));
    let resolved = resolve::resolve_extractions(input.extractions)?;
    // Reconcile the database against the resolved index in place: unchanged
    // symbols and edges keep their ids (only the changed delta mutates) and the
    // first index simply mints everything against a fresh store.
    on_progress(stage(IndexStage::Store, 4));
    let database = store::oxgraph::reconcile_database(&root, &resolved)?;
    let stats = IndexStats {
        root,
        database,
        files: resolved.files.len(),
        symbols: resolved.nodes.len(),
        edges: resolved.edges.len(),
        unresolved_references: resolved.unresolved.len(),
        skipped_unsupported_files: input.skipped_unsupported_files,
        failed_files,
        partial_files,
    };
    manifest::store(&stats.root, &manifest::Manifest::from_stats(digest, &stats))?;
    cache::store(&stats.root, &next_cache)?;
    Ok(stats)
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

    /// Builds a bounded, PageRank-curated task-oriented context, capping the
    /// rendered source at `max_bytes` characters.
    pub fn context(
        &self,
        query: &str,
        limit: usize,
        depth: usize,
        max_bytes: usize,
    ) -> Result<ContextReport> {
        self.store
            .with_read(|read| read.context(query, limit, depth, max_bytes))
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

    /// Builds a navigation report over `edge_kind` edges for one selector.
    pub fn navigate(
        &self,
        selector: &str,
        edge_kind: EdgeKind,
        walk: GraphWalk,
    ) -> Result<CallGraphReport> {
        self.store
            .with_read(|read| read.navigate(selector, edge_kind, walk))
    }

    /// Builds a navigation report from one text navigation query.
    ///
    /// The query grammar is `<edge> <direction> <selector> [depth <n>]
    /// [limit <n>]` (see [`NavQuery`]); it lowers into [`Self::navigate`].
    pub fn navigate_query(&self, query: &str) -> Result<CallGraphReport> {
        let nav = NavQuery::parse(query).map_err(|error| Error::InvalidQuery(error.to_string()))?;
        self.navigate(
            &nav.selector,
            nav.edge_kind,
            GraphWalk {
                direction: nav.direction,
                depth: nav.depth,
                limit: nav.limit,
            },
        )
    }

    /// Executes one raw OxGraph database query.
    pub fn query(&self, query: &str) -> Result<QueryResult> {
        self.store.query(query)
    }

    /// Returns OxGraph's plan explanation for one query.
    pub fn explain(&self, query: &str) -> Result<String> {
        self.store.explain(query)
    }

    /// Executes and expands one query in a single read snapshot.
    pub fn query_expanded(&self, query: &str) -> Result<ExpandedQueryReport> {
        self.store.with_read(|read| read.query_expanded(query))
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

    /// Builds a bounded, PageRank-curated task-oriented context, capping the
    /// rendered source at `max_bytes` characters.
    pub fn context(
        &self,
        query: &str,
        limit: usize,
        depth: usize,
        max_bytes: usize,
    ) -> Result<ContextReport> {
        self.read.context(query, limit, depth, max_bytes)
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

    /// Builds a navigation report over `edge_kind` edges for one selector.
    pub fn navigate(
        &self,
        selector: &str,
        edge_kind: EdgeKind,
        walk: GraphWalk,
    ) -> Result<CallGraphReport> {
        self.read.navigate(selector, edge_kind, walk)
    }

    /// Builds a navigation report from one text navigation query.
    ///
    /// The query grammar is `<edge> <direction> <selector> [depth <n>]
    /// [limit <n>]` (see [`NavQuery`]); it lowers into [`Self::navigate`].
    pub fn navigate_query(&self, query: &str) -> Result<CallGraphReport> {
        let nav = NavQuery::parse(query).map_err(|error| Error::InvalidQuery(error.to_string()))?;
        self.navigate(
            &nav.selector,
            nav.edge_kind,
            GraphWalk {
                direction: nav.direction,
                depth: nav.depth,
                limit: nav.limit,
            },
        )
    }

    /// Executes one raw query against this snapshot.
    pub fn query(&self, query: &str) -> Result<QueryResult> {
        self.read.execute_query(query)
    }

    /// Executes and expands one query against this snapshot.
    pub fn query_expanded(&self, query: &str) -> Result<ExpandedQueryReport> {
        self.read.query_expanded(query)
    }

    /// Expands a previously executed raw result against this snapshot.
    pub fn expand(&self, result: &QueryResult) -> Result<ExpandedQueryReport> {
        self.read.expand_query_result(result)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// Writes `content` to `root/rel`, creating parent directories.
    fn write_file(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create dirs");
        }
        fs::write(path, content).expect("write file");
    }

    #[test]
    fn incremental_reindex_preserves_element_ids_and_adds_new_symbols() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write_file(
            root,
            "Cargo.toml",
            "[package]\nname = \"demo\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        );
        write_file(root, "src/lib.rs", "pub mod a;\npub mod b;\n");
        write_file(root, "src/a.rs", "pub fn alpha() {}\n");
        write_file(root, "src/b.rs", "pub fn beta() {}\n");

        index_project(root).expect("first index");
        let id_before = {
            let index = ProjectIndex::open(root).expect("open");
            let matches = index.resolve_selector("name:alpha").expect("resolve");
            assert_eq!(matches.len(), 1);
            matches[0].id
        };

        // Edit b.rs only; a.rs (containing alpha) is unchanged, so the
        // incremental path must preserve alpha's element id.
        write_file(root, "src/b.rs", "pub fn beta() {}\npub fn gamma() {}\n");
        index_project(root).expect("incremental index");

        let index = ProjectIndex::open(root).expect("reopen");
        let after = index.resolve_selector("name:alpha").expect("resolve");
        assert_eq!(after.len(), 1);
        assert_eq!(
            after[0].id, id_before,
            "unchanged symbol keeps its element id across an incremental reindex"
        );
        assert_eq!(
            index.resolve_selector("name:gamma").expect("resolve").len(),
            1,
            "newly added symbol is indexed"
        );
    }

    #[test]
    fn incremental_reindex_tombstones_a_removed_symbol() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write_file(
            root,
            "Cargo.toml",
            "[package]\nname = \"demo\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        );
        write_file(root, "src/lib.rs", "pub mod a;\n");
        // a.rs starts with two symbols; the second is removed by the edit below.
        write_file(root, "src/a.rs", "pub fn keep() {}\npub fn drop_me() {}\n");

        index_project(root).expect("first index");
        {
            let index = ProjectIndex::open(root).expect("open");
            assert_eq!(
                index
                    .resolve_selector("name:drop_me")
                    .expect("resolve")
                    .len(),
                1,
                "removed symbol is present before the edit"
            );
        }

        // Rewrite a.rs dropping `drop_me` (but keeping `keep`). The complement-
        // based tombstoning must delete the removed symbol's element while reusing
        // the surviving one.
        write_file(root, "src/a.rs", "pub fn keep() {}\n");
        index_project(root).expect("incremental index");

        let index = ProjectIndex::open(root).expect("reopen");
        assert_eq!(
            index
                .resolve_selector("name:drop_me")
                .expect("resolve")
                .len(),
            0,
            "removed symbol resolves to zero matches after an incremental reindex"
        );
        assert_eq!(
            index.resolve_selector("name:keep").expect("resolve").len(),
            1,
            "surviving symbol in the edited file is retained"
        );
    }

    #[test]
    fn navigate_generalizes_traversal_to_each_edge_kind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write_file(
            root,
            "Cargo.toml",
            "[package]\nname = \"demo\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        );
        write_file(
            root,
            "src/lib.rs",
            "pub fn helper() {}\npub fn entry() { helper(); }\n",
        );
        index_project(root).expect("index");
        let index = ProjectIndex::open(root).expect("open");

        // navigate over the Calls projection reaches the callee.
        let calls = index
            .navigate(
                "name:entry",
                EdgeKind::Calls,
                GraphWalk {
                    direction: GraphDirection::Outgoing,
                    depth: 3,
                    limit: 100,
                },
            )
            .expect("navigate calls");
        let reached: std::collections::BTreeSet<String> = calls
            .symbols
            .iter()
            .map(|node| node.symbol.name.clone())
            .collect();
        assert!(
            reached.contains("helper"),
            "Calls navigation reaches helper: {reached:?}"
        );

        // navigate over a different (Contains) projection executes against its
        // own edges rather than the call graph.
        let contains = index
            .navigate(
                "name:helper",
                EdgeKind::Contains,
                GraphWalk {
                    direction: GraphDirection::Incoming,
                    depth: 3,
                    limit: 100,
                },
            )
            .expect("navigate contains");
        assert_eq!(contains.seed.name, "helper");
    }

    #[test]
    fn navigate_query_text_grammar_lowers_into_navigate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write_file(
            root,
            "Cargo.toml",
            "[package]\nname = \"demo\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        );
        write_file(
            root,
            "src/lib.rs",
            "pub fn helper() {}\npub fn entry() { helper(); }\n",
        );
        index_project(root).expect("index");
        let index = ProjectIndex::open(root).expect("open");

        // The text grammar resolves to the same traversal as the typed call.
        let report = index
            .navigate_query("calls outgoing name:entry depth 3 limit 100")
            .expect("navigate_query");
        let reached: std::collections::BTreeSet<String> = report
            .symbols
            .iter()
            .map(|node| node.symbol.name.clone())
            .collect();
        assert!(
            reached.contains("helper"),
            "text navigation reaches helper: {reached:?}"
        );

        // A malformed query surfaces a typed error rather than panicking.
        let error = index
            .navigate_query("calls sideways name:entry")
            .expect_err("unknown direction is rejected");
        assert!(
            matches!(error, Error::InvalidQuery(_)),
            "unknown direction maps to InvalidQuery: {error:?}"
        );
    }
}

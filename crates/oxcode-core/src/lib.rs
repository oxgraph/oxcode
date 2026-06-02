//! OxGraph-native code indexing, query, and agent navigation facade.

use std::path::Path;

pub use oxcode_model::*;
pub use oxgraph::db::{
    ElementId as OxElementId, QueryLanguage as OxQueryLanguage, QueryResult as OxQueryResult,
    QueryValue as OxQueryValue,
};
use oxgraph::db::{QueryLanguage, QueryResult};

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

/// Indexes one project root into a native OxGraph database.
///
/// Re-indexing an unchanged project is a near-instant no-op: a content digest
/// over every discovered source file is compared against the manifest written
/// by the previous run, and when it matches (and the database still exists) the
/// recorded stats are returned without re-extracting, re-resolving, or
/// rebuilding the database.
pub fn index_project(root: impl AsRef<Path>) -> Result<IndexStats> {
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
    let resolved = resolve::resolve_extractions(input.extractions)?;
    // Update an existing database in place (preserving element ids of unchanged
    // symbols); build from scratch on the first index.
    let database = if database_path.exists() {
        store::oxgraph::apply_delta(&root, &resolved)?
    } else {
        store::oxgraph::rebuild_database(&root, &resolved)?
    };
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
                index.resolve_selector("name:drop_me").expect("resolve").len(),
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
            index.resolve_selector("name:drop_me").expect("resolve").len(),
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

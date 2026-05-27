//! OxGraph-native code indexing and query engine for `oxcode`.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
    path::{Path, PathBuf},
    str,
};

use ignore::WalkBuilder;
use oxgraph::db::{
    CatalogSummary, Database, DbError, ElementId, GraphProjectionDefinition, IndexDefinition,
    LabelId, ProjectionDefinition, PropertyFamily, PropertyKeyId, PropertySubject, PropertyType,
    PropertyValue, QueryLanguage, QueryResult, QueryValue, RelationId, RelationTypeId,
};
use oxgraph::{
    graph::{EdgeSourceGraph, EdgeTargetGraph, IncomingGraph, OutgoingGraph},
    topology::{
        CanonicalElementIdentity, CanonicalRelationIdentity, LocalElementIdentity,
        LocalRelationIdentity,
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tree_sitter_language_pack::{Node, Tree};

/// Re-exported OxGraph database query language.
pub use oxgraph::db::QueryLanguage as OxQueryLanguage;

/// Project-local index directory name.
pub const INDEX_DIR: &str = ".oxcode";
/// Native OxGraph database directory name inside [`INDEX_DIR`].
pub const DATABASE_DIR: &str = "index.oxgdb";

/// Convenient result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Oxcode failure surface.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Filesystem operation failed.
    #[error("filesystem error at {path}: {source}")]
    Fs {
        /// Path being accessed.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// Tree-sitter parsing failed.
    #[error("parse error in {path}: {message}")]
    Parse {
        /// File being parsed.
        path: PathBuf,
        /// Human-readable parse message.
        message: String,
    },

    /// OxGraph database operation failed.
    #[error("oxgraph database error: {0}")]
    Database(#[from] DbError),

    /// JSON serialization failed.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// Integer conversion overflowed.
    #[error("integer value {value} cannot be represented in the target type")]
    IntegerOverflow {
        /// Overflowing value.
        value: usize,
    },

    /// The project database is missing catalog metadata expected by oxcode.
    #[error("database catalog is missing {item} {name}")]
    MissingCatalog {
        /// Catalog item category.
        item: &'static str,
        /// Missing catalog name.
        name: String,
    },

    /// A database subject is missing a property expected by oxcode.
    #[error("database subject is missing property {name}")]
    MissingProperty {
        /// Missing property name.
        name: String,
    },

    /// A selector did not match any symbol.
    #[error("selector {selector:?} did not match any symbol")]
    SelectorNotFound {
        /// Original selector text.
        selector: String,
    },

    /// A selector matched more than one symbol.
    #[error("selector {selector:?} matched multiple symbols")]
    AmbiguousSelector {
        /// Original selector text.
        selector: String,
        /// Candidate matches.
        matches: Vec<SymbolSummary>,
    },
}

impl Error {
    /// Wraps an I/O error with the path that produced it.
    fn fs(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Fs {
            path: path.into(),
            source,
        }
    }
}

/// Stable language identifier.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LanguageId(String);

impl LanguageId {
    /// Creates a new language identifier.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the underlying identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for LanguageId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl fmt::Display for LanguageId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A byte and line span in a source file.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct SourceSpan {
    /// Inclusive start byte.
    pub start_byte: usize,
    /// Exclusive end byte.
    pub end_byte: usize,
    /// One-based start line.
    pub start_line: usize,
    /// Zero-based start column.
    pub start_column: usize,
    /// One-based end line.
    pub end_line: usize,
    /// Zero-based end column.
    pub end_column: usize,
}

/// Kind of code symbol stored as an OxGraph element label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// A source file.
    File,
    /// A module.
    Module,
    /// A namespace.
    Namespace,
    /// A package.
    Package,
    /// A class.
    Class,
    /// A struct.
    Struct,
    /// An enum.
    Enum,
    /// A trait.
    Trait,
    /// An interface.
    Interface,
    /// An implementation block.
    ImplBlock,
    /// A free function.
    Function,
    /// A method.
    Method,
    /// A field declaration.
    Field,
    /// A variable declaration.
    Variable,
    /// A constant declaration.
    Constant,
    /// A type alias.
    TypeAlias,
    /// A macro definition or invocation target.
    Macro,
}

impl NodeKind {
    /// All known node kinds.
    const ALL: [Self; 17] = [
        Self::File,
        Self::Module,
        Self::Namespace,
        Self::Package,
        Self::Class,
        Self::Struct,
        Self::Enum,
        Self::Trait,
        Self::Interface,
        Self::ImplBlock,
        Self::Function,
        Self::Method,
        Self::Field,
        Self::Variable,
        Self::Constant,
        Self::TypeAlias,
        Self::Macro,
    ];

    /// Returns the stable storage representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Module => "module",
            Self::Namespace => "namespace",
            Self::Package => "package",
            Self::Class => "class",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Interface => "interface",
            Self::ImplBlock => "impl_block",
            Self::Function => "function",
            Self::Method => "method",
            Self::Field => "field",
            Self::Variable => "variable",
            Self::Constant => "constant",
            Self::TypeAlias => "type_alias",
            Self::Macro => "macro",
        }
    }
}

impl fmt::Display for NodeKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Kind of code relationship stored as an OxGraph relation type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// Parent owns or syntactically contains child.
    Contains,
    /// Source imports the target.
    Imports,
    /// Source calls target.
    Calls,
    /// Source references target.
    References,
    /// Source implements target.
    Implements,
    /// Source defines target.
    Defines,
}

impl EdgeKind {
    /// All known edge kinds.
    const ALL: [Self; 6] = [
        Self::Contains,
        Self::Imports,
        Self::Calls,
        Self::References,
        Self::Implements,
        Self::Defines,
    ];

    /// Returns the stable storage representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::Imports => "imports",
            Self::Calls => "calls",
            Self::References => "references",
            Self::Implements => "implements",
            Self::Defines => "defines",
        }
    }
}

impl fmt::Display for EdgeKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One source file accepted by an extractor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceUnit {
    /// Repository-relative path with forward separators.
    pub path: String,
    /// Explicit extractor language.
    pub language: LanguageId,
    /// SHA-256 hash of the file contents.
    pub hash: String,
    /// File size in bytes.
    pub byte_len: usize,
}

/// A language-neutral reference target emitted by an extractor.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ReferenceTarget {
    /// Raw spelling from the source file.
    pub raw: String,
    /// Normalized spelling used by language-neutral resolution.
    pub normalized: String,
    /// Optional qualifier, such as a module, type, receiver, or namespace.
    pub qualifier: Option<String>,
    /// Optional language-specific target category hint.
    pub kind_hint: Option<String>,
}

impl ReferenceTarget {
    /// Creates a target where raw and normalized spelling are identical.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        Self {
            raw: value.clone(),
            normalized: value,
            qualifier: None,
            kind_hint: None,
        }
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

/// Extraction output for one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extraction {
    /// Indexed source file metadata.
    pub file: SourceUnit,
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

/// Summary of one indexing run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexStats {
    /// Canonical project root.
    pub root: PathBuf,
    /// Native OxGraph database directory.
    pub database: PathBuf,
    /// Number of source files indexed.
    pub files: usize,
    /// Number of symbols stored.
    pub symbols: usize,
    /// Number of resolved edges stored.
    pub edges: usize,
    /// Number of unresolved references retained as diagnostics.
    pub unresolved_references: usize,
    /// Number of files ignored because no explicit extractor exists.
    pub skipped_unsupported_files: usize,
}

/// Project index status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectStatus {
    /// Canonical project root.
    pub root: PathBuf,
    /// Native OxGraph database directory.
    pub database: PathBuf,
    /// Whether the native database exists.
    pub database_exists: bool,
    /// Visible commit sequence.
    pub visible_commit_seq: Option<u64>,
    /// Last writer transaction high-water mark.
    pub last_transaction_id: Option<u64>,
    /// Visible element count.
    pub elements: usize,
    /// Visible relation count.
    pub relations: usize,
    /// Visible incidence count.
    pub incidences: usize,
    /// Indexed file count.
    pub files: usize,
    /// Call relation count.
    pub calls: usize,
    /// Unresolved reference diagnostic count.
    pub unresolved_references: usize,
    /// Catalog-size summary.
    pub catalog: CatalogStatus,
}

/// Serializable catalog-size summary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogStatus {
    /// Role count.
    pub role_count: usize,
    /// Label count.
    pub label_count: usize,
    /// Relation type count.
    pub relation_type_count: usize,
    /// Property key count.
    pub property_key_count: usize,
    /// Projection count.
    pub projection_count: usize,
    /// Index count.
    pub index_count: usize,
}

impl From<CatalogSummary> for CatalogStatus {
    fn from(summary: CatalogSummary) -> Self {
        Self {
            role_count: summary.role_count,
            label_count: summary.label_count,
            relation_type_count: summary.relation_type_count,
            property_key_count: summary.property_key_count,
            projection_count: summary.projection_count,
            index_count: summary.index_count,
        }
    }
}

/// Supported extractor information shown by `oxcode languages`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageSupport {
    /// Language name.
    pub language: LanguageId,
    /// Whether the parser backend can provide a parser.
    pub parser_available: bool,
    /// Whether oxcode has an explicit extractor.
    pub extractor_available: bool,
}

/// Direction for agent-friendly call graph navigation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphDirection {
    /// Follow calls made by the seed.
    #[default]
    Outgoing,
    /// Follow callers of the seed.
    Incoming,
    /// Follow both outgoing and incoming calls.
    Both,
}

impl GraphDirection {
    /// Returns the stable CLI spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Outgoing => "outgoing",
            Self::Incoming => "incoming",
            Self::Both => "both",
        }
    }
}

impl fmt::Display for GraphDirection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Compact source location shown in agent-facing reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeLocation {
    /// Repository-relative file path.
    pub file_path: String,
    /// Inclusive start byte.
    pub start_byte: usize,
    /// Exclusive end byte.
    pub end_byte: usize,
    /// One-based start line.
    pub start_line: usize,
    /// Zero-based start column.
    pub start_column: usize,
    /// One-based end line.
    pub end_line: usize,
    /// Zero-based end column.
    pub end_column: usize,
}

/// Symbol details resolved from the OxGraph database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolSummary {
    /// OxGraph element ID.
    pub id: u64,
    /// Stable symbol key.
    pub stable_key: String,
    /// Simple display name.
    pub name: String,
    /// Qualified language-level name.
    pub qualified_name: String,
    /// Stored symbol kind.
    pub kind: String,
    /// Extractor language.
    pub language: String,
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
    /// Hop depth at which this edge was traversed.
    pub depth: usize,
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

/// Indexes one project root into a native OxGraph database.
pub fn index_project(root: impl AsRef<Path>) -> Result<IndexStats> {
    let root = canonical_root(root.as_ref())?;
    let IndexInput {
        extractions,
        skipped_unsupported_files,
    } = extract_project(&root)?;
    let resolved = resolve_extractions(extractions)?;
    let database = rebuild_database(&root, &resolved)?;
    Ok(IndexStats {
        root,
        database,
        files: resolved.files.len(),
        symbols: resolved.nodes.len(),
        edges: resolved.edges.len(),
        unresolved_references: resolved.unresolved.len(),
        skipped_unsupported_files,
    })
}

/// Prepares and executes one OxGraph DB query.
pub fn query_project(
    root: impl AsRef<Path>,
    language: QueryLanguage,
    query: &str,
) -> Result<QueryResult> {
    let database = open_database(root.as_ref())?;
    let prepared = database.prepare(language, query)?;
    Ok(database.begin_read().execute(&prepared)?)
}

/// Prepares one query and returns OxGraph's plan explanation.
pub fn explain_project(
    root: impl AsRef<Path>,
    language: QueryLanguage,
    query: &str,
) -> Result<String> {
    let database = open_database(root.as_ref())?;
    let prepared = database.prepare(language, query)?;
    Ok(database.begin_read().explain(&prepared))
}

/// Returns project database status.
pub fn project_status(root: impl AsRef<Path>) -> Result<ProjectStatus> {
    let root = canonical_root(root.as_ref())?;
    let database_path = database_dir(&root);
    if !database_path.join("store.oxgdb").exists() {
        return Ok(ProjectStatus {
            root,
            database: database_path,
            database_exists: false,
            visible_commit_seq: None,
            last_transaction_id: None,
            elements: 0,
            relations: 0,
            incidences: 0,
            files: 0,
            calls: 0,
            unresolved_references: 0,
            catalog: CatalogStatus::default(),
        });
    }

    let database = Database::open(&database_path)?;
    let status = database.status();
    let read = database.begin_read();
    let files = count_query(&database, &read, "MATCH ELEMENTS HAS LABEL file")?;
    let calls = count_query(&database, &read, "MATCH RELATIONS TYPE calls")?;
    let unresolved_references = count_query(
        &database,
        &read,
        "MATCH ELEMENTS HAS LABEL unresolved_reference",
    )?;
    Ok(ProjectStatus {
        root,
        database: database_path,
        database_exists: true,
        visible_commit_seq: Some(status.visible_commit_seq.get()),
        last_transaction_id: Some(status.last_transaction_id.get()),
        elements: status.element_count,
        relations: status.relation_count,
        incidences: status.incidence_count,
        files,
        calls,
        unresolved_references,
        catalog: status.catalog.into(),
    })
}

/// Returns explicit extractor support.
#[must_use]
pub fn language_support() -> Vec<LanguageSupport> {
    vec![LanguageSupport {
        language: rust_language(),
        parser_available: tree_sitter_language_pack::get_parser("rust").is_ok(),
        extractor_available: true,
    }]
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

/// Resolves one selector into matching symbols.
pub fn resolve_selector(root: impl AsRef<Path>, selector: &str) -> Result<Vec<SymbolSummary>> {
    let database = open_database(root.as_ref())?;
    let read = database.begin_read();
    let keys = ElementPropertyKeys::load(&read)?;
    resolve_selector_in_read(&read, &keys, selector)
}

/// Describes one selected symbol.
pub fn describe_symbol(root: impl AsRef<Path>, selector: &str) -> Result<SymbolReport> {
    let database = open_database(root.as_ref())?;
    let read = database.begin_read();
    let keys = ElementPropertyKeys::load(&read)?;
    let symbol = resolve_one_symbol_in_read(&read, &keys, selector)?;
    Ok(SymbolReport {
        selector: selector.to_string(),
        symbol,
    })
}

/// Builds an agent-friendly call graph report for one selector.
pub fn call_graph(
    root: impl AsRef<Path>,
    selector: &str,
    direction: GraphDirection,
    depth: usize,
    limit: usize,
) -> Result<CallGraphReport> {
    let database = open_database(root.as_ref())?;
    let read = database.begin_read();
    let element_keys = ElementPropertyKeys::load(&read)?;
    let relation_keys = RelationPropertyKeys::load(&read)?;
    let seed = resolve_one_symbol_in_read(&read, &element_keys, selector)?;
    let seed_id = ElementId::new(seed.id);
    let mut symbols = vec![TraversedSymbol {
        depth: 0,
        symbol: seed.clone(),
    }];
    let mut edges = Vec::new();

    let Ok(graph) = read.graph_projection_by_name("calls") else {
        return Ok(CallGraphReport {
            selector: selector.to_string(),
            seed,
            direction,
            depth,
            limit,
            symbols,
            edges,
        });
    };
    let Some(seed_local) = graph.local_element_id(seed_id) else {
        return Ok(CallGraphReport {
            selector: selector.to_string(),
            seed,
            direction,
            depth,
            limit,
            symbols,
            edges,
        });
    };

    let mut discovered = BTreeMap::from([(seed_id, 0_usize)]);
    let mut queued = VecDeque::from([(seed_local, 0_usize)]);
    let mut emitted_relations = BTreeSet::new();

    while let Some((current, current_depth)) = queued.pop_front() {
        if current_depth >= depth {
            continue;
        }
        if matches!(direction, GraphDirection::Outgoing | GraphDirection::Both) {
            visit_call_edges(
                &read,
                &element_keys,
                &relation_keys,
                &graph,
                current,
                current_depth,
                limit,
                EdgeVisitDirection::Outgoing,
                &mut discovered,
                &mut queued,
                &mut symbols,
                &mut emitted_relations,
                &mut edges,
            )?;
        }
        if matches!(direction, GraphDirection::Incoming | GraphDirection::Both) {
            visit_call_edges(
                &read,
                &element_keys,
                &relation_keys,
                &graph,
                current,
                current_depth,
                limit,
                EdgeVisitDirection::Incoming,
                &mut discovered,
                &mut queued,
                &mut symbols,
                &mut emitted_relations,
                &mut edges,
            )?;
        }
    }

    Ok(CallGraphReport {
        selector: selector.to_string(),
        seed,
        direction,
        depth,
        limit,
        symbols,
        edges,
    })
}

/// Expands raw OxGraph query values into code-aware rows when possible.
pub fn expand_query_result(
    root: impl AsRef<Path>,
    result: QueryResult,
) -> Result<ExpandedQueryReport> {
    let database = open_database(root.as_ref())?;
    let read = database.begin_read();
    let element_keys = ElementPropertyKeys::load(&read)?;
    let relation_keys = RelationPropertyKeys::load(&read)?;
    let graph = read.graph_projection_by_name("calls").ok();
    let rows = result
        .rows()
        .iter()
        .map(|row| {
            let values = row
                .values
                .iter()
                .map(|value| {
                    expand_query_value(&read, &element_keys, &relation_keys, graph.as_ref(), value)
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(ExpandedQueryRow { values })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ExpandedQueryReport { rows })
}

/// Formats one query value for compact CLI output.
#[must_use]
pub fn format_query_value(value: &QueryValue) -> String {
    match value {
        QueryValue::Element(id) => format!("element:{}", id.get()),
        QueryValue::Relation(id) => format!("relation:{}", id.get()),
        QueryValue::Incidence(record) => format!(
            "incidence:{} relation={} element={} role={}",
            record.id.get(),
            record.relation.get(),
            record.element.get(),
            record.role.get()
        ),
        QueryValue::Subject(subject) => match subject {
            PropertySubject::Element(id) => format!("element:{}", id.get()),
            PropertySubject::Relation(id) => format!("relation:{}", id.get()),
            PropertySubject::Incidence(id) => format!("incidence:{}", id.get()),
        },
        QueryValue::Property(value) => value.to_string(),
        QueryValue::Text(value) => value.clone(),
        QueryValue::Projection(id) => format!("projection:{}", id.get()),
    }
}

/// Formats one symbol report for agent-facing CLI output.
#[must_use]
pub fn format_symbol_report(report: &SymbolReport) -> String {
    let mut output = String::new();
    push_symbol_block(&mut output, "symbol", &report.symbol);
    output
}

/// Formats one call graph report for agent-facing CLI output.
#[must_use]
pub fn format_call_graph_report(report: &CallGraphReport) -> String {
    let mut output = String::new();
    push_symbol_block(&mut output, "seed", &report.seed);
    output.push_str(&format!(
        "walk calls direction={} depth={} limit={}\n",
        report.direction, report.depth, report.limit
    ));
    if report.edges.is_empty() {
        output.push_str("  no call edges found\n");
        return output;
    }
    for edge in &report.edges {
        output.push_str(&format!(
            "  depth {} relation:{}\n",
            edge.depth, edge.relation_id
        ));
        output.push_str(&format!(
            "    {} -> {}\n",
            symbol_inline(&edge.source),
            symbol_inline(&edge.target)
        ));
        if let Some(call_site) = &edge.call_site {
            output.push_str(&format!(
                "    called from {}\n",
                location_range(&call_site.location)
            ));
            if !call_site.text.is_empty() {
                output.push_str(&format!("    expression {}\n", call_site.text));
            }
        } else {
            output.push_str("    call site unavailable\n");
        }
    }
    output
}

/// Formats expanded query rows for agent-facing CLI output.
#[must_use]
pub fn format_expanded_query_report(report: &ExpandedQueryReport) -> String {
    let mut output = String::new();
    if report.rows.is_empty() {
        output.push_str("no rows\n");
        return output;
    }
    for (index, row) in report.rows.iter().enumerate() {
        output.push_str(&format!("row {}\n", index + 1));
        for value in &row.values {
            output.push_str(&format!("  {}\n", value.raw));
            if let Some(symbol) = &value.symbol {
                output.push_str(&format!("    {}\n", symbol_inline(symbol)));
                output.push_str(&format!(
                    "    defined at {}\n",
                    location_range(&symbol.definition)
                ));
            }
            if let Some(edge) = &value.call_edge {
                output.push_str(&format!(
                    "    calls {} -> {}\n",
                    symbol_inline(&edge.source),
                    symbol_inline(&edge.target)
                ));
                if let Some(call_site) = &edge.call_site {
                    output.push_str(&format!(
                        "    called from {}\n",
                        location_range(&call_site.location)
                    ));
                    if !call_site.text.is_empty() {
                        output.push_str(&format!("    expression {}\n", call_site.text));
                    }
                }
            }
        }
    }
    output
}

/// Formats selector ambiguity matches for agent-facing CLI output.
#[must_use]
pub fn format_selector_matches(selector: &str, matches: &[SymbolSummary]) -> String {
    let mut output = format!("selector {selector:?} matched multiple symbols\n");
    for symbol in matches {
        output.push_str(&format!(
            "  {} retry: element:{} or {}\n",
            symbol_inline(symbol),
            symbol.id,
            symbol.qualified_name
        ));
    }
    output
}

/// Property keys needed for symbol expansion.
struct ElementPropertyKeys {
    /// Stable key property.
    stable_key: PropertyKeyId,
    /// Name property.
    name: PropertyKeyId,
    /// Qualified name property.
    qualified_name: PropertyKeyId,
    /// Kind property.
    kind: PropertyKeyId,
    /// Language property.
    language: PropertyKeyId,
    /// File path property.
    file_path: PropertyKeyId,
    /// Start byte property.
    start_byte: PropertyKeyId,
    /// End byte property.
    end_byte: PropertyKeyId,
    /// Start line property.
    start_line: PropertyKeyId,
    /// Start column property.
    start_column: PropertyKeyId,
    /// End line property.
    end_line: PropertyKeyId,
    /// End column property.
    end_column: PropertyKeyId,
}

impl ElementPropertyKeys {
    /// Loads required element property keys from the catalog.
    fn load(read: &oxgraph::db::ReadTransaction) -> Result<Self> {
        Ok(Self {
            stable_key: require_property_key(read, "stable_key")?,
            name: require_property_key(read, "name")?,
            qualified_name: require_property_key(read, "qualified_name")?,
            kind: require_property_key(read, "kind")?,
            language: require_property_key(read, "language")?,
            file_path: require_property_key(read, "file_path")?,
            start_byte: require_property_key(read, "start_byte")?,
            end_byte: require_property_key(read, "end_byte")?,
            start_line: require_property_key(read, "start_line")?,
            start_column: require_property_key(read, "start_column")?,
            end_line: require_property_key(read, "end_line")?,
            end_column: require_property_key(read, "end_column")?,
        })
    }
}

/// Property keys needed for relation expansion.
struct RelationPropertyKeys {
    /// Call file path property.
    call_file_path: PropertyKeyId,
    /// Call start line property.
    call_start_line: PropertyKeyId,
    /// Call start column property.
    call_start_column: PropertyKeyId,
    /// Call end line property.
    call_end_line: PropertyKeyId,
    /// Call end column property.
    call_end_column: PropertyKeyId,
    /// Call start byte property.
    call_start_byte: PropertyKeyId,
    /// Call end byte property.
    call_end_byte: PropertyKeyId,
    /// Call expression text property.
    call_text: PropertyKeyId,
}

impl RelationPropertyKeys {
    /// Loads required relation property keys from the catalog.
    fn load(read: &oxgraph::db::ReadTransaction) -> Result<Self> {
        Ok(Self {
            call_file_path: require_property_key(read, "call_file_path")?,
            call_start_line: require_property_key(read, "call_start_line")?,
            call_start_column: require_property_key(read, "call_start_column")?,
            call_end_line: require_property_key(read, "call_end_line")?,
            call_end_column: require_property_key(read, "call_end_column")?,
            call_start_byte: require_property_key(read, "call_start_byte")?,
            call_end_byte: require_property_key(read, "call_end_byte")?,
            call_text: require_property_key(read, "call_text")?,
        })
    }
}

/// Direction used for one edge-expansion pass.
#[derive(Clone, Copy)]
enum EdgeVisitDirection {
    /// Visit outgoing projection edges.
    Outgoing,
    /// Visit incoming projection edges.
    Incoming,
}

/// Resolves exactly one symbol or returns a selector error.
fn resolve_one_symbol_in_read(
    read: &oxgraph::db::ReadTransaction,
    keys: &ElementPropertyKeys,
    selector: &str,
) -> Result<SymbolSummary> {
    let matches = resolve_selector_in_read(read, keys, selector)?;
    match matches.as_slice() {
        [symbol] => Ok(symbol.clone()),
        [] => Err(Error::SelectorNotFound {
            selector: selector.to_string(),
        }),
        _ => Err(Error::AmbiguousSelector {
            selector: selector.to_string(),
            matches,
        }),
    }
}

/// Resolves one selector against a read transaction.
fn resolve_selector_in_read(
    read: &oxgraph::db::ReadTransaction,
    keys: &ElementPropertyKeys,
    selector: &str,
) -> Result<Vec<SymbolSummary>> {
    let selector = selector.trim();
    if let Some(raw) = selector.strip_prefix("element:") {
        let Ok(id) = raw.parse::<u64>() else {
            return Ok(Vec::new());
        };
        return symbol_summary_from_element(read, keys, ElementId::new(id)).map(|summary| {
            summary
                .filter(is_agent_symbol)
                .into_iter()
                .collect::<Vec<_>>()
        });
    }
    if let Some(name) = selector.strip_prefix("name:") {
        return lookup_symbols_by_property(read, keys, keys.name, name);
    }
    if let Some(file_selector) = selector.strip_prefix("file:") {
        return resolve_file_line_selector(read, keys, file_selector);
    }
    lookup_symbols_by_property(read, keys, keys.qualified_name, selector)
}

/// Looks up symbols by one exact text property.
fn lookup_symbols_by_property(
    read: &oxgraph::db::ReadTransaction,
    keys: &ElementPropertyKeys,
    key: PropertyKeyId,
    value: &str,
) -> Result<Vec<SymbolSummary>> {
    let mut symbols = read
        .lookup_property_equal(key, &PropertyValue::Text(value.to_string()))?
        .into_iter()
        .filter_map(|subject| match subject {
            PropertySubject::Element(id) => Some(id),
            PropertySubject::Relation(_) | PropertySubject::Incidence(_) => None,
        })
        .map(|id| symbol_summary_from_element(read, keys, id))
        .filter_map(|result| match result {
            Ok(Some(symbol)) => Some(Ok(symbol)),
            Ok(None) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<Result<Vec<_>>>()?;
    symbols.retain(is_agent_symbol);
    symbols.sort_by(|left, right| {
        left.qualified_name
            .cmp(&right.qualified_name)
            .then(left.definition.file_path.cmp(&right.definition.file_path))
            .then(left.definition.start_byte.cmp(&right.definition.start_byte))
    });
    Ok(symbols)
}

/// Resolves a `file:path:line` selector to the innermost matching symbol.
fn resolve_file_line_selector(
    read: &oxgraph::db::ReadTransaction,
    keys: &ElementPropertyKeys,
    selector: &str,
) -> Result<Vec<SymbolSummary>> {
    let Some((file_path, line)) = selector.rsplit_once(':') else {
        return Ok(Vec::new());
    };
    let Ok(line) = line.parse::<usize>() else {
        return Ok(Vec::new());
    };
    let mut symbols = lookup_symbols_by_property(read, keys, keys.file_path, file_path)?;
    symbols.retain(|symbol| {
        !matches!(symbol.kind.as_str(), "file" | "unresolved_reference")
            && symbol.definition.start_line <= line
            && line <= symbol.definition.end_line
    });
    let Some(shortest_span) = symbols
        .iter()
        .map(|symbol| {
            symbol
                .definition
                .end_byte
                .saturating_sub(symbol.definition.start_byte)
        })
        .min()
    else {
        return Ok(Vec::new());
    };
    symbols.retain(|symbol| {
        symbol
            .definition
            .end_byte
            .saturating_sub(symbol.definition.start_byte)
            == shortest_span
    });
    Ok(symbols)
}

/// Visits one direction of graph edges from a frontier element.
#[expect(
    clippy::too_many_arguments,
    reason = "keeps traversal state local to call_graph"
)]
fn visit_call_edges(
    read: &oxgraph::db::ReadTransaction,
    element_keys: &ElementPropertyKeys,
    relation_keys: &RelationPropertyKeys,
    graph: &oxgraph::db::GraphProjection,
    current: oxgraph::db::ProjectionElementId,
    current_depth: usize,
    limit: usize,
    direction: EdgeVisitDirection,
    discovered: &mut BTreeMap<ElementId, usize>,
    queued: &mut VecDeque<(oxgraph::db::ProjectionElementId, usize)>,
    symbols: &mut Vec<TraversedSymbol>,
    emitted_relations: &mut BTreeSet<RelationId>,
    edges: &mut Vec<CallEdgeSummary>,
) -> Result<()> {
    let local_edges = match direction {
        EdgeVisitDirection::Outgoing => graph.outgoing_edges(current).collect::<Vec<_>>(),
        EdgeVisitDirection::Incoming => graph.incoming_edges(current).collect::<Vec<_>>(),
    };
    let next_depth = current_depth + 1;
    for local_edge in local_edges {
        let source_local = graph.source(local_edge);
        let target_local = graph.target(local_edge);
        let neighbor_local = match direction {
            EdgeVisitDirection::Outgoing => target_local,
            EdgeVisitDirection::Incoming => source_local,
        };
        let neighbor = graph.canonical_element_id(neighbor_local);
        if let std::collections::btree_map::Entry::Vacant(entry) = discovered.entry(neighbor) {
            if symbols.len().saturating_sub(1) >= limit {
                continue;
            }
            let Some(symbol) = symbol_summary_from_element(read, element_keys, neighbor)? else {
                continue;
            };
            entry.insert(next_depth);
            queued.push_back((neighbor_local, next_depth));
            symbols.push(TraversedSymbol {
                depth: next_depth,
                symbol,
            });
        }

        let relation = graph.canonical_relation_id(local_edge);
        if emitted_relations.insert(relation)
            && let Some(edge) = call_edge_summary(
                read,
                element_keys,
                relation_keys,
                relation,
                graph.canonical_element_id(source_local),
                graph.canonical_element_id(target_local),
                next_depth,
            )?
        {
            edges.push(edge);
        }
    }
    Ok(())
}

/// Expands one query value.
fn expand_query_value(
    read: &oxgraph::db::ReadTransaction,
    element_keys: &ElementPropertyKeys,
    relation_keys: &RelationPropertyKeys,
    graph: Option<&oxgraph::db::GraphProjection>,
    value: &QueryValue,
) -> Result<ExpandedQueryValue> {
    let raw = format_query_value(value);
    let mut expanded = ExpandedQueryValue {
        raw,
        symbol: None,
        call_edge: None,
    };
    match value {
        QueryValue::Element(id) => {
            expanded.symbol = symbol_summary_from_element(read, element_keys, *id)?;
        }
        QueryValue::Relation(id) => {
            expanded.call_edge = graph
                .and_then(|graph| graph.local_relation_id(*id).map(|local| (graph, local)))
                .map(|(graph, local)| {
                    call_edge_summary(
                        read,
                        element_keys,
                        relation_keys,
                        *id,
                        graph.canonical_element_id(graph.source(local)),
                        graph.canonical_element_id(graph.target(local)),
                        1,
                    )
                })
                .transpose()?
                .flatten();
        }
        QueryValue::Incidence(record) => {
            expanded.symbol = symbol_summary_from_element(read, element_keys, record.element)?;
        }
        QueryValue::Subject(subject) => {
            if let PropertySubject::Element(id) = subject {
                expanded.symbol = symbol_summary_from_element(read, element_keys, *id)?;
            }
        }
        QueryValue::Property(_) | QueryValue::Text(_) | QueryValue::Projection(_) => {}
    }
    Ok(expanded)
}

/// Builds one call edge summary from canonical endpoint IDs.
fn call_edge_summary(
    read: &oxgraph::db::ReadTransaction,
    element_keys: &ElementPropertyKeys,
    relation_keys: &RelationPropertyKeys,
    relation: RelationId,
    source: ElementId,
    target: ElementId,
    depth: usize,
) -> Result<Option<CallEdgeSummary>> {
    let Some(source) = symbol_summary_from_element(read, element_keys, source)? else {
        return Ok(None);
    };
    let Some(target) = symbol_summary_from_element(read, element_keys, target)? else {
        return Ok(None);
    };
    Ok(Some(CallEdgeSummary {
        relation_id: relation.get(),
        depth,
        source,
        target,
        call_site: call_site_summary(read, relation_keys, relation),
    }))
}

/// Reads call-site metadata from one relation.
fn call_site_summary(
    read: &oxgraph::db::ReadTransaction,
    keys: &RelationPropertyKeys,
    relation: RelationId,
) -> Option<CallSiteSummary> {
    let subject = PropertySubject::Relation(relation);
    let file_path = optional_text_property(read, subject, keys.call_file_path)?;
    let location = CodeLocation {
        file_path,
        start_byte: optional_usize_property(read, subject, keys.call_start_byte)?,
        end_byte: optional_usize_property(read, subject, keys.call_end_byte)?,
        start_line: optional_usize_property(read, subject, keys.call_start_line)?,
        start_column: optional_usize_property(read, subject, keys.call_start_column)?,
        end_line: optional_usize_property(read, subject, keys.call_end_line)?,
        end_column: optional_usize_property(read, subject, keys.call_end_column)?,
    };
    Some(CallSiteSummary {
        location,
        text: optional_text_property(read, subject, keys.call_text).unwrap_or_default(),
    })
}

/// Reads symbol properties for one element.
fn symbol_summary_from_element(
    read: &oxgraph::db::ReadTransaction,
    keys: &ElementPropertyKeys,
    id: ElementId,
) -> Result<Option<SymbolSummary>> {
    if read.element(id).is_none() {
        return Ok(None);
    }
    let subject = PropertySubject::Element(id);
    let Some(stable_key) = optional_text_property(read, subject, keys.stable_key) else {
        return Ok(None);
    };
    let Some(name) = optional_text_property(read, subject, keys.name) else {
        return Ok(None);
    };
    let Some(qualified_name) = optional_text_property(read, subject, keys.qualified_name) else {
        return Ok(None);
    };
    let Some(kind) = optional_text_property(read, subject, keys.kind) else {
        return Ok(None);
    };
    let Some(file_path) = optional_text_property(read, subject, keys.file_path) else {
        return Ok(None);
    };
    let Some(start_byte) = optional_usize_property(read, subject, keys.start_byte) else {
        return Ok(None);
    };
    let Some(end_byte) = optional_usize_property(read, subject, keys.end_byte) else {
        return Ok(None);
    };
    let Some(start_line) = optional_usize_property(read, subject, keys.start_line) else {
        return Ok(None);
    };
    let Some(start_column) = optional_usize_property(read, subject, keys.start_column) else {
        return Ok(None);
    };
    let Some(end_line) = optional_usize_property(read, subject, keys.end_line) else {
        return Ok(None);
    };
    let Some(end_column) = optional_usize_property(read, subject, keys.end_column) else {
        return Ok(None);
    };
    Ok(Some(SymbolSummary {
        id: id.get(),
        stable_key,
        name,
        qualified_name,
        kind,
        language: optional_text_property(read, subject, keys.language).unwrap_or_default(),
        definition: CodeLocation {
            file_path,
            start_byte,
            end_byte,
            start_line,
            start_column,
            end_line,
            end_column,
        },
    }))
}

/// Returns whether a symbol should participate in agent selectors.
fn is_agent_symbol(symbol: &SymbolSummary) -> bool {
    symbol.kind != "unresolved_reference"
}

/// Requires a property key by catalog name.
fn require_property_key(
    read: &oxgraph::db::ReadTransaction,
    name: &'static str,
) -> Result<PropertyKeyId> {
    read.catalog()
        .property_key_id(name)
        .ok_or_else(|| Error::MissingCatalog {
            item: "property",
            name: name.to_string(),
        })
}

/// Reads one optional text property.
fn optional_text_property(
    read: &oxgraph::db::ReadTransaction,
    subject: PropertySubject,
    key: PropertyKeyId,
) -> Option<String> {
    match read.property(subject, key) {
        Some(PropertyValue::Text(value)) => Some(value.clone()),
        Some(PropertyValue::Boolean(_) | PropertyValue::Integer(_)) | None => None,
    }
}

/// Reads one optional unsigned integer property.
fn optional_usize_property(
    read: &oxgraph::db::ReadTransaction,
    subject: PropertySubject,
    key: PropertyKeyId,
) -> Option<usize> {
    match read.property(subject, key) {
        Some(PropertyValue::Integer(value)) => usize::try_from(*value).ok(),
        Some(PropertyValue::Boolean(_) | PropertyValue::Text(_)) | None => None,
    }
}

/// Appends one multi-line symbol block.
fn push_symbol_block(output: &mut String, label: &str, symbol: &SymbolSummary) {
    output.push_str(&format!("{label} element:{}\n", symbol.id));
    output.push_str(&format!("  {}\n", symbol.qualified_name));
    output.push_str(&format!("  {}\n", symbol.kind));
    output.push_str(&format!(
        "  defined at {}\n",
        location_range(&symbol.definition)
    ));
}

/// Formats one symbol on a single line.
fn symbol_inline(symbol: &SymbolSummary) -> String {
    format!(
        "element:{} {} {} {}",
        symbol.id,
        symbol.qualified_name,
        symbol.kind,
        location_range(&symbol.definition)
    )
}

/// Formats one source range.
fn location_range(location: &CodeLocation) -> String {
    format!(
        "{}:{}:{}-{}:{}",
        location.file_path,
        location.start_line,
        location.start_column,
        location.end_line,
        location.end_column
    )
}

/// Returns the project-local OxGraph database directory.
#[must_use]
pub fn database_dir(root: &Path) -> PathBuf {
    root.join(INDEX_DIR).join(DATABASE_DIR)
}

/// Returns the project-local index directory.
#[must_use]
pub fn index_dir(root: &Path) -> PathBuf {
    root.join(INDEX_DIR)
}

/// Input accumulated during extraction.
struct IndexInput {
    /// Per-file extractions.
    extractions: Vec<Extraction>,
    /// Unsupported known source files.
    skipped_unsupported_files: usize,
}

/// Extracts all supported source files under a root.
fn extract_project(root: &Path) -> Result<IndexInput> {
    let mut extractions = Vec::new();
    let mut skipped_unsupported_files = 0_usize;

    for entry in WalkBuilder::new(root)
        .standard_filters(true)
        .hidden(false)
        .build()
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let path = entry.path();
        if !entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
        {
            continue;
        }
        if should_skip_path(root, path) {
            continue;
        }

        if is_rust_file(path) {
            let source = std::fs::read(path).map_err(|source| Error::fs(path, source))?;
            let relative_path = normalize_relative_path(root, path);
            let tree = parse_rust(path, &source)?;
            extractions.push(extract_rust(&relative_path, &source, &tree));
        } else if is_recognized_unsupported_source(path) {
            skipped_unsupported_files = skipped_unsupported_files.saturating_add(1);
        }
    }

    extractions.sort_by(|left, right| left.file.path.cmp(&right.file.path));
    Ok(IndexInput {
        extractions,
        skipped_unsupported_files,
    })
}

/// Resolves all file extractions into symbolic graph data.
pub fn resolve_extractions(extractions: Vec<Extraction>) -> Result<ResolvedIndex> {
    let mut files = Vec::with_capacity(extractions.len());
    let mut symbols = Vec::new();
    let mut symbolic_edges = Vec::new();
    let mut references = Vec::new();

    for extraction in extractions {
        files.push(extraction.file);
        symbols.extend(extraction.nodes);
        symbolic_edges.extend(extraction.edges);
        references.extend(extraction.references);
    }

    files.sort_by(|left, right| left.path.cmp(&right.path));
    symbols.sort_by(|left, right| left.stable_key.cmp(&right.stable_key));
    dedupe_symbols(&mut symbols);

    let stable_keys = symbols
        .iter()
        .map(|symbol| symbol.stable_key.clone())
        .collect::<BTreeSet<_>>();
    let (qualified, simple) = build_name_maps(&symbols);

    let mut edge_set = BTreeSet::new();
    for edge in symbolic_edges {
        if stable_keys.contains(&edge.source_key) && stable_keys.contains(&edge.target_key) {
            edge_set.insert(ResolvedEdge {
                source_key: edge.source_key,
                target_key: edge.target_key,
                kind: edge.kind,
                reference: None,
            });
        }
    }

    let mut unresolved = Vec::new();
    for reference in references {
        if !stable_keys.contains(&reference.source_key) {
            continue;
        }
        match resolve_target(&reference.target.normalized, &qualified, &simple) {
            ResolveTarget::Resolved(target_key) => {
                if reference.source_key != target_key {
                    edge_set.insert(ResolvedEdge {
                        source_key: reference.source_key,
                        target_key,
                        kind: reference.kind,
                        reference: Some(ReferenceSite {
                            file_path: reference.file_path,
                            span: reference.span,
                            text: reference.text,
                        }),
                    });
                }
            }
            ResolveTarget::Unresolved(reason) => {
                let mut unresolved_reference = reference;
                unresolved_reference.reason = Some(reason);
                unresolved.push(unresolved_reference);
            }
        }
    }

    Ok(ResolvedIndex {
        files,
        nodes: symbols,
        edges: edge_set.into_iter().collect(),
        unresolved,
    })
}

/// Resolution outcome for one unresolved reference.
enum ResolveTarget {
    /// Resolved to a unique stable key.
    Resolved(String),
    /// Could not resolve, with reason.
    Unresolved(String),
}

/// Removes duplicated stable keys, keeping the first deterministic entry.
fn dedupe_symbols(symbols: &mut Vec<SymbolNode>) {
    let mut seen = BTreeSet::new();
    symbols.retain(|symbol| seen.insert(symbol.stable_key.clone()));
}

/// Builds exact and simple-name indexes for the resolver.
fn build_name_maps(
    nodes: &[SymbolNode],
) -> (BTreeMap<String, Vec<String>>, BTreeMap<String, Vec<String>>) {
    let mut qualified = BTreeMap::<String, Vec<String>>::new();
    let mut simple = BTreeMap::<String, Vec<String>>::new();
    for node in nodes {
        qualified
            .entry(node.qualified_name.clone())
            .or_default()
            .push(node.stable_key.clone());
        simple
            .entry(node.name.clone())
            .or_default()
            .push(node.stable_key.clone());
    }
    (qualified, simple)
}

/// Resolves one reference target against exact names, then unique simple names.
fn resolve_target(
    target: &str,
    qualified: &BTreeMap<String, Vec<String>>,
    simple: &BTreeMap<String, Vec<String>>,
) -> ResolveTarget {
    let normalized = target.trim_start_matches("crate::");
    if let Some(keys) = qualified.get(normalized) {
        return unique_or_ambiguous(target, keys);
    }
    let last = normalized
        .rsplit("::")
        .next()
        .unwrap_or(normalized)
        .trim_start_matches("Self::");
    if let Some(keys) = simple.get(last) {
        return unique_or_ambiguous(target, keys);
    }
    ResolveTarget::Unresolved("no matching symbol".to_string())
}

/// Converts a candidate list into a resolved key or ambiguity reason.
fn unique_or_ambiguous(target: &str, keys: &[String]) -> ResolveTarget {
    match keys {
        [key] => ResolveTarget::Resolved(key.clone()),
        [] => ResolveTarget::Unresolved("no matching symbol".to_string()),
        _ => ResolveTarget::Unresolved(format!("{target} matched {} symbols", keys.len())),
    }
}

/// Rebuilds the native OxGraph database for one resolved index.
fn rebuild_database(root: &Path, index: &ResolvedIndex) -> Result<PathBuf> {
    let index_directory = index_dir(root);
    let database_directory = database_dir(root);
    if database_directory.exists() {
        std::fs::remove_dir_all(&database_directory)
            .map_err(|source| Error::fs(&database_directory, source))?;
    }
    std::fs::create_dir_all(&index_directory)
        .map_err(|source| Error::fs(&index_directory, source))?;
    remove_legacy_outputs(&index_directory)?;

    let mut database = Database::create(&database_directory)?;
    let mut writer = database.begin_write()?;

    let source_role = writer.register_role("source")?;
    let target_role = writer.register_role("target")?;
    let labels = register_labels(&mut writer)?;
    let unresolved_label = writer.register_label("unresolved_reference")?;
    let relation_types = register_relation_types(&mut writer)?;
    let element_properties = register_element_properties(&mut writer)?;
    let relation_properties = register_relation_properties(&mut writer)?;
    define_property_indexes(&mut writer, &element_properties)?;
    writer.define_projection(ProjectionDefinition::Graph(GraphProjectionDefinition {
        name: "calls".to_owned(),
        relation_types: BTreeSet::from([relation_types[&EdgeKind::Calls]]),
        source_role,
        target_role,
    }))?;

    let files = index
        .files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect::<BTreeMap<_, _>>();
    let mut elements = BTreeMap::new();
    for node in &index.nodes {
        let element = writer.create_element()?;
        writer.add_element_label(element, labels[&node.kind])?;
        set_symbol_properties(
            &mut writer,
            element,
            &element_properties,
            node,
            files.get(node.file_path.as_str()).copied(),
        )?;
        elements.insert(node.stable_key.clone(), element);
    }

    for unresolved in &index.unresolved {
        let element = writer.create_element()?;
        writer.add_element_label(element, unresolved_label)?;
        set_unresolved_properties(&mut writer, element, &element_properties, unresolved)?;
    }

    for edge in &index.edges {
        let Some(&source) = elements.get(&edge.source_key) else {
            continue;
        };
        let Some(&target) = elements.get(&edge.target_key) else {
            continue;
        };
        let relation = writer.create_relation()?;
        writer.set_relation_type(relation, relation_types[&edge.kind])?;
        set_relation_properties(&mut writer, relation, &relation_properties, edge)?;
        writer.create_incidence(relation, source, source_role)?;
        writer.create_incidence(relation, target, target_role)?;
    }

    writer.commit()?;
    database.validate()?;
    Ok(database_directory)
}

/// Removes storage artifacts from the pre-OxGraph-native prototype.
fn remove_legacy_outputs(index_directory: &Path) -> Result<()> {
    for file_name in ["index.sqlite", "forward.oxgsnap", "reverse.oxgsnap"] {
        let path = index_directory.join(file_name);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|source| Error::fs(&path, source))?;
        }
    }
    Ok(())
}

/// Registers code symbol and diagnostic labels.
fn register_labels(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
) -> Result<BTreeMap<NodeKind, LabelId>> {
    let mut labels = BTreeMap::new();
    for kind in NodeKind::ALL {
        labels.insert(kind, writer.register_label(kind.as_str())?);
    }
    Ok(labels)
}

/// Registers code edge relation types.
fn register_relation_types(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
) -> Result<BTreeMap<EdgeKind, RelationTypeId>> {
    let mut relation_types = BTreeMap::new();
    for kind in EdgeKind::ALL {
        relation_types.insert(kind, writer.register_relation_type(kind.as_str())?);
    }
    Ok(relation_types)
}

/// Registers element property keys.
fn register_element_properties(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
) -> Result<BTreeMap<&'static str, PropertyKeyId>> {
    let definitions = [
        ("stable_key", PropertyType::Text),
        ("name", PropertyType::Text),
        ("qualified_name", PropertyType::Text),
        ("kind", PropertyType::Text),
        ("raw_kind", PropertyType::Text),
        ("language", PropertyType::Text),
        ("file_path", PropertyType::Text),
        ("path", PropertyType::Text),
        ("hash", PropertyType::Text),
        ("byte_len", PropertyType::Integer),
        ("start_byte", PropertyType::Integer),
        ("end_byte", PropertyType::Integer),
        ("start_line", PropertyType::Integer),
        ("start_column", PropertyType::Integer),
        ("end_line", PropertyType::Integer),
        ("end_column", PropertyType::Integer),
        ("unresolved_source_key", PropertyType::Text),
        ("target_raw", PropertyType::Text),
        ("target_normalized", PropertyType::Text),
        ("target_qualifier", PropertyType::Text),
        ("target_kind_hint", PropertyType::Text),
        ("unresolved_edge_kind", PropertyType::Text),
        ("reason", PropertyType::Text),
    ];
    let mut properties = BTreeMap::new();
    for (name, value_type) in definitions {
        let key = writer.register_property_key(name, PropertyFamily::Element, value_type)?;
        properties.insert(name, key);
    }
    Ok(properties)
}

/// Registers relation property keys.
fn register_relation_properties(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
) -> Result<BTreeMap<&'static str, PropertyKeyId>> {
    let definitions = [
        ("edge_kind", PropertyType::Text),
        ("source_key", PropertyType::Text),
        ("target_key", PropertyType::Text),
        ("call_file_path", PropertyType::Text),
        ("call_start_line", PropertyType::Integer),
        ("call_start_column", PropertyType::Integer),
        ("call_end_line", PropertyType::Integer),
        ("call_end_column", PropertyType::Integer),
        ("call_start_byte", PropertyType::Integer),
        ("call_end_byte", PropertyType::Integer),
        ("call_text", PropertyType::Text),
    ];
    let mut properties = BTreeMap::new();
    for (name, value_type) in definitions {
        let key = writer.register_property_key(name, PropertyFamily::Relation, value_type)?;
        properties.insert(name, key);
    }
    Ok(properties)
}

/// Defines equality indexes for common element query keys.
fn define_property_indexes(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
    properties: &BTreeMap<&'static str, PropertyKeyId>,
) -> Result<()> {
    for name in [
        "stable_key",
        "name",
        "qualified_name",
        "kind",
        "file_path",
        "language",
    ] {
        writer.define_index(
            format!("element_{name}_eq"),
            IndexDefinition::PropertyEquality {
                key: properties[name],
            },
        )?;
    }
    Ok(())
}

/// Writes symbol properties to one element.
fn set_symbol_properties(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
    element: ElementId,
    properties: &BTreeMap<&'static str, PropertyKeyId>,
    node: &SymbolNode,
    file: Option<&SourceUnit>,
) -> Result<()> {
    set_text(writer, element, properties, "stable_key", &node.stable_key)?;
    set_text(writer, element, properties, "name", &node.name)?;
    set_text(
        writer,
        element,
        properties,
        "qualified_name",
        &node.qualified_name,
    )?;
    set_text(writer, element, properties, "kind", node.kind.as_str())?;
    if let Some(raw_kind) = &node.raw_kind {
        set_text(writer, element, properties, "raw_kind", raw_kind)?;
    }
    set_text(
        writer,
        element,
        properties,
        "language",
        node.language.as_str(),
    )?;
    set_text(writer, element, properties, "file_path", &node.file_path)?;
    set_span_properties(writer, element, properties, node.span)?;

    if node.kind == NodeKind::File
        && let Some(file) = file
    {
        set_text(writer, element, properties, "path", &file.path)?;
        set_text(writer, element, properties, "hash", &file.hash)?;
        set_usize(writer, element, properties, "byte_len", file.byte_len)?;
    }
    Ok(())
}

/// Writes unresolved-reference diagnostic properties.
fn set_unresolved_properties(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
    element: ElementId,
    properties: &BTreeMap<&'static str, PropertyKeyId>,
    unresolved: &UnresolvedReference,
) -> Result<()> {
    let stable_key = format!(
        "unresolved:{}:{}:{}:{}",
        unresolved.source_key,
        unresolved.kind.as_str(),
        unresolved.target.normalized,
        unresolved.span.start_byte
    );
    set_text(writer, element, properties, "stable_key", &stable_key)?;
    set_text(writer, element, properties, "name", &unresolved.target.raw)?;
    set_text(
        writer,
        element,
        properties,
        "qualified_name",
        &unresolved.target.normalized,
    )?;
    set_text(writer, element, properties, "kind", "unresolved_reference")?;
    set_text(
        writer,
        element,
        properties,
        "file_path",
        &unresolved.file_path,
    )?;
    set_text(
        writer,
        element,
        properties,
        "unresolved_source_key",
        &unresolved.source_key,
    )?;
    set_text(
        writer,
        element,
        properties,
        "target_raw",
        &unresolved.target.raw,
    )?;
    set_text(
        writer,
        element,
        properties,
        "target_normalized",
        &unresolved.target.normalized,
    )?;
    if let Some(qualifier) = &unresolved.target.qualifier {
        set_text(writer, element, properties, "target_qualifier", qualifier)?;
    }
    if let Some(kind_hint) = &unresolved.target.kind_hint {
        set_text(writer, element, properties, "target_kind_hint", kind_hint)?;
    }
    set_text(
        writer,
        element,
        properties,
        "unresolved_edge_kind",
        unresolved.kind.as_str(),
    )?;
    if let Some(reason) = &unresolved.reason {
        set_text(writer, element, properties, "reason", reason)?;
    }
    set_span_properties(writer, element, properties, unresolved.span)
}

/// Writes relation properties to one code edge.
fn set_relation_properties(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
    relation: RelationId,
    properties: &BTreeMap<&'static str, PropertyKeyId>,
    edge: &ResolvedEdge,
) -> Result<()> {
    set_relation_text(
        writer,
        relation,
        properties,
        "edge_kind",
        edge.kind.as_str(),
    )?;
    set_relation_text(writer, relation, properties, "source_key", &edge.source_key)?;
    set_relation_text(writer, relation, properties, "target_key", &edge.target_key)?;

    if edge.kind == EdgeKind::Calls
        && let Some(reference) = &edge.reference
    {
        set_relation_text(
            writer,
            relation,
            properties,
            "call_file_path",
            &reference.file_path,
        )?;
        set_relation_usize(
            writer,
            relation,
            properties,
            "call_start_line",
            reference.span.start_line,
        )?;
        set_relation_usize(
            writer,
            relation,
            properties,
            "call_start_column",
            reference.span.start_column,
        )?;
        set_relation_usize(
            writer,
            relation,
            properties,
            "call_end_line",
            reference.span.end_line,
        )?;
        set_relation_usize(
            writer,
            relation,
            properties,
            "call_end_column",
            reference.span.end_column,
        )?;
        set_relation_usize(
            writer,
            relation,
            properties,
            "call_start_byte",
            reference.span.start_byte,
        )?;
        set_relation_usize(
            writer,
            relation,
            properties,
            "call_end_byte",
            reference.span.end_byte,
        )?;
        set_relation_text(writer, relation, properties, "call_text", &reference.text)?;
    }
    Ok(())
}

/// Writes source span properties.
fn set_span_properties(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
    element: ElementId,
    properties: &BTreeMap<&'static str, PropertyKeyId>,
    span: SourceSpan,
) -> Result<()> {
    set_usize(writer, element, properties, "start_byte", span.start_byte)?;
    set_usize(writer, element, properties, "end_byte", span.end_byte)?;
    set_usize(writer, element, properties, "start_line", span.start_line)?;
    set_usize(
        writer,
        element,
        properties,
        "start_column",
        span.start_column,
    )?;
    set_usize(writer, element, properties, "end_line", span.end_line)?;
    set_usize(writer, element, properties, "end_column", span.end_column)
}

/// Sets a text property on a relation.
fn set_relation_text(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
    relation: RelationId,
    properties: &BTreeMap<&'static str, PropertyKeyId>,
    key: &'static str,
    value: &str,
) -> Result<()> {
    writer.set_property(
        PropertySubject::Relation(relation),
        properties[key],
        PropertyValue::Text(value.to_string()),
    )?;
    Ok(())
}

/// Sets a usize property on a relation as an OxGraph integer.
fn set_relation_usize(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
    relation: RelationId,
    properties: &BTreeMap<&'static str, PropertyKeyId>,
    key: &'static str,
    value: usize,
) -> Result<()> {
    let value = i64::try_from(value).map_err(|_| Error::IntegerOverflow { value })?;
    writer.set_property(
        PropertySubject::Relation(relation),
        properties[key],
        PropertyValue::Integer(value),
    )?;
    Ok(())
}

/// Sets a text property on an element.
fn set_text(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
    element: ElementId,
    properties: &BTreeMap<&'static str, PropertyKeyId>,
    key: &'static str,
    value: &str,
) -> Result<()> {
    writer.set_property(
        PropertySubject::Element(element),
        properties[key],
        PropertyValue::Text(value.to_string()),
    )?;
    Ok(())
}

/// Sets a usize property on an element as an OxGraph integer.
fn set_usize(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
    element: ElementId,
    properties: &BTreeMap<&'static str, PropertyKeyId>,
    key: &'static str,
    value: usize,
) -> Result<()> {
    let value = i64::try_from(value).map_err(|_| Error::IntegerOverflow { value })?;
    writer.set_property(
        PropertySubject::Element(element),
        properties[key],
        PropertyValue::Integer(value),
    )?;
    Ok(())
}

/// Counts rows from one OxGraph query.
fn count_query(
    database: &Database,
    read: &oxgraph::db::ReadTransaction,
    query: &str,
) -> Result<usize> {
    let prepared = database.prepare(QueryLanguage::Oxql, query)?;
    Ok(read.execute(&prepared)?.rows().len())
}

/// Opens an existing project database.
fn open_database(root: &Path) -> Result<Database> {
    let root = canonical_root(root)?;
    Ok(Database::open(database_dir(&root))?)
}

/// Canonicalizes a root path.
fn canonical_root(root: &Path) -> Result<PathBuf> {
    std::fs::canonicalize(root).map_err(|source| Error::fs(root, source))
}

/// Returns a stable forward-slash relative path.
#[must_use]
fn normalize_relative_path(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    normalize_path(relative)
}

/// Returns a stable forward-slash path.
#[must_use]
fn normalize_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Skips generated, dependency, VCS, and index storage paths.
fn should_skip_path(root: &Path, path: &Path) -> bool {
    let relative = path.strip_prefix(root).unwrap_or(path);
    relative.components().any(|component| {
        let part = component.as_os_str().to_string_lossy();
        matches!(
            part.as_ref(),
            ".git" | ".oxcode" | "target" | "node_modules" | "vendor"
        )
    })
}

/// Returns whether a path is owned by the Rust extractor.
fn is_rust_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("rs")
    )
}

/// Returns whether a file extension is known source text but lacks an extractor.
fn is_recognized_unsupported_source(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("ts" | "tsx" | "js" | "jsx" | "py" | "go" | "java" | "c" | "h" | "cpp")
    )
}

/// Tree-sitter parsed tree wrapper.
#[derive(Clone)]
struct ParsedTree {
    /// Parsed tree.
    tree: Tree,
}

impl ParsedTree {
    /// Returns the root syntax node.
    fn root_node(&self) -> Node {
        self.tree.root_node()
    }
}

/// Parses Rust source.
fn parse_rust(path: &Path, source: &[u8]) -> Result<ParsedTree> {
    let mut parser =
        tree_sitter_language_pack::get_parser("rust").map_err(|error| Error::Parse {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    let tree = parser.parse_bytes(source).ok_or_else(|| Error::Parse {
        path: path.to_path_buf(),
        message: "tree-sitter returned no parse tree".to_string(),
    })?;
    Ok(ParsedTree { tree })
}

/// Extracts code graph nodes and references from one Rust source file.
fn extract_rust(relative_path: &str, source: &[u8], tree: &ParsedTree) -> Extraction {
    let relative = relative_path.to_string();
    let module_scope = module_scope_for_path(&relative);
    let file_key = format!("file:{relative}");
    let language = rust_language();

    let file_node = SymbolNode {
        stable_key: file_key.clone(),
        name: relative.clone(),
        qualified_name: module_scope
            .as_ref()
            .map_or_else(|| "crate".to_string(), |scope| scope.join("::")),
        kind: NodeKind::File,
        raw_kind: Some("source_file".to_string()),
        language: language.clone(),
        file_path: relative.clone(),
        span: file_span(source),
    };

    let root_node = tree.root_node();
    let mut extractor = RustWalker {
        source,
        file_path: relative.clone(),
        language,
        nodes: vec![file_node],
        edges: Vec::new(),
        references: Vec::new(),
    };

    let scope = module_scope.unwrap_or_default();
    extractor.visit_children(root_node, &file_key, &file_key, &scope, None);

    Extraction {
        file: source_unit(&relative, rust_language(), source),
        nodes: extractor.nodes,
        edges: extractor.edges,
        references: extractor.references,
    }
}

/// Stateful Rust CST walker.
struct RustWalker<'source> {
    /// Source bytes.
    source: &'source [u8],
    /// Repository-relative path.
    file_path: String,
    /// Extractor language.
    language: LanguageId,
    /// Extracted nodes.
    nodes: Vec<SymbolNode>,
    /// Resolved syntactic edges.
    edges: Vec<SymbolEdge>,
    /// References that require name resolution.
    references: Vec<UnresolvedReference>,
}

impl RustWalker<'_> {
    /// Visits all named children under `node`.
    fn visit_children(
        &mut self,
        node: Node,
        parent_key: &str,
        owner_key: &str,
        scope: &[String],
        impl_target: Option<&str>,
    ) {
        for index in 0..node.named_child_count() {
            if let Some(child) = node.named_child(u32::try_from(index).unwrap_or(u32::MAX)) {
                self.visit_node(child, parent_key, owner_key, scope, impl_target);
            }
        }
    }

    /// Visits one CST node and emits graph data when it represents code intent.
    fn visit_node(
        &mut self,
        node: Node,
        parent_key: &str,
        owner_key: &str,
        scope: &[String],
        impl_target: Option<&str>,
    ) {
        match node.kind().as_str() {
            "mod_item" => {
                if let Some(name) = item_name(&node, self.source) {
                    let qualified = qualify(scope, &name);
                    let symbol =
                        self.push_symbol(&node, NodeKind::Module, "mod_item", &name, &qualified);
                    self.push_edge(parent_key, &symbol.stable_key, EdgeKind::Contains);
                    let mut child_scope = scope.to_vec();
                    child_scope.push(name);
                    let key = symbol.stable_key;
                    self.visit_children(node, &key, &key, &child_scope, None);
                }
            }
            "struct_item" => self.visit_named_item(
                node,
                parent_key,
                owner_key,
                scope,
                NodeKind::Struct,
                "struct_item",
            ),
            "enum_item" => self.visit_named_item(
                node,
                parent_key,
                owner_key,
                scope,
                NodeKind::Enum,
                "enum_item",
            ),
            "trait_item" => {
                if let Some(name) = item_name(&node, self.source) {
                    let qualified = qualify(scope, &name);
                    let symbol =
                        self.push_symbol(&node, NodeKind::Trait, "trait_item", &name, &qualified);
                    self.push_edge(parent_key, &symbol.stable_key, EdgeKind::Contains);
                    let mut trait_scope = scope.to_vec();
                    trait_scope.push(name);
                    let key = symbol.stable_key;
                    self.visit_children(node, &key, &key, &trait_scope, None);
                }
            }
            "impl_item" => self.visit_impl(node, parent_key, owner_key, scope),
            "function_item" => self.visit_function(node, parent_key, scope, impl_target),
            "const_item" => self.visit_named_item(
                node,
                parent_key,
                owner_key,
                scope,
                NodeKind::Constant,
                "const_item",
            ),
            "type_item" => self.visit_named_item(
                node,
                parent_key,
                owner_key,
                scope,
                NodeKind::TypeAlias,
                "type_item",
            ),
            "macro_definition" => self.visit_named_item(
                node,
                parent_key,
                owner_key,
                scope,
                NodeKind::Macro,
                "macro_definition",
            ),
            "use_declaration" => {
                for target in import_targets(&node_text(&node, self.source)) {
                    self.push_reference(&node, owner_key, target, EdgeKind::Imports);
                }
                self.visit_children(node, parent_key, owner_key, scope, impl_target);
            }
            "call_expression" => {
                if let Some(target) = call_target(&node, self.source) {
                    self.push_reference(&node, owner_key, target, EdgeKind::Calls);
                }
                self.visit_children(node, parent_key, owner_key, scope, impl_target);
            }
            "method_call_expression" => {
                if let Some(target) = method_call_target(&node, self.source) {
                    self.push_reference(&node, owner_key, target, EdgeKind::Calls);
                }
                self.visit_children(node, parent_key, owner_key, scope, impl_target);
            }
            "macro_invocation" => {
                if let Some(target) = item_name(&node, self.source) {
                    self.push_reference(&node, owner_key, target, EdgeKind::Calls);
                }
                self.visit_children(node, parent_key, owner_key, scope, impl_target);
            }
            _ => self.visit_children(node, parent_key, owner_key, scope, impl_target),
        }
    }

    /// Emits a named item and keeps traversing with the current owner.
    fn visit_named_item(
        &mut self,
        node: Node,
        parent_key: &str,
        owner_key: &str,
        scope: &[String],
        kind: NodeKind,
        raw_kind: &str,
    ) {
        if let Some(name) = item_name(&node, self.source) {
            let qualified = qualify(scope, &name);
            let symbol = self.push_symbol(&node, kind, raw_kind, &name, &qualified);
            self.push_edge(parent_key, &symbol.stable_key, EdgeKind::Contains);
            let key = symbol.stable_key;
            self.visit_children(node, &key, owner_key, scope, None);
        }
    }

    /// Emits an implementation block and traverses methods inside it.
    fn visit_impl(&mut self, node: Node, parent_key: &str, owner_key: &str, scope: &[String]) {
        let target = impl_target(&node, self.source).unwrap_or_else(|| "impl".to_string());
        let name = format!("impl {target}");
        let qualified = qualify(scope, &name);
        let symbol = self.push_symbol(&node, NodeKind::ImplBlock, "impl_item", &name, &qualified);
        self.push_edge(parent_key, &symbol.stable_key, EdgeKind::Contains);

        if let Some(trait_name) = impl_trait(&node, self.source) {
            self.push_reference(&node, &symbol.stable_key, trait_name, EdgeKind::Implements);
        }

        let key = symbol.stable_key;
        self.visit_children(node, &key, owner_key, scope, Some(&target));
    }

    /// Emits a free function or method and makes it the owner for nested calls.
    fn visit_function(
        &mut self,
        node: Node,
        parent_key: &str,
        scope: &[String],
        impl_target: Option<&str>,
    ) {
        if let Some(name) = item_name(&node, self.source) {
            let kind = if impl_target.is_some() {
                NodeKind::Method
            } else {
                NodeKind::Function
            };
            let qualified = impl_target.map_or_else(
                || qualify(scope, &name),
                |target| qualify_with_extra(scope, &[target, &name]),
            );
            let symbol = self.push_symbol(&node, kind, "function_item", &name, &qualified);
            self.push_edge(parent_key, &symbol.stable_key, EdgeKind::Contains);
            let key = symbol.stable_key;
            self.visit_children(node, &key, &key, scope, impl_target);
        }
    }

    /// Pushes one symbol and returns a clone for immediate edge wiring.
    fn push_symbol(
        &mut self,
        node: &Node,
        kind: NodeKind,
        raw_kind: &str,
        name: &str,
        qualified_name: &str,
    ) -> SymbolNode {
        let span = span(node);
        let stable_key = format!(
            "symbol:{}:{}:{}:{}",
            self.file_path,
            kind.as_str(),
            qualified_name,
            span.start_byte
        );
        let symbol = SymbolNode {
            stable_key,
            name: name.to_string(),
            qualified_name: qualified_name.to_string(),
            kind,
            raw_kind: Some(raw_kind.to_string()),
            language: self.language.clone(),
            file_path: self.file_path.clone(),
            span,
        };
        self.nodes.push(symbol.clone());
        symbol
    }

    /// Pushes one already-resolved edge.
    fn push_edge(&mut self, source_key: &str, target_key: &str, kind: EdgeKind) {
        self.edges.push(SymbolEdge {
            source_key: source_key.to_string(),
            target_key: target_key.to_string(),
            kind,
        });
    }

    /// Pushes one unresolved reference.
    fn push_reference(&mut self, node: &Node, source_key: &str, target: String, kind: EdgeKind) {
        if target.is_empty() {
            return;
        }
        let text = compact_source_text(&node_text(node, self.source));
        self.references.push(UnresolvedReference {
            source_key: source_key.to_string(),
            target: ReferenceTarget::new(target),
            kind,
            file_path: self.file_path.clone(),
            span: span(node),
            text,
            reason: None,
        });
    }
}

/// Returns a node name from its `name` field or first identifier-like child.
fn item_name(node: &Node, source: &[u8]) -> Option<String> {
    node.child_by_field_name("name")
        .map(|child| clean_identifier(&node_text(&child, source)))
        .filter(|text| !text.is_empty())
        .or_else(|| {
            for index in 0..node.named_child_count() {
                let Some(child) = node.named_child(u32::try_from(index).unwrap_or(u32::MAX)) else {
                    continue;
                };
                if matches!(
                    child.kind().as_str(),
                    "identifier" | "type_identifier" | "field_identifier"
                ) {
                    let text = clean_identifier(&node_text(&child, source));
                    if !text.is_empty() {
                        return Some(text);
                    }
                }
            }
            None
        })
}

/// Returns a normalized call target for a `call_expression`.
fn call_target(node: &Node, source: &[u8]) -> Option<String> {
    node.child_by_field_name("function")
        .or_else(|| node.named_child(0))
        .map(|child| clean_reference(&node_text(&child, source)))
        .filter(|text| !text.is_empty())
}

/// Returns a normalized target for a `method_call_expression`.
fn method_call_target(node: &Node, source: &[u8]) -> Option<String> {
    node.child_by_field_name("name")
        .map(|child| clean_reference(&node_text(&child, source)))
        .filter(|text| !text.is_empty())
}

/// Extracts the target type from a Rust impl header.
fn impl_target(node: &Node, source: &[u8]) -> Option<String> {
    let header = impl_header(node, source);
    let after_for = header
        .rsplit_once(" for ")
        .map_or(header.as_str(), |(_, tail)| tail);
    let cleaned = after_for
        .trim_start_matches("impl")
        .trim()
        .trim_end_matches('{')
        .trim();
    let without_generics = cleaned
        .split('<')
        .next()
        .unwrap_or(cleaned)
        .trim()
        .trim_start_matches('&')
        .trim();
    (!without_generics.is_empty()).then(|| clean_reference(without_generics))
}

/// Extracts the implemented trait name when an impl header contains `for`.
fn impl_trait(node: &Node, source: &[u8]) -> Option<String> {
    let header = impl_header(node, source);
    header
        .rsplit_once(" for ")
        .map(|(head, _)| head.trim_start_matches("impl").trim())
        .map(clean_reference)
        .filter(|text| !text.is_empty())
}

/// Returns an impl header without its body.
fn impl_header(node: &Node, source: &[u8]) -> String {
    node_text(node, source)
        .split('{')
        .next()
        .unwrap_or_default()
        .replace('\n', " ")
}

/// Extracts simple import targets from a Rust `use` declaration.
fn import_targets(text: &str) -> Vec<String> {
    let body = text
        .trim()
        .trim_start_matches("pub")
        .trim()
        .trim_start_matches("use")
        .trim()
        .trim_end_matches(';')
        .trim();
    body.split([',', '{', '}'])
        .filter_map(|part| {
            let mut names = part
                .split("::")
                .filter_map(|segment| {
                    let clean = segment
                        .split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .trim_matches(['(', ')']);
                    match clean {
                        "" | "self" | "super" | "crate" | "as" | "*" => None,
                        other => Some(other),
                    }
                })
                .collect::<Vec<_>>();
            let name = names.pop().map(clean_reference)?;
            (!name.is_empty()).then_some(name)
        })
        .collect()
}

/// Joins a module scope with one item name.
fn qualify(scope: &[String], name: &str) -> String {
    qualify_with_extra(scope, &[name])
}

/// Joins a module scope with extra path components.
fn qualify_with_extra(scope: &[String], extra: &[&str]) -> String {
    scope
        .iter()
        .map(String::as_str)
        .chain(extra.iter().copied())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("::")
}

/// Cleans identifier text.
fn clean_identifier(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("r#")
        .trim_end_matches('!')
        .to_string()
}

/// Cleans reference text into a resolver-friendly spelling.
fn clean_reference(value: &str) -> String {
    value
        .split("::<")
        .next()
        .unwrap_or(value)
        .replace(char::is_whitespace, "")
        .trim_start_matches("r#")
        .trim_end_matches('!')
        .trim_matches(';')
        .to_string()
}

/// Collapses source text to one readable line for agent-facing context.
fn compact_source_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Derives a Rust module scope from a repository-relative path.
fn module_scope_for_path(relative: &str) -> Option<Vec<String>> {
    let path = PathBuf::from(relative);
    let mut parts = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    if parts.first().is_some_and(|part| part == "src") {
        parts.remove(0);
    }
    if parts.is_empty() {
        return None;
    }
    let file = parts.pop()?;
    match file.as_str() {
        "lib.rs" | "main.rs" => {}
        "mod.rs" => {}
        other => parts.push(other.trim_end_matches(".rs").to_string()),
    }
    (!parts.is_empty()).then_some(parts)
}

/// Returns the Rust language ID.
fn rust_language() -> LanguageId {
    LanguageId::from("rust")
}

/// Creates source unit metadata for one extracted file.
fn source_unit(relative_path: &str, language: LanguageId, source: &[u8]) -> SourceUnit {
    SourceUnit {
        path: relative_path.to_string(),
        language,
        hash: hex_hash(source),
        byte_len: source.len(),
    }
}

/// Returns a source span covering an entire source file.
fn file_span(source: &[u8]) -> SourceSpan {
    let source_text = str::from_utf8(source).unwrap_or_default();
    SourceSpan {
        start_byte: 0,
        end_byte: source.len(),
        start_line: 1,
        start_column: 0,
        end_line: source_text.lines().count().max(1),
        end_column: 0,
    }
}

/// Returns source text for a CST node.
fn node_text(node: &Node, source: &[u8]) -> String {
    let range = node.byte_range();
    source
        .get(range.start..range.end)
        .and_then(|bytes| str::from_utf8(bytes).ok())
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Converts a CST node span to the storage representation.
fn span(node: &Node) -> SourceSpan {
    let start = node.start_position();
    let end = node.end_position();
    SourceSpan {
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
        start_line: start.row + 1,
        start_column: start.column,
        end_line: end.row + 1,
        end_column: end.column,
    }
}

/// Returns a lowercase hex SHA-256 digest.
fn hex_hash(source: &[u8]) -> String {
    let digest = Sha256::digest(source);
    digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_targets_handles_groups() {
        assert_eq!(
            import_targets("use crate::{alpha, beta::Gamma};"),
            vec!["alpha", "Gamma"]
        );
    }

    #[test]
    fn module_scope_skips_crate_roots() {
        assert_eq!(module_scope_for_path("src/lib.rs"), None);
        assert_eq!(
            module_scope_for_path("src/graph/mod.rs"),
            Some(vec!["graph".to_string()])
        );
        assert_eq!(
            module_scope_for_path("src/graph/query.rs"),
            Some(vec!["graph".to_string(), "query".to_string()])
        );
    }

    #[test]
    fn resolver_turns_simple_call_into_edge() {
        let source = SourceUnit {
            path: "src/lib.rs".to_string(),
            language: LanguageId::from("rust"),
            hash: "hash".to_string(),
            byte_len: 1,
        };
        let caller = symbol("caller", "caller", 0);
        let callee = symbol("callee", "callee", 10);
        let caller_key = caller.stable_key.clone();
        let callee_key = callee.stable_key.clone();
        let resolved = resolve_extractions(vec![Extraction {
            file: source,
            nodes: vec![caller, callee],
            edges: Vec::new(),
            references: vec![UnresolvedReference {
                source_key: caller_key.clone(),
                target: ReferenceTarget::new("callee"),
                kind: EdgeKind::Calls,
                file_path: "src/lib.rs".to_string(),
                span: SourceSpan::default(),
                text: "callee()".to_string(),
                reason: None,
            }],
        }])
        .expect("resolve");
        let edge = resolved
            .edges
            .iter()
            .find(|edge| {
                edge.source_key == caller_key
                    && edge.target_key == callee_key
                    && edge.kind == EdgeKind::Calls
            })
            .expect("call edge");
        let reference = edge.reference.as_ref().expect("reference site");
        assert_eq!(reference.file_path, "src/lib.rs");
        assert_eq!(reference.text, "callee()");
    }

    fn symbol(name: &str, qualified_name: &str, start_byte: usize) -> SymbolNode {
        SymbolNode {
            stable_key: format!("symbol:src/lib.rs:function:{qualified_name}:{start_byte}"),
            name: name.to_string(),
            qualified_name: qualified_name.to_string(),
            kind: NodeKind::Function,
            raw_kind: Some("function_item".to_string()),
            language: LanguageId::from("rust"),
            file_path: "src/lib.rs".to_string(),
            span: SourceSpan {
                start_byte,
                ..SourceSpan::default()
            },
        }
    }
}

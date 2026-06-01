//! Storage-neutral model and report types for oxcode.

use std::{fmt, path::PathBuf};

use serde::{Deserialize, Serialize};

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

impl From<String> for LanguageId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl fmt::Display for LanguageId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

macro_rules! string_newtype {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Creates a new value.
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Returns the underlying string.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self::new(value)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self::new(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

string_newtype!(SymbolKey, "Stable symbol key.");
string_newtype!(QualifiedName, "Qualified language-level symbol name.");
string_newtype!(SourcePath, "Repository-relative source path.");

/// OxGraph element identifier for a symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SymbolId(pub u64);

impl SymbolId {
    /// Creates a symbol ID.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the raw OxGraph element ID.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for SymbolId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// Parsed selector accepted by agent navigation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Selector {
    /// Exact OxGraph element ID.
    Element(SymbolId),
    /// Exact qualified name.
    QualifiedName(QualifiedName),
    /// Exact simple symbol name.
    Name(String),
    /// Innermost symbol covering a source line.
    FileLine { path: SourcePath, line: usize },
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
    pub const ALL: [Self; 17] = [
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

    /// Parses a stable storage representation.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "file" => Some(Self::File),
            "module" => Some(Self::Module),
            "namespace" => Some(Self::Namespace),
            "package" => Some(Self::Package),
            "class" => Some(Self::Class),
            "struct" => Some(Self::Struct),
            "enum" => Some(Self::Enum),
            "trait" => Some(Self::Trait),
            "interface" => Some(Self::Interface),
            "impl_block" => Some(Self::ImplBlock),
            "function" => Some(Self::Function),
            "method" => Some(Self::Method),
            "field" => Some(Self::Field),
            "variable" => Some(Self::Variable),
            "constant" => Some(Self::Constant),
            "type_alias" => Some(Self::TypeAlias),
            "macro" => Some(Self::Macro),
            _ => None,
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
    pub const ALL: [Self; 6] = [
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
    pub kind: String,
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
    pub path: String,
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
    pub path: String,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newtypes_serialize_as_plain_values() {
        assert_eq!(
            serde_json::to_string(&SymbolKey::from("symbol:one")).expect("json"),
            "\"symbol:one\""
        );
        assert_eq!(
            serde_json::to_string(&SymbolId::new(42)).expect("json"),
            "42"
        );
    }
}

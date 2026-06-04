//! The graph schema catalog: one declaration of every property the store
//! writes and reads, plus the projection/role names. The storage layer derives
//! its registration, read-key cache, and index set from these tables so the
//! schema cannot drift across the write and read paths.

/// Storage value kind for a property (maps to an OxGraph property type).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyKind {
    /// UTF-8 text.
    Text,
    /// Signed integer.
    Integer,
}

/// Catalog name of the `calls` graph projection.
pub const CALLS_PROJECTION: &str = "calls";

/// Returns the catalog name of the graph projection for `edge_kind`. The
/// `calls` projection keeps its historical name; every other kind is
/// `edges_<kind>`, so navigation can traverse any code edge kind.
#[must_use]
pub fn projection_name(edge_kind: crate::EdgeKind) -> String {
    match edge_kind {
        crate::EdgeKind::Calls => CALLS_PROJECTION.to_owned(),
        other => format!("edges_{}", other.as_str()),
    }
}
/// Role name for the source endpoint of a relation.
pub const SOURCE_ROLE: &str = "source";
/// Role name for the target endpoint of a relation.
pub const TARGET_ROLE: &str = "target";

/// A property attached to graph elements (symbols and diagnostics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementProperty {
    /// Stable, deterministic symbol key.
    StableKey,
    /// Simple display name.
    Name,
    /// Qualified language-level name.
    QualifiedName,
    /// Stored node kind.
    Kind,
    /// Native syntax kind emitted by the extractor.
    RawKind,
    /// Extractor language.
    Language,
    /// Repository-relative file path.
    FilePath,
    /// Compact declaration or header.
    Signature,
    /// Documentation comments directly attached to the symbol.
    Docstring,
    /// Bounded source excerpt for agent-facing context.
    SourcePreview,
    /// Inclusive start byte.
    StartByte,
    /// Exclusive end byte.
    EndByte,
    /// One-based start line.
    StartLine,
    /// Zero-based start column.
    StartColumn,
    /// One-based end line.
    EndLine,
    /// Zero-based end column.
    EndColumn,
    /// Source key of an unresolved reference's origin symbol.
    UnresolvedSourceKey,
    /// Raw spelling of an unresolved reference target.
    TargetRaw,
    /// Normalized `::`-joined path of an unresolved reference target.
    TargetPath,
    /// Receiver/qualifier of an unresolved reference target.
    TargetQualifier,
    /// Language-neutral category of an unresolved reference target.
    TargetKindHint,
    /// Intended edge kind of an unresolved reference.
    UnresolvedEdgeKind,
    /// Reason a reference could not be resolved.
    Reason,
}

impl ElementProperty {
    /// Every element property, in registration order.
    pub const ALL: &'static [Self] = &[
        Self::StableKey,
        Self::Name,
        Self::QualifiedName,
        Self::Kind,
        Self::RawKind,
        Self::Language,
        Self::FilePath,
        Self::Signature,
        Self::Docstring,
        Self::SourcePreview,
        Self::StartByte,
        Self::EndByte,
        Self::StartLine,
        Self::StartColumn,
        Self::EndLine,
        Self::EndColumn,
        Self::UnresolvedSourceKey,
        Self::TargetRaw,
        Self::TargetPath,
        Self::TargetQualifier,
        Self::TargetKindHint,
        Self::UnresolvedEdgeKind,
        Self::Reason,
    ];

    /// Element properties that get an equality index for selector lookups.
    pub const INDEXED: &'static [Self] = &[
        Self::StableKey,
        Self::Name,
        Self::QualifiedName,
        Self::Kind,
        Self::Language,
        Self::FilePath,
    ];

    /// Returns the stable storage key.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::StableKey => "stable_key",
            Self::Name => "name",
            Self::QualifiedName => "qualified_name",
            Self::Kind => "kind",
            Self::RawKind => "raw_kind",
            Self::Language => "language",
            Self::FilePath => "file_path",
            Self::Signature => "signature",
            Self::Docstring => "docstring",
            Self::SourcePreview => "source_preview",
            Self::StartByte => "start_byte",
            Self::EndByte => "end_byte",
            Self::StartLine => "start_line",
            Self::StartColumn => "start_column",
            Self::EndLine => "end_line",
            Self::EndColumn => "end_column",
            Self::UnresolvedSourceKey => "unresolved_source_key",
            Self::TargetRaw => "target_raw",
            Self::TargetPath => "target_path",
            Self::TargetQualifier => "target_qualifier",
            Self::TargetKindHint => "target_kind_hint",
            Self::UnresolvedEdgeKind => "unresolved_edge_kind",
            Self::Reason => "reason",
        }
    }

    /// Returns the storage value kind.
    #[must_use]
    pub const fn kind(self) -> PropertyKind {
        match self {
            Self::StartByte
            | Self::EndByte
            | Self::StartLine
            | Self::StartColumn
            | Self::EndLine
            | Self::EndColumn => PropertyKind::Integer,
            _ => PropertyKind::Text,
        }
    }
}

/// A property attached to graph relations (edges).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationProperty {
    /// Deterministic, per-edge identity key used to resolve-or-mint the relation
    /// across reindexes (the relation analogue of [`ElementProperty::StableKey`]).
    EdgeStableKey,
    /// Edge kind spelling.
    EdgeKind,
    /// How the edge target was resolved.
    Resolution,
    /// Reference-site file path.
    SiteFilePath,
    /// Reference-site start byte.
    SiteStartByte,
    /// Reference-site end byte.
    SiteEndByte,
    /// Reference-site start line.
    SiteStartLine,
    /// Reference-site start column.
    SiteStartColumn,
    /// Reference-site end line.
    SiteEndLine,
    /// Reference-site end column.
    SiteEndColumn,
    /// Reference-site expression text.
    SiteText,
}

impl RelationProperty {
    /// Every relation property, in registration order.
    pub const ALL: &'static [Self] = &[
        Self::EdgeStableKey,
        Self::EdgeKind,
        Self::Resolution,
        Self::SiteFilePath,
        Self::SiteStartByte,
        Self::SiteEndByte,
        Self::SiteStartLine,
        Self::SiteStartColumn,
        Self::SiteEndLine,
        Self::SiteEndColumn,
        Self::SiteText,
    ];

    /// Returns the stable storage key.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::EdgeStableKey => "edge_stable_key",
            Self::EdgeKind => "edge_kind",
            Self::Resolution => "resolution",
            Self::SiteFilePath => "site_file_path",
            Self::SiteStartByte => "site_start_byte",
            Self::SiteEndByte => "site_end_byte",
            Self::SiteStartLine => "site_start_line",
            Self::SiteStartColumn => "site_start_column",
            Self::SiteEndLine => "site_end_line",
            Self::SiteEndColumn => "site_end_column",
            Self::SiteText => "site_text",
        }
    }

    /// Returns the storage value kind.
    #[must_use]
    pub const fn kind(self) -> PropertyKind {
        match self {
            Self::SiteStartByte
            | Self::SiteEndByte
            | Self::SiteStartLine
            | Self::SiteStartColumn
            | Self::SiteEndLine
            | Self::SiteEndColumn => PropertyKind::Integer,
            _ => PropertyKind::Text,
        }
    }
}

//! Index run statistics and project status report types.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::LanguageId;

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
    /// Number of files that failed to read or parse (recorded, not fatal).
    pub failed_files: usize,
    /// Number of files parsed with recoverable errors (partial symbols).
    pub partial_files: usize,
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

/// Language extractor availability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageSupport {
    /// Language name.
    pub language: LanguageId,
    /// Whether the parser backend can provide a parser.
    pub parser_available: bool,
    /// Whether oxcode has an explicit extractor.
    pub extractor_available: bool,
}

//! Data-only description of a generic, query-driven language.
//!
//! A [`LanguageProfile`] turns "support a language" into supplying a tree-sitter
//! query plus a capture→role table, rather than writing a CST walker. The
//! generic [`crate::extract::query_extractor::QueryExtractor`] interprets any
//! profile uniformly.

use oxcode_model::{EdgeKind, NodeKind, ReferenceKind};

use crate::extract::scope::ScopeKind;

/// How the generic extractor treats one query capture.
#[derive(Clone, Copy)]
pub(crate) enum CaptureRole {
    /// A symbol definition of the given kind; the captured node is its anchor.
    Definition(NodeKind),
    /// The identifier naming the enclosing definition or reference.
    Name,
    /// A reference (call/import/…) that becomes an unresolved edge.
    Reference {
        /// Edge kind to emit if the reference resolves.
        edge: EdgeKind,
        /// Language-neutral target category.
        hint: ReferenceKind,
    },
    /// The receiver/path qualifier of the enclosing reference.
    Qualifier,
}

/// Static, data-only description of one query-driven language.
pub(crate) struct LanguageProfile {
    /// Stable language ID (also the reported language).
    pub(crate) language_id: &'static str,
    /// File extensions owned by this profile.
    pub(crate) extensions: &'static [&'static str],
    /// Grammar name (see [`crate::extract::grammar`]).
    pub(crate) parser_name: &'static str,
    /// The symbol-extraction query (`include_str!` of a `.scm` file).
    pub(crate) query_source: &'static str,
    /// Capture-name → role table for `query_source`.
    pub(crate) captures: &'static [(&'static str, CaptureRole)],
    /// Module-scope strategy.
    pub(crate) scope: ScopeKind,
    /// Line-comment prefixes that mark a doc comment above a definition (empty
    /// to disable doc extraction).
    pub(crate) doc_prefixes: &'static [&'static str],
}

impl LanguageProfile {
    /// Returns the role assigned to a capture name, if any.
    pub(crate) fn role(&self, capture_name: &str) -> Option<CaptureRole> {
        self.captures
            .iter()
            .find(|(name, _)| *name == capture_name)
            .map(|(_, role)| *role)
    }
}

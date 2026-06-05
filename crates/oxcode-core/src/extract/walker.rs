//! Language-neutral scaffolding shared by the hand-written extractors.
//!
//! [`SymbolBuilder`] owns the symbol/edge/reference accumulators and assigns
//! byte-offset-independent stable keys; the pure string/IR helpers
//! ([`qualify`], [`reference_target`], [`path_segments`], …) are node-agnostic
//! and reused by every extractor, including the generic query-driven one. The
//! `Node`-reading helpers ([`source_preview`], [`field_name`]) operate on the
//! pack `Node` shared by the hand-written extractors.
//!
//! Invariant: walkers must traverse via [`crate::extract::cst::named_children`]
//! (declaration order) so the per-`(file, kind, qualified name)` ordinal that
//! disambiguates stable keys stays stable across edits that only shift bytes.

use std::collections::BTreeMap;

use oxcode_model::{
    EdgeKind, LanguageId, NodeKind, ReferenceKind, ReferenceTarget, SourceSpan, SourceUnit,
    SymbolEdge, SymbolNode, UnresolvedReference,
};
use tree_sitter_language_pack::Node;

use crate::extract::cst::{named_children, node_text};

/// A language's doc-comment and signature conventions.
pub(crate) trait CommentStrategy {
    /// Documentation comments directly attached to `node`.
    fn docstring(&self, node: &Node, source: &[u8]) -> Option<String>;
    /// Compact declaration header for `node`.
    fn signature(&self, node: &Node, source: &[u8]) -> Option<String>;
}

/// Describes a symbol to emit: graph `kind`, native `raw_kind`, `name`, and the
/// fully `qualified_name`.
pub(crate) struct SymbolSpec<'a> {
    pub(crate) kind: NodeKind,
    pub(crate) raw_kind: &'a str,
    pub(crate) name: &'a str,
    pub(crate) qualified_name: &'a str,
}

/// The source-derived fields of a symbol, computed by the caller from its node.
pub(crate) struct SymbolFields {
    pub(crate) span: SourceSpan,
    pub(crate) signature: Option<String>,
    pub(crate) docstring: Option<String>,
    pub(crate) source_preview: Option<String>,
}

/// The source location and compacted text of one reference expression.
pub(crate) struct ReferenceSpan {
    pub(crate) span: SourceSpan,
    pub(crate) text: String,
}

/// Node-agnostic accumulator for symbols, edges, and references.
///
/// `push_symbol` assigns each symbol a stable key of the form
/// `symbol:{file}:{kind}:{qualified}#{ordinal}`, where the ordinal is a
/// declaration-order counter per `(file, kind, qualified name)` triple. This
/// makes identity independent of byte offsets, so an edit that only shifts a
/// symbol's position leaves its key unchanged.
pub(crate) struct SymbolBuilder {
    file_path: String,
    language: LanguageId,
    /// Emitted symbols.
    pub(crate) nodes: Vec<SymbolNode>,
    /// Emitted intra-file edges.
    pub(crate) edges: Vec<SymbolEdge>,
    /// Emitted references pending cross-file resolution.
    pub(crate) references: Vec<UnresolvedReference>,
    ordinals: BTreeMap<String, u32>,
}

impl SymbolBuilder {
    /// Creates an empty builder for one source file.
    pub(crate) fn new(file_path: String, language: LanguageId) -> Self {
        Self {
            file_path,
            language,
            nodes: Vec::new(),
            edges: Vec::new(),
            references: Vec::new(),
            ordinals: BTreeMap::new(),
        }
    }

    /// Pushes a pre-built node (e.g. the file node) verbatim, without assigning
    /// an ordinal.
    pub(crate) fn push_node(&mut self, node: SymbolNode) {
        self.nodes.push(node);
    }

    /// Pushes one symbol with a byte-offset-independent stable key and returns
    /// that key for immediate edge wiring.
    pub(crate) fn push_symbol(&mut self, spec: SymbolSpec<'_>, fields: SymbolFields) -> String {
        let prefix = format!(
            "symbol:{}:{}:{}",
            self.file_path,
            spec.kind.as_str(),
            spec.qualified_name
        );
        let ordinal = {
            let counter = self.ordinals.entry(prefix.clone()).or_insert(0);
            let current = *counter;
            *counter += 1;
            current
        };
        let stable_key = format!("{prefix}#{ordinal}");
        self.nodes.push(SymbolNode {
            stable_key: stable_key.clone(),
            name: spec.name.to_string(),
            qualified_name: spec.qualified_name.to_string(),
            kind: spec.kind,
            raw_kind: Some(spec.raw_kind.to_string()),
            language: self.language.clone(),
            file_path: self.file_path.clone(),
            span: fields.span,
            signature: fields.signature,
            docstring: fields.docstring,
            source_preview: fields.source_preview,
        });
        stable_key
    }

    /// Pushes one already-resolved edge.
    pub(crate) fn push_edge(&mut self, source_key: &str, target_key: &str, kind: EdgeKind) {
        self.edges.push(SymbolEdge {
            source_key: source_key.to_string(),
            target_key: target_key.to_string(),
            kind,
        });
    }

    /// Pushes one unresolved reference, dropping empty targets.
    pub(crate) fn push_reference(
        &mut self,
        source_key: &str,
        target: ReferenceTarget,
        kind: EdgeKind,
        at: ReferenceSpan,
    ) {
        if target.path.is_empty() {
            return;
        }
        self.references.push(UnresolvedReference {
            source_key: source_key.to_string(),
            target,
            kind,
            file_path: self.file_path.clone(),
            span: at.span,
            text: at.text,
            reason: None,
        });
    }
}

/// Returns a node name from its `name` field or first child whose kind is one of
/// `identifier_kinds`.
pub(crate) fn field_name(node: &Node, source: &[u8], identifier_kinds: &[&str]) -> Option<String> {
    if let Some(name) = crate::extract::cst::field(node, "name") {
        let text = clean_identifier(node_text(&name, source));
        if !text.is_empty() {
            return Some(text);
        }
    }
    for child in named_children(node) {
        if identifier_kinds.contains(&child.kind().as_str()) {
            let text = clean_identifier(node_text(&child, source));
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

/// Returns a bounded source excerpt for an indexed symbol.
pub(crate) fn source_preview(node: &Node, source: &[u8]) -> Option<String> {
    bounded_preview(node_text(node, source))
}

/// Returns a bounded source excerpt from already-extracted node text.
pub(crate) fn bounded_preview(text: &str) -> Option<String> {
    let preview = text
        .lines()
        .map(str::trim_end)
        .skip_while(|line| line.trim().is_empty())
        .take(24)
        .collect::<Vec<_>>()
        .join("\n");
    bounded_text(&preview, 1200)
}

/// Slices a declaration to its header (up to `{` or `;`), compacted and bounded.
pub(crate) fn header_signature(declaration: &str) -> Option<String> {
    let header = declaration
        .split('{')
        .next()
        .unwrap_or(declaration)
        .split(';')
        .next()
        .unwrap_or(declaration);
    bounded_text(&compact_source_text(header), 300)
}

/// Builds a reference target with explicit path segments.
pub(crate) fn reference_target(
    raw: impl Into<String>,
    path: Vec<String>,
    qualifier: Option<String>,
    kind: ReferenceKind,
) -> ReferenceTarget {
    ReferenceTarget {
        raw: raw.into(),
        path,
        qualifier,
        kind_hint: kind,
    }
}

/// Builds an import reference target from full path segments.
pub(crate) fn import_target(path: Vec<String>, kind: ReferenceKind) -> ReferenceTarget {
    let raw = path.join("::");
    let qualifier = (path.len() > 1).then(|| path[..path.len() - 1].join("::"));
    ReferenceTarget {
        raw,
        path,
        qualifier,
        kind_hint: kind,
    }
}

/// Splits isolated path text into non-empty `::` segments.
pub(crate) fn path_segments(text: &str) -> Vec<String> {
    clean_path(text)
        .split("::")
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect()
}

/// Joins a module scope with one item name.
pub(crate) fn qualify(scope: &[String], name: &str) -> String {
    qualify_with_extra(scope, &[name])
}

/// Joins a module scope with extra path components.
pub(crate) fn qualify_with_extra(scope: &[String], extra: &[&str]) -> String {
    scope
        .iter()
        .map(String::as_str)
        .chain(extra.iter().copied())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("::")
}

/// Cleans an isolated identifier (strips raw markers and a trailing `!`).
pub(crate) fn clean_identifier(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("r#")
        .trim_end_matches('!')
        .to_string()
}

/// Cleans isolated path text into a resolver-friendly spelling.
pub(crate) fn clean_path(value: &str) -> String {
    let without_whitespace = value.split_whitespace().collect::<String>();
    without_whitespace
        .trim_start_matches("r#")
        .trim_end_matches('!')
        .trim_matches(';')
        .to_string()
}

/// Collapses source text to one readable line for agent-facing context.
pub(crate) fn compact_source_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Returns bounded non-empty text.
pub(crate) fn bounded_text(text: &str, max_chars: usize) -> Option<String> {
    let compact = text.trim();
    if compact.is_empty() {
        return None;
    }
    let mut output = String::new();
    for character in compact.chars().take(max_chars) {
        output.push(character);
    }
    Some(output)
}

/// Creates source unit metadata for one extracted file.
pub(crate) fn source_unit(relative_path: &str, language: LanguageId) -> SourceUnit {
    SourceUnit {
        path: relative_path.to_string(),
        language,
    }
}

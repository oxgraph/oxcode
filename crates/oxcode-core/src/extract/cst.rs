//! Shared concrete-syntax-tree accessors every language extractor builds on.
//!
//! The convention is: read a child by field name, descend into known node kinds
//! to find the base identifier, and walk children with a [`TreeCursor`] — never
//! re-parse a node's source text. Language extractors reuse these helpers so the
//! field-based discipline stays uniform.

use std::str;

use oxcode_model::SourceSpan;
use tree_sitter_language_pack::Node;

/// Returns borrowed source text for `node` (zero-copy; not trimmed).
#[must_use]
pub(crate) fn node_text<'source>(node: &Node, source: &'source [u8]) -> &'source str {
    let range = node.byte_range();
    source
        .get(range.start..range.end)
        .and_then(|bytes| str::from_utf8(bytes).ok())
        .unwrap_or_default()
}

/// Returns the named child stored under `field`, if present.
#[must_use]
pub(crate) fn field(node: &Node, field: &str) -> Option<Node> {
    node.child_by_field_name(field)
}

/// Collects a node's named children in a single linear cursor walk.
#[must_use]
pub(crate) fn named_children(node: &Node) -> Vec<Node> {
    let mut cursor = node.walk();
    let mut children = Vec::new();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            if child.is_named() {
                children.push(child);
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    children
}

/// Converts a node's position to the stored span representation.
#[must_use]
pub(crate) fn span(node: &Node) -> SourceSpan {
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

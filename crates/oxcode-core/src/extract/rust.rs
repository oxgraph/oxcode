use std::{
    path::{Path, PathBuf},
    str,
};

use oxcode_model::{
    EdgeKind, Extraction, LanguageId, NodeKind, ReferenceTarget, SourceSpan, SourceUnit,
    SymbolEdge, SymbolNode, UnresolvedReference,
};
use sha2::{Digest, Sha256};
use tree_sitter_language_pack::{Node, Tree};

use crate::{
    error::{Error, Result},
    extract::{ExtractionInput, LanguageExtractor},
};

/// Rust tree-sitter extractor.
pub(crate) struct RustExtractor;

impl LanguageExtractor for RustExtractor {
    fn language_id(&self) -> LanguageId {
        rust_language()
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["rs"]
    }

    fn parser_name(&self) -> &'static str {
        "rust"
    }

    fn extract(&self, input: ExtractionInput<'_>) -> Result<Extraction> {
        let tree = parse_rust(input.path, &input.source)?;
        Ok(extract_rust(&input.relative_path, &input.source, &tree))
    }
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
        signature: None,
        docstring: None,
        source_preview: None,
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
            signature: symbol_signature(node, self.source),
            docstring: symbol_docstring(node, self.source),
            source_preview: symbol_source_preview(node, self.source),
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

/// Returns a compact item declaration suitable for search output.
fn symbol_signature(node: &Node, source: &[u8]) -> Option<String> {
    let text = strip_leading_metadata(&node_text(node, source));
    let header = text
        .split('{')
        .next()
        .unwrap_or(text.as_str())
        .split(';')
        .next()
        .unwrap_or(text.as_str());
    bounded_text(&compact_source_text(header), 300)
}

/// Returns contiguous Rust doc comments directly attached to an item.
fn symbol_docstring(node: &Node, source: &[u8]) -> Option<String> {
    let source = str::from_utf8(source).ok()?;
    let before = source.get(..node.start_byte()).unwrap_or_default();
    let mut lines = Vec::new();
    for line in before.lines().rev() {
        let trimmed = line.trim();
        if trimmed.starts_with("#[") && lines.is_empty() {
            continue;
        }
        if let Some(doc) = clean_doc_comment(trimmed) {
            lines.push(doc);
            continue;
        }
        break;
    }
    lines.reverse();

    if lines.is_empty() {
        for line in node_text(node, source.as_bytes()).lines() {
            let trimmed = line.trim();
            if let Some(doc) = clean_doc_comment(trimmed) {
                lines.push(doc);
                continue;
            }
            if trimmed.starts_with("#[") || trimmed.is_empty() {
                continue;
            }
            break;
        }
    }

    bounded_text(&lines.join("\n"), 800)
}

/// Returns a bounded source excerpt for an indexed symbol.
fn symbol_source_preview(node: &Node, source: &[u8]) -> Option<String> {
    let text = node_text(node, source);
    let preview = text
        .lines()
        .map(str::trim_end)
        .skip_while(|line| line.trim().is_empty())
        .take(24)
        .collect::<Vec<_>>()
        .join("\n");
    bounded_text(&preview, 1200)
}

/// Removes leading doc and attribute metadata from a declaration-like string.
fn strip_leading_metadata(text: &str) -> String {
    text.lines()
        .skip_while(|line| {
            let trimmed = line.trim();
            trimmed.is_empty()
                || trimmed.starts_with("///")
                || trimmed.starts_with("//!")
                || trimmed.starts_with("#[")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Cleans one Rust doc comment line.
fn clean_doc_comment(line: &str) -> Option<String> {
    line.strip_prefix("///")
        .or_else(|| line.strip_prefix("//!"))
        .map(str::trim_start)
        .map(ToOwned::to_owned)
        .filter(|line| !line.is_empty())
}

/// Returns bounded non-empty text.
fn bounded_text(text: &str, max_chars: usize) -> Option<String> {
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
}

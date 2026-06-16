//! Rust extractor: walks the tree-sitter CST by node kind and field children,
//! emitting symbols and language-neutral reference targets. It never derives
//! structure from raw node text — method names, impl types, traits, and import
//! paths all come from typed field children.

use std::{path::Path, str};

use oxcode_model::{
    EdgeKind, Extraction, FileParseStatus, LanguageId, NodeKind, ReferenceKind, ReferenceTarget,
    SymbolNode,
};
use tree_sitter::{Node, Tree};

use crate::{
    error::{Error, Result},
    extract::{
        ExtractionInput, LanguageExtractor,
        cst::{field, named_children, node_text, span},
        grammar,
        scope::{RustScope, ScopeStrategy},
        walker::{
            CommentStrategy, ReferenceSpan, SymbolBuilder, SymbolFields, SymbolSpec, bounded_text,
            clean_identifier, clean_path, compact_source_text, field_name, header_signature,
            import_target, path_segments, qualify, qualify_with_extra, reference_target,
            source_preview, source_unit,
        },
    },
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

    fn extract(&self, input: ExtractionInput<'_>) -> Result<Extraction> {
        let scope = RustScope.base_scope(input.path, &input.relative_path);
        let tree = parse_rust(input.path, &input.source)?;
        Ok(extract_rust(
            &input.relative_path,
            &scope,
            &input.source,
            &tree,
        ))
    }
}

/// Grammar name for Rust.
const PARSER_NAME: &str = "rust";

/// Parses Rust source into a syntax tree.
fn parse_rust(path: &Path, source: &[u8]) -> Result<Tree> {
    grammar::parse(PARSER_NAME, source).ok_or_else(|| Error::Parse {
        path: path.to_path_buf(),
        message: "tree-sitter returned no parse tree".to_string(),
    })
}

/// Extracts code graph nodes and references from one Rust source file.
///
/// `base_scope` is the crate-qualified module scope (`[crate, ..modules]`) that
/// every qualified name in this file is anchored to.
fn extract_rust(
    relative_path: &str,
    base_scope: &[String],
    source: &[u8],
    tree: &Tree,
) -> Extraction {
    let relative = relative_path.to_string();
    let file_key = format!("file:{relative}");
    let language = rust_language();
    let root = tree.root_node();

    let file_node = SymbolNode {
        stable_key: file_key.clone(),
        name: relative.clone(),
        qualified_name: base_scope.join("::"),
        kind: NodeKind::File,
        raw_kind: Some("source_file".to_string()),
        language: language.clone(),
        file_path: relative.clone(),
        span: span(&root),
        signature: None,
        docstring: None,
        source_preview: None,
    };

    let mut builder = SymbolBuilder::new(relative.clone(), language);
    builder.push_node(file_node);
    let mut walker = RustWalker {
        source,
        builder,
        comments: RustComments,
    };

    walker.visit_children(
        &root,
        VisitContext {
            parent_key: &file_key,
            owner_key: &file_key,
            scope: base_scope,
            owner_type: None,
        },
    );

    let parse_status = if root.has_error() {
        FileParseStatus::Partial
    } else {
        FileParseStatus::Ok
    };

    Extraction {
        file: source_unit(&relative, rust_language()),
        parse_status,
        nodes: walker.builder.nodes,
        edges: walker.builder.edges,
        references: walker.builder.references,
    }
}

/// Stateful Rust CST walker.
struct RustWalker<'source> {
    source: &'source [u8],
    builder: SymbolBuilder,
    comments: RustComments,
}

/// Traversal state threaded through the walker: the containing symbol
/// (`parent_key`), the symbol that calls and references are attributed to
/// (`owner_key`), the module `scope`, and the enclosing impl/trait `owner_type`
/// used to qualify and kind methods.
#[derive(Clone, Copy)]
struct VisitContext<'a> {
    parent_key: &'a str,
    owner_key: &'a str,
    scope: &'a [String],
    owner_type: Option<&'a str>,
}

impl RustWalker<'_> {
    /// Visits all named children under `node`.
    fn visit_children(&mut self, node: &Node, ctx: VisitContext<'_>) {
        for child in named_children(node) {
            self.visit_node(&child, ctx);
        }
    }

    /// Visits one CST node, emitting graph data when it represents code intent.
    fn visit_node(&mut self, node: &Node, ctx: VisitContext<'_>) {
        match node.kind() {
            "mod_item" => self.visit_module(node, ctx),
            "struct_item" => self.visit_named(node, ctx, NodeKind::Struct, "struct_item"),
            "union_item" => self.visit_named(node, ctx, NodeKind::Struct, "union_item"),
            "enum_item" => self.visit_named(node, ctx, NodeKind::Enum, "enum_item"),
            "trait_item" => self.visit_trait(node, ctx),
            "impl_item" => self.visit_impl(node, ctx),
            "function_item" => self.visit_function(node, ctx, "function_item"),
            "function_signature_item" => self.visit_function(node, ctx, "function_signature_item"),
            "const_item" => self.visit_named(node, ctx, NodeKind::Constant, "const_item"),
            "static_item" => self.visit_named(node, ctx, NodeKind::Constant, "static_item"),
            "type_item" => self.visit_named(node, ctx, NodeKind::TypeAlias, "type_item"),
            "macro_definition" => self.visit_named(node, ctx, NodeKind::Macro, "macro_definition"),
            "use_declaration" => self.visit_use(node, ctx),
            "call_expression" => self.visit_call(node, ctx),
            "macro_invocation" => self.visit_macro(node, ctx),
            _ => self.visit_children(node, ctx),
        }
    }

    /// Emits import references for a `use` declaration, then recurses.
    fn visit_use(&mut self, node: &Node, ctx: VisitContext<'_>) {
        if let Some(argument) = field(node, "argument") {
            for target in use_targets(&argument, &[], self.source) {
                self.push_reference(node, ctx.owner_key, target, EdgeKind::Imports);
            }
        }
        self.visit_children(node, ctx);
    }

    /// Emits a call reference for a `call_expression`, then recurses.
    ///
    /// The enclosing `owner_type` carried in `ctx` resolves `self`/`Self`
    /// receivers to a concrete type so method calls resolve by receiver type.
    fn visit_call(&mut self, node: &Node, ctx: VisitContext<'_>) {
        if let Some(function) = field(node, "function")
            && let Some(target) = callee_target(&function, self.source, ctx.owner_type)
        {
            self.push_reference(node, ctx.owner_key, target, EdgeKind::Calls);
        }
        self.visit_children(node, ctx);
    }

    /// Emits a call reference for a `macro_invocation`, then recurses.
    fn visit_macro(&mut self, node: &Node, ctx: VisitContext<'_>) {
        if let Some(target) = macro_target(node, self.source) {
            self.push_reference(node, ctx.owner_key, target, EdgeKind::Calls);
        }
        self.visit_children(node, ctx);
    }

    /// Emits a named item and keeps traversing with the current owner.
    fn visit_named(&mut self, node: &Node, ctx: VisitContext<'_>, kind: NodeKind, raw_kind: &str) {
        if let Some(name) = item_name(node, self.source) {
            let qualified = qualify(ctx.scope, &name);
            let key = self.push_symbol(
                node,
                SymbolSpec {
                    kind,
                    raw_kind,
                    name: &name,
                    qualified_name: &qualified,
                },
            );
            self.push_edge(ctx.parent_key, &key, EdgeKind::Contains);
            self.visit_children(
                node,
                VisitContext {
                    parent_key: &key,
                    owner_key: ctx.owner_key,
                    scope: ctx.scope,
                    owner_type: None,
                },
            );
        }
    }

    /// Emits a module and recurses into its body with an extended scope.
    fn visit_module(&mut self, node: &Node, ctx: VisitContext<'_>) {
        if let Some(name) = item_name(node, self.source) {
            let qualified = qualify(ctx.scope, &name);
            let key = self.push_symbol(
                node,
                SymbolSpec {
                    kind: NodeKind::Module,
                    raw_kind: "mod_item",
                    name: &name,
                    qualified_name: &qualified,
                },
            );
            self.push_edge(ctx.parent_key, &key, EdgeKind::Contains);
            let mut child_scope = ctx.scope.to_vec();
            child_scope.push(name);
            self.visit_children(
                node,
                VisitContext {
                    parent_key: &key,
                    owner_key: &key,
                    scope: &child_scope,
                    owner_type: None,
                },
            );
        }
    }

    /// Emits a trait and traverses its body, treating the trait as the owner
    /// type so trait methods (declarations and defaults) are `Method`s qualified
    /// `Trait::method`.
    fn visit_trait(&mut self, node: &Node, ctx: VisitContext<'_>) {
        if let Some(name) = item_name(node, self.source) {
            let qualified = qualify(ctx.scope, &name);
            let key = self.push_symbol(
                node,
                SymbolSpec {
                    kind: NodeKind::Trait,
                    raw_kind: "trait_item",
                    name: &name,
                    qualified_name: &qualified,
                },
            );
            self.push_edge(ctx.parent_key, &key, EdgeKind::Contains);
            self.visit_children(
                node,
                VisitContext {
                    parent_key: &key,
                    owner_key: &key,
                    scope: ctx.scope,
                    owner_type: Some(&name),
                },
            );
        }
    }

    /// Emits an implementation block and traverses methods with the impl's type
    /// as the owner type.
    fn visit_impl(&mut self, node: &Node, ctx: VisitContext<'_>) {
        let target = impl_type_name(node, self.source).unwrap_or_else(|| "<impl>".to_string());
        let name = format!("impl {target}");
        let qualified = qualify(ctx.scope, &name);
        let key = self.push_symbol(
            node,
            SymbolSpec {
                kind: NodeKind::ImplBlock,
                raw_kind: "impl_item",
                name: &name,
                qualified_name: &qualified,
            },
        );
        self.push_edge(ctx.parent_key, &key, EdgeKind::Contains);

        if let Some(trait_name) = impl_trait_name(node, self.source) {
            let trait_target = reference_target(
                trait_name.clone(),
                vec![trait_name],
                None,
                ReferenceKind::Trait,
            );
            self.push_reference(node, &key, trait_target, EdgeKind::Implements);
        }

        // The concrete type the impl is for, as a full scoped path so the resolver
        // can disambiguate cross-module types (e.g. `impl other::Foo`). Anchored at
        // the type node so the reference site is the type, not the whole impl body.
        if let Some(type_node) = field(node, "type")
            && let Some(type_path) = type_path_segments(&type_node, self.source)
        {
            let type_target =
                reference_target(type_path.join("::"), type_path, None, ReferenceKind::Type);
            self.push_reference(&type_node, &key, type_target, EdgeKind::ImplementsFor);
        }

        self.visit_children(
            node,
            VisitContext {
                parent_key: &key,
                owner_key: &key,
                scope: ctx.scope,
                owner_type: Some(&target),
            },
        );
    }

    /// Emits a free function or method and makes it the owner for nested calls.
    fn visit_function(&mut self, node: &Node, ctx: VisitContext<'_>, raw_kind: &str) {
        if let Some(name) = item_name(node, self.source) {
            let kind = if ctx.owner_type.is_some() {
                NodeKind::Method
            } else {
                NodeKind::Function
            };
            let qualified = ctx.owner_type.map_or_else(
                || qualify(ctx.scope, &name),
                |target| qualify_with_extra(ctx.scope, &[target, &name]),
            );
            let key = self.push_symbol(
                node,
                SymbolSpec {
                    kind,
                    raw_kind,
                    name: &name,
                    qualified_name: &qualified,
                },
            );
            self.push_edge(ctx.parent_key, &key, EdgeKind::Contains);
            // Calls in the body keep the enclosing type so `self`/`Self`
            // receivers resolve to it.
            self.visit_children(
                node,
                VisitContext {
                    parent_key: &key,
                    owner_key: &key,
                    scope: ctx.scope,
                    owner_type: ctx.owner_type,
                },
            );
        }
    }

    /// Computes a symbol's span/signature/docstring/preview from `node` and
    /// pushes it, returning its stable key.
    fn push_symbol(&mut self, node: &Node, spec: SymbolSpec<'_>) -> String {
        let fields = SymbolFields {
            span: span(node),
            signature: self.comments.signature(node, self.source),
            docstring: self.comments.docstring(node, self.source),
            source_preview: source_preview(node, self.source),
        };
        self.builder.push_symbol(spec, fields)
    }

    /// Pushes one already-resolved edge.
    fn push_edge(&mut self, source_key: &str, target_key: &str, kind: EdgeKind) {
        self.builder.push_edge(source_key, target_key, kind);
    }

    /// Pushes one unresolved reference for cross-file resolution.
    fn push_reference(
        &mut self,
        node: &Node,
        source_key: &str,
        target: ReferenceTarget,
        kind: EdgeKind,
    ) {
        let at = ReferenceSpan {
            span: span(node),
            text: compact_source_text(node_text(node, self.source)),
        };
        self.builder.push_reference(source_key, target, kind, at);
    }
}

/// Rust doc-comment and signature conventions.
struct RustComments;

impl CommentStrategy for RustComments {
    /// Returns contiguous Rust doc comments directly attached to an item, read
    /// from the lines above it or, failing that, from its own leading lines.
    fn docstring(&self, node: &Node, source: &[u8]) -> Option<String> {
        let source = str::from_utf8(source).ok()?;
        let before = source.get(..node.start_byte()).unwrap_or_default();
        let mut lines = leading_doc_lines(before);
        if lines.is_empty() {
            lines = inner_doc_lines(node_text(node, source.as_bytes()));
        }
        bounded_text(&lines.join("\n"), 800)
    }

    /// Returns a compact item declaration suitable for search output.
    fn signature(&self, node: &Node, source: &[u8]) -> Option<String> {
        header_signature(&strip_leading_metadata(node_text(node, source)))
    }
}

/// Collects contiguous doc-comment lines immediately preceding an item, skipping
/// leading attributes.
fn leading_doc_lines(before: &str) -> Vec<String> {
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
    lines
}

/// Collects inner (`//!`) doc-comment lines from the head of an item's own text.
fn inner_doc_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for line in text.lines() {
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
    lines
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

/// Returns a node name from its `name` field or first identifier-like child.
fn item_name(node: &Node, source: &[u8]) -> Option<String> {
    field_name(
        node,
        source,
        &["identifier", "type_identifier", "field_identifier"],
    )
}

/// Builds a reference target from a `call_expression`'s `function` child.
///
/// `owner_type` is the enclosing impl/trait type used to resolve `self`/`Self`
/// receivers to a concrete type so method calls can resolve by receiver type.
fn callee_target(
    function: &Node,
    source: &[u8],
    owner_type: Option<&str>,
) -> Option<ReferenceTarget> {
    match function.kind() {
        "identifier" => {
            let name = clean_identifier(node_text(function, source));
            (!name.is_empty())
                .then(|| reference_target(name.clone(), vec![name], None, ReferenceKind::Function))
        }
        "field_expression" => {
            let name = field(function, "field")
                .map(|node| clean_identifier(node_text(&node, source)))
                .filter(|text| !text.is_empty())?;
            let receiver = field(function, "value")
                .map(|node| compact_source_text(node_text(&node, source)))
                .filter(|text| !text.is_empty());
            let qualifier = resolve_receiver(receiver, owner_type);
            Some(reference_target(
                name.clone(),
                vec![name],
                qualifier,
                ReferenceKind::Method,
            ))
        }
        "scoped_identifier" => {
            let name = field(function, "name")
                .map(|node| clean_identifier(node_text(&node, source)))
                .filter(|text| !text.is_empty())?;
            let prefix = substitute_self(
                field(function, "path")
                    .map(|node| path_segments(node_text(&node, source)))
                    .unwrap_or_default(),
                owner_type,
            );
            let qualifier = (!prefix.is_empty()).then(|| prefix.join("::"));
            let mut path = prefix;
            path.push(name);
            Some(reference_target(
                path.join("::"),
                path,
                qualifier,
                ReferenceKind::Function,
            ))
        }
        "generic_function" => {
            field(function, "function").and_then(|inner| callee_target(&inner, source, owner_type))
        }
        _ => None,
    }
}

/// Resolves a method-call receiver to a type qualifier when possible.
///
/// `self`/`Self` map to the enclosing impl/trait type; other receiver
/// expressions are kept verbatim for best-effort matching.
fn resolve_receiver(receiver: Option<String>, owner_type: Option<&str>) -> Option<String> {
    match receiver.as_deref() {
        Some("self" | "Self") => owner_type.map(str::to_string),
        _ => receiver,
    }
}

/// Replaces a leading `Self` path segment with the enclosing owner type.
fn substitute_self(mut segments: Vec<String>, owner_type: Option<&str>) -> Vec<String> {
    if let (Some(first), Some(owner)) = (segments.first_mut(), owner_type)
        && first == "Self"
    {
        *first = owner.to_string();
    }
    segments
}

/// Builds a reference target for a `macro_invocation`.
fn macro_target(node: &Node, source: &[u8]) -> Option<ReferenceTarget> {
    let macro_node = field(node, "macro")?;
    let raw = clean_path(node_text(&macro_node, source));
    let name = raw.rsplit("::").next().unwrap_or(&raw).to_string();
    (!name.is_empty())
        .then(|| reference_target(name.clone(), vec![name], None, ReferenceKind::Macro))
}

/// Extracts the implemented type's base name from an `impl_item` `type` field.
fn impl_type_name(node: &Node, source: &[u8]) -> Option<String> {
    field(node, "type").and_then(|node| base_type_name(&node, source))
}

/// Extracts the implemented trait's base name from an `impl_item` `trait` field.
fn impl_trait_name(node: &Node, source: &[u8]) -> Option<String> {
    field(node, "trait").and_then(|node| base_type_name(&node, source))
}

/// Descends a type node to its nominal path segments, generics and reference
/// markers stripped (`other::Foo<T>` → `["other", "Foo"]`), mirroring
/// [`base_type_name`] but preserving the scoped prefix. Returns `None` for
/// non-nominal types (tuples, slices, trait objects).
fn type_path_segments(node: &Node, source: &[u8]) -> Option<Vec<String>> {
    match node.kind() {
        "type_identifier" | "identifier" | "primitive_type" => {
            let text = clean_identifier(node_text(node, source));
            (!text.is_empty()).then(|| vec![text])
        }
        "scoped_type_identifier" | "scoped_identifier" => {
            let segments = path_segments(node_text(node, source));
            (!segments.is_empty()).then_some(segments)
        }
        "generic_type" | "reference_type" | "pointer_type" => {
            field(node, "type").and_then(|inner| type_path_segments(&inner, source))
        }
        _ => None,
    }
}

/// Descends a type node to a stable, deterministic base name.
fn base_type_name(node: &Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        "type_identifier" | "identifier" | "primitive_type" => {
            let text = clean_identifier(node_text(node, source));
            (!text.is_empty()).then_some(text)
        }
        "scoped_type_identifier" | "scoped_identifier" => field(node, "name")
            .and_then(|name| base_type_name(&name, source))
            .or_else(|| {
                let text = clean_path(node_text(node, source));
                text.rsplit("::")
                    .next()
                    .map(str::to_string)
                    .filter(|segment| !segment.is_empty())
            }),
        "generic_type" | "reference_type" | "pointer_type" => {
            field(node, "type").and_then(|inner| base_type_name(&inner, source))
        }
        "tuple_type" => Some("<tuple>".to_string()),
        "unit_type" => Some("<unit>".to_string()),
        "array_type" | "slice_type" => Some("<slice>".to_string()),
        "dynamic_type" => field(node, "trait")
            .and_then(|inner| base_type_name(&inner, source))
            .map_or_else(
                || Some("<dyn>".to_string()),
                |name| Some(format!("dyn {name}")),
            ),
        _ => {
            let text = clean_path(node_text(node, source));
            (!text.is_empty()).then_some(text)
        }
    }
}

/// Walks a `use` argument subtree into one reference target per imported leaf,
/// accumulating the path prefix so each leaf carries its full path segments.
fn use_targets(node: &Node, prefix: &[String], source: &[u8]) -> Vec<ReferenceTarget> {
    match node.kind() {
        "scoped_identifier" => {
            let Some(name) = field(node, "name")
                .map(|node| clean_identifier(node_text(&node, source)))
                .filter(|text| !text.is_empty())
            else {
                return Vec::new();
            };
            let mut path = prefix.to_vec();
            if let Some(node) = field(node, "path") {
                path.extend(path_segments(node_text(&node, source)));
            }
            path.push(name);
            vec![import_target(path, ReferenceKind::Import)]
        }
        "identifier" => {
            let name = clean_identifier(node_text(node, source));
            if name.is_empty() {
                return Vec::new();
            }
            let mut path = prefix.to_vec();
            path.push(name);
            vec![import_target(path, ReferenceKind::Import)]
        }
        "use_as_clause" => field(node, "path")
            .map(|path| use_targets(&path, prefix, source))
            .unwrap_or_default(),
        "use_list" => named_children(node)
            .iter()
            .flat_map(|child| use_targets(child, prefix, source))
            .collect(),
        "scoped_use_list" => {
            let mut nested = prefix.to_vec();
            if let Some(node) = field(node, "path") {
                nested.extend(path_segments(node_text(&node, source)));
            }
            field(node, "list")
                .map(|list| {
                    named_children(&list)
                        .iter()
                        .flat_map(|child| use_targets(child, &nested, source))
                        .collect()
                })
                .unwrap_or_default()
        }
        "use_wildcard" => {
            let mut path = prefix.to_vec();
            let base = field(node, "path")
                .map(|node| path_segments(node_text(&node, source)))
                .unwrap_or_else(|| {
                    let text = clean_path(node_text(node, source));
                    path_segments(text.trim_end_matches('*').trim_end_matches(':'))
                });
            path.extend(base);
            if path.is_empty() {
                Vec::new()
            } else {
                vec![import_target(path, ReferenceKind::ImportGlob)]
            }
        }
        _ => Vec::new(),
    }
}

/// Returns the Rust language ID.
fn rust_language() -> LanguageId {
    LanguageId::from(PARSER_NAME)
}

#[cfg(test)]
mod tests {
    use oxcode_model::UnresolvedReference;

    use super::*;

    /// Parses a snippet and returns its extraction (crate-root `src/lib.rs`).
    fn extract(source: &str) -> Extraction {
        let tree = parse_rust(Path::new("src/lib.rs"), source.as_bytes()).expect("parse");
        extract_rust(
            "src/lib.rs",
            &["crate".to_string()],
            source.as_bytes(),
            &tree,
        )
    }

    fn reference<'a>(extraction: &'a Extraction, text_contains: &str) -> &'a UnresolvedReference {
        extraction
            .references
            .iter()
            .find(|reference| reference.text.contains(text_contains))
            .unwrap_or_else(|| panic!("no reference containing {text_contains:?}"))
    }

    #[test]
    fn stable_keys_survive_byte_shifting_edits() {
        let key_of = |extraction: &Extraction, name: &str| -> String {
            extraction
                .nodes
                .iter()
                .find(|node| node.name == name && node.kind == NodeKind::Function)
                .unwrap_or_else(|| panic!("symbol {name}"))
                .stable_key
                .clone()
        };
        let original = extract("fn alpha() {}\nfn beta() {}\n");
        // Prepend a comment so every byte offset shifts while declaration order
        // is unchanged; a byte-offset-independent key must be unaffected.
        let edited =
            extract("// a leading comment that shifts every byte\nfn alpha() {}\nfn beta() {}\n");
        assert_eq!(key_of(&original, "alpha"), key_of(&edited, "alpha"));
        assert_eq!(key_of(&original, "beta"), key_of(&edited, "beta"));
        assert!(key_of(&original, "alpha").ends_with("#0"));
    }

    #[test]
    fn method_call_extracts_method_name_and_receiver_not_text() {
        let extraction = extract("fn entry(self) { self.run(); }");
        let call = reference(&extraction, "self.run()");
        assert_eq!(call.target.path, vec!["run".to_string()]);
        // `self` receiver with no enclosing impl resolves to no qualifier.
        assert_eq!(call.target.kind_hint, ReferenceKind::Method);
    }

    #[test]
    fn self_method_call_qualifier_resolves_to_impl_type() {
        let extraction = extract("struct Foo; impl Foo { fn run(&self) { self.help(); } }");
        let call = reference(&extraction, "self.help()");
        assert_eq!(call.target.path, vec!["help".to_string()]);
        assert_eq!(call.target.qualifier.as_deref(), Some("Foo"));
    }

    #[test]
    fn chained_call_uses_final_method_name() {
        let extraction = extract("fn entry(v: Vec<u8>) { v.iter().count(); }");
        let call = reference(&extraction, "v.iter().count()");
        assert_eq!(call.target.path, vec!["count".to_string()]);
        // Receiver is the immediate inner expression, not deeply resolved.
        assert!(call.target.qualifier.is_some());
        assert!(!call.target.joined().contains('.'));
        assert!(!call.target.joined().contains('('));
    }

    #[test]
    fn free_and_associated_calls_resolve_to_names() {
        let extraction = extract("fn entry() { helper(); Thing::new(); }");
        assert_eq!(
            reference(&extraction, "helper()").target.path,
            vec!["helper".to_string()]
        );
        let assoc = reference(&extraction, "Thing::new()");
        assert_eq!(
            assoc.target.path,
            vec!["Thing".to_string(), "new".to_string()]
        );
        assert_eq!(assoc.target.joined(), "Thing::new");
        assert_eq!(assoc.target.qualifier.as_deref(), Some("Thing"));
    }

    #[test]
    fn impl_on_reference_type_keeps_base_type_name() {
        let extraction = extract("struct Foo; impl Trait for &mut Foo { fn run(&self) {} }");
        let method = extraction
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::Method)
            .expect("method");
        assert_eq!(method.qualified_name, "crate::Foo::run");
        // The Implements reference targets the trait base name.
        assert!(
            extraction
                .references
                .iter()
                .any(|reference| reference.kind == EdgeKind::Implements
                    && reference.target.last_segment() == Some("Trait"))
        );
    }

    #[test]
    fn impl_with_where_clause_and_generics_keeps_base_type() {
        let extraction =
            extract("struct Bar; impl<T> Foo<T> for Bar where T: Clone { fn m(&self) {} }");
        let method = extraction
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::Method)
            .expect("method");
        assert_eq!(method.qualified_name, "crate::Bar::m");
    }

    #[test]
    fn trait_method_declarations_are_methods() {
        let extraction = extract("trait T { fn decl(&self); fn def(&self) {} }");
        let methods: Vec<&str> = extraction
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Method)
            .map(|node| node.qualified_name.as_str())
            .collect();
        assert!(methods.contains(&"crate::T::decl"), "{methods:?}");
        assert!(methods.contains(&"crate::T::def"), "{methods:?}");
    }

    #[test]
    fn use_alias_and_glob_emit_imports() {
        let extraction = extract("use a::b as c; use d::e::*; use f::{g, h::I};");
        let imports: Vec<(Option<&str>, ReferenceKind)> = extraction
            .references
            .iter()
            .filter(|reference| reference.kind == EdgeKind::Imports)
            .map(|reference| (reference.target.last_segment(), reference.target.kind_hint))
            .collect();
        // `use a::b as c` imports the underlying item `b`.
        assert!(
            imports.contains(&(Some("b"), ReferenceKind::Import)),
            "{imports:?}"
        );
        // Glob is marked, not dropped.
        assert!(
            imports.contains(&(Some("e"), ReferenceKind::ImportGlob)),
            "{imports:?}"
        );
        // Grouped imports expand per-leaf.
        assert!(
            imports.iter().any(|(name, _)| *name == Some("g")),
            "{imports:?}"
        );
        assert!(
            imports.iter().any(|(name, _)| *name == Some("I")),
            "{imports:?}"
        );
    }

    #[test]
    fn syntax_error_marks_partial_but_still_extracts() {
        // `entry` parses cleanly; the trailing item is malformed, so the tree
        // has error nodes but the recoverable symbols are still extracted.
        let extraction = extract("fn entry() { helper(); }\nfn broken( {");
        assert_eq!(extraction.parse_status, FileParseStatus::Partial);
        assert!(
            extraction
                .nodes
                .iter()
                .any(|node| node.qualified_name == "crate::entry")
        );
    }
}

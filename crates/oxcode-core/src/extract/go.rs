//! Go extractor: walks the tree-sitter CST by node kind and field children,
//! emitting symbols and language-neutral reference targets. Methods are
//! qualified by their receiver type, and a named receiver resolves like Rust's
//! `self` so receiver-typed method calls resolve by type.

use std::path::Path;

use oxcode_model::{
    EdgeKind, Extraction, FileParseStatus, LanguageId, NodeKind, ReferenceKind, ReferenceTarget,
    SymbolNode,
};
use tree_sitter_language_pack::{Node, Tree};

use crate::{
    error::{Error, Result},
    extract::{
        ExtractionInput, LanguageExtractor,
        cst::{field, named_children, node_text, span},
        scope::{GoScope, ScopeStrategy},
        walker::{
            CommentStrategy, ReferenceSpan, SymbolBuilder, SymbolFields, SymbolSpec, bounded_text,
            clean_identifier, compact_source_text, field_name, header_signature, import_target,
            qualify, qualify_with_extra, reference_target, source_preview, source_unit,
        },
    },
};

/// Go tree-sitter extractor.
pub(crate) struct GoExtractor;

impl LanguageExtractor for GoExtractor {
    fn language_id(&self) -> LanguageId {
        go_language()
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["go"]
    }

    fn parser_name(&self) -> &'static str {
        PARSER_NAME
    }

    fn extract(&self, input: ExtractionInput<'_>) -> Result<Extraction> {
        let scope = GoScope.base_scope(input.path, &input.relative_path);
        let tree = parse_go(input.path, &input.source)?;
        Ok(extract_go(
            &input.relative_path,
            &scope,
            &input.source,
            &tree,
        ))
    }
}

/// tree-sitter-language-pack parser name for Go.
const PARSER_NAME: &str = "go";

/// Parses Go source into a syntax tree.
fn parse_go(path: &Path, source: &[u8]) -> Result<Tree> {
    let mut parser =
        tree_sitter_language_pack::get_parser(PARSER_NAME).map_err(|error| Error::Parse {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    parser.parse_bytes(source).ok_or_else(|| Error::Parse {
        path: path.to_path_buf(),
        message: "tree-sitter returned no parse tree".to_string(),
    })
}

/// Extracts code graph nodes and references from one Go source file.
///
/// `base_scope` is the package import path (`[module.., dir..]`) that every
/// qualified name in this file is anchored to.
fn extract_go(
    relative_path: &str,
    base_scope: &[String],
    source: &[u8],
    tree: &Tree,
) -> Extraction {
    let relative = relative_path.to_string();
    let file_key = format!("file:{relative}");
    let language = go_language();
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
    let mut walker = GoWalker {
        source,
        builder,
        comments: GoComments,
    };

    walker.visit_children(
        &root,
        VisitContext {
            parent_key: &file_key,
            owner_key: &file_key,
            scope: base_scope,
            owner_type: None,
            receiver_var: None,
        },
    );

    let parse_status = if root.has_error() {
        FileParseStatus::Partial
    } else {
        FileParseStatus::Ok
    };

    Extraction {
        file: source_unit(&relative, go_language()),
        parse_status,
        nodes: walker.builder.nodes,
        edges: walker.builder.edges,
        references: walker.builder.references,
    }
}

/// Stateful Go CST walker.
struct GoWalker<'source> {
    source: &'source [u8],
    builder: SymbolBuilder,
    comments: GoComments,
}

/// Traversal state: the containing symbol (`parent_key`), the symbol calls and
/// references are attributed to (`owner_key`), the package `scope`, the
/// enclosing type that qualifies methods (`owner_type`), and the method's named
/// receiver variable (`receiver_var`) used to resolve `recv.method()` by type.
#[derive(Clone, Copy)]
struct VisitContext<'a> {
    parent_key: &'a str,
    owner_key: &'a str,
    scope: &'a [String],
    owner_type: Option<&'a str>,
    receiver_var: Option<&'a str>,
}

impl GoWalker<'_> {
    /// Visits all named children under `node`.
    fn visit_children(&mut self, node: &Node, ctx: VisitContext<'_>) {
        for child in named_children(node) {
            self.visit_node(&child, ctx);
        }
    }

    /// Visits one CST node, emitting graph data when it represents code intent.
    fn visit_node(&mut self, node: &Node, ctx: VisitContext<'_>) {
        match node.kind().as_str() {
            "function_declaration" => self.visit_function(node, ctx),
            "method_declaration" => self.visit_method(node, ctx),
            "type_spec" | "type_alias" => self.visit_type(node, ctx),
            "const_spec" => self.visit_value(node, ctx, NodeKind::Constant, "const_spec"),
            "var_spec" => self.visit_value(node, ctx, NodeKind::Variable, "var_spec"),
            "import_spec" => self.visit_import(node, ctx),
            "call_expression" => self.visit_call(node, ctx),
            _ => self.visit_children(node, ctx),
        }
    }

    /// Emits a free function and traverses its body as the owner.
    fn visit_function(&mut self, node: &Node, ctx: VisitContext<'_>) {
        let Some(name) = item_name(node, self.source) else {
            return;
        };
        let qualified = qualify(ctx.scope, &name);
        let key = self.push_symbol(
            node,
            SymbolSpec {
                kind: NodeKind::Function,
                raw_kind: "function_declaration",
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
                owner_type: None,
                receiver_var: None,
            },
        );
    }

    /// Emits a method qualified by its receiver type and binds the receiver
    /// variable so `recv.other()` resolves to that type.
    fn visit_method(&mut self, node: &Node, ctx: VisitContext<'_>) {
        let Some(name) = item_name(node, self.source) else {
            return;
        };
        let receiver =
            field(node, "receiver").and_then(|list| receiver_binding(&list, self.source));
        let (owner_type, receiver_var) = match &receiver {
            Some(binding) => (Some(binding.type_name.as_str()), binding.var.as_deref()),
            None => (None, None),
        };
        let qualified = owner_type.map_or_else(
            || qualify(ctx.scope, &name),
            |owner| qualify_with_extra(ctx.scope, &[owner, &name]),
        );
        let key = self.push_symbol(
            node,
            SymbolSpec {
                kind: NodeKind::Method,
                raw_kind: "method_declaration",
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
                owner_type,
                receiver_var,
            },
        );
    }

    /// Emits a type declaration, descending struct fields and interface methods.
    fn visit_type(&mut self, node: &Node, ctx: VisitContext<'_>) {
        let Some(name) = item_name(node, self.source) else {
            return;
        };
        let body = field(node, "type");
        let (kind, raw_kind) = match body.as_ref().map(|node| node.kind()) {
            Some(kind) if kind == "struct_type" => (NodeKind::Struct, "struct_type"),
            Some(kind) if kind == "interface_type" => (NodeKind::Interface, "interface_type"),
            _ => (NodeKind::TypeAlias, "type_spec"),
        };
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
        if let Some(body) = body {
            self.visit_type_body(
                &body,
                VisitContext {
                    parent_key: &key,
                    owner_key: &key,
                    scope: ctx.scope,
                    owner_type: Some(&name),
                    receiver_var: None,
                },
            );
        }
    }

    /// Emits struct fields and interface method signatures as members of a type.
    fn visit_type_body(&mut self, node: &Node, ctx: VisitContext<'_>) {
        for child in named_children(node) {
            match child.kind().as_str() {
                "field_declaration_list" | "struct_type" => self.visit_type_body(&child, ctx),
                "field_declaration" => {
                    self.visit_member(&child, ctx, NodeKind::Field, "field_declaration")
                }
                "method_elem" | "method_spec" => {
                    self.visit_member(&child, ctx, NodeKind::Method, "method_spec");
                }
                _ => {}
            }
        }
    }

    /// Emits one struct field or interface method, qualified by its owner type.
    fn visit_member(&mut self, node: &Node, ctx: VisitContext<'_>, kind: NodeKind, raw_kind: &str) {
        let Some(name) = item_name(node, self.source) else {
            return;
        };
        let qualified = ctx.owner_type.map_or_else(
            || qualify(ctx.scope, &name),
            |owner| qualify_with_extra(ctx.scope, &[owner, &name]),
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
    }

    /// Emits a const/var binding for each declared name.
    fn visit_value(&mut self, node: &Node, ctx: VisitContext<'_>, kind: NodeKind, raw_kind: &str) {
        let Some(name) = item_name(node, self.source) else {
            return;
        };
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
    }

    /// Emits an import reference; the bound local name is the path's last
    /// segment unless an explicit alias is given.
    fn visit_import(&mut self, node: &Node, ctx: VisitContext<'_>) {
        let Some(path_node) = field(node, "path") else {
            return;
        };
        let path = import_path_segments(node_text(&path_node, self.source));
        if path.is_empty() {
            return;
        }
        let target = import_target(path, ReferenceKind::Import);
        self.push_reference(node, ctx.owner_key, target, EdgeKind::Imports);
    }

    /// Emits a call reference, then recurses.
    fn visit_call(&mut self, node: &Node, ctx: VisitContext<'_>) {
        if let Some(function) = field(node, "function")
            && let Some(target) = callee_target(&function, self.source, ctx)
        {
            self.push_reference(node, ctx.owner_key, target, EdgeKind::Calls);
        }
        self.visit_children(node, ctx);
    }

    /// Computes a symbol's fields from `node` and pushes it, returning its key.
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

/// A method's receiver: its variable name (if named) and base type.
struct ReceiverBinding {
    var: Option<String>,
    type_name: String,
}

/// Extracts a method's receiver variable and base type from its receiver list.
fn receiver_binding(list: &Node, source: &[u8]) -> Option<ReceiverBinding> {
    let declaration = named_children(list)
        .into_iter()
        .find(|child| child.kind() == "parameter_declaration")?;
    let type_name = field(&declaration, "type").and_then(|node| base_type_name(&node, source))?;
    let var = field(&declaration, "name")
        .map(|node| clean_identifier(node_text(&node, source)))
        .filter(|text| !text.is_empty());
    Some(ReceiverBinding { var, type_name })
}

/// Builds a reference target from a `call_expression`'s `function` child.
fn callee_target(function: &Node, source: &[u8], ctx: VisitContext<'_>) -> Option<ReferenceTarget> {
    match function.kind().as_str() {
        "identifier" => {
            let name = clean_identifier(node_text(function, source));
            (!name.is_empty())
                .then(|| reference_target(name.clone(), vec![name], None, ReferenceKind::Function))
        }
        "selector_expression" => {
            let name = field(function, "field")
                .map(|node| clean_identifier(node_text(&node, source)))
                .filter(|text| !text.is_empty())?;
            let operand = field(function, "operand")?;
            selector_target(&operand, name, source, ctx)
        }
        _ => None,
    }
}

/// Builds a target for `operand.name()`. A simple identifier operand becomes a
/// two-segment path (resolved by import or receiver type); the method receiver
/// is substituted for its type. Complex operands keep just the method name.
fn selector_target(
    operand: &Node,
    name: String,
    source: &[u8],
    ctx: VisitContext<'_>,
) -> Option<ReferenceTarget> {
    match operand.kind().as_str() {
        "identifier" | "type_identifier" | "package_identifier" => {
            let text = clean_identifier(node_text(operand, source));
            let base = if Some(text.as_str()) == ctx.receiver_var {
                ctx.owner_type.unwrap_or(&text).to_string()
            } else {
                text
            };
            let path = vec![base.clone(), name];
            Some(reference_target(
                path.join("::"),
                path,
                Some(base),
                ReferenceKind::Method,
            ))
        }
        _ => {
            let receiver = compact_source_text(node_text(operand, source));
            Some(reference_target(
                name.clone(),
                vec![name],
                (!receiver.is_empty()).then_some(receiver),
                ReferenceKind::Method,
            ))
        }
    }
}

/// Descends a Go type node to a stable base type name.
fn base_type_name(node: &Node, source: &[u8]) -> Option<String> {
    match node.kind().as_str() {
        "type_identifier" => {
            let text = clean_identifier(node_text(node, source));
            (!text.is_empty()).then_some(text)
        }
        "pointer_type" | "generic_type" | "parenthesized_type" => named_children(node)
            .iter()
            .find_map(|child| base_type_name(child, source)),
        "qualified_type" => field(node, "name")
            .and_then(|name| base_type_name(&name, source))
            .or_else(|| {
                let text = clean_identifier(node_text(node, source));
                text.rsplit('.')
                    .next()
                    .map(str::to_string)
                    .filter(|segment| !segment.is_empty())
            }),
        _ => {
            let text = clean_identifier(node_text(node, source));
            (!text.is_empty()).then_some(text)
        }
    }
}

/// Splits a quoted import path literal into `/`-separated segments.
fn import_path_segments(literal: &str) -> Vec<String> {
    literal
        .trim()
        .trim_matches('"')
        .trim_matches('`')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect()
}

/// Returns a declaration's name from its `name` field or first identifier child.
fn item_name(node: &Node, source: &[u8]) -> Option<String> {
    field_name(
        node,
        source,
        &[
            "identifier",
            "field_identifier",
            "type_identifier",
            "package_identifier",
        ],
    )
}

/// Go doc-comment and signature conventions.
struct GoComments;

impl CommentStrategy for GoComments {
    /// Returns contiguous `//` line comments immediately above a declaration.
    fn docstring(&self, node: &Node, source: &[u8]) -> Option<String> {
        let source = std::str::from_utf8(source).ok()?;
        let before = source.get(..node.start_byte()).unwrap_or_default();
        let mut lines = Vec::new();
        for line in before.lines().rev() {
            let trimmed = line.trim();
            if let Some(doc) = trimmed.strip_prefix("//") {
                lines.push(doc.trim_start().to_string());
                continue;
            }
            break;
        }
        lines.reverse();
        bounded_text(&lines.join("\n"), 800)
    }

    /// Returns the declaration header up to the body or assignment.
    fn signature(&self, node: &Node, source: &[u8]) -> Option<String> {
        header_signature(node_text(node, source))
    }
}

/// Returns the Go language ID.
fn go_language() -> LanguageId {
    LanguageId::from(PARSER_NAME)
}

#[cfg(test)]
mod tests {
    use oxcode_model::UnresolvedReference;

    use super::*;

    /// Extracts a snippet as `db/conn.go` in package `example.com/m/db`.
    fn extract(source: &str) -> Extraction {
        let tree = parse_go(Path::new("db/conn.go"), source.as_bytes()).expect("parse");
        extract_go(
            "db/conn.go",
            &["example.com".to_string(), "m".to_string(), "db".to_string()],
            source.as_bytes(),
            &tree,
        )
    }

    fn node<'a>(extraction: &'a Extraction, name: &str, kind: NodeKind) -> &'a SymbolNode {
        extraction
            .nodes
            .iter()
            .find(|node| node.name == name && node.kind == kind)
            .unwrap_or_else(|| panic!("no {kind:?} named {name}"))
    }

    fn reference<'a>(extraction: &'a Extraction, text_contains: &str) -> &'a UnresolvedReference {
        extraction
            .references
            .iter()
            .find(|reference| reference.text.contains(text_contains))
            .unwrap_or_else(|| panic!("no reference containing {text_contains:?}"))
    }

    const SAMPLE: &str = "package db\n\nimport \"example.com/m/util\"\n\ntype Server struct { name string }\n\ntype Store interface { Get(k string) string }\n\n// New builds a Server.\nfunc New() *Server { return &Server{} }\n\nfunc (s *Server) Run() { s.helper(); util.Log() }\n\nfunc (s *Server) helper() {}\n";

    #[test]
    fn package_function_is_qualified_at_the_import_path() {
        let extraction = extract(SAMPLE);
        let new = node(&extraction, "New", NodeKind::Function);
        assert_eq!(new.qualified_name, "example.com::m::db::New");
    }

    #[test]
    fn method_is_qualified_by_receiver_type() {
        let extraction = extract(SAMPLE);
        let run = node(&extraction, "Run", NodeKind::Method);
        assert_eq!(run.qualified_name, "example.com::m::db::Server::Run");
    }

    #[test]
    fn struct_interface_and_their_members_are_emitted() {
        let extraction = extract(SAMPLE);
        assert_eq!(
            node(&extraction, "Server", NodeKind::Struct).qualified_name,
            "example.com::m::db::Server"
        );
        assert_eq!(
            node(&extraction, "Store", NodeKind::Interface).qualified_name,
            "example.com::m::db::Store"
        );
        assert_eq!(
            node(&extraction, "Get", NodeKind::Method).qualified_name,
            "example.com::m::db::Store::Get"
        );
        assert_eq!(
            node(&extraction, "name", NodeKind::Field).qualified_name,
            "example.com::m::db::Server::name"
        );
    }

    #[test]
    fn receiver_call_resolves_to_the_owner_type() {
        let extraction = extract(SAMPLE);
        let call = reference(&extraction, "s.helper()");
        assert_eq!(
            call.target.path,
            vec!["Server".to_string(), "helper".to_string()]
        );
        assert_eq!(call.target.qualifier.as_deref(), Some("Server"));
    }

    #[test]
    fn package_call_keeps_the_package_qualifier() {
        let extraction = extract(SAMPLE);
        let call = reference(&extraction, "util.Log()");
        assert_eq!(
            call.target.path,
            vec!["util".to_string(), "Log".to_string()]
        );
        assert_eq!(call.target.qualifier.as_deref(), Some("util"));
    }

    #[test]
    fn import_expands_to_path_with_last_segment_local() {
        let extraction = extract(SAMPLE);
        let import = reference(&extraction, "example.com/m/util");
        assert_eq!(import.kind, EdgeKind::Imports);
        assert_eq!(import.target.last_segment(), Some("util"));
        assert_eq!(
            import.target.path,
            vec![
                "example.com".to_string(),
                "m".to_string(),
                "util".to_string()
            ]
        );
    }

    #[test]
    fn stable_keys_survive_byte_shifting_edits() {
        let key_of = |extraction: &Extraction, name: &str| -> String {
            node(extraction, name, NodeKind::Function)
                .stable_key
                .clone()
        };
        let original = extract("package db\nfunc alpha() {}\nfunc beta() {}\n");
        let edited =
            extract("package db\n// a shifting comment\nfunc alpha() {}\nfunc beta() {}\n");
        assert_eq!(key_of(&original, "alpha"), key_of(&edited, "alpha"));
        assert!(key_of(&original, "alpha").ends_with("#0"));
    }

    #[test]
    fn syntax_error_marks_partial_but_still_extracts() {
        let extraction = extract("package db\nfunc entry() { helper() }\nfunc broken( {");
        assert_eq!(extraction.parse_status, FileParseStatus::Partial);
        assert!(
            extraction
                .nodes
                .iter()
                .any(|node| node.qualified_name == "example.com::m::db::entry")
        );
    }
}

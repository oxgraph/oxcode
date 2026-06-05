//! TypeScript/JavaScript extractor (`.ts`/`.tsx`/`.js`/`.jsx`/`.mts`/`.cts`).
//!
//! ES modules are identified by file path, so imports are *path-based*. The
//! extractor resolves a relative import specifier (`./util`) to the target
//! module's path-anchored scope and emits a normal name-based reference target,
//! so the language-neutral resolver handles it with no special cases.

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
        scope::{JsTsScope, ScopeStrategy},
        walker::{
            CommentStrategy, ReferenceSpan, SymbolBuilder, SymbolFields, SymbolSpec, bounded_text,
            clean_identifier, compact_source_text, field_name, header_signature, import_target,
            qualify, qualify_with_extra, reference_target, source_preview, source_unit,
        },
    },
};

/// TypeScript/JavaScript tree-sitter extractor.
pub(crate) struct TypeScriptExtractor;

impl LanguageExtractor for TypeScriptExtractor {
    fn language_id(&self) -> LanguageId {
        LanguageId::from("typescript")
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["ts", "tsx", "js", "jsx", "mts", "cts", "mjs", "cjs"]
    }

    fn parser_name(&self) -> &'static str {
        "typescript"
    }

    fn extract(&self, input: ExtractionInput<'_>) -> Result<Extraction> {
        let extension = input
            .path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("ts");
        let parser_name = parser_for_extension(extension);
        let language = language_for_extension(extension);
        let scope = JsTsScope.base_scope(input.path, &input.relative_path);
        let tree = parse(input.path, parser_name, &input.source)?;
        Ok(extract_module(
            &input.relative_path,
            &scope,
            language,
            &input.source,
            &tree,
        ))
    }
}

/// Returns the tree-sitter-language-pack parser for a source extension.
fn parser_for_extension(extension: &str) -> &'static str {
    match extension {
        "tsx" => "tsx",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        _ => "typescript",
    }
}

/// Returns the reported language ID for a source extension.
fn language_for_extension(extension: &str) -> LanguageId {
    match extension {
        "js" | "jsx" | "mjs" | "cjs" => LanguageId::from("javascript"),
        _ => LanguageId::from("typescript"),
    }
}

/// Parses source with a named grammar into a syntax tree.
fn parse(path: &Path, parser_name: &str, source: &[u8]) -> Result<Tree> {
    let mut parser =
        tree_sitter_language_pack::get_parser(parser_name).map_err(|error| Error::Parse {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    parser.parse_bytes(source).ok_or_else(|| Error::Parse {
        path: path.to_path_buf(),
        message: "tree-sitter returned no parse tree".to_string(),
    })
}

/// Extracts symbols from a `<script>` body parsed as TypeScript.
///
/// `source` is expected to be the whole host file with non-script bytes masked
/// to whitespace, so the extracted spans stay accurate to the original file.
/// Used by the Svelte/Vue host extractors.
pub(crate) fn extract_script(
    relative_path: &str,
    base_scope: &[String],
    language: LanguageId,
    source: &[u8],
) -> Result<Extraction> {
    let tree = parse(Path::new(relative_path), "typescript", source)?;
    Ok(extract_module(
        relative_path,
        base_scope,
        language,
        source,
        &tree,
    ))
}

/// Extracts code graph nodes and references from one TS/JS source file.
fn extract_module(
    relative_path: &str,
    base_scope: &[String],
    language: LanguageId,
    source: &[u8],
    tree: &Tree,
) -> Extraction {
    let relative = relative_path.to_string();
    let file_key = format!("file:{relative}");
    let root = tree.root_node();

    let file_node = SymbolNode {
        stable_key: file_key.clone(),
        name: relative.clone(),
        qualified_name: base_scope.join("::"),
        kind: NodeKind::File,
        raw_kind: Some("program".to_string()),
        language: language.clone(),
        file_path: relative.clone(),
        span: span(&root),
        signature: None,
        docstring: None,
        source_preview: None,
    };

    let mut builder = SymbolBuilder::new(relative.clone(), language.clone());
    builder.push_node(file_node);
    let mut walker = TsWalker {
        source,
        relative: relative.clone(),
        builder,
        comments: TsComments,
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
        file: source_unit(&relative, language),
        parse_status,
        nodes: walker.builder.nodes,
        edges: walker.builder.edges,
        references: walker.builder.references,
    }
}

/// Stateful TS/JS CST walker.
struct TsWalker<'source> {
    source: &'source [u8],
    relative: String,
    builder: SymbolBuilder,
    comments: TsComments,
}

/// Traversal state: containing symbol (`parent_key`), attribution target
/// (`owner_key`), module `scope`, and the enclosing class/interface
/// (`owner_type`) that qualifies members and resolves `this`.
#[derive(Clone, Copy)]
struct VisitContext<'a> {
    parent_key: &'a str,
    owner_key: &'a str,
    scope: &'a [String],
    owner_type: Option<&'a str>,
}

impl TsWalker<'_> {
    /// Visits all named children under `node`.
    fn visit_children(&mut self, node: &Node, ctx: VisitContext<'_>) {
        for child in named_children(node) {
            self.visit_node(&child, ctx);
        }
    }

    /// Visits one CST node, emitting graph data when it represents code intent.
    fn visit_node(&mut self, node: &Node, ctx: VisitContext<'_>) {
        match node.kind().as_str() {
            "class_declaration" | "abstract_class_declaration" => {
                self.visit_type(node, ctx, NodeKind::Class, "class_declaration");
            }
            "interface_declaration" => {
                self.visit_type(node, ctx, NodeKind::Interface, "interface_declaration");
            }
            "enum_declaration" => self.visit_named(node, ctx, NodeKind::Enum, "enum_declaration"),
            "type_alias_declaration" => {
                self.visit_named(node, ctx, NodeKind::TypeAlias, "type_alias_declaration");
            }
            "function_declaration" | "generator_function_declaration" => {
                self.visit_function(node, ctx, "function_declaration");
            }
            "method_definition" => {
                self.visit_member(node, ctx, NodeKind::Method, "method_definition")
            }
            "public_field_definition" | "property_signature" => {
                self.visit_field(node, ctx);
            }
            "lexical_declaration" | "variable_declaration" => self.visit_variables(node, ctx),
            "internal_module" | "module" => self.visit_namespace(node, ctx),
            "import_statement" => self.visit_import(node, ctx),
            "export_statement" => self.visit_export(node, ctx),
            "call_expression" | "new_expression" => self.visit_call(node, ctx),
            _ => self.visit_children(node, ctx),
        }
    }

    /// Emits a class/interface and descends its body with it as the owner type.
    fn visit_type(&mut self, node: &Node, ctx: VisitContext<'_>, kind: NodeKind, raw_kind: &str) {
        let Some(name) = item_name(node, self.source) else {
            return self.visit_children(node, ctx);
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

    /// Emits a free function and traverses its body as the owner.
    fn visit_function(&mut self, node: &Node, ctx: VisitContext<'_>, raw_kind: &str) {
        let Some(name) = item_name(node, self.source) else {
            return self.visit_children(node, ctx);
        };
        let qualified = qualify(ctx.scope, &name);
        let key = self.push_symbol(
            node,
            SymbolSpec {
                kind: NodeKind::Function,
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
                owner_key: &key,
                scope: ctx.scope,
                owner_type: None,
            },
        );
    }

    /// Emits a class method, qualified by its owner type, and traverses its body.
    fn visit_member(&mut self, node: &Node, ctx: VisitContext<'_>, kind: NodeKind, raw_kind: &str) {
        let Some(name) = item_name(node, self.source) else {
            return self.visit_children(node, ctx);
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

    /// Emits a class field/property (no body to traverse).
    fn visit_field(&mut self, node: &Node, ctx: VisitContext<'_>) {
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
                kind: NodeKind::Field,
                raw_kind: "field",
                name: &name,
                qualified_name: &qualified,
            },
        );
        self.push_edge(ctx.parent_key, &key, EdgeKind::Contains);
    }

    /// Emits a top-level named item with no owner type.
    fn visit_named(&mut self, node: &Node, ctx: VisitContext<'_>, kind: NodeKind, raw_kind: &str) {
        let Some(name) = item_name(node, self.source) else {
            return self.visit_children(node, ctx);
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

    /// Emits `const`/`let`/`var` bindings; an arrow/function initializer makes
    /// the binding a `Function`.
    fn visit_variables(&mut self, node: &Node, ctx: VisitContext<'_>) {
        for declarator in named_children(node) {
            if declarator.kind() != "variable_declarator" {
                continue;
            }
            let Some(name) = field(&declarator, "name")
                .map(|node| clean_identifier(node_text(&node, self.source)))
                .filter(|text| !text.is_empty())
            else {
                continue;
            };
            let is_callable = field(&declarator, "value").is_some_and(|value| {
                matches!(
                    value.kind().as_str(),
                    "arrow_function" | "function" | "function_expression"
                )
            });
            let kind = if is_callable {
                NodeKind::Function
            } else {
                NodeKind::Variable
            };
            let qualified = qualify(ctx.scope, &name);
            let key = self.push_symbol(
                &declarator,
                SymbolSpec {
                    kind,
                    raw_kind: "variable_declarator",
                    name: &name,
                    qualified_name: &qualified,
                },
            );
            self.push_edge(ctx.parent_key, &key, EdgeKind::Contains);
            self.visit_children(
                &declarator,
                VisitContext {
                    parent_key: &key,
                    owner_key: &key,
                    scope: ctx.scope,
                    owner_type: None,
                },
            );
        }
    }

    /// Emits a namespace and recurses into its body with an extended scope.
    fn visit_namespace(&mut self, node: &Node, ctx: VisitContext<'_>) {
        let Some(name) = item_name(node, self.source) else {
            return self.visit_children(node, ctx);
        };
        let qualified = qualify(ctx.scope, &name);
        let key = self.push_symbol(
            node,
            SymbolSpec {
                kind: NodeKind::Namespace,
                raw_kind: "internal_module",
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

    /// Unwraps an export statement, dispatching the exported declaration.
    fn visit_export(&mut self, node: &Node, ctx: VisitContext<'_>) {
        if let Some(declaration) = field(node, "declaration") {
            self.visit_node(&declaration, ctx);
        } else {
            self.visit_children(node, ctx);
        }
    }

    /// Emits one import reference per imported binding, resolving relative
    /// specifiers to the target module's path-anchored scope.
    fn visit_import(&mut self, node: &Node, ctx: VisitContext<'_>) {
        let Some(source_node) = field(node, "source") else {
            return;
        };
        let specifier = clean_string_literal(node_text(&source_node, self.source));
        let Some(anchor) = module_anchor_from_relative_import(&self.relative, &specifier) else {
            return;
        };
        let Some(clause) = named_children(node)
            .into_iter()
            .find(|child| child.kind() == "import_clause")
        else {
            return;
        };
        for binding in import_bindings(&clause, self.source) {
            // A namespace import (`* as ns`) binds the whole module, so it
            // anchors at the module itself; named/default imports append the
            // imported name so the resolver binds that local name.
            let mut path = anchor.clone();
            let kind = if binding.namespace {
                ReferenceKind::ImportGlob
            } else {
                path.push(binding.imported);
                ReferenceKind::Import
            };
            let target = import_target(path, kind);
            self.push_reference(node, ctx.owner_key, target, EdgeKind::Imports);
        }
    }

    /// Emits a call/construction reference, then recurses.
    fn visit_call(&mut self, node: &Node, ctx: VisitContext<'_>) {
        let callee = field(node, "function").or_else(|| field(node, "constructor"));
        if let Some(callee) = callee
            && let Some(target) = callee_target(&callee, self.source, ctx)
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

/// One imported binding: the imported (source-module) name and whether it is a
/// namespace import (`* as ns`).
struct ImportBinding {
    imported: String,
    namespace: bool,
}

/// Collects the bindings introduced by an `import_clause`.
fn import_bindings(clause: &Node, source: &[u8]) -> Vec<ImportBinding> {
    let mut bindings = Vec::new();
    for child in named_children(clause) {
        match child.kind().as_str() {
            // `import Default from '...'`
            "identifier" => bindings.push(ImportBinding {
                imported: "default".to_string(),
                namespace: false,
            }),
            // `import * as ns from '...'`
            "namespace_import" => {
                if let Some(name) = field_name(&child, source, &["identifier"]) {
                    bindings.push(ImportBinding {
                        imported: name,
                        namespace: true,
                    });
                }
            }
            // `import { a, b as c } from '...'`
            "named_imports" => bindings.extend(named_import_bindings(&child, source)),
            _ => {}
        }
    }
    bindings
}

/// Collects the imported names from a `named_imports` clause.
fn named_import_bindings(named: &Node, source: &[u8]) -> Vec<ImportBinding> {
    named_children(named)
        .iter()
        .filter(|specifier| specifier.kind() == "import_specifier")
        .filter_map(|specifier| field(specifier, "name"))
        .map(|name| clean_identifier(node_text(&name, source)))
        .filter(|name| !name.is_empty())
        .map(|imported| ImportBinding {
            imported,
            namespace: false,
        })
        .collect()
}

/// Resolves a relative import specifier against the importing file to the
/// target module's path-anchored scope segments (matching [`JsTsScope`]).
///
/// Bare/package specifiers (not starting with `.`) are external and return
/// `None`.
fn module_anchor_from_relative_import(
    importer_relative: &str,
    specifier: &str,
) -> Option<Vec<String>> {
    if !specifier.starts_with('.') {
        return None;
    }
    let importer_dir = Path::new(importer_relative)
        .parent()
        .unwrap_or(Path::new(""));
    let mut segments: Vec<String> = Vec::new();
    for component in importer_dir.join(specifier).components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                segments.pop();
            }
            std::path::Component::Normal(part) => segments.push(part.to_string_lossy().to_string()),
            _ => {}
        }
    }
    if let Some(last) = segments.last_mut() {
        for extension in [
            ".d.ts", ".ts", ".tsx", ".js", ".jsx", ".mts", ".cts", ".mjs", ".cjs",
        ] {
            if let Some(stripped) = last.strip_suffix(extension) {
                *last = stripped.to_string();
                break;
            }
        }
    }
    if segments.last().is_some_and(|segment| segment == "index") {
        segments.pop();
    }
    (!segments.is_empty()).then_some(segments)
}

/// Builds a reference target from a call/new callee expression.
fn callee_target(callee: &Node, source: &[u8], ctx: VisitContext<'_>) -> Option<ReferenceTarget> {
    match callee.kind().as_str() {
        "identifier" => {
            let name = clean_identifier(node_text(callee, source));
            (!name.is_empty())
                .then(|| reference_target(name.clone(), vec![name], None, ReferenceKind::Function))
        }
        "member_expression" => {
            let name = field(callee, "property")
                .map(|node| clean_identifier(node_text(&node, source)))
                .filter(|text| !text.is_empty())?;
            let object = field(callee, "object")?;
            Some(member_target(&object, name, source, ctx))
        }
        _ => None,
    }
}

/// Builds a target for `object.name()`. A simple identifier object becomes a
/// two-segment path (resolved by import or receiver type); `this` resolves to
/// the enclosing type. Complex objects keep just the method name.
fn member_target(
    object: &Node,
    name: String,
    source: &[u8],
    ctx: VisitContext<'_>,
) -> ReferenceTarget {
    match object.kind().as_str() {
        "identifier" | "this" => {
            let text = clean_identifier(node_text(object, source));
            let base = if text == "this" {
                ctx.owner_type.unwrap_or("this").to_string()
            } else {
                text
            };
            let path = vec![base.clone(), name];
            reference_target(path.join("::"), path, Some(base), ReferenceKind::Method)
        }
        _ => {
            let receiver = compact_source_text(node_text(object, source));
            reference_target(
                name.clone(),
                vec![name],
                (!receiver.is_empty()).then_some(receiver),
                ReferenceKind::Method,
            )
        }
    }
}

/// Strips quotes from a string literal's source text.
fn clean_string_literal(text: &str) -> String {
    text.trim().trim_matches(['"', '\'', '`']).to_string()
}

/// Returns a declaration's name from its `name` field or first identifier child.
fn item_name(node: &Node, source: &[u8]) -> Option<String> {
    field_name(
        node,
        source,
        &["identifier", "type_identifier", "property_identifier"],
    )
}

/// TypeScript/JavaScript doc-comment and signature conventions.
struct TsComments;

impl CommentStrategy for TsComments {
    /// Returns the `/** … */` or `//` comment block directly above an item.
    fn docstring(&self, node: &Node, source: &[u8]) -> Option<String> {
        let source = std::str::from_utf8(source).ok()?;
        let before = source.get(..node.start_byte()).unwrap_or_default();
        let lines = doc_lines_above(before);
        bounded_text(&lines.join("\n"), 800)
    }

    /// Returns the declaration header up to the body, `=>`, or `;`.
    fn signature(&self, node: &Node, source: &[u8]) -> Option<String> {
        let text = node_text(node, source);
        let header = text.split("=>").next().unwrap_or(text);
        header_signature(header)
    }
}

/// Collects a contiguous `/** … */` or `//` comment block immediately above an
/// item, cleaned of comment markers.
fn doc_lines_above(before: &str) -> Vec<String> {
    let trimmed_end = before.trim_end();
    if trimmed_end.ends_with("*/") {
        if let Some(start) = trimmed_end.rfind("/*") {
            return trimmed_end[start + 2..trimmed_end.len() - 2]
                .lines()
                .map(|line| line.trim().trim_start_matches('*').trim().to_string())
                .filter(|line| !line.is_empty())
                .collect();
        }
        return Vec::new();
    }
    let mut lines = Vec::new();
    for line in before.lines().rev() {
        let line = line.trim();
        if let Some(doc) = line.strip_prefix("//") {
            lines.push(doc.trim().to_string());
            continue;
        }
        break;
    }
    lines.reverse();
    lines
}

#[cfg(test)]
mod tests {
    use oxcode_model::UnresolvedReference;

    use super::*;

    const SAMPLE: &str = "import { Helper, Other as O } from './util';\nimport * as ns from './lib/n';\n\n/** Greeter greets. */\nexport class Greeter {\n  name: string;\n  greet() { this.build(); Helper(); ns.run(); }\n  build() {}\n}\n\nexport function entry() { return Helper(); }\n\nexport const run = () => entry();\n";

    /// Extracts a snippet as `src/app.ts`.
    fn extract(source: &str) -> Extraction {
        let tree = parse(Path::new("src/app.ts"), "typescript", source.as_bytes()).expect("parse");
        extract_module(
            "src/app.ts",
            &["src".to_string(), "app".to_string()],
            LanguageId::from("typescript"),
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

    #[test]
    fn class_method_and_field_are_qualified_by_owner() {
        let extraction = extract(SAMPLE);
        assert_eq!(
            node(&extraction, "Greeter", NodeKind::Class).qualified_name,
            "src::app::Greeter"
        );
        assert_eq!(
            node(&extraction, "greet", NodeKind::Method).qualified_name,
            "src::app::Greeter::greet"
        );
        assert_eq!(
            node(&extraction, "name", NodeKind::Field).qualified_name,
            "src::app::Greeter::name"
        );
    }

    #[test]
    fn function_and_arrow_const_are_functions() {
        let extraction = extract(SAMPLE);
        assert_eq!(
            node(&extraction, "entry", NodeKind::Function).qualified_name,
            "src::app::entry"
        );
        // `const run = () => ...` is a callable binding, so it is a Function.
        assert_eq!(
            node(&extraction, "run", NodeKind::Function).qualified_name,
            "src::app::run"
        );
    }

    #[test]
    fn named_import_resolves_to_target_module_anchor() {
        let extraction = extract(SAMPLE);
        let import = reference(&extraction, "./util");
        assert_eq!(import.kind, EdgeKind::Imports);
        // `./util` from `src/app.ts` anchors at `src::util`.
        assert_eq!(import.target.last_segment(), Some("Helper"));
        assert_eq!(
            import.target.path,
            vec!["src".to_string(), "util".to_string(), "Helper".to_string()]
        );
    }

    #[test]
    fn namespace_import_anchors_at_the_module() {
        let extraction = extract(SAMPLE);
        let import = reference(&extraction, "./lib/n");
        assert_eq!(import.target.kind_hint, ReferenceKind::ImportGlob);
        // `* as ns` binds the whole module `src/lib/n`, not a sub-name.
        assert_eq!(
            import.target.path,
            vec!["src".to_string(), "lib".to_string(), "n".to_string()]
        );
    }

    #[test]
    fn this_method_call_resolves_to_the_owner_type() {
        let extraction = extract(SAMPLE);
        let call = reference(&extraction, "this.build()");
        assert_eq!(
            call.target.path,
            vec!["Greeter".to_string(), "build".to_string()]
        );
        assert_eq!(call.target.qualifier.as_deref(), Some("Greeter"));
    }

    #[test]
    fn bare_and_member_calls_extract_names() {
        let extraction = extract(SAMPLE);
        assert_eq!(
            reference(&extraction, "Helper()").target.path,
            vec!["Helper".to_string()]
        );
        let member = reference(&extraction, "ns.run()");
        assert_eq!(
            member.target.path,
            vec!["ns".to_string(), "run".to_string()]
        );
        assert_eq!(member.target.qualifier.as_deref(), Some("ns"));
    }

    #[test]
    fn index_file_collapses_to_its_directory() {
        // An `index.ts` import resolves to the directory anchor.
        let source =
            "import { Thing } from './widgets';\nexport function use() { return Thing(); }\n";
        let extraction = extract(source);
        let import = reference(&extraction, "./widgets");
        assert_eq!(
            import.target.path,
            vec![
                "src".to_string(),
                "widgets".to_string(),
                "Thing".to_string()
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
        let original = extract("export function alpha() {}\nexport function beta() {}\n");
        let edited = extract("// shift\nexport function alpha() {}\nexport function beta() {}\n");
        assert_eq!(key_of(&original, "alpha"), key_of(&edited, "alpha"));
        assert!(key_of(&original, "alpha").ends_with("#0"));
    }

    #[test]
    fn syntax_error_marks_partial_but_still_extracts() {
        let extraction = extract("export function entry() { helper(); }\nfunction broken( {");
        assert_eq!(extraction.parse_status, FileParseStatus::Partial);
        assert!(
            extraction
                .nodes
                .iter()
                .any(|node| node.qualified_name == "src::app::entry")
        );
    }
}

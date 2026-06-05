//! Generic, query-driven extractor shared by every [`LanguageProfile`].
//!
//! Where the hand-written extractors walk the CST, this one runs a tree-sitter
//! query and reconstructs structure from byte-span nesting: definitions are
//! sorted and stacked so the innermost enclosing definition is each symbol's
//! parent, which yields containment edges and qualified-name prefixes. Fidelity
//! is lower than a hand-written extractor (references resolve only at the
//! scoped/simple tiers), but adding a language costs only a query and a profile.
//!
//! The grammar comes from the statically-linked [`crate::extract::grammar`]
//! registry; this path runs a `tree_sitter::Query` against the parse tree.

use std::{path::Path, sync::OnceLock};

use oxcode_model::{
    EdgeKind, Extraction, FileParseStatus, LanguageId, NodeKind, ReferenceKind, SourceSpan,
    SymbolNode,
};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Parser, Query, QueryCursor};

use crate::{
    error::{Error, Result},
    extract::{
        ExtractionInput, LanguageExtractor, grammar,
        profile::{CaptureRole, LanguageProfile},
        scope,
        walker::{
            ReferenceSpan, SymbolBuilder, SymbolFields, SymbolSpec, bounded_preview, bounded_text,
            compact_source_text, header_signature, qualify_with_extra, reference_target,
            source_unit,
        },
    },
};

/// Generic extractor instantiated from one static [`LanguageProfile`].
pub(crate) struct QueryExtractor {
    profile: &'static LanguageProfile,
    compiled: OnceLock<Compiled>,
}

/// A profile's lazily compiled grammar, query, and capture-index→role table.
struct Compiled {
    language: tree_sitter::Language,
    query: Query,
    /// Role per capture index, resolved once from `query.capture_names()`.
    roles: Vec<Option<CaptureRole>>,
}

impl QueryExtractor {
    /// Creates an extractor for `profile`; the query compiles on first use.
    pub(crate) const fn new(profile: &'static LanguageProfile) -> Self {
        Self {
            profile,
            compiled: OnceLock::new(),
        }
    }

    /// Returns the compiled grammar/query, compiling and caching it on first use.
    fn compiled(&self, path: &Path) -> Result<&Compiled> {
        if let Some(compiled) = self.compiled.get() {
            return Ok(compiled);
        }
        let compiled = self.build_compiled(path)?;
        // A lost race just rebuilds once; the stored value still wins.
        let _ = self.compiled.set(compiled);
        Ok(self.compiled.get().expect("compiled set"))
    }

    /// Compiles the grammar and query and resolves the capture-role table.
    fn build_compiled(&self, path: &Path) -> Result<Compiled> {
        let language = grammar::language(self.profile.parser_name).ok_or_else(|| Error::Parse {
            path: path.to_path_buf(),
            message: format!("no bundled grammar {:?}", self.profile.parser_name),
        })?;
        let query =
            Query::new(&language, self.profile.query_source).map_err(|error| Error::Parse {
                path: path.to_path_buf(),
                message: format!("query for {:?}: {error}", self.profile.language_id),
            })?;
        let roles = query
            .capture_names()
            .iter()
            .map(|name| self.profile.role(name))
            .collect();
        Ok(Compiled {
            language,
            query,
            roles,
        })
    }
}

impl LanguageExtractor for QueryExtractor {
    fn language_id(&self) -> LanguageId {
        LanguageId::from(self.profile.language_id)
    }

    fn extensions(&self) -> &'static [&'static str] {
        self.profile.extensions
    }

    fn extract(&self, input: ExtractionInput<'_>) -> Result<Extraction> {
        let compiled = self.compiled(input.path)?;
        let scope =
            scope::strategy_for(self.profile.scope).base_scope(input.path, &input.relative_path);
        let mut parser = Parser::new();
        parser
            .set_language(&compiled.language)
            .map_err(|error| Error::Parse {
                path: input.path.to_path_buf(),
                message: error.to_string(),
            })?;
        let tree = parser
            .parse(&input.source, None)
            .ok_or_else(|| Error::Parse {
                path: input.path.to_path_buf(),
                message: "tree-sitter returned no parse tree".to_string(),
            })?;
        Ok(self.extract_tree(&input, &scope, &tree, compiled))
    }
}

impl QueryExtractor {
    /// Runs the query and assembles the extraction from matched captures.
    fn extract_tree(
        &self,
        input: &ExtractionInput<'_>,
        scope: &[String],
        tree: &tree_sitter::Tree,
        compiled: &Compiled,
    ) -> Extraction {
        let relative = input.relative_path.clone();
        let source = input.source.as_slice();
        let file_key = format!("file:{relative}");
        let language = LanguageId::from(self.profile.language_id);
        let root = tree.root_node();

        let file_node = SymbolNode {
            stable_key: file_key.clone(),
            name: relative.clone(),
            qualified_name: scope.join("::"),
            kind: NodeKind::File,
            raw_kind: Some(root.kind().to_string()),
            language: language.clone(),
            file_path: relative.clone(),
            span: node_span(&root),
            signature: None,
            docstring: None,
            source_preview: None,
        };

        let mut builder = SymbolBuilder::new(relative.clone(), language.clone());
        builder.push_node(file_node);

        let (mut defs, refs) = self.collect(&root, source, compiled);
        // Sort so a definition's true ancestors precede it and nest by span:
        // earliest start first, and on ties the widest (outer) range first.
        defs.sort_by(|left, right| left.start.cmp(&right.start).then(right.end.cmp(&left.end)));

        let placed = place_definitions(&mut builder, scope, &file_key, &defs);
        attach_references(&mut builder, &file_key, &placed, &refs);

        let parse_status = if root.has_error() {
            FileParseStatus::Partial
        } else {
            FileParseStatus::Ok
        };

        Extraction {
            file: source_unit(&relative, language),
            parse_status,
            nodes: builder.nodes,
            edges: builder.edges,
            references: builder.references,
        }
    }

    /// Runs the query and splits matched captures into definitions and
    /// references, precomputing each definition's text-derived fields.
    fn collect(
        &self,
        root: &Node,
        source: &[u8],
        compiled: &Compiled,
    ) -> (Vec<RawDef>, Vec<RawRef>) {
        let mut defs = Vec::new();
        let mut refs = Vec::new();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&compiled.query, *root, source);
        while let Some(matched) = matches.next() {
            let captured = interpret(matched.captures, &compiled.roles, source);
            let Some(name) = captured.name.filter(|text| !text.is_empty()) else {
                continue;
            };
            if let Some((node, kind)) = captured.anchor {
                defs.push(self.raw_def(&node, kind, name, source));
            } else if let Some((node, edge, hint)) = captured.reference {
                refs.push(RawRef {
                    start: node.start_byte(),
                    end: node.end_byte(),
                    span: node_span(&node),
                    name,
                    qualifier: captured.qualifier.filter(|text| !text.is_empty()),
                    edge,
                    hint,
                    text: compact_source_text(&node_text(&node, source)),
                });
            }
        }
        (defs, refs)
    }

    /// Builds a definition record, precomputing its signature/doc/preview.
    fn raw_def(&self, node: &Node, kind: NodeKind, name: String, source: &[u8]) -> RawDef {
        let text = node_text(node, source);
        RawDef {
            start: node.start_byte(),
            end: node.end_byte(),
            span: node_span(node),
            name,
            kind,
            raw_kind: node.kind(),
            signature: header_signature(&text),
            docstring: doc_above(self.profile.doc_prefixes, source, node.start_byte()),
            preview: bounded_preview(&text),
        }
    }
}

/// The captures of interest from one query match.
struct Captured<'tree> {
    anchor: Option<(Node<'tree>, NodeKind)>,
    reference: Option<(Node<'tree>, EdgeKind, ReferenceKind)>,
    name: Option<String>,
    qualifier: Option<String>,
}

/// Reduces one match's captures to its anchor/reference/name/qualifier by role.
fn interpret<'tree>(
    captures: &[tree_sitter::QueryCapture<'tree>],
    roles: &[Option<CaptureRole>],
    source: &[u8],
) -> Captured<'tree> {
    let mut captured = Captured {
        anchor: None,
        reference: None,
        name: None,
        qualifier: None,
    };
    for capture in captures {
        match roles.get(capture.index as usize).copied().flatten() {
            Some(CaptureRole::Definition(kind)) => captured.anchor = Some((capture.node, kind)),
            Some(CaptureRole::Reference { edge, hint }) => {
                captured.reference = Some((capture.node, edge, hint));
            }
            Some(CaptureRole::Name) => captured.name = Some(node_text(&capture.node, source)),
            Some(CaptureRole::Qualifier) => {
                captured.qualifier = Some(node_text(&capture.node, source));
            }
            None => {}
        }
    }
    captured
}

/// Emits each definition by span nesting, returning each placed definition's
/// byte range and stable key so references can be attributed to an owner.
fn place_definitions(
    builder: &mut SymbolBuilder,
    scope: &[String],
    file_key: &str,
    defs: &[RawDef],
) -> Vec<(usize, usize, String)> {
    let mut stack: Vec<Frame> = Vec::new();
    let mut placed: Vec<(usize, usize, String)> = Vec::new();
    for def in defs {
        while stack.last().is_some_and(|frame| frame.end <= def.start) {
            stack.pop();
        }
        let parent_key = stack.last().map_or(file_key, |frame| frame.key.as_str());
        let prefix = stack
            .iter()
            .map(|frame| frame.name.as_str())
            .chain(std::iter::once(def.name.as_str()))
            .collect::<Vec<_>>();
        let qualified = qualify_with_extra(scope, &prefix);
        let key = builder.push_symbol(
            SymbolSpec {
                kind: def.kind,
                raw_kind: def.raw_kind,
                name: &def.name,
                qualified_name: &qualified,
            },
            SymbolFields {
                span: def.span,
                signature: def.signature.clone(),
                docstring: def.docstring.clone(),
                source_preview: def.preview.clone(),
            },
        );
        builder.push_edge(parent_key, &key, EdgeKind::Contains);
        placed.push((def.start, def.end, key.clone()));
        stack.push(Frame {
            end: def.end,
            name: def.name.clone(),
            key,
        });
    }
    placed
}

/// Attributes each reference to its innermost containing definition (or the
/// file) and emits it for cross-file resolution.
fn attach_references(
    builder: &mut SymbolBuilder,
    file_key: &str,
    placed: &[(usize, usize, String)],
    refs: &[RawRef],
) {
    for reference in refs {
        let owner = innermost_owner(placed, reference.start, reference.end).unwrap_or(file_key);
        let target = reference_target(
            reference.name.clone(),
            vec![reference.name.clone()],
            reference.qualifier.clone(),
            reference.hint,
        );
        builder.push_reference(
            owner,
            target,
            reference.edge,
            ReferenceSpan {
                span: reference.span,
                text: reference.text.clone(),
            },
        );
    }
}

/// One definition with its text-derived fields precomputed.
struct RawDef {
    start: usize,
    end: usize,
    span: SourceSpan,
    name: String,
    kind: NodeKind,
    raw_kind: &'static str,
    signature: Option<String>,
    docstring: Option<String>,
    preview: Option<String>,
}

/// One reference (call/import) awaiting attribution and resolution.
struct RawRef {
    start: usize,
    end: usize,
    span: SourceSpan,
    name: String,
    qualifier: Option<String>,
    edge: EdgeKind,
    hint: ReferenceKind,
    text: String,
}

/// A definition currently open on the containment stack.
struct Frame {
    end: usize,
    name: String,
    key: String,
}

/// Returns the stable key of the innermost placed definition containing a
/// reference's byte range, if any.
fn innermost_owner(placed: &[(usize, usize, String)], start: usize, end: usize) -> Option<&str> {
    placed
        .iter()
        .filter(|(def_start, def_end, _)| *def_start <= start && end <= *def_end)
        .max_by_key(|(def_start, _, _)| *def_start)
        .map(|(_, _, key)| key.as_str())
}

/// Returns a node's UTF-8 source text (empty on invalid UTF-8).
fn node_text(node: &Node, source: &[u8]) -> String {
    node.utf8_text(source).unwrap_or_default().to_string()
}

/// Converts a node's position to the stored span representation.
fn node_span(node: &Node) -> SourceSpan {
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

/// Collects contiguous line-comment documentation immediately above
/// `start_byte`, matching any of `prefixes` (empty disables extraction).
fn doc_above(prefixes: &[&str], source: &[u8], start_byte: usize) -> Option<String> {
    if prefixes.is_empty() {
        return None;
    }
    let text = std::str::from_utf8(source).ok()?;
    let before = text.get(..start_byte).unwrap_or_default();
    let mut lines = Vec::new();
    for line in before.lines().rev() {
        let trimmed = line.trim();
        let stripped = prefixes
            .iter()
            .find_map(|prefix| trimmed.strip_prefix(prefix));
        match stripped {
            Some(doc) => lines.push(doc.trim().to_string()),
            None => break,
        }
    }
    lines.reverse();
    bounded_text(&lines.join("\n"), 800)
}

#[cfg(test)]
mod tests {
    use oxcode_model::UnresolvedReference;

    use super::*;
    use crate::extract::profiles::PROFILES;

    fn profile(language_id: &str) -> &'static LanguageProfile {
        PROFILES
            .iter()
            .find(|profile| profile.language_id == language_id)
            .unwrap_or_else(|| panic!("no profile for {language_id}"))
    }

    fn extract(language_id: &str, relative: &str, source: &str) -> Extraction {
        QueryExtractor::new(profile(language_id))
            .extract(ExtractionInput {
                path: Path::new(relative),
                relative_path: relative.to_string(),
                source: source.as_bytes().to_vec(),
            })
            .expect("extract")
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
    fn every_profile_query_compiles() {
        // A malformed `.scm` or a capture name absent from the query would fail
        // here rather than silently skipping every file at runtime.
        for profile in PROFILES {
            QueryExtractor::new(profile)
                .build_compiled(Path::new("probe"))
                .unwrap_or_else(|error| panic!("{}: {error}", profile.language_id));
        }
    }

    #[test]
    fn generic_languages_extract_expected_symbols() {
        struct Case {
            lang: &'static str,
            relative: &'static str,
            source: &'static str,
            want: &'static [(&'static str, NodeKind)],
        }
        let cases = [
            Case {
                lang: "csharp",
                relative: "App.cs",
                source: "class C { void M() { Helper(); } void Helper() {} }",
                want: &[("C", NodeKind::Class), ("M", NodeKind::Method)],
            },
            Case {
                lang: "php",
                relative: "app.php",
                source: "<?php\nfunction helper() {}\nclass C { function m() {} }",
                want: &[("helper", NodeKind::Function), ("m", NodeKind::Method)],
            },
            Case {
                lang: "ruby",
                relative: "app.rb",
                source: "class C\n  def m\n  end\nend\n",
                want: &[("C", NodeKind::Class), ("m", NodeKind::Method)],
            },
            Case {
                lang: "swift",
                relative: "app.swift",
                source: "class C {}\nfunc top() {}\n",
                want: &[("C", NodeKind::Class), ("top", NodeKind::Function)],
            },
            Case {
                lang: "kotlin",
                relative: "app.kt",
                source: "class C\nfun top() {}\n",
                want: &[("C", NodeKind::Class), ("top", NodeKind::Function)],
            },
            Case {
                lang: "scala",
                relative: "app.scala",
                source: "object O { def m(): Unit = {} }\n",
                want: &[("O", NodeKind::Class), ("m", NodeKind::Function)],
            },
            Case {
                lang: "dart",
                relative: "app.dart",
                source: "class C { void m() {} }\nvoid top() {}\n",
                want: &[("C", NodeKind::Class), ("top", NodeKind::Function)],
            },
            Case {
                lang: "lua",
                relative: "app.lua",
                source: "function entry()\n  helper()\nend\n",
                want: &[("entry", NodeKind::Function)],
            },
            Case {
                lang: "luau",
                relative: "app.luau",
                source: "function entry()\nend\n",
                want: &[("entry", NodeKind::Function)],
            },
            Case {
                lang: "objc",
                relative: "app.m",
                source: "@implementation C\n- (void)m {}\n@end\n",
                want: &[("C", NodeKind::Class), ("m", NodeKind::Method)],
            },
            Case {
                lang: "pascal",
                relative: "app.pas",
                source: "function Entry: Integer;\nbegin\nend;\n",
                want: &[("Entry", NodeKind::Function)],
            },
        ];
        for case in cases {
            let extraction = extract(case.lang, case.relative, case.source);
            for (name, kind) in case.want {
                assert!(
                    extraction
                        .nodes
                        .iter()
                        .any(|node| node.name == *name && node.kind == *kind),
                    "{}: expected {kind:?} {name}, got {:?}",
                    case.lang,
                    extraction
                        .nodes
                        .iter()
                        .map(|node| (node.name.as_str(), node.kind))
                        .collect::<Vec<_>>()
                );
            }
        }
    }

    #[test]
    fn python_functions_and_calls_are_extracted() {
        let source = "def helper():\n    return 1\n\ndef entry():\n    return helper()\n";
        let extraction = extract("python", "mod.py", source);
        assert_eq!(
            node(&extraction, "helper", NodeKind::Function).qualified_name,
            "mod::helper"
        );
        assert_eq!(
            node(&extraction, "entry", NodeKind::Function).qualified_name,
            "mod::entry"
        );
        assert_eq!(
            reference(&extraction, "helper()").target.path,
            vec!["helper".to_string()]
        );
    }

    #[test]
    fn python_method_nests_under_its_class_by_span() {
        let source = "class Service:\n    def run(self):\n        return self.helper()\n\n    def helper(self):\n        return 1\n";
        let extraction = extract("python", "mod.py", source);
        assert_eq!(
            node(&extraction, "Service", NodeKind::Class).qualified_name,
            "mod::Service"
        );
        // Span nesting qualifies the method under its class.
        assert_eq!(
            node(&extraction, "run", NodeKind::Function).qualified_name,
            "mod::Service::run"
        );
        // `self.helper()` keeps the receiver as a qualifier.
        let call = reference(&extraction, "self.helper()");
        assert_eq!(call.target.path, vec!["helper".to_string()]);
        assert_eq!(call.target.qualifier.as_deref(), Some("self"));
    }

    #[test]
    fn python_partial_parse_still_extracts() {
        let extraction = extract(
            "python",
            "mod.py",
            "def entry():\n    helper()\ndef broken(",
        );
        assert_eq!(extraction.parse_status, FileParseStatus::Partial);
        assert!(
            extraction
                .nodes
                .iter()
                .any(|node| node.qualified_name == "mod::entry")
        );
    }
}

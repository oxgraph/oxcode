//! Crate-aware, scope/import/receiver-tiered reference resolution.
//!
//! The resolver is language-neutral: it consumes the structured
//! [`ReferenceTarget`]s the extractor produces (already normalized) and does no
//! language-specific string surgery. For each reference it tries, in order:
//! exact crate-qualified name, enclosing-module scope, in-scope imports,
//! receiver type, then bare simple name. It emits the best non-empty tier and,
//! when that tier is ambiguous, keeps every candidate as an edge marked
//! [`ResolutionKind::Ambiguous`] rather than silently dropping it.

use std::collections::{BTreeMap, BTreeSet};

use oxcode_model::{
    EdgeKind, Extraction, ReferenceKind, ReferenceSite, ResolutionKind, ResolvedEdge,
    ResolvedIndex, SymbolNode, UnresolvedReference,
};

use crate::error::Result;

/// Resolves all file extractions into symbolic graph data.
pub fn resolve_extractions(extractions: Vec<Extraction>) -> Result<ResolvedIndex> {
    let mut files = Vec::with_capacity(extractions.len());
    let mut symbols = Vec::new();
    let mut symbolic_edges = Vec::new();
    let mut references = Vec::new();

    for extraction in extractions {
        files.push(extraction.file);
        symbols.extend(extraction.nodes);
        symbolic_edges.extend(extraction.edges);
        references.extend(extraction.references);
    }

    files.sort_by(|left, right| left.path.cmp(&right.path));
    symbols.sort_by(|left, right| left.stable_key.cmp(&right.stable_key));
    dedupe_symbols(&mut symbols);

    let index = SymbolIndex::build(&symbols);
    let imports = ImportMap::build(&references);

    let mut edge_set = BTreeSet::new();
    for edge in symbolic_edges {
        if index.contains(&edge.source_key) && index.contains(&edge.target_key) {
            edge_set.insert(ResolvedEdge {
                source_key: edge.source_key,
                target_key: edge.target_key,
                kind: edge.kind,
                resolution: ResolutionKind::Exact,
                reference: None,
            });
        }
    }

    let mut unresolved = Vec::new();
    for reference in references {
        if !index.contains(&reference.source_key) {
            continue;
        }
        match index.resolve(&reference, &imports) {
            Resolution::Resolved { targets, kind } => {
                let site = ReferenceSite {
                    file_path: reference.file_path.clone(),
                    span: reference.span,
                    text: reference.text.clone(),
                };
                for target_key in targets {
                    edge_set.insert(ResolvedEdge {
                        source_key: reference.source_key.clone(),
                        target_key,
                        kind: reference.kind,
                        resolution: kind,
                        reference: Some(site.clone()),
                    });
                }
            }
            Resolution::Unresolved(reason) => {
                let mut unresolved_reference = reference;
                unresolved_reference.reason = Some(reason);
                unresolved.push(unresolved_reference);
            }
        }
    }

    Ok(ResolvedIndex {
        files,
        nodes: symbols,
        edges: edge_set.into_iter().collect(),
        unresolved,
    })
}

/// Outcome of resolving one reference.
enum Resolution {
    /// One or more target keys with the tier that matched.
    Resolved {
        targets: Vec<String>,
        kind: ResolutionKind,
    },
    /// Could not resolve, with reason.
    Unresolved(String),
}

/// Removes duplicated stable keys, keeping the first deterministic entry.
fn dedupe_symbols(symbols: &mut Vec<SymbolNode>) {
    let mut seen = BTreeSet::new();
    symbols.retain(|symbol| seen.insert(symbol.stable_key.clone()));
}

/// File-scoped import map: local name -> imported path segments.
struct ImportMap {
    by_file: BTreeMap<String, BTreeMap<String, Vec<String>>>,
}

impl ImportMap {
    fn build(references: &[UnresolvedReference]) -> Self {
        let mut by_file = BTreeMap::<String, BTreeMap<String, Vec<String>>>::new();
        for reference in references {
            if reference.kind != EdgeKind::Imports
                || reference.target.kind_hint == ReferenceKind::ImportGlob
            {
                continue;
            }
            if let Some(local) = reference.target.last_segment() {
                by_file
                    .entry(reference.file_path.clone())
                    .or_default()
                    .entry(local.to_string())
                    .or_insert_with(|| reference.target.path.clone());
            }
        }
        Self { by_file }
    }

    fn resolve(&self, file: &str, local: &str) -> Option<&[String]> {
        self.by_file
            .get(file)
            .and_then(|imports| imports.get(local))
            .map(Vec::as_slice)
    }
}

/// Lookup indexes over resolved symbols.
struct SymbolIndex {
    keys: BTreeSet<String>,
    qualified_of: BTreeMap<String, String>,
    by_qualified: BTreeMap<String, Vec<String>>,
    by_simple: BTreeMap<String, Vec<String>>,
    by_type_member: BTreeMap<(String, String), Vec<String>>,
}

impl SymbolIndex {
    fn build(symbols: &[SymbolNode]) -> Self {
        let mut keys = BTreeSet::new();
        let mut qualified_of = BTreeMap::new();
        let mut by_qualified = BTreeMap::<String, Vec<String>>::new();
        let mut by_simple = BTreeMap::<String, Vec<String>>::new();
        let mut by_type_member = BTreeMap::<(String, String), Vec<String>>::new();
        for node in symbols {
            keys.insert(node.stable_key.clone());
            qualified_of.insert(node.stable_key.clone(), node.qualified_name.clone());
            by_qualified
                .entry(node.qualified_name.clone())
                .or_default()
                .push(node.stable_key.clone());
            by_simple
                .entry(node.name.clone())
                .or_default()
                .push(node.stable_key.clone());
            let segments = node.qualified_name.split("::").collect::<Vec<_>>();
            if segments.len() >= 2 {
                let owner = segments[segments.len() - 2].to_string();
                let member = segments[segments.len() - 1].to_string();
                by_type_member
                    .entry((owner, member))
                    .or_default()
                    .push(node.stable_key.clone());
            }
        }
        Self {
            keys,
            qualified_of,
            by_qualified,
            by_simple,
            by_type_member,
        }
    }

    fn contains(&self, key: &str) -> bool {
        self.keys.contains(key)
    }

    /// Tier 2 of [`Self::resolve`]: walks the enclosing-module ancestors of the
    /// reference's source, trying each scope prefix as a qualifier, and returns
    /// the first match.
    fn resolve_in_scope(&self, reference: &UnresolvedReference) -> Option<Resolution> {
        let source_qualified = self.qualified_of.get(&reference.source_key)?;
        let scope = enclosing_scope(source_qualified);
        let joined = reference.target.joined();
        for take in (1..=scope.len()).rev() {
            let candidate = format!("{}::{}", scope[..take].join("::"), joined);
            if let Some(targets) = self.by_qualified.get(&candidate) {
                return Some(resolved(targets, ResolutionKind::Scoped));
            }
        }
        None
    }

    /// Resolves one reference through the tiers, returning the first match.
    fn resolve(&self, reference: &UnresolvedReference, imports: &ImportMap) -> Resolution {
        let target = &reference.target;
        let Some(last) = target.last_segment() else {
            return Resolution::Unresolved("empty reference target".to_string());
        };

        // 1. Exact crate-qualified name.
        if let Some(targets) = self.by_qualified.get(&target.joined()) {
            return resolved(targets, ResolutionKind::Exact);
        }

        // 2. Enclosing-module scope, walking up ancestors.
        if let Some(resolution) = self.resolve_in_scope(reference) {
            return resolution;
        }

        // 3. In-scope imports (first path segment is an imported local name).
        if let Some(first) = target.path.first()
            && let Some(prefix) = imports.resolve(&reference.file_path, first)
        {
            let mut segments = prefix.to_vec();
            segments.extend(target.path[1..].iter().cloned());
            if let Some(targets) = self.by_qualified.get(&segments.join("::")) {
                return resolved(targets, ResolutionKind::Import);
            }
        }

        // 4. Receiver type (`Type::member` / `self.member` with a known type).
        if let Some(qualifier) = &target.qualifier {
            let owner = qualifier
                .rsplit("::")
                .next()
                .unwrap_or(qualifier)
                .to_string();
            if let Some(targets) = self.by_type_member.get(&(owner, last.to_string())) {
                return resolved(targets, ResolutionKind::Receiver);
            }
        }

        // 5. Bare simple name.
        if let Some(targets) = self.by_simple.get(last) {
            return resolved(targets, ResolutionKind::Simple);
        }

        Resolution::Unresolved("no matching symbol".to_string())
    }
}

/// Returns the enclosing module scope of a symbol (its qualified name minus its
/// own trailing segment).
fn enclosing_scope(qualified: &str) -> Vec<String> {
    let mut segments = qualified
        .split("::")
        .map(str::to_string)
        .collect::<Vec<_>>();
    segments.pop();
    segments
}

/// Wraps a non-empty candidate list, marking it ambiguous when not unique.
fn resolved(targets: &[String], kind: ResolutionKind) -> Resolution {
    let kind = if targets.len() == 1 {
        kind
    } else {
        ResolutionKind::Ambiguous
    };
    Resolution::Resolved {
        targets: targets.to_vec(),
        kind,
    }
}

#[cfg(test)]
mod tests {
    use oxcode_model::{
        EdgeKind, FileParseStatus, LanguageId, NodeKind, ReferenceKind, ReferenceTarget,
        SourceSpan, SourceUnit, SymbolNode, UnresolvedReference,
    };

    use super::*;

    fn source_unit() -> SourceUnit {
        SourceUnit {
            path: "src/lib.rs".to_string(),
            language: LanguageId::from("rust"),
        }
    }

    fn symbol(name: &str, qualified_name: &str, kind: NodeKind, start_byte: usize) -> SymbolNode {
        SymbolNode {
            stable_key: format!(
                "symbol:src/lib.rs:{}:{qualified_name}:{start_byte}",
                kind.as_str()
            ),
            name: name.to_string(),
            qualified_name: qualified_name.to_string(),
            kind,
            raw_kind: Some("item".to_string()),
            language: LanguageId::from("rust"),
            file_path: "src/lib.rs".to_string(),
            span: SourceSpan {
                start_byte,
                ..SourceSpan::default()
            },
            signature: Some(format!("fn {qualified_name}()")),
            docstring: None,
            source_preview: Some(format!("fn {qualified_name}() {{}}")),
        }
    }

    fn call(source_key: &str, target: ReferenceTarget) -> UnresolvedReference {
        UnresolvedReference {
            source_key: source_key.to_string(),
            target,
            kind: EdgeKind::Calls,
            file_path: "src/lib.rs".to_string(),
            span: SourceSpan::default(),
            text: "call()".to_string(),
            reason: None,
        }
    }

    fn target(path: &[&str], qualifier: Option<&str>, kind: ReferenceKind) -> ReferenceTarget {
        ReferenceTarget {
            raw: path.join("::"),
            path: path.iter().map(|segment| (*segment).to_string()).collect(),
            qualifier: qualifier.map(str::to_string),
            kind_hint: kind,
        }
    }

    fn resolve(nodes: Vec<SymbolNode>, references: Vec<UnresolvedReference>) -> ResolvedIndex {
        resolve_extractions(vec![Extraction {
            file: source_unit(),
            parse_status: FileParseStatus::Ok,
            nodes,
            edges: Vec::new(),
            references,
        }])
        .expect("resolve")
    }

    #[test]
    fn scoped_resolution_finds_module_sibling_and_keeps_call_site() {
        let caller = symbol("caller", "crate::caller", NodeKind::Function, 0);
        let callee = symbol("callee", "crate::callee", NodeKind::Function, 10);
        let caller_key = caller.stable_key.clone();
        let callee_key = callee.stable_key.clone();
        let resolved = resolve(
            vec![caller, callee],
            vec![call(
                &caller_key,
                target(&["callee"], None, ReferenceKind::Function),
            )],
        );
        let edge = resolved
            .edges
            .iter()
            .find(|edge| edge.target_key == callee_key && edge.kind == EdgeKind::Calls)
            .expect("call edge");
        assert_eq!(edge.resolution, ResolutionKind::Scoped);
        assert!(edge.reference.is_some());
    }

    #[test]
    fn receiver_typed_method_resolves_to_type_member() {
        let method = symbol("run", "crate::Foo::run", NodeKind::Method, 0);
        let caller = symbol("entry", "crate::entry", NodeKind::Function, 50);
        let method_key = method.stable_key.clone();
        let caller_key = caller.stable_key.clone();
        let resolved = resolve(
            vec![method, caller],
            vec![call(
                &caller_key,
                target(&["run"], Some("Foo"), ReferenceKind::Method),
            )],
        );
        let edge = resolved
            .edges
            .iter()
            .find(|edge| edge.target_key == method_key)
            .expect("receiver edge");
        assert_eq!(edge.resolution, ResolutionKind::Receiver);
    }

    #[test]
    fn ambiguous_simple_name_keeps_all_candidates() {
        let new_a = symbol("new", "crate::A::new", NodeKind::Method, 0);
        let new_b = symbol("new", "crate::B::new", NodeKind::Method, 20);
        let caller = symbol("entry", "crate::entry", NodeKind::Function, 60);
        let caller_key = caller.stable_key.clone();
        // A bare `new()` with no receiver type -> simple tier, two candidates.
        let resolved = resolve(
            vec![new_a, new_b, caller],
            vec![call(
                &caller_key,
                target(&["new"], None, ReferenceKind::Function),
            )],
        );
        let edges = resolved
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Calls)
            .collect::<Vec<_>>();
        assert_eq!(edges.len(), 2, "both candidates kept");
        assert!(
            edges
                .iter()
                .all(|edge| edge.resolution == ResolutionKind::Ambiguous)
        );
    }

    #[test]
    fn self_recursive_call_produces_self_edge() {
        let f = symbol("f", "crate::f", NodeKind::Function, 0);
        let f_key = f.stable_key.clone();
        let resolved = resolve(
            vec![f],
            vec![call(&f_key, target(&["f"], None, ReferenceKind::Function))],
        );
        assert!(
            resolved
                .edges
                .iter()
                .any(|edge| edge.source_key == f_key && edge.target_key == f_key)
        );
    }

    #[test]
    fn path_based_esm_import_resolves_without_resolver_changes() {
        // The TS extractor resolves `import { Bar } from './a'` to the target
        // module's path-anchored scope and emits a normal name-based target, so
        // the existing import tier resolves a bare `Bar()` with no special case.
        let bar = symbol("Bar", "src::a::Bar", NodeKind::Function, 0);
        let caller = symbol("entry", "src::app::entry", NodeKind::Function, 80);
        let bar_key = bar.stable_key.clone();
        let caller_key = caller.stable_key.clone();
        let import = UnresolvedReference {
            source_key: caller_key.clone(),
            target: target(&["src", "a", "Bar"], Some("src::a"), ReferenceKind::Import),
            kind: EdgeKind::Imports,
            file_path: "src/lib.rs".to_string(),
            span: SourceSpan::default(),
            text: "import { Bar } from './a';".to_string(),
            reason: None,
        };
        let bare_call = call(&caller_key, target(&["Bar"], None, ReferenceKind::Function));
        let resolved = resolve(vec![bar, caller], vec![import, bare_call]);
        let edge = resolved
            .edges
            .iter()
            .find(|edge| edge.target_key == bar_key && edge.kind == EdgeKind::Calls)
            .expect("import-resolved call edge");
        assert_eq!(edge.resolution, ResolutionKind::Import);
    }

    #[test]
    fn import_first_hop_resolves_to_imported_symbol() {
        let bar = symbol("Bar", "crate::a::Bar", NodeKind::Struct, 0);
        let caller = symbol("entry", "crate::entry", NodeKind::Function, 80);
        let bar_key = bar.stable_key.clone();
        let caller_key = caller.stable_key.clone();
        let import = UnresolvedReference {
            source_key: caller_key.clone(),
            target: target(
                &["crate", "a", "Bar"],
                Some("crate::a"),
                ReferenceKind::Import,
            ),
            kind: EdgeKind::Imports,
            file_path: "src/lib.rs".to_string(),
            span: SourceSpan::default(),
            text: "use crate::a::Bar;".to_string(),
            reason: None,
        };
        let bare_call = call(&caller_key, target(&["Bar"], None, ReferenceKind::Function));
        let resolved = resolve(vec![bar, caller], vec![import, bare_call]);
        let edge = resolved
            .edges
            .iter()
            .find(|edge| edge.target_key == bar_key && edge.kind == EdgeKind::Calls)
            .expect("import-resolved edge");
        assert_eq!(edge.resolution, ResolutionKind::Import);
    }
}

use std::collections::{BTreeMap, BTreeSet};

use oxcode_model::{Extraction, ReferenceSite, ResolvedEdge, ResolvedIndex, SymbolNode};

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

    let stable_keys = symbols
        .iter()
        .map(|symbol| symbol.stable_key.clone())
        .collect::<BTreeSet<_>>();
    let (qualified, simple) = build_name_maps(&symbols);

    let mut edge_set = BTreeSet::new();
    for edge in symbolic_edges {
        if stable_keys.contains(&edge.source_key) && stable_keys.contains(&edge.target_key) {
            edge_set.insert(ResolvedEdge {
                source_key: edge.source_key,
                target_key: edge.target_key,
                kind: edge.kind,
                reference: None,
            });
        }
    }

    let mut unresolved = Vec::new();
    for reference in references {
        if !stable_keys.contains(&reference.source_key) {
            continue;
        }
        match resolve_target(&reference.target.normalized, &qualified, &simple) {
            ResolveTarget::Resolved(target_key) => {
                if reference.source_key != target_key {
                    edge_set.insert(ResolvedEdge {
                        source_key: reference.source_key,
                        target_key,
                        kind: reference.kind,
                        reference: Some(ReferenceSite {
                            file_path: reference.file_path,
                            span: reference.span,
                            text: reference.text,
                        }),
                    });
                }
            }
            ResolveTarget::Unresolved(reason) => {
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

/// Resolution outcome for one unresolved reference.
enum ResolveTarget {
    /// Resolved to a unique stable key.
    Resolved(String),
    /// Could not resolve, with reason.
    Unresolved(String),
}

/// Removes duplicated stable keys, keeping the first deterministic entry.
fn dedupe_symbols(symbols: &mut Vec<SymbolNode>) {
    let mut seen = BTreeSet::new();
    symbols.retain(|symbol| seen.insert(symbol.stable_key.clone()));
}

/// Builds exact and simple-name indexes for the resolver.
fn build_name_maps(
    nodes: &[SymbolNode],
) -> (BTreeMap<String, Vec<String>>, BTreeMap<String, Vec<String>>) {
    let mut qualified = BTreeMap::<String, Vec<String>>::new();
    let mut simple = BTreeMap::<String, Vec<String>>::new();
    for node in nodes {
        qualified
            .entry(node.qualified_name.clone())
            .or_default()
            .push(node.stable_key.clone());
        simple
            .entry(node.name.clone())
            .or_default()
            .push(node.stable_key.clone());
    }
    (qualified, simple)
}

/// Resolves one reference target against exact names, then unique simple names.
fn resolve_target(
    target: &str,
    qualified: &BTreeMap<String, Vec<String>>,
    simple: &BTreeMap<String, Vec<String>>,
) -> ResolveTarget {
    let normalized = target.trim_start_matches("crate::");
    if let Some(keys) = qualified.get(normalized) {
        return unique_or_ambiguous(target, keys);
    }
    let last = normalized
        .rsplit("::")
        .next()
        .unwrap_or(normalized)
        .trim_start_matches("Self::");
    if let Some(keys) = simple.get(last) {
        return unique_or_ambiguous(target, keys);
    }
    ResolveTarget::Unresolved("no matching symbol".to_string())
}

/// Converts a candidate list into a resolved key or ambiguity reason.
fn unique_or_ambiguous(target: &str, keys: &[String]) -> ResolveTarget {
    match keys {
        [key] => ResolveTarget::Resolved(key.clone()),
        [] => ResolveTarget::Unresolved("no matching symbol".to_string()),
        _ => ResolveTarget::Unresolved(format!("{target} matched {} symbols", keys.len())),
    }
}

#[cfg(test)]
mod tests {
    use oxcode_model::{
        EdgeKind, LanguageId, NodeKind, ReferenceTarget, SourceSpan, SourceUnit, SymbolNode,
        UnresolvedReference,
    };

    use super::*;

    #[test]
    fn resolver_turns_simple_call_into_edge() {
        let source = SourceUnit {
            path: "src/lib.rs".to_string(),
            language: LanguageId::from("rust"),
            hash: "hash".to_string(),
            byte_len: 1,
        };
        let caller = symbol("caller", "caller", 0);
        let callee = symbol("callee", "callee", 10);
        let caller_key = caller.stable_key.clone();
        let callee_key = callee.stable_key.clone();
        let resolved = resolve_extractions(vec![Extraction {
            file: source,
            nodes: vec![caller, callee],
            edges: Vec::new(),
            references: vec![UnresolvedReference {
                source_key: caller_key.clone(),
                target: ReferenceTarget::new("callee"),
                kind: EdgeKind::Calls,
                file_path: "src/lib.rs".to_string(),
                span: SourceSpan::default(),
                text: "callee()".to_string(),
                reason: None,
            }],
        }])
        .expect("resolve");
        let edge = resolved
            .edges
            .iter()
            .find(|edge| {
                edge.source_key == caller_key
                    && edge.target_key == callee_key
                    && edge.kind == EdgeKind::Calls
            })
            .expect("call edge");
        let reference = edge.reference.as_ref().expect("reference site");
        assert_eq!(reference.file_path, "src/lib.rs");
        assert_eq!(reference.text, "callee()");
    }

    fn symbol(name: &str, qualified_name: &str, start_byte: usize) -> SymbolNode {
        SymbolNode {
            stable_key: format!("symbol:src/lib.rs:function:{qualified_name}:{start_byte}"),
            name: name.to_string(),
            qualified_name: qualified_name.to_string(),
            kind: NodeKind::Function,
            raw_kind: Some("function_item".to_string()),
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
}

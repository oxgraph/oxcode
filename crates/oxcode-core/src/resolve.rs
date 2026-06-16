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
    EdgeKind, Extraction, HyperedgeKind, HyperedgeParticipant, LanguageId, NodeKind,
    ParticipantRole, ReferenceKind, ReferenceSite, ResolutionKind, ResolvedEdge, ResolvedHyperedge,
    ResolvedIndex, SourceSpan, SymbolNode, UnresolvedReference,
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

    let mut edges: Vec<ResolvedEdge> = edge_set.into_iter().collect();

    // Synthesize the crate/module containment spine and lifted dependency graph
    // AFTER resolution (so synthesized nodes never entered `SymbolIndex` and could
    // not pollute name resolution), then group hyperedges over the enriched graph.
    let (container_nodes, container_edges) = synthesize_architecture(&symbols, &edges);
    symbols.extend(container_nodes);
    edges.extend(container_edges);

    let hyperedges = group_hyperedges(&symbols, &edges);

    Ok(ResolvedIndex {
        files,
        nodes: symbols,
        edges,
        hyperedges,
        unresolved,
    })
}

/// Returns whether a repository-relative path is a library/binary source file
/// (under a `src/` directory). Only these are woven into the crate/module spine
/// and dependency graph; `tests/`/`examples/`/`benches/`/`build.rs` are separate
/// build targets, not part of the module tree.
fn is_library_source(file_path: &str) -> bool {
    file_path.split('/').any(|segment| segment == "src")
}

/// The container node keys a library file belongs to, at each altitude.
struct FileContainers {
    /// `Package` node key (the crate).
    crate_key: String,
    /// Deepest `Module` node key, or the crate key for a crate-root file.
    module_key: String,
    /// The `File` node key.
    file_key: String,
}

/// Synthesizes crate/module container nodes + the `Contains` spine, and lifts
/// symbol edges into file/module/crate `DependsOn` edges. Pure post-processing
/// over resolved File nodes + edges; container nodes carry a real representative
/// file path so search/render stay sound.
fn synthesize_architecture(
    nodes: &[SymbolNode],
    edges: &[ResolvedEdge],
) -> (Vec<SymbolNode>, Vec<ResolvedEdge>) {
    let (mut minter, library_files) = ContainerMinter::new(nodes);
    let mut spine: BTreeSet<(String, String)> = BTreeSet::new();
    let mut containers: BTreeMap<&str, FileContainers> = BTreeMap::new();

    for file in &library_files {
        let segments: Vec<&str> = file
            .qualified_name
            .split("::")
            .filter(|segment| !segment.is_empty())
            .collect();
        let Some(&crate_name) = segments.first() else {
            continue;
        };
        let crate_key = minter.key(NodeKind::Package, crate_name, file);

        let mut parent_key = crate_key.clone();
        let mut module_key = crate_key.clone();
        for end in 2..=segments.len() {
            let key = minter.key(NodeKind::Module, &segments[..end].join("::"), file);
            spine.insert((parent_key, key.clone()));
            parent_key = key.clone();
            module_key = key;
        }
        spine.insert((parent_key, file.stable_key.clone()));
        containers.insert(
            file.file_path.as_str(),
            FileContainers {
                crate_key,
                module_key,
                file_key: file.stable_key.clone(),
            },
        );
    }

    let deps = lift_dependencies(nodes, edges, &containers);

    let mut out_edges = Vec::with_capacity(spine.len() + deps.len());
    out_edges.extend(
        spine
            .into_iter()
            .map(|(source_key, target_key)| ResolvedEdge {
                source_key,
                target_key,
                kind: EdgeKind::Contains,
                resolution: ResolutionKind::Exact,
                reference: None,
            }),
    );
    out_edges.extend(
        deps.into_iter()
            .map(|(source_key, target_key)| ResolvedEdge {
                source_key,
                target_key,
                kind: EdgeKind::DependsOn,
                resolution: ResolutionKind::Exact,
                reference: None,
            }),
    );
    (minter.new_nodes.into_values().collect(), out_edges)
}

/// Mints (or reuses) crate/module container nodes, deduplicating into `new_nodes`.
struct ContainerMinter<'a> {
    /// Existing real container nodes by `(kind, qualified name)` → stable key, so
    /// a synthetic node is never minted when an inline `mod` already covers it.
    existing: BTreeMap<(NodeKind, &'a str), &'a str>,
    /// Representative `(path, language)` for a container qualified name, when a
    /// library file's module path equals it exactly.
    file_for_qname: BTreeMap<&'a str, (&'a str, &'a LanguageId)>,
    /// Synthesized container nodes by stable key.
    new_nodes: BTreeMap<String, SymbolNode>,
}

impl<'a> ContainerMinter<'a> {
    /// Indexes existing containers and library-source files from `nodes`.
    fn new(nodes: &'a [SymbolNode]) -> (Self, Vec<&'a SymbolNode>) {
        let mut existing = BTreeMap::new();
        let mut file_for_qname = BTreeMap::new();
        let mut library_files = Vec::new();
        for node in nodes {
            match node.kind {
                NodeKind::Module | NodeKind::Namespace | NodeKind::Package => {
                    existing
                        .entry((node.kind, node.qualified_name.as_str()))
                        .or_insert(node.stable_key.as_str());
                }
                NodeKind::File if is_library_source(&node.file_path) => {
                    library_files.push(node);
                    file_for_qname
                        .entry(node.qualified_name.as_str())
                        .or_insert((node.file_path.as_str(), &node.language));
                }
                _ => {}
            }
        }
        (
            Self {
                existing,
                file_for_qname,
                new_nodes: BTreeMap::new(),
            },
            library_files,
        )
    }

    /// Returns the stable key for the `kind` container named `qualified_name`,
    /// reusing a real node when present, else minting a synthetic one located at a
    /// representative file (or the triggering file).
    fn key(
        &mut self,
        kind: NodeKind,
        qualified_name: &str,
        triggering_file: &SymbolNode,
    ) -> String {
        if let Some(&key) = self.existing.get(&(kind, qualified_name)) {
            return key.to_owned();
        }
        let prefix = if kind == NodeKind::Package {
            "package"
        } else {
            "module"
        };
        let stable_key = format!("{prefix}:{qualified_name}");
        if !self.new_nodes.contains_key(&stable_key) {
            let (file_path, language) = self.file_for_qname.get(qualified_name).map_or_else(
                || {
                    (
                        triggering_file.file_path.clone(),
                        triggering_file.language.clone(),
                    )
                },
                |(path, language)| ((*path).to_owned(), (*language).clone()),
            );
            let node = SymbolNode {
                stable_key: stable_key.clone(),
                name: qualified_name
                    .rsplit("::")
                    .next()
                    .unwrap_or(qualified_name)
                    .to_owned(),
                qualified_name: qualified_name.to_owned(),
                kind,
                raw_kind: Some("synthesized".to_owned()),
                language,
                file_path,
                span: SourceSpan::default(),
                signature: None,
                docstring: None,
                source_preview: None,
            };
            self.new_nodes.insert(stable_key.clone(), node);
        }
        stable_key
    }
}

/// Lifts symbol-level dependency edges to file/module/crate `DependsOn` pairs,
/// deduplicated. Only edges between two library symbols contribute; an edge
/// within the same container at a level emits nothing at that level.
fn lift_dependencies(
    nodes: &[SymbolNode],
    edges: &[ResolvedEdge],
    containers: &BTreeMap<&str, FileContainers>,
) -> BTreeSet<(String, String)> {
    let file_of_symbol: BTreeMap<&str, &str> = nodes
        .iter()
        .map(|node| (node.stable_key.as_str(), node.file_path.as_str()))
        .collect();

    let mut deps: BTreeSet<(String, String)> = BTreeSet::new();
    for edge in edges {
        if !matches!(
            edge.kind,
            EdgeKind::Calls
                | EdgeKind::References
                | EdgeKind::Imports
                | EdgeKind::Implements
                | EdgeKind::ImplementsFor
        ) {
            continue;
        }
        // Only confident resolutions lift: a real cross-container dependency uses a
        // crate-qualified path, an import, an enclosing scope, or a receiver type.
        // `Simple` (bare last-segment) and `Ambiguous` matches cross container
        // boundaries by coincidence and would fabricate false dependencies.
        if !matches!(
            edge.resolution,
            ResolutionKind::Exact
                | ResolutionKind::Scoped
                | ResolutionKind::Import
                | ResolutionKind::Receiver
        ) {
            continue;
        }
        let (Some(source), Some(target)) = (
            file_of_symbol
                .get(edge.source_key.as_str())
                .and_then(|path| containers.get(path)),
            file_of_symbol
                .get(edge.target_key.as_str())
                .and_then(|path| containers.get(path)),
        ) else {
            continue;
        };
        for (from, to) in [
            (&source.file_key, &target.file_key),
            (&source.module_key, &target.module_key),
            (&source.crate_key, &target.crate_key),
        ] {
            if from != to {
                deps.insert((from.clone(), to.clone()));
            }
        }
    }
    deps
}

/// Returns whether `kind` is an architecture-level container whose direct
/// membership is worth grouping into a `Membership` hyperedge for altitude
/// ranking. Leaf-type containers (structs, enums) are intentionally excluded —
/// their members are fields/methods, not architectural structure.
fn is_architecture_container(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::File | NodeKind::Module | NodeKind::Namespace | NodeKind::Package
    )
}

/// Groups resolved binary structure into n-ary hyperedges: trait impls
/// (`ImplGroup`) and high-level container membership (`Membership`). This is pure
/// post-processing over already-resolved nodes and edges — no parsing — so the
/// hyperedge layer adds no extraction cost.
///
/// The impl group's concrete type is omitted: no resolved edge links an impl
/// block to the type it is `impl`-ing, so the type is not yet an addressable
/// participant (added once resolution emits that edge).
fn group_hyperedges(nodes: &[SymbolNode], edges: &[ResolvedEdge]) -> Vec<ResolvedHyperedge> {
    let kind_by_key: BTreeMap<&str, NodeKind> = nodes
        .iter()
        .map(|node| (node.stable_key.as_str(), node.kind))
        .collect();
    let mut out_by_source: BTreeMap<&str, Vec<&ResolvedEdge>> = BTreeMap::new();
    for edge in edges {
        out_by_source
            .entry(edge.source_key.as_str())
            .or_default()
            .push(edge);
    }

    let mut hyperedges = Vec::new();

    // Membership: each architecture-level container and the symbols it directly
    // contains. Anchor = container (target side), members = contained (source).
    for (&container, container_edges) in &out_by_source {
        if !kind_by_key
            .get(container)
            .is_some_and(|kind| is_architecture_container(*kind))
        {
            continue;
        }
        let mut builder = HyperedgeBuilder::new(HyperedgeKind::Membership, container);
        for edge in container_edges {
            if edge.kind == EdgeKind::Contains {
                builder.participant(&edge.target_key, ParticipantRole::Member);
            }
        }
        hyperedges.extend(builder.build());
    }

    // ImplGroup: each impl block, the trait it implements, and its methods.
    // Anchor = impl block (target side), trait/methods = source side.
    for node in nodes {
        if node.kind != NodeKind::ImplBlock {
            continue;
        }
        let mut builder = HyperedgeBuilder::new(HyperedgeKind::ImplGroup, &node.stable_key);
        for edge in out_by_source
            .get(node.stable_key.as_str())
            .into_iter()
            .flatten()
        {
            let role = match edge.kind {
                EdgeKind::Implements => ParticipantRole::ImplTrait,
                EdgeKind::ImplementsFor => ParticipantRole::ImplType,
                EdgeKind::Contains => ParticipantRole::Member,
                _ => continue,
            };
            builder.participant(&edge.target_key, role);
        }
        hyperedges.extend(builder.build());
    }

    hyperedges.sort();
    hyperedges
}

/// Accumulates a hyperedge around a single anchor, then canonicalizes and
/// validates it on [`Self::build`].
///
/// A hyperedge always has exactly one target-side [`ParticipantRole::Anchor`]
/// (seeded at construction) and zero or more source-side participants added with
/// [`Self::participant`].
struct HyperedgeBuilder {
    /// Kind of the hyperedge under construction.
    kind: HyperedgeKind,
    /// Accumulated participants, beginning with the anchor.
    participants: Vec<HyperedgeParticipant>,
}

impl HyperedgeBuilder {
    /// Starts a `kind` hyperedge anchored on `anchor` (the target-side unit).
    fn new(kind: HyperedgeKind, anchor: &str) -> Self {
        Self {
            kind,
            participants: vec![HyperedgeParticipant {
                key: anchor.to_owned(),
                role: ParticipantRole::Anchor,
            }],
        }
    }

    /// Adds one source-side participant playing `role`.
    fn participant(&mut self, key: &str, role: ParticipantRole) -> &mut Self {
        self.participants.push(HyperedgeParticipant {
            key: key.to_owned(),
            role,
        });
        self
    }

    /// Canonicalizes participants (sort + dedup) and yields the hyperedge, or
    /// `None` when it has no source-side participant beyond the anchor — the
    /// hypergraph projection only materializes relations carrying both a source
    /// and a target.
    fn build(mut self) -> Option<ResolvedHyperedge> {
        self.participants.sort();
        self.participants.dedup();
        let has_source = self
            .participants
            .iter()
            .any(|participant| participant.role != ParticipantRole::Anchor);
        has_source.then_some(ResolvedHyperedge {
            kind: self.kind,
            participants: self.participants,
        })
    }
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
    fn synthesize_architecture_mints_containers_and_lifts_crate_deps() {
        let file_node = |crate_qn: &str, path: &str| SymbolNode {
            stable_key: format!("file:{path}"),
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            qualified_name: crate_qn.to_string(),
            kind: NodeKind::File,
            raw_kind: Some("source_file".to_string()),
            language: LanguageId::from("rust"),
            file_path: path.to_string(),
            span: SourceSpan::default(),
            signature: None,
            docstring: None,
            source_preview: None,
        };
        let function = |key: &str, qn: &str, path: &str| SymbolNode {
            stable_key: key.to_string(),
            name: qn.rsplit("::").next().unwrap_or(qn).to_string(),
            qualified_name: qn.to_string(),
            kind: NodeKind::Function,
            raw_kind: None,
            language: LanguageId::from("rust"),
            file_path: path.to_string(),
            span: SourceSpan::default(),
            signature: None,
            docstring: None,
            source_preview: None,
        };
        let nodes = vec![
            file_node("a", "crates/a/src/lib.rs"),
            file_node("b", "crates/b/src/lib.rs"),
            function("sym_a", "a::foo", "crates/a/src/lib.rs"),
            function("sym_b", "b::bar", "crates/b/src/lib.rs"),
        ];
        let edges = vec![ResolvedEdge {
            source_key: "sym_a".to_string(),
            target_key: "sym_b".to_string(),
            kind: EdgeKind::Calls,
            resolution: ResolutionKind::Exact,
            reference: None,
        }];

        let (new_nodes, new_edges) = synthesize_architecture(&nodes, &edges);

        assert!(
            new_nodes
                .iter()
                .any(|n| n.stable_key == "package:a" && n.kind == NodeKind::Package)
        );
        assert!(new_nodes.iter().any(|n| n.stable_key == "package:b"));
        let has_edge = |kind: EdgeKind, src: &str, tgt: &str| {
            new_edges
                .iter()
                .any(|e| e.kind == kind && e.source_key == src && e.target_key == tgt)
        };
        // Crate-level dependency a -> b (a symbol in a calls one in b).
        assert!(has_edge(EdgeKind::DependsOn, "package:a", "package:b"));
        // File-level dependency too.
        assert!(has_edge(
            EdgeKind::DependsOn,
            "file:crates/a/src/lib.rs",
            "file:crates/b/src/lib.rs"
        ));
        // Containment spine: the crate contains its root file.
        assert!(has_edge(
            EdgeKind::Contains,
            "package:a",
            "file:crates/a/src/lib.rs"
        ));
        // No self-dependency within one crate.
        assert!(!has_edge(EdgeKind::DependsOn, "package:a", "package:a"));
    }

    #[test]
    fn group_hyperedges_builds_impl_group_and_container_membership() {
        let module = symbol("m", "crate::m", NodeKind::Module, 0);
        let strukt = symbol("Foo", "crate::m::Foo", NodeKind::Struct, 10);
        let impl_block = symbol(
            "impl Foo",
            "crate::m::impl Display for Foo",
            NodeKind::ImplBlock,
            20,
        );
        let trait_node = symbol("Display", "core::fmt::Display", NodeKind::Trait, 30);
        let method = symbol("fmt", "crate::m::Foo::fmt", NodeKind::Method, 40);

        let edge = |source: &SymbolNode, target: &SymbolNode, kind| ResolvedEdge {
            source_key: source.stable_key.clone(),
            target_key: target.stable_key.clone(),
            kind,
            resolution: ResolutionKind::Exact,
            reference: None,
        };
        let edges = vec![
            edge(&module, &strukt, EdgeKind::Contains),
            edge(&module, &impl_block, EdgeKind::Contains),
            edge(&impl_block, &method, EdgeKind::Contains),
            edge(&impl_block, &trait_node, EdgeKind::Implements),
            edge(&impl_block, &strukt, EdgeKind::ImplementsFor),
        ];
        let nodes = vec![
            module.clone(),
            strukt.clone(),
            impl_block.clone(),
            trait_node.clone(),
            method.clone(),
        ];

        let hyperedges = group_hyperedges(&nodes, &edges);

        let anchor_key = |hyperedge: &ResolvedHyperedge| {
            hyperedge
                .participants
                .iter()
                .find(|participant| participant.role == ParticipantRole::Anchor)
                .expect("anchor")
                .key
                .clone()
        };
        let role_keys = |hyperedge: &ResolvedHyperedge, role: ParticipantRole| {
            hyperedge
                .participants
                .iter()
                .filter(|participant| participant.role == role)
                .map(|participant| participant.key.clone())
                .collect::<Vec<_>>()
        };

        // The impl block anchors one ImplGroup carrying its trait and method.
        let impl_group = hyperedges
            .iter()
            .find(|hyperedge| hyperedge.kind == HyperedgeKind::ImplGroup)
            .expect("impl group");
        assert_eq!(anchor_key(impl_group), impl_block.stable_key);
        assert_eq!(
            role_keys(impl_group, ParticipantRole::ImplTrait),
            vec![trait_node.stable_key.clone()]
        );
        assert_eq!(
            role_keys(impl_group, ParticipantRole::Member),
            vec![method.stable_key.clone()]
        );
        assert_eq!(
            role_keys(impl_group, ParticipantRole::ImplType),
            vec![strukt.stable_key.clone()]
        );

        // The module anchors one Membership over the symbols it contains.
        let membership = hyperedges
            .iter()
            .find(|hyperedge| hyperedge.kind == HyperedgeKind::Membership)
            .expect("membership");
        assert_eq!(anchor_key(membership), module.stable_key);
        let members = role_keys(membership, ParticipantRole::Member);
        assert!(members.contains(&strukt.stable_key));
        assert!(members.contains(&impl_block.stable_key));

        // A struct is not an architecture container: its members are not grouped.
        assert!(
            !hyperedges
                .iter()
                .any(|hyperedge| anchor_key(hyperedge) == strukt.stable_key)
        );
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

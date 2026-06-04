//! Db write path: reconciles the OxGraph store from a resolved index against a
//! declarative [`Schema`].
//!
//! The schema (roles, labels, relation types, typed property keys, equality
//! indexes, and graph projections) is declared once from the model's typed
//! catalog and applied idempotently inside the write transaction. The body then
//! drives the engine's identity-reconcile verbs:
//!
//! * [`Writer::upsert_element`] resolves-or-mints each symbol by its stable key, so an unchanged
//!   symbol keeps its element id across reindexes (`O(change)`);
//! * [`Writer::retain`] tombstones the vanished complement (the prune step);
//! * [`Writer::upsert_relation`] resolves-or-mints each edge by a deterministic per-edge key,
//!   reusing the relation id when the edge is unchanged.
//!
//! There is one path: a cold store simply upserts everything and prunes nothing.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use oxcode_model::{
    EdgeKind, ElementProperty, NodeKind, PropertyKind, RelationProperty, ResolvedEdge,
    ResolvedIndex, SOURCE_ROLE, SourceSpan, SymbolNode, TARGET_ROLE, UnresolvedReference,
    projection_name,
};
use oxgraph::db::{
    Bound, Db, DbError, ElementId, Int, PropertyFamily, RelationId, Schema, Text, Writer,
};

use crate::{
    error::{Error, Result},
    paths::{database_dir, index_dir},
};

/// Equality index name resolving an element by its [`ElementProperty::StableKey`].
const STABLE_KEY_INDEX: &str = "element_stable_key_eq";
/// Equality index name resolving a relation by its [`RelationProperty::EdgeStableKey`].
const EDGE_KEY_INDEX: &str = "relation_edge_key_eq";

/// Equality index name for one indexed element property key.
pub(super) fn element_index_name(key: &str) -> String {
    format!("element_{key}_eq")
}

/// Equality index name over the [`RelationProperty::EdgeKind`] property, consulted
/// to count relations of a given edge kind.
pub(super) fn edge_kind_index_name() -> String {
    format!("relation_{}_eq", RelationProperty::EdgeKind.key())
}

/// Builds the declarative code-graph schema once.
///
/// Declares every role, label, relation type, typed property key, equality
/// index, and graph projection the store needs, derived from the model's typed
/// catalog so the write and read paths cannot drift. Applying it twice registers
/// nothing new (the apply is idempotent).
fn code_schema() -> Schema {
    let mut schema = Schema::new().role(SOURCE_ROLE).role(TARGET_ROLE);

    for kind in NodeKind::ALL {
        schema = schema.label(kind.as_str());
    }
    schema = schema.label(NodeKind::Unresolved.as_str());
    for kind in EdgeKind::ALL {
        schema = schema.relation_type(kind.as_str());
    }

    for property in ElementProperty::ALL {
        schema = declare_key(
            schema,
            property.key(),
            property.kind(),
            PropertyFamily::Element,
        );
    }
    for property in RelationProperty::ALL {
        schema = declare_key(
            schema,
            property.key(),
            property.kind(),
            PropertyFamily::Relation,
        );
    }

    // Equality indexes for selector lookups, the two identity indexes the
    // reconcile verbs probe (resolve-or-mint elements and relations), and the
    // edge-kind index consulted by status counts.
    for property in ElementProperty::INDEXED {
        schema = schema.equality_index(&element_index_name(property.key()), property.key());
    }
    schema = schema
        .equality_index(EDGE_KEY_INDEX, RelationProperty::EdgeStableKey.key())
        .equality_index(&edge_kind_index_name(), RelationProperty::EdgeKind.key());

    // One graph projection per edge kind so navigation can traverse any kind.
    for kind in EdgeKind::ALL {
        let name = projection_name(kind);
        schema = schema.graph_projection(&name, &[kind.as_str()], SOURCE_ROLE, TARGET_ROLE);
    }
    schema
}

/// Declares one typed property key on the schema, mapping the model's value kind
/// to the engine's typed key constructor.
fn declare_key(schema: Schema, name: &str, kind: PropertyKind, family: PropertyFamily) -> Schema {
    match kind {
        PropertyKind::Text => schema.key::<Text>(name, family),
        PropertyKind::Integer => schema.key::<Int>(name, family),
    }
}

/// Reconciles the native OxGraph database to one resolved index.
///
/// The database is crash-safe natively (atomic superblock publish + log
/// recovery), so this writes straight into the database directory. The schema is
/// applied idempotently, then the body reconciles by identity: every current
/// symbol and edge is upserted (reusing the id of anything unchanged) and the
/// vanished complement is pruned. A cold store therefore mints everything and
/// prunes nothing; a warm store mutates only what changed (`O(change)`).
pub(crate) fn reconcile_database(root: &Path, index: &ResolvedIndex) -> Result<PathBuf> {
    let index_directory = index_dir(root);
    let database_directory = database_dir(root);
    std::fs::create_dir_all(&index_directory)
        .map_err(|source| Error::fs(&index_directory, source))?;
    write_index_gitignore(&index_directory)?;

    // Open the existing store in place, or create one on the first index (and on
    // a store that fails to open — a missing, corrupt, or stale-format database is
    // wiped and regenerated, since the index is derived and cheap to rebuild). A
    // single `open` attempt is used rather than `validate_path` + `open`, which
    // would open (and fully decode the base) twice. Both arms reconcile below.
    let mut database = match Db::open(&database_directory) {
        Ok(database) => database,
        Err(_) => {
            if database_directory.exists() {
                std::fs::remove_dir_all(&database_directory)
                    .map_err(|source| Error::fs(&database_directory, source))?;
            }
            Db::create(&database_directory)?
        }
    };

    let schema = code_schema();
    let node_keys: Vec<&str> = index
        .nodes
        .iter()
        .map(|node| node.stable_key.as_str())
        .collect();
    let edge_keys: Vec<String> = index.edges.iter().map(edge_stable_key).collect();

    database.write(|writer| {
        let bound = writer.apply_schema(&schema)?;
        let stable_key_eq = bound.equality_index::<Text>(STABLE_KEY_INDEX)?;
        let edge_key_eq = bound.equality_index::<Text>(EDGE_KEY_INDEX)?;

        // 1. Upsert every symbol by its stable key (an unchanged symbol keeps its element id),
        //    tracking the resolved id for edge endpoints.
        let mut id_by_key = BTreeMap::new();
        for node in &index.nodes {
            let element = writer.upsert_element(stable_key_eq, node.stable_key.as_str())?;
            writer.add_label(element, bound.label(node.kind.as_str())?)?;
            set_symbol_properties(writer, &bound, element, node)?;
            id_by_key.insert(node.stable_key.clone(), element);
        }

        // Unresolved diagnostics carry their own stable key (which encodes the
        // reference site), so they reconcile identically — unchanged diagnostics
        // keep their ids and stale ones are pruned by `retain` below.
        let mut diagnostic_keys = Vec::new();
        for unresolved in &index.unresolved {
            let key = unresolved_stable_key(unresolved);
            let element = writer.upsert_element(stable_key_eq, key.as_str())?;
            writer.add_label(element, bound.label(NodeKind::Unresolved.as_str())?)?;
            set_unresolved_properties(writer, &bound, element, unresolved, &key)?;
            diagnostic_keys.push(key);
        }

        // 2. Prune vanished symbols and diagnostics: keep every current stable key.
        let mut keep_element_keys: Vec<&str> = node_keys.clone();
        keep_element_keys.extend(diagnostic_keys.iter().map(String::as_str));
        writer.retain(stable_key_eq, &keep_element_keys)?;

        // 3. Upsert edges by a deterministic per-edge key (an unchanged edge keeps its relation id
        //    and endpoints).
        for (edge, key) in index.edges.iter().zip(&edge_keys) {
            let (Some(&source), Some(&target)) = (
                id_by_key.get(&edge.source_key),
                id_by_key.get(&edge.target_key),
            ) else {
                continue;
            };
            let relation = writer.upsert_relation(
                edge_key_eq,
                key.as_str(),
                bound.relation_type(edge.kind.as_str())?,
                &[
                    (source, bound.role(SOURCE_ROLE)?),
                    (target, bound.role(TARGET_ROLE)?),
                ],
            )?;
            set_relation_properties(writer, &bound, relation, edge)?;
        }

        // 4. Prune vanished edges: keep every current edge key. An edge dropped because an endpoint
        //    went missing is absent from `keep` and pruned.
        let kept_edge_keys: Vec<&str> = index
            .edges
            .iter()
            .zip(&edge_keys)
            .filter(|(edge, _)| {
                id_by_key.contains_key(&edge.source_key) && id_by_key.contains_key(&edge.target_key)
            })
            .map(|(_, key)| key.as_str())
            .collect();
        writer.retain(edge_key_eq, &kept_edge_keys)?;

        Ok(())
    })?;
    // The engine auto-checkpoints (folds the delta-log into a fresh base) when the
    // log outgrows the base under its size-ratio policy, so a small incremental
    // reindex stays `O(change)` and never pays a full `O(base)` fold. The first
    // index's large commit over a near-empty base trips that policy and folds on
    // its own — so no explicit `compact()` is needed (forcing one every reindex
    // would defeat the amortization and pay `O(base)` each time).
    Ok(database_directory)
}

/// Builds the deterministic per-edge identity key.
///
/// The key must be distinct for multi-edges between the same `(source, target,
/// kind)` triple, so it folds in the reference site (file path plus byte span)
/// when present. An edge with no reference site (a purely structural edge such as
/// `contains`/`defines`) is unique per triple already, so the `-` placeholder is
/// stable and collision-free for those.
pub(super) fn edge_stable_key(edge: &ResolvedEdge) -> String {
    let site = match &edge.reference {
        Some(reference) => format!(
            "{}@{}..{}",
            reference.file_path, reference.span.start_byte, reference.span.end_byte
        ),
        None => "-".to_owned(),
    };
    format!(
        "{}|{}|{}|{}",
        edge.source_key,
        edge.kind.as_str(),
        edge.target_key,
        site
    )
}

/// Builds the deterministic stable key for one unresolved-reference diagnostic.
fn unresolved_stable_key(unresolved: &UnresolvedReference) -> String {
    format!(
        "unresolved:{}:{}:{}:{}",
        unresolved.source_key,
        unresolved.kind.as_str(),
        unresolved.target.joined(),
        unresolved.span.start_byte
    )
}

/// Writes a `.gitignore` that ignores the whole index directory, so users do
/// not accidentally commit the generated database. Idempotent.
fn write_index_gitignore(index_directory: &Path) -> Result<()> {
    let path = index_directory.join(".gitignore");
    if !path.exists() {
        std::fs::write(&path, "*\n").map_err(|source| Error::fs(&path, source))?;
    }
    Ok(())
}

/// Sets a text property keyed by an [`ElementProperty`] on an element.
fn set_element_text(
    writer: &mut Writer<'_>,
    bound: &Bound,
    element: ElementId,
    property: ElementProperty,
    value: &str,
) -> std::result::Result<(), DbError> {
    writer.set(element, bound.key::<Text>(property.key())?, value)?;
    Ok(())
}

/// Sets a `usize` property keyed by an [`ElementProperty`] on an element.
///
/// The span/offset is passed directly: the engine's `Assignable<Int>` for
/// `usize` performs a checked conversion, surfacing `DbError::ValueOutOfRange`.
fn set_element_int(
    writer: &mut Writer<'_>,
    bound: &Bound,
    element: ElementId,
    property: ElementProperty,
    value: usize,
) -> std::result::Result<(), DbError> {
    writer.set(element, bound.key::<Int>(property.key())?, value)?;
    Ok(())
}

/// Writes symbol properties to one element.
fn set_symbol_properties(
    writer: &mut Writer<'_>,
    bound: &Bound,
    element: ElementId,
    node: &SymbolNode,
) -> std::result::Result<(), DbError> {
    set_element_text(
        writer,
        bound,
        element,
        ElementProperty::StableKey,
        &node.stable_key,
    )?;
    set_element_text(writer, bound, element, ElementProperty::Name, &node.name)?;
    set_element_text(
        writer,
        bound,
        element,
        ElementProperty::QualifiedName,
        &node.qualified_name,
    )?;
    set_element_text(
        writer,
        bound,
        element,
        ElementProperty::Kind,
        node.kind.as_str(),
    )?;
    set_element_text(
        writer,
        bound,
        element,
        ElementProperty::Language,
        node.language.as_str(),
    )?;
    set_element_text(
        writer,
        bound,
        element,
        ElementProperty::FilePath,
        &node.file_path,
    )?;
    set_element_span(writer, bound, element, node.span)?;

    // Optional properties: set when present, otherwise unset so a reused element
    // never retains a stale value (for example a docstring removed by an edit).
    set_or_unset_text(
        writer,
        bound,
        element,
        ElementProperty::RawKind,
        node.raw_kind.as_deref(),
    )?;
    set_or_unset_text(
        writer,
        bound,
        element,
        ElementProperty::Signature,
        node.signature.as_deref(),
    )?;
    set_or_unset_text(
        writer,
        bound,
        element,
        ElementProperty::Docstring,
        node.docstring.as_deref(),
    )?;
    set_or_unset_text(
        writer,
        bound,
        element,
        ElementProperty::SourcePreview,
        node.source_preview.as_deref(),
    )
}

/// Sets an optional text property, or removes it when absent so a reused element
/// never retains a stale value.
fn set_or_unset_text(
    writer: &mut Writer<'_>,
    bound: &Bound,
    element: ElementId,
    property: ElementProperty,
    value: Option<&str>,
) -> std::result::Result<(), DbError> {
    let key = bound.key::<Text>(property.key())?;
    match value {
        Some(value) => writer.set(element, key, value)?,
        None => writer.unset(element, key)?,
    }
    Ok(())
}

/// Writes unresolved-reference diagnostic properties.
fn set_unresolved_properties(
    writer: &mut Writer<'_>,
    bound: &Bound,
    element: ElementId,
    unresolved: &UnresolvedReference,
    stable_key: &str,
) -> std::result::Result<(), DbError> {
    let joined = unresolved.target.joined();
    set_element_text(
        writer,
        bound,
        element,
        ElementProperty::StableKey,
        stable_key,
    )?;
    set_element_text(
        writer,
        bound,
        element,
        ElementProperty::Name,
        &unresolved.target.raw,
    )?;
    set_element_text(
        writer,
        bound,
        element,
        ElementProperty::QualifiedName,
        &joined,
    )?;
    set_element_text(
        writer,
        bound,
        element,
        ElementProperty::Kind,
        NodeKind::Unresolved.as_str(),
    )?;
    set_element_text(
        writer,
        bound,
        element,
        ElementProperty::FilePath,
        &unresolved.file_path,
    )?;
    set_element_text(
        writer,
        bound,
        element,
        ElementProperty::UnresolvedSourceKey,
        &unresolved.source_key,
    )?;
    set_element_text(
        writer,
        bound,
        element,
        ElementProperty::TargetRaw,
        &unresolved.target.raw,
    )?;
    set_element_text(writer, bound, element, ElementProperty::TargetPath, &joined)?;
    set_element_text(
        writer,
        bound,
        element,
        ElementProperty::TargetKindHint,
        unresolved.target.kind_hint.as_str(),
    )?;
    set_element_text(
        writer,
        bound,
        element,
        ElementProperty::UnresolvedEdgeKind,
        unresolved.kind.as_str(),
    )?;
    set_element_span(writer, bound, element, unresolved.span)?;

    set_or_unset_text(
        writer,
        bound,
        element,
        ElementProperty::TargetQualifier,
        unresolved.target.qualifier.as_deref(),
    )?;
    set_or_unset_text(
        writer,
        bound,
        element,
        ElementProperty::Reason,
        unresolved.reason.as_deref(),
    )
}

/// Writes relation properties to one code edge.
fn set_relation_properties(
    writer: &mut Writer<'_>,
    bound: &Bound,
    relation: RelationId,
    edge: &ResolvedEdge,
) -> std::result::Result<(), DbError> {
    set_relation_text(
        writer,
        bound,
        relation,
        RelationProperty::EdgeKind,
        edge.kind.as_str(),
    )?;
    set_relation_text(
        writer,
        bound,
        relation,
        RelationProperty::Resolution,
        edge.resolution.as_str(),
    )?;

    if let Some(reference) = &edge.reference {
        set_relation_text(
            writer,
            bound,
            relation,
            RelationProperty::SiteFilePath,
            &reference.file_path,
        )?;
        set_relation_int(
            writer,
            bound,
            relation,
            RelationProperty::SiteStartLine,
            reference.span.start_line,
        )?;
        set_relation_int(
            writer,
            bound,
            relation,
            RelationProperty::SiteStartColumn,
            reference.span.start_column,
        )?;
        set_relation_int(
            writer,
            bound,
            relation,
            RelationProperty::SiteEndLine,
            reference.span.end_line,
        )?;
        set_relation_int(
            writer,
            bound,
            relation,
            RelationProperty::SiteEndColumn,
            reference.span.end_column,
        )?;
        set_relation_int(
            writer,
            bound,
            relation,
            RelationProperty::SiteStartByte,
            reference.span.start_byte,
        )?;
        set_relation_int(
            writer,
            bound,
            relation,
            RelationProperty::SiteEndByte,
            reference.span.end_byte,
        )?;
        set_relation_text(
            writer,
            bound,
            relation,
            RelationProperty::SiteText,
            &reference.text,
        )?;
    }
    Ok(())
}

/// Sets a text property keyed by a [`RelationProperty`] on a relation.
fn set_relation_text(
    writer: &mut Writer<'_>,
    bound: &Bound,
    relation: RelationId,
    property: RelationProperty,
    value: &str,
) -> std::result::Result<(), DbError> {
    writer.set(relation, bound.key::<Text>(property.key())?, value)?;
    Ok(())
}

/// Sets a `usize` property keyed by a [`RelationProperty`] on a relation.
fn set_relation_int(
    writer: &mut Writer<'_>,
    bound: &Bound,
    relation: RelationId,
    property: RelationProperty,
    value: usize,
) -> std::result::Result<(), DbError> {
    writer.set(relation, bound.key::<Int>(property.key())?, value)?;
    Ok(())
}

/// Writes the six span properties to one element.
fn set_element_span(
    writer: &mut Writer<'_>,
    bound: &Bound,
    element: ElementId,
    span: SourceSpan,
) -> std::result::Result<(), DbError> {
    set_element_int(
        writer,
        bound,
        element,
        ElementProperty::StartByte,
        span.start_byte,
    )?;
    set_element_int(
        writer,
        bound,
        element,
        ElementProperty::EndByte,
        span.end_byte,
    )?;
    set_element_int(
        writer,
        bound,
        element,
        ElementProperty::StartLine,
        span.start_line,
    )?;
    set_element_int(
        writer,
        bound,
        element,
        ElementProperty::StartColumn,
        span.start_column,
    )?;
    set_element_int(
        writer,
        bound,
        element,
        ElementProperty::EndLine,
        span.end_line,
    )?;
    set_element_int(
        writer,
        bound,
        element,
        ElementProperty::EndColumn,
        span.end_column,
    )
}

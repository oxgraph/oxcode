//! Database write path: rebuilds the OxGraph store from a resolved index and
//! derives the whole catalog (labels, relation types, properties, indexes) from
//! the model's typed schema.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use oxcode_model::{
    EdgeKind, ElementProperty, NodeKind, PropertyKind, RelationProperty, ResolvedEdge,
    ResolvedIndex, SOURCE_ROLE, SourceSpan, SymbolNode, TARGET_ROLE, UnresolvedReference,
    projection_name,
};
use oxgraph::db::{
    Database, ElementId, GraphProjectionDefinition, IndexDefinition, IndexId, LabelId,
    ProjectionDefinition, PropertyFamily, PropertyKeyId, PropertySubject, PropertyType,
    PropertyValue, RelationId, RelationTypeId, RoleId,
};

use crate::{
    error::{Error, Result},
    paths::{database_dir, index_dir},
};

/// Maps a model property kind to the OxGraph storage type.
const fn property_type(kind: PropertyKind) -> PropertyType {
    match kind {
        PropertyKind::Text => PropertyType::Text,
        PropertyKind::Integer => PropertyType::Integer,
    }
}

/// Index name for counting elements carrying a node-kind label.
pub(super) fn label_index_name(kind: NodeKind) -> String {
    format!("label_{}", kind.as_str())
}

/// Index name for counting relations of an edge kind.
pub(super) fn type_index_name(kind: EdgeKind) -> String {
    format!("type_{}", kind.as_str())
}

/// Rebuilds the native OxGraph database for one resolved index.
///
/// The database is crash-safe natively (atomic superblock publish + log
/// recovery), so the rebuild writes straight into the database directory with a
/// `Database::create` + a single write transaction. A stale directory from a
/// prior run is removed first so the create starts from a clean slate.
pub(crate) fn rebuild_database(root: &Path, index: &ResolvedIndex) -> Result<PathBuf> {
    let index_directory = index_dir(root);
    let database_directory = database_dir(root);
    std::fs::create_dir_all(&index_directory)
        .map_err(|source| Error::fs(&index_directory, source))?;
    write_index_gitignore(&index_directory)?;

    if database_directory.exists() {
        std::fs::remove_dir_all(&database_directory)
            .map_err(|source| Error::fs(&database_directory, source))?;
    }

    let mut database = Database::create(&database_directory)?;
    let mut writer = database.begin_write()?;

    let source_role = writer.register_role(SOURCE_ROLE)?;
    let target_role = writer.register_role(TARGET_ROLE)?;
    let labels = register_labels(&mut writer)?;
    let unresolved_label = writer.register_label(NodeKind::Unresolved.as_str())?;
    let relation_types = register_relation_types(&mut writer)?;
    let element_properties = register_element_properties(&mut writer)?;
    let relation_properties = register_relation_properties(&mut writer)?;
    define_property_indexes(&mut writer, &element_properties)?;
    define_count_indexes(&mut writer, &labels, unresolved_label, &relation_types)?;
    for kind in EdgeKind::ALL {
        writer.define_projection(ProjectionDefinition::Graph(GraphProjectionDefinition {
            name: projection_name(kind),
            relation_types: BTreeSet::from([relation_types[&kind]]),
            source_role,
            target_role,
        }))?;
    }

    let mut elements = BTreeMap::new();
    for node in &index.nodes {
        let element = writer.create_element()?;
        writer.add_element_label(element, labels[&node.kind])?;
        set_symbol_properties(&mut writer, element, &element_properties, node)?;
        elements.insert(node.stable_key.clone(), element);
    }

    for unresolved in &index.unresolved {
        let element = writer.create_element()?;
        writer.add_element_label(element, unresolved_label)?;
        set_unresolved_properties(&mut writer, element, &element_properties, unresolved)?;
    }

    for edge in &index.edges {
        let Some(&source) = elements.get(&edge.source_key) else {
            continue;
        };
        let Some(&target) = elements.get(&edge.target_key) else {
            continue;
        };
        let relation = writer.create_relation()?;
        writer.set_relation_type(relation, relation_types[&edge.kind])?;
        set_relation_properties(&mut writer, relation, &relation_properties, edge)?;
        writer.create_incidence(relation, source, source_role)?;
        writer.create_incidence(relation, target, target_role)?;
    }

    writer.commit()?;
    // Fold the commit into a fresh base so the delta-log does not carry the whole
    // rebuild across future reindexes, bounding the log immediately after a full
    // build.
    database.checkpoint()?;
    Ok(database_directory)
}

/// Catalog id maps loaded by name from an existing database, used by the
/// incremental [`apply_delta`] path instead of re-registering the catalog.
struct CatalogMaps {
    /// Symbol-kind labels by node kind.
    labels: BTreeMap<NodeKind, LabelId>,
    /// The diagnostic `unresolved_reference` label.
    unresolved_label: LabelId,
    /// Edge relation types by edge kind.
    relation_types: BTreeMap<EdgeKind, RelationTypeId>,
    /// Element property keys by stable name.
    element_properties: BTreeMap<&'static str, PropertyKeyId>,
    /// Relation property keys by stable name.
    relation_properties: BTreeMap<&'static str, PropertyKeyId>,
    /// The source incidence role.
    source_role: RoleId,
    /// The target incidence role.
    target_role: RoleId,
    /// The `element_stable_key_eq` equality index, probed per symbol to resolve
    /// an existing element id without scanning the whole database.
    stable_key_index: IndexId,
}

/// Applies a resolved index to an existing database in place, preserving the
/// element id of every symbol whose stable key is unchanged, tombstoning removed
/// symbols, and fully replacing edges and unresolved diagnostics. Falls back to a
/// full rebuild when the on-disk catalog does not match the current schema.
///
/// Existing element ids are resolved by probing the native `stable_key` equality
/// index per symbol (`O(log n)` each). Removed symbols and the prior run's
/// unresolved diagnostics are the complement of the reused (kept) ids over the
/// current element set; relations are regenerated wholesale.
pub(crate) fn apply_delta(root: &Path, index: &ResolvedIndex) -> Result<PathBuf> {
    let database_directory = database_dir(root);
    let mut database = Database::open(&database_directory)?;

    let Some(maps) = load_catalog_maps(&database) else {
        return rebuild_database(root, index);
    };

    // Resolve the element id to reuse for each new node by probing the native
    // stable_key index (an unchanged symbol keeps its id). A node with no current
    // match mints a fresh element below.
    let read = database.begin_read();
    let mut reuse = BTreeMap::new();
    for node in &index.nodes {
        if let Some(id) = lookup_element_by_stable_key(&read, &maps, &node.stable_key) {
            reuse.insert(node.stable_key.clone(), id);
        }
    }
    // The kept set is exactly the reused element ids; every other current element
    // (a removed resolved symbol, or any prior unresolved diagnostic — which is
    // always regenerated fresh) is tombstoned. Relations are replaced wholesale.
    let kept: BTreeSet<ElementId> = reuse.values().copied().collect();
    let stale_elements: Vec<ElementId> = read
        .element_ids()
        .into_iter()
        .filter(|id| !kept.contains(id))
        .collect();
    let current_relations = read.relation_ids();
    drop(read);

    let mut writer = database.begin_write()?;

    // Edges and unresolved diagnostics are regenerated every run, so replace them
    // wholesale: tombstone all current relations and every non-reused element.
    for relation in current_relations {
        writer.tombstone_relation(relation)?;
    }
    for id in stale_elements {
        writer.tombstone_element(id)?;
    }

    let mut elements = BTreeMap::new();
    for node in &index.nodes {
        let element = match reuse.get(&node.stable_key) {
            Some(&id) => {
                clear_symbol_optionals(&mut writer, id, &maps.element_properties)?;
                id
            }
            None => {
                let element = writer.create_element()?;
                writer.add_element_label(element, maps.labels[&node.kind])?;
                element
            }
        };
        set_symbol_properties(&mut writer, element, &maps.element_properties, node)?;
        elements.insert(node.stable_key.clone(), element);
    }

    for unresolved in &index.unresolved {
        let element = writer.create_element()?;
        writer.add_element_label(element, maps.unresolved_label)?;
        set_unresolved_properties(&mut writer, element, &maps.element_properties, unresolved)?;
    }

    for edge in &index.edges {
        let (Some(&source), Some(&target)) = (
            elements.get(&edge.source_key),
            elements.get(&edge.target_key),
        ) else {
            continue;
        };
        let relation = writer.create_relation()?;
        writer.set_relation_type(relation, maps.relation_types[&edge.kind])?;
        set_relation_properties(&mut writer, relation, &maps.relation_properties, edge)?;
        writer.create_incidence(relation, source, maps.source_role)?;
        writer.create_incidence(relation, target, maps.target_role)?;
    }

    writer.commit()?;
    // Bound the delta-log: fold this reindex into a fresh base so the log does not
    // grow unbounded across repeated incremental reindexes.
    database.checkpoint()?;
    Ok(database_directory)
}

/// Loads catalog id maps by name; returns `None` on any missing entry (schema
/// drift), so the caller can fall back to a full rebuild.
fn load_catalog_maps(database: &Database) -> Option<CatalogMaps> {
    let read = database.begin_read();
    let catalog = read.catalog();
    let mut labels = BTreeMap::new();
    for kind in NodeKind::ALL {
        labels.insert(kind, catalog.label_id(kind.as_str())?);
    }
    let mut relation_types = BTreeMap::new();
    for kind in EdgeKind::ALL {
        relation_types.insert(kind, catalog.relation_type_id(kind.as_str())?);
    }
    let mut element_properties = BTreeMap::new();
    for property in ElementProperty::ALL {
        element_properties.insert(property.key(), catalog.property_key_id(property.key())?);
    }
    let mut relation_properties = BTreeMap::new();
    for property in RelationProperty::ALL {
        relation_properties.insert(property.key(), catalog.property_key_id(property.key())?);
    }
    Some(CatalogMaps {
        unresolved_label: catalog.label_id(NodeKind::Unresolved.as_str())?,
        source_role: catalog.role_id(SOURCE_ROLE)?,
        target_role: catalog.role_id(TARGET_ROLE)?,
        stable_key_index: catalog.index_id("element_stable_key_eq")?,
        labels,
        relation_types,
        element_properties,
        relation_properties,
    })
}

/// Resolves the element id of a stored symbol by its stable key via the native
/// equality index (`O(log n)`), or `None` when no current element carries it.
fn lookup_element_by_stable_key(
    read: &oxgraph::db::ReadTransaction,
    maps: &CatalogMaps,
    stable_key: &str,
) -> Option<ElementId> {
    let value = PropertyValue::Text(stable_key.to_string());
    read.lookup_index(maps.stable_key_index, oxgraph::db::IndexLookup::Equal(&value))
        .ok()?
        .into_iter()
        .find_map(|subject| match subject {
            PropertySubject::Element(id) => Some(id),
            PropertySubject::Relation(_) | PropertySubject::Incidence(_) => None,
        })
}

/// Removes the optional symbol properties so a reused element never retains a
/// stale value (for example a docstring removed by an edit).
fn clear_symbol_optionals(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
    element: ElementId,
    properties: &BTreeMap<&'static str, PropertyKeyId>,
) -> Result<()> {
    for key in ["raw_kind", "signature", "docstring", "source_preview"] {
        writer.remove_property(PropertySubject::Element(element), properties[key])?;
    }
    Ok(())
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

/// Registers code symbol and diagnostic labels.
fn register_labels(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
) -> Result<BTreeMap<NodeKind, LabelId>> {
    let mut labels = BTreeMap::new();
    for kind in NodeKind::ALL {
        labels.insert(kind, writer.register_label(kind.as_str())?);
    }
    Ok(labels)
}

/// Registers code edge relation types.
fn register_relation_types(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
) -> Result<BTreeMap<EdgeKind, RelationTypeId>> {
    let mut relation_types = BTreeMap::new();
    for kind in EdgeKind::ALL {
        relation_types.insert(kind, writer.register_relation_type(kind.as_str())?);
    }
    Ok(relation_types)
}

/// Registers element property keys, derived from the model catalog.
fn register_element_properties(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
) -> Result<BTreeMap<&'static str, PropertyKeyId>> {
    let mut properties = BTreeMap::new();
    for property in ElementProperty::ALL {
        let key = writer.register_property_key(
            property.key(),
            PropertyFamily::Element,
            property_type(property.kind()),
        )?;
        properties.insert(property.key(), key);
    }
    Ok(properties)
}

/// Registers relation property keys, derived from the model catalog.
fn register_relation_properties(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
) -> Result<BTreeMap<&'static str, PropertyKeyId>> {
    let mut properties = BTreeMap::new();
    for property in RelationProperty::ALL {
        let key = writer.register_property_key(
            property.key(),
            PropertyFamily::Relation,
            property_type(property.kind()),
        )?;
        properties.insert(property.key(), key);
    }
    Ok(properties)
}

/// Defines equality indexes for the catalog's indexed element keys.
fn define_property_indexes(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
    properties: &BTreeMap<&'static str, PropertyKeyId>,
) -> Result<()> {
    for property in ElementProperty::INDEXED {
        let name = property.key();
        writer.define_index(
            format!("element_{name}_eq"),
            IndexDefinition::PropertyEquality {
                key: properties[name],
            },
        )?;
    }
    Ok(())
}

/// Defines membership indexes used by status counts (consulted via `lookup_index`).
fn define_count_indexes(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
    labels: &BTreeMap<NodeKind, LabelId>,
    unresolved_label: LabelId,
    relation_types: &BTreeMap<EdgeKind, RelationTypeId>,
) -> Result<()> {
    writer.define_index(
        label_index_name(NodeKind::File),
        IndexDefinition::Label {
            label: labels[&NodeKind::File],
        },
    )?;
    writer.define_index(
        label_index_name(NodeKind::Unresolved),
        IndexDefinition::Label {
            label: unresolved_label,
        },
    )?;
    writer.define_index(
        type_index_name(EdgeKind::Calls),
        IndexDefinition::RelationType {
            relation_type: relation_types[&EdgeKind::Calls],
        },
    )?;
    Ok(())
}

/// Writes symbol properties to one element.
fn set_symbol_properties(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
    element: ElementId,
    properties: &BTreeMap<&'static str, PropertyKeyId>,
    node: &SymbolNode,
) -> Result<()> {
    set_text(writer, element, properties, "stable_key", &node.stable_key)?;
    set_text(writer, element, properties, "name", &node.name)?;
    set_text(
        writer,
        element,
        properties,
        "qualified_name",
        &node.qualified_name,
    )?;
    set_text(writer, element, properties, "kind", node.kind.as_str())?;
    if let Some(raw_kind) = &node.raw_kind {
        set_text(writer, element, properties, "raw_kind", raw_kind)?;
    }
    set_text(
        writer,
        element,
        properties,
        "language",
        node.language.as_str(),
    )?;
    set_text(writer, element, properties, "file_path", &node.file_path)?;
    if let Some(signature) = &node.signature {
        set_text(writer, element, properties, "signature", signature)?;
    }
    if let Some(docstring) = &node.docstring {
        set_text(writer, element, properties, "docstring", docstring)?;
    }
    if let Some(source_preview) = &node.source_preview {
        set_text(
            writer,
            element,
            properties,
            "source_preview",
            source_preview,
        )?;
    }
    set_span_properties(writer, element, properties, node.span)?;
    Ok(())
}

/// Writes unresolved-reference diagnostic properties.
fn set_unresolved_properties(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
    element: ElementId,
    properties: &BTreeMap<&'static str, PropertyKeyId>,
    unresolved: &UnresolvedReference,
) -> Result<()> {
    let joined = unresolved.target.joined();
    let stable_key = format!(
        "unresolved:{}:{}:{}:{}",
        unresolved.source_key,
        unresolved.kind.as_str(),
        joined,
        unresolved.span.start_byte
    );
    set_text(writer, element, properties, "stable_key", &stable_key)?;
    set_text(writer, element, properties, "name", &unresolved.target.raw)?;
    set_text(writer, element, properties, "qualified_name", &joined)?;
    set_text(writer, element, properties, "kind", "unresolved_reference")?;
    set_text(
        writer,
        element,
        properties,
        "file_path",
        &unresolved.file_path,
    )?;
    set_text(
        writer,
        element,
        properties,
        "unresolved_source_key",
        &unresolved.source_key,
    )?;
    set_text(
        writer,
        element,
        properties,
        "target_raw",
        &unresolved.target.raw,
    )?;
    set_text(writer, element, properties, "target_path", &joined)?;
    if let Some(qualifier) = &unresolved.target.qualifier {
        set_text(writer, element, properties, "target_qualifier", qualifier)?;
    }
    set_text(
        writer,
        element,
        properties,
        "target_kind_hint",
        unresolved.target.kind_hint.as_str(),
    )?;
    set_text(
        writer,
        element,
        properties,
        "unresolved_edge_kind",
        unresolved.kind.as_str(),
    )?;
    if let Some(reason) = &unresolved.reason {
        set_text(writer, element, properties, "reason", reason)?;
    }
    set_span_properties(writer, element, properties, unresolved.span)
}

/// Writes relation properties to one code edge.
fn set_relation_properties(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
    relation: RelationId,
    properties: &BTreeMap<&'static str, PropertyKeyId>,
    edge: &ResolvedEdge,
) -> Result<()> {
    set_relation_text(
        writer,
        relation,
        properties,
        "edge_kind",
        edge.kind.as_str(),
    )?;
    set_relation_text(writer, relation, properties, "source_key", &edge.source_key)?;
    set_relation_text(writer, relation, properties, "target_key", &edge.target_key)?;
    set_relation_text(
        writer,
        relation,
        properties,
        "resolution",
        edge.resolution.as_str(),
    )?;

    if let Some(reference) = &edge.reference {
        set_relation_text(
            writer,
            relation,
            properties,
            "site_file_path",
            &reference.file_path,
        )?;
        set_relation_usize(
            writer,
            relation,
            properties,
            "site_start_line",
            reference.span.start_line,
        )?;
        set_relation_usize(
            writer,
            relation,
            properties,
            "site_start_column",
            reference.span.start_column,
        )?;
        set_relation_usize(
            writer,
            relation,
            properties,
            "site_end_line",
            reference.span.end_line,
        )?;
        set_relation_usize(
            writer,
            relation,
            properties,
            "site_end_column",
            reference.span.end_column,
        )?;
        set_relation_usize(
            writer,
            relation,
            properties,
            "site_start_byte",
            reference.span.start_byte,
        )?;
        set_relation_usize(
            writer,
            relation,
            properties,
            "site_end_byte",
            reference.span.end_byte,
        )?;
        set_relation_text(writer, relation, properties, "site_text", &reference.text)?;
    }
    Ok(())
}

/// Writes source span properties.
fn set_span_properties(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
    element: ElementId,
    properties: &BTreeMap<&'static str, PropertyKeyId>,
    span: SourceSpan,
) -> Result<()> {
    set_usize(writer, element, properties, "start_byte", span.start_byte)?;
    set_usize(writer, element, properties, "end_byte", span.end_byte)?;
    set_usize(writer, element, properties, "start_line", span.start_line)?;
    set_usize(
        writer,
        element,
        properties,
        "start_column",
        span.start_column,
    )?;
    set_usize(writer, element, properties, "end_line", span.end_line)?;
    set_usize(writer, element, properties, "end_column", span.end_column)
}

/// Sets a text property on any subject (the one place that logic lives).
fn set_property_text(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
    subject: PropertySubject,
    properties: &BTreeMap<&'static str, PropertyKeyId>,
    key: &'static str,
    value: &str,
) -> Result<()> {
    writer.set_property(
        subject,
        properties[key],
        PropertyValue::Text(value.to_string()),
    )?;
    Ok(())
}

/// Sets a usize property (as an OxGraph integer) on any subject.
fn set_property_int(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
    subject: PropertySubject,
    properties: &BTreeMap<&'static str, PropertyKeyId>,
    key: &'static str,
    value: usize,
) -> Result<()> {
    let value = i64::try_from(value).map_err(|_| Error::IntegerOverflow { key, value })?;
    writer.set_property(subject, properties[key], PropertyValue::Integer(value))?;
    Ok(())
}

/// Sets a text property on a relation.
fn set_relation_text(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
    relation: RelationId,
    properties: &BTreeMap<&'static str, PropertyKeyId>,
    key: &'static str,
    value: &str,
) -> Result<()> {
    set_property_text(
        writer,
        PropertySubject::Relation(relation),
        properties,
        key,
        value,
    )
}

/// Sets a usize property on a relation as an OxGraph integer.
fn set_relation_usize(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
    relation: RelationId,
    properties: &BTreeMap<&'static str, PropertyKeyId>,
    key: &'static str,
    value: usize,
) -> Result<()> {
    set_property_int(
        writer,
        PropertySubject::Relation(relation),
        properties,
        key,
        value,
    )
}

/// Sets a text property on an element.
fn set_text(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
    element: ElementId,
    properties: &BTreeMap<&'static str, PropertyKeyId>,
    key: &'static str,
    value: &str,
) -> Result<()> {
    set_property_text(
        writer,
        PropertySubject::Element(element),
        properties,
        key,
        value,
    )
}

/// Sets a usize property on an element as an OxGraph integer.
fn set_usize(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
    element: ElementId,
    properties: &BTreeMap<&'static str, PropertyKeyId>,
    key: &'static str,
    value: usize,
) -> Result<()> {
    set_property_int(
        writer,
        PropertySubject::Element(element),
        properties,
        key,
        value,
    )
}

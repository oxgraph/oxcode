//! Database write path: rebuilds the OxGraph store from a resolved index and
//! derives the whole catalog (labels, relation types, properties, indexes) from
//! the model's typed schema.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use oxcode_model::{
    CALLS_PROJECTION, EdgeKind, ElementProperty, NodeKind, PropertyKind, RelationProperty,
    ResolvedEdge, ResolvedIndex, SOURCE_ROLE, SourceSpan, SymbolNode, TARGET_ROLE,
    UnresolvedReference,
};
use oxgraph::db::{
    Database, ElementId, GraphProjectionDefinition, IndexDefinition, LabelId, ProjectionDefinition,
    PropertyFamily, PropertyKeyId, PropertySubject, PropertyType, PropertyValue, RelationId,
    RelationTypeId,
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
pub(crate) fn rebuild_database(root: &Path, index: &ResolvedIndex) -> Result<PathBuf> {
    let index_directory = index_dir(root);
    let database_directory = database_dir(root);
    let temp_directory = index_directory.join("index.oxgdb.tmp");
    let backup_directory = index_directory.join("index.oxgdb.old");
    std::fs::create_dir_all(&index_directory)
        .map_err(|source| Error::fs(&index_directory, source))?;
    write_index_gitignore(&index_directory)?;

    // Recover from a crash between the two swap renames: the live database is
    // gone but a validated copy survives in the backup. Promote it before any
    // deletion so the backup machinery cannot destroy the only copy.
    if !database_directory.exists()
        && backup_directory.exists()
        && Database::validate_path(&backup_directory).is_ok()
    {
        std::fs::rename(&backup_directory, &database_directory)
            .map_err(|source| Error::fs(&backup_directory, source))?;
    }

    for stale in [&temp_directory, &backup_directory] {
        if stale.exists() {
            std::fs::remove_dir_all(stale).map_err(|source| Error::fs(stale, source))?;
        }
    }

    let mut database = Database::create(&temp_directory)?;
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
    writer.define_projection(ProjectionDefinition::Graph(GraphProjectionDefinition {
        name: CALLS_PROJECTION.to_owned(),
        relation_types: BTreeSet::from([relation_types[&EdgeKind::Calls]]),
        source_role,
        target_role,
    }))?;

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
    database.validate()?;
    drop(database);

    if database_directory.exists() {
        std::fs::rename(&database_directory, &backup_directory)
            .map_err(|source| Error::fs(&database_directory, source))?;
    }
    std::fs::rename(&temp_directory, &database_directory)
        .map_err(|source| Error::fs(&temp_directory, source))?;
    if backup_directory.exists() {
        std::fs::remove_dir_all(&backup_directory)
            .map_err(|source| Error::fs(&backup_directory, source))?;
    }
    Ok(database_directory)
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

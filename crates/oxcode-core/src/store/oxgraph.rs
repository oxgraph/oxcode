//! Native OxGraph storage and typed read adapter.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use oxcode_model::{
    CALLS_PROJECTION, CallEdgeSummary, CallGraphReport, CallSiteSummary, CatalogStatus,
    CodeLocation, EdgeKind, ElementProperty, ExpandedQueryReport, ExpandedQueryRow,
    ExpandedQueryValue, GraphDirection, LanguageId, NodeKind, ProjectStatus, QualifiedName,
    RelationProperty, Selector, SourceSpan, SymbolId, SymbolKey, SymbolReport, SymbolSummary,
    TraversedSymbol,
};
use oxgraph::{
    db::{
        Database, ElementId, PropertyKeyId, PropertySubject, PropertyValue, QueryLanguage,
        QueryResult, QueryValue, RelationId, TraversalDirection, TraversalOptions,
    },
    graph::{EdgeSourceGraph, EdgeTargetGraph, OutgoingGraph},
    topology::{
        CanonicalElementIdentity, CanonicalRelationIdentity, LocalElementIdentity,
        LocalRelationIdentity,
    },
};

use crate::{
    error::{Error, Result},
    format::format_query_value,
    paths::{canonical_root, database_dir},
};

mod write;

pub(crate) use write::rebuild_database;
use write::{label_index_name, type_index_name};

/// Returns project database status.
pub(crate) fn project_status(root: impl AsRef<Path>) -> Result<ProjectStatus> {
    let root = canonical_root(root.as_ref())?;
    let database_path = database_dir(&root);
    // Ask the storage crate whether a valid store exists rather than probing its
    // private on-disk filename.
    if Database::validate_path(&database_path).is_err() {
        return Ok(ProjectStatus {
            root,
            database: database_path,
            database_exists: false,
            visible_commit_seq: None,
            last_transaction_id: None,
            elements: 0,
            relations: 0,
            incidences: 0,
            files: 0,
            calls: 0,
            unresolved_references: 0,
            catalog: CatalogStatus::default(),
        });
    }

    let database = Database::open(&database_path)?;
    let status = database.status();
    let read = database.begin_read();
    let files = count_index(&read, &label_index_name(NodeKind::File))?;
    let calls = count_index(&read, &type_index_name(EdgeKind::Calls))?;
    let unresolved_references = count_index(&read, &label_index_name(NodeKind::Unresolved))?;
    Ok(ProjectStatus {
        root,
        database: database_path,
        database_exists: true,
        visible_commit_seq: Some(status.visible_commit_seq.get()),
        last_transaction_id: Some(status.last_transaction_id.get()),
        elements: status.element_count,
        relations: status.relation_count,
        incidences: status.incidence_count,
        files,
        calls,
        unresolved_references,
        catalog: CatalogStatus {
            role_count: status.catalog.role_count,
            label_count: status.catalog.label_count,
            relation_type_count: status.catalog.relation_type_count,
            property_key_count: status.catalog.property_key_count,
            projection_count: status.catalog.projection_count,
            index_count: status.catalog.index_count,
        },
    })
}

/// An opened project database with its property-key schema resolved once.
pub(crate) struct OxGraphStore {
    database: Database,
    element_keys: ElementPropertyKeys,
    relation_keys: RelationPropertyKeys,
}

impl OxGraphStore {
    pub(crate) fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = canonical_root(root.as_ref())?;
        let database = Database::open(database_dir(&root))?;
        let read = database.begin_read();
        let element_keys = ElementPropertyKeys::load(&read)?;
        let relation_keys = RelationPropertyKeys::load(&read)?;
        drop(read);
        Ok(Self {
            database,
            element_keys,
            relation_keys,
        })
    }

    pub(crate) fn query(&self, language: QueryLanguage, query: &str) -> Result<QueryResult> {
        let prepared = self.database.prepare(language, query)?;
        Ok(self.database.begin_read().execute(&prepared)?)
    }

    pub(crate) fn explain(&self, language: QueryLanguage, query: &str) -> Result<String> {
        let prepared = self.database.prepare(language, query)?;
        Ok(self.database.begin_read().explain(&prepared))
    }

    pub(crate) fn with_read<T>(&self, f: impl FnOnce(&ReadSession<'_>) -> Result<T>) -> Result<T> {
        let session = ReadSession {
            database: &self.database,
            read: self.database.begin_read(),
            element_keys: &self.element_keys,
            relation_keys: &self.relation_keys,
        };
        f(&session)
    }
}

/// One read snapshot over an opened store, sharing the store's resolved schema.
pub(crate) struct ReadSession<'store> {
    database: &'store Database,
    read: oxgraph::db::ReadTransaction,
    element_keys: &'store ElementPropertyKeys,
    relation_keys: &'store RelationPropertyKeys,
}

impl ReadSession<'_> {
    pub(crate) fn execute_query(
        &self,
        language: QueryLanguage,
        query: &str,
    ) -> Result<QueryResult> {
        let prepared = self.database.prepare(language, query)?;
        Ok(self.read.execute(&prepared)?)
    }

    pub(crate) fn resolve_selector(&self, selector: &str) -> Result<Vec<SymbolSummary>> {
        resolve_selector_in_read(&self.read, self.element_keys, selector)
    }

    pub(crate) fn resolve_one_symbol(&self, selector: &str) -> Result<SymbolSummary> {
        resolve_one_symbol_in_read(&self.read, self.element_keys, selector)
    }

    /// Describes one selected symbol as an agent-facing report.
    pub(crate) fn describe_symbol(&self, selector: &str) -> Result<SymbolReport> {
        Ok(SymbolReport {
            selector: selector.to_string(),
            symbol: self.resolve_one_symbol(selector)?,
        })
    }

    /// Executes a query and expands its result on this same snapshot.
    pub(crate) fn query_expanded(
        &self,
        language: QueryLanguage,
        query: &str,
    ) -> Result<ExpandedQueryReport> {
        let result = self.execute_query(language, query)?;
        self.expand_query_result(&result)
    }

    pub(crate) fn call_graph(
        &self,
        selector: &str,
        direction: GraphDirection,
        depth: usize,
        limit: usize,
    ) -> Result<CallGraphReport> {
        let element_keys = self.element_keys;
        let relation_keys = self.relation_keys;
        let seed = resolve_one_symbol_in_read(&self.read, element_keys, selector)?;
        let seed_id = ElementId::new(seed.id.get());
        let mut symbols = vec![TraversedSymbol {
            depth: 0,
            symbol: seed.clone(),
        }];
        let mut edges = Vec::new();

        // Resolve the calls projection (id for the engine traversal, materialized
        // graph for edge hydration) and confirm the seed participates in it.
        let projection = self
            .read
            .catalog()
            .projection_id(CALLS_PROJECTION)
            .and_then(|id| {
                self.read
                    .graph_projection_by_name(CALLS_PROJECTION)
                    .ok()
                    .map(|graph| (id, graph))
            })
            .filter(|(_, graph)| graph.local_element_id(seed_id).is_some());
        let Some((projection_id, graph)) = projection else {
            return Ok(CallGraphReport {
                selector: selector.to_string(),
                seed,
                direction,
                depth,
                limit,
                symbols,
                edges,
            });
        };

        // Discover the node set + shortest depths with the engine's deterministic
        // BFS (clean stop at the row limit), then hydrate.
        let options = TraversalOptions {
            max_depth: depth,
            direction: traversal_direction(direction),
            limit,
            include_start: false,
        };
        let traversal = self
            .read
            .traverse_graph(projection_id, &[seed_id], options)?;
        let mut depth_of = BTreeMap::from([(seed_id, 0_usize)]);
        for row in traversal.rows() {
            if let Some(symbol) =
                symbol_summary_from_element(&self.read, element_keys, row.element)?
            {
                depth_of.insert(row.element, row.depth);
                symbols.push(TraversedSymbol {
                    depth: row.depth,
                    symbol,
                });
            }
        }

        // Emit every calls edge whose endpoints are both in the discovered set,
        // so the report never references a symbol it omitted. Ordering is
        // deterministic (sorted element/relation ids).
        let mut emitted = BTreeSet::new();
        let discovered_ids = depth_of.keys().copied().collect::<Vec<_>>();
        let mut local_edges = Vec::new();
        for element in discovered_ids {
            if let Some(local) = graph.local_element_id(element) {
                local_edges.extend(graph.outgoing_edges(local).collect::<Vec<_>>());
            }
        }
        for local_edge in local_edges {
            let source = graph.canonical_element_id(graph.source(local_edge));
            let target = graph.canonical_element_id(graph.target(local_edge));
            if !depth_of.contains_key(&target) {
                continue;
            }
            let relation = graph.canonical_relation_id(local_edge);
            if !emitted.insert(relation) {
                continue;
            }
            let edge_depth = depth_of
                .get(&source)
                .copied()
                .unwrap_or(0)
                .max(depth_of.get(&target).copied().unwrap_or(0));
            if let Some(edge) = call_edge_summary(
                &self.read,
                element_keys,
                relation_keys,
                CallEdgeRef {
                    relation,
                    source,
                    target,
                    depth: Some(edge_depth),
                },
            )? {
                edges.push(edge);
            }
        }

        Ok(CallGraphReport {
            selector: selector.to_string(),
            seed,
            direction,
            depth,
            limit,
            symbols,
            edges,
        })
    }

    pub(crate) fn expand_query_result(&self, result: &QueryResult) -> Result<ExpandedQueryReport> {
        let graph = self.read.graph_projection_by_name(CALLS_PROJECTION).ok();
        let rows = result
            .rows()
            .iter()
            .map(|row| {
                let values = row
                    .values
                    .iter()
                    .map(|value| {
                        expand_query_value(
                            &self.read,
                            self.element_keys,
                            self.relation_keys,
                            graph.as_ref(),
                            value,
                        )
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(ExpandedQueryRow { values })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(ExpandedQueryReport { rows })
    }
}

/// Property keys needed for symbol expansion.
struct ElementPropertyKeys {
    /// Stable key property.
    stable_key: PropertyKeyId,
    /// Name property.
    name: PropertyKeyId,
    /// Qualified name property.
    qualified_name: PropertyKeyId,
    /// Kind property.
    kind: PropertyKeyId,
    /// Language property.
    language: PropertyKeyId,
    /// File path property.
    file_path: PropertyKeyId,
    /// Start byte property.
    start_byte: PropertyKeyId,
    /// End byte property.
    end_byte: PropertyKeyId,
    /// Start line property.
    start_line: PropertyKeyId,
    /// Start column property.
    start_column: PropertyKeyId,
    /// End line property.
    end_line: PropertyKeyId,
    /// End column property.
    end_column: PropertyKeyId,
}

impl ElementPropertyKeys {
    /// Loads required element property keys from the catalog.
    fn load(read: &oxgraph::db::ReadTransaction) -> Result<Self> {
        Ok(Self {
            stable_key: require_element_key(read, ElementProperty::StableKey)?,
            name: require_element_key(read, ElementProperty::Name)?,
            qualified_name: require_element_key(read, ElementProperty::QualifiedName)?,
            kind: require_element_key(read, ElementProperty::Kind)?,
            language: require_element_key(read, ElementProperty::Language)?,
            file_path: require_element_key(read, ElementProperty::FilePath)?,
            start_byte: require_element_key(read, ElementProperty::StartByte)?,
            end_byte: require_element_key(read, ElementProperty::EndByte)?,
            start_line: require_element_key(read, ElementProperty::StartLine)?,
            start_column: require_element_key(read, ElementProperty::StartColumn)?,
            end_line: require_element_key(read, ElementProperty::EndLine)?,
            end_column: require_element_key(read, ElementProperty::EndColumn)?,
        })
    }
}

/// Property keys needed for relation expansion.
struct RelationPropertyKeys {
    /// Reference-site file path property.
    site_file_path: PropertyKeyId,
    /// Reference-site start line property.
    site_start_line: PropertyKeyId,
    /// Reference-site start column property.
    site_start_column: PropertyKeyId,
    /// Reference-site end line property.
    site_end_line: PropertyKeyId,
    /// Reference-site end column property.
    site_end_column: PropertyKeyId,
    /// Reference-site start byte property.
    site_start_byte: PropertyKeyId,
    /// Reference-site end byte property.
    site_end_byte: PropertyKeyId,
    /// Reference-site expression text property.
    site_text: PropertyKeyId,
}

impl RelationPropertyKeys {
    /// Loads required relation property keys from the catalog.
    fn load(read: &oxgraph::db::ReadTransaction) -> Result<Self> {
        Ok(Self {
            site_file_path: require_relation_key(read, RelationProperty::SiteFilePath)?,
            site_start_line: require_relation_key(read, RelationProperty::SiteStartLine)?,
            site_start_column: require_relation_key(read, RelationProperty::SiteStartColumn)?,
            site_end_line: require_relation_key(read, RelationProperty::SiteEndLine)?,
            site_end_column: require_relation_key(read, RelationProperty::SiteEndColumn)?,
            site_start_byte: require_relation_key(read, RelationProperty::SiteStartByte)?,
            site_end_byte: require_relation_key(read, RelationProperty::SiteEndByte)?,
            site_text: require_relation_key(read, RelationProperty::SiteText)?,
        })
    }
}

/// Maps an agent traversal direction to the engine's traversal direction.
const fn traversal_direction(direction: GraphDirection) -> TraversalDirection {
    match direction {
        GraphDirection::Outgoing => TraversalDirection::Outgoing,
        GraphDirection::Incoming => TraversalDirection::Incoming,
        GraphDirection::Both => TraversalDirection::Both,
    }
}

/// Resolves exactly one symbol or returns a selector error.
fn resolve_one_symbol_in_read(
    read: &oxgraph::db::ReadTransaction,
    keys: &ElementPropertyKeys,
    selector: &str,
) -> Result<SymbolSummary> {
    let matches = resolve_selector_in_read(read, keys, selector)?;
    match matches.as_slice() {
        [symbol] => Ok(symbol.clone()),
        [] => Err(Error::SelectorNotFound {
            selector: selector.to_string(),
        }),
        _ => Err(Error::AmbiguousSelector {
            selector: selector.to_string(),
            matches,
        }),
    }
}

/// Resolves one selector against a read transaction.
///
/// The selector grammar lives entirely in [`Selector::parse`]; this only maps
/// the parsed variants to lookups. Malformed selectors resolve to no matches
/// (callers needing exactly one symbol then report `SelectorNotFound`).
fn resolve_selector_in_read(
    read: &oxgraph::db::ReadTransaction,
    keys: &ElementPropertyKeys,
    selector: &str,
) -> Result<Vec<SymbolSummary>> {
    match Selector::parse(selector) {
        Ok(Selector::Element(id)) => {
            symbol_summary_from_element(read, keys, ElementId::new(id.get()))
                .map(|summary| summary.filter(is_agent_symbol).into_iter().collect())
        }
        Ok(Selector::Name(name)) => {
            lookup_symbols_by_property(read, keys, ElementProperty::Name, &name)
        }
        Ok(Selector::QualifiedName(qualified)) => lookup_symbols_by_property(
            read,
            keys,
            ElementProperty::QualifiedName,
            qualified.as_str(),
        ),
        Ok(Selector::FileLine { path, line }) => {
            resolve_file_line_selector(read, keys, path.as_str(), line)
        }
        Err(_) => Ok(Vec::new()),
    }
}

/// Looks up symbols by one exact text property via the property's equality
/// index.
///
/// Every property passed here is in [`ElementProperty::INDEXED`], so the write
/// path always defines its `element_<key>_eq` index. A missing index therefore
/// means catalog drift and fails loudly rather than silently scanning.
fn lookup_symbols_by_property(
    read: &oxgraph::db::ReadTransaction,
    keys: &ElementPropertyKeys,
    property: ElementProperty,
    value: &str,
) -> Result<Vec<SymbolSummary>> {
    let value_property = PropertyValue::Text(value.to_string());
    let index_name = format!("element_{}_eq", property.key());
    let index_id = read
        .catalog()
        .index_id(&index_name)
        .ok_or(Error::MissingCatalog {
            item: "index",
            name: index_name,
        })?;
    let subjects = read.lookup_index(index_id, oxgraph::db::IndexLookup::Equal(&value_property))?;
    let mut symbols = subjects
        .into_iter()
        .filter_map(|subject| match subject {
            PropertySubject::Element(id) => Some(id),
            PropertySubject::Relation(_) | PropertySubject::Incidence(_) => None,
        })
        .map(|id| symbol_summary_from_element(read, keys, id))
        .filter_map(|result| match result {
            Ok(Some(symbol)) => Some(Ok(symbol)),
            Ok(None) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<Result<Vec<_>>>()?;
    symbols.retain(is_agent_symbol);
    symbols.sort_by(|left, right| {
        left.qualified_name
            .cmp(&right.qualified_name)
            .then(left.definition.file_path.cmp(&right.definition.file_path))
            .then(
                left.definition
                    .span
                    .start_byte
                    .cmp(&right.definition.span.start_byte),
            )
    });
    Ok(symbols)
}

/// Resolves a `file:path:line` selector to the innermost matching symbol.
fn resolve_file_line_selector(
    read: &oxgraph::db::ReadTransaction,
    keys: &ElementPropertyKeys,
    file_path: &str,
    line: usize,
) -> Result<Vec<SymbolSummary>> {
    let mut symbols = lookup_symbols_by_property(read, keys, ElementProperty::FilePath, file_path)?;
    symbols.retain(|symbol| {
        !matches!(symbol.kind, NodeKind::File | NodeKind::Unresolved)
            && symbol.definition.span.contains_line(line)
    });
    let Some(shortest_span) = symbols
        .iter()
        .map(|symbol| symbol.definition.span.byte_len())
        .min()
    else {
        return Ok(Vec::new());
    };
    symbols.retain(|symbol| symbol.definition.span.byte_len() == shortest_span);
    // Break ties deterministically toward the innermost definition (greatest
    // start byte), then by stable key, and return exactly one symbol so a
    // cursor-position lookup never errors with an ambiguity.
    symbols.sort_by(|left, right| {
        right
            .definition
            .span
            .start_byte
            .cmp(&left.definition.span.start_byte)
            .then(left.stable_key.cmp(&right.stable_key))
    });
    symbols.truncate(1);
    Ok(symbols)
}

/// Expands one query value.
fn expand_query_value(
    read: &oxgraph::db::ReadTransaction,
    element_keys: &ElementPropertyKeys,
    relation_keys: &RelationPropertyKeys,
    graph: Option<&oxgraph::db::GraphProjection>,
    value: &QueryValue,
) -> Result<ExpandedQueryValue> {
    let raw = format_query_value(value);
    let mut expanded = ExpandedQueryValue {
        raw,
        symbol: None,
        call_edge: None,
    };
    match value {
        QueryValue::Element(id) => {
            expanded.symbol = symbol_summary_from_element(read, element_keys, *id)?;
        }
        QueryValue::Relation(id) => {
            expanded.call_edge = graph
                .and_then(|graph| graph.local_relation_id(*id).map(|local| (graph, local)))
                .map(|(graph, local)| {
                    call_edge_summary(
                        read,
                        element_keys,
                        relation_keys,
                        CallEdgeRef {
                            relation: *id,
                            source: graph.canonical_element_id(graph.source(local)),
                            target: graph.canonical_element_id(graph.target(local)),
                            depth: None,
                        },
                    )
                })
                .transpose()?
                .flatten();
        }
        QueryValue::Incidence(record) => {
            expanded.symbol = symbol_summary_from_element(read, element_keys, record.element)?;
        }
        QueryValue::Subject(subject) => {
            if let PropertySubject::Element(id) = subject {
                expanded.symbol = symbol_summary_from_element(read, element_keys, *id)?;
            }
        }
        QueryValue::Property(_) | QueryValue::Text(_) | QueryValue::Projection(_) => {}
    }
    Ok(expanded)
}

/// Canonical identity of one call edge: its `relation` plus endpoint elements
/// and traversal `depth`.
struct CallEdgeRef {
    relation: RelationId,
    source: ElementId,
    target: ElementId,
    depth: Option<usize>,
}

/// Builds one call edge summary from canonical endpoint IDs.
fn call_edge_summary(
    read: &oxgraph::db::ReadTransaction,
    element_keys: &ElementPropertyKeys,
    relation_keys: &RelationPropertyKeys,
    edge: CallEdgeRef,
) -> Result<Option<CallEdgeSummary>> {
    let Some(source) = symbol_summary_from_element(read, element_keys, edge.source)? else {
        return Ok(None);
    };
    let Some(target) = symbol_summary_from_element(read, element_keys, edge.target)? else {
        return Ok(None);
    };
    Ok(Some(CallEdgeSummary {
        relation_id: edge.relation.get(),
        depth: edge.depth,
        source,
        target,
        call_site: call_site_summary(read, relation_keys, edge.relation),
    }))
}

/// Reads call-site metadata from one relation.
fn call_site_summary(
    read: &oxgraph::db::ReadTransaction,
    keys: &RelationPropertyKeys,
    relation: RelationId,
) -> Option<CallSiteSummary> {
    let subject = PropertySubject::Relation(relation);
    let file_path = optional_text_property(read, subject, keys.site_file_path)?;
    let span = SourceSpan {
        start_byte: optional_usize_property(read, subject, keys.site_start_byte)?,
        end_byte: optional_usize_property(read, subject, keys.site_end_byte)?,
        start_line: optional_usize_property(read, subject, keys.site_start_line)?,
        start_column: optional_usize_property(read, subject, keys.site_start_column)?,
        end_line: optional_usize_property(read, subject, keys.site_end_line)?,
        end_column: optional_usize_property(read, subject, keys.site_end_column)?,
    };
    Some(CallSiteSummary {
        location: CodeLocation::new(file_path, span),
        text: optional_text_property(read, subject, keys.site_text).unwrap_or_default(),
    })
}

/// Reads symbol properties for one element, returning `None` when the element
/// or a required property is absent.
fn symbol_summary_from_element(
    read: &oxgraph::db::ReadTransaction,
    keys: &ElementPropertyKeys,
    id: ElementId,
) -> Result<Option<SymbolSummary>> {
    // An element that does not exist in this snapshot is simply not a symbol;
    // an element that DOES exist but is missing a required property is corrupt
    // and fails loudly rather than silently vanishing from results.
    if read.element(id).is_none() {
        return Ok(None);
    }
    let subject = PropertySubject::Element(id);
    let stable_key = require_text(read, subject, keys.stable_key, ElementProperty::StableKey)?;
    let name = require_text(read, subject, keys.name, ElementProperty::Name)?;
    let qualified_name = require_text(
        read,
        subject,
        keys.qualified_name,
        ElementProperty::QualifiedName,
    )?;
    let kind_text = require_text(read, subject, keys.kind, ElementProperty::Kind)?;
    let kind = NodeKind::try_from(kind_text.as_str()).map_err(|_| Error::CorruptValue {
        kind: "node kind",
        value: kind_text,
    })?;
    let file_path = require_text(read, subject, keys.file_path, ElementProperty::FilePath)?;
    let span = SourceSpan {
        start_byte: require_int(read, subject, keys.start_byte, ElementProperty::StartByte)?,
        end_byte: require_int(read, subject, keys.end_byte, ElementProperty::EndByte)?,
        start_line: require_int(read, subject, keys.start_line, ElementProperty::StartLine)?,
        start_column: require_int(
            read,
            subject,
            keys.start_column,
            ElementProperty::StartColumn,
        )?,
        end_line: require_int(read, subject, keys.end_line, ElementProperty::EndLine)?,
        end_column: require_int(read, subject, keys.end_column, ElementProperty::EndColumn)?,
    };
    Ok(Some(SymbolSummary {
        id: SymbolId::new(id.get()),
        stable_key: SymbolKey::from(stable_key),
        name,
        qualified_name: QualifiedName::from(qualified_name),
        kind,
        language: LanguageId::from(
            optional_text_property(read, subject, keys.language).unwrap_or_default(),
        ),
        definition: CodeLocation::new(file_path, span),
    }))
}

/// Reads a required text property, erroring if a present element lacks it.
fn require_text(
    read: &oxgraph::db::ReadTransaction,
    subject: PropertySubject,
    key: PropertyKeyId,
    property: ElementProperty,
) -> Result<String> {
    optional_text_property(read, subject, key).ok_or_else(|| Error::MissingProperty {
        name: property.key().to_string(),
    })
}

/// Reads a required unsigned-integer property, erroring if it is absent.
fn require_int(
    read: &oxgraph::db::ReadTransaction,
    subject: PropertySubject,
    key: PropertyKeyId,
    property: ElementProperty,
) -> Result<usize> {
    optional_usize_property(read, subject, key).ok_or_else(|| Error::MissingProperty {
        name: property.key().to_string(),
    })
}

/// Returns whether a symbol should participate in agent selectors.
fn is_agent_symbol(symbol: &SymbolSummary) -> bool {
    symbol.kind != NodeKind::Unresolved
}

/// Requires a property key by catalog name.
fn require_property_key(
    read: &oxgraph::db::ReadTransaction,
    name: &'static str,
) -> Result<PropertyKeyId> {
    read.catalog()
        .property_key_id(name)
        .ok_or_else(|| Error::MissingCatalog {
            item: "property",
            name: name.to_string(),
        })
}

/// Requires a catalog element property key.
fn require_element_key(
    read: &oxgraph::db::ReadTransaction,
    property: ElementProperty,
) -> Result<PropertyKeyId> {
    require_property_key(read, property.key())
}

/// Requires a catalog relation property key.
fn require_relation_key(
    read: &oxgraph::db::ReadTransaction,
    property: RelationProperty,
) -> Result<PropertyKeyId> {
    require_property_key(read, property.key())
}

/// Reads one optional text property.
fn optional_text_property(
    read: &oxgraph::db::ReadTransaction,
    subject: PropertySubject,
    key: PropertyKeyId,
) -> Option<String> {
    match read.property(subject, key) {
        Some(PropertyValue::Text(value)) => Some(value.clone()),
        Some(PropertyValue::Boolean(_) | PropertyValue::Integer(_)) | None => None,
    }
}

/// Reads one optional unsigned integer property.
fn optional_usize_property(
    read: &oxgraph::db::ReadTransaction,
    subject: PropertySubject,
    key: PropertyKeyId,
) -> Option<usize> {
    match read.property(subject, key) {
        Some(PropertyValue::Integer(value)) => usize::try_from(*value).ok(),
        Some(PropertyValue::Boolean(_) | PropertyValue::Text(_)) | None => None,
    }
}

/// Counts index members by name, returning `0` when the index is absent.
///
/// Uses the membership index rather than materializing query result rows.
fn count_index(read: &oxgraph::db::ReadTransaction, name: &str) -> Result<usize> {
    match read.catalog().index_id(name) {
        Some(index_id) => Ok(read
            .lookup_index(index_id, oxgraph::db::IndexLookup::All)?
            .len()),
        None => Ok(0),
    }
}

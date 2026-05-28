//! Native OxGraph storage and typed read adapter.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::{Path, PathBuf},
};

use oxcode_model::{
    CallEdgeSummary, CallGraphReport, CallSiteSummary, CatalogStatus, CodeLocation, EdgeKind,
    ExpandedQueryReport, ExpandedQueryRow, ExpandedQueryValue, GraphDirection, NodeKind,
    ProjectStatus, ResolvedEdge, ResolvedIndex, SourceSpan, SourceUnit, SymbolNode, SymbolSummary,
    TraversedSymbol, UnresolvedReference,
};
use oxgraph::db::{
    Database, ElementId, GraphProjectionDefinition, IndexDefinition, LabelId, ProjectionDefinition,
    PropertyFamily, PropertyKeyId, PropertySubject, PropertyType, PropertyValue, QueryLanguage,
    QueryResult, QueryValue, RelationId, RelationTypeId,
};
use oxgraph::graph::{EdgeSourceGraph, EdgeTargetGraph, IncomingGraph, OutgoingGraph};
use oxgraph::topology::{
    CanonicalElementIdentity, CanonicalRelationIdentity, LocalElementIdentity,
    LocalRelationIdentity,
};

use crate::{
    error::{Error, Result},
    format::format_query_value,
    paths::{canonical_root, database_dir, index_dir},
};

mod schema {
    pub(super) const CALLS_PROJECTION: &str = "calls";
    pub(super) const SOURCE_ROLE: &str = "source";
    pub(super) const TARGET_ROLE: &str = "target";
    pub(super) const UNRESOLVED_LABEL: &str = "unresolved_reference";

    pub(super) const ELEMENT_INDEXED_KEYS: [&str; 6] = [
        "stable_key",
        "name",
        "qualified_name",
        "kind",
        "file_path",
        "language",
    ];
}

/// Returns project database status.
pub(crate) fn project_status(root: impl AsRef<Path>) -> Result<ProjectStatus> {
    let root = canonical_root(root.as_ref())?;
    let database_path = database_dir(&root);
    if !database_path.join("store.oxgdb").exists() {
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
    let files = count_query(&database, &read, "MATCH ELEMENTS HAS LABEL file")?;
    let calls = count_query(&database, &read, "MATCH RELATIONS TYPE calls")?;
    let unresolved_references = count_query(
        &database,
        &read,
        "MATCH ELEMENTS HAS LABEL unresolved_reference",
    )?;
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

/// Opens a project database and keeps query expansion on one read snapshot.
pub(crate) struct OxGraphStore {
    database: Database,
}

impl OxGraphStore {
    pub(crate) fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = canonical_root(root.as_ref())?;
        Ok(Self {
            database: Database::open(database_dir(&root))?,
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
        };
        f(&session)
    }
}

pub(crate) struct ReadSession<'database> {
    database: &'database Database,
    read: oxgraph::db::ReadTransaction,
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
        let keys = ElementPropertyKeys::load(&self.read)?;
        resolve_selector_in_read(&self.read, &keys, selector)
    }

    pub(crate) fn resolve_one_symbol(&self, selector: &str) -> Result<SymbolSummary> {
        let keys = ElementPropertyKeys::load(&self.read)?;
        resolve_one_symbol_in_read(&self.read, &keys, selector)
    }

    pub(crate) fn call_graph(
        &self,
        selector: &str,
        direction: GraphDirection,
        depth: usize,
        limit: usize,
    ) -> Result<CallGraphReport> {
        let element_keys = ElementPropertyKeys::load(&self.read)?;
        let relation_keys = RelationPropertyKeys::load(&self.read)?;
        let seed = resolve_one_symbol_in_read(&self.read, &element_keys, selector)?;
        let seed_id = ElementId::new(seed.id);
        let mut symbols = vec![TraversedSymbol {
            depth: 0,
            symbol: seed.clone(),
        }];
        let mut edges = Vec::new();

        let Ok(graph) = self.read.graph_projection_by_name(schema::CALLS_PROJECTION) else {
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
        let Some(seed_local) = graph.local_element_id(seed_id) else {
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

        let mut discovered = BTreeMap::from([(seed_id, 0_usize)]);
        let mut queued = VecDeque::from([(seed_local, 0_usize)]);
        let mut emitted_relations = BTreeSet::new();

        while let Some((current, current_depth)) = queued.pop_front() {
            if current_depth >= depth {
                continue;
            }
            if matches!(direction, GraphDirection::Outgoing | GraphDirection::Both) {
                visit_call_edges(
                    &self.read,
                    &element_keys,
                    &relation_keys,
                    &graph,
                    current,
                    current_depth,
                    limit,
                    EdgeVisitDirection::Outgoing,
                    &mut discovered,
                    &mut queued,
                    &mut symbols,
                    &mut emitted_relations,
                    &mut edges,
                )?;
            }
            if matches!(direction, GraphDirection::Incoming | GraphDirection::Both) {
                visit_call_edges(
                    &self.read,
                    &element_keys,
                    &relation_keys,
                    &graph,
                    current,
                    current_depth,
                    limit,
                    EdgeVisitDirection::Incoming,
                    &mut discovered,
                    &mut queued,
                    &mut symbols,
                    &mut emitted_relations,
                    &mut edges,
                )?;
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
        let element_keys = ElementPropertyKeys::load(&self.read)?;
        let relation_keys = RelationPropertyKeys::load(&self.read)?;
        let graph = self
            .read
            .graph_projection_by_name(schema::CALLS_PROJECTION)
            .ok();
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
                            &element_keys,
                            &relation_keys,
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
            stable_key: require_property_key(read, "stable_key")?,
            name: require_property_key(read, "name")?,
            qualified_name: require_property_key(read, "qualified_name")?,
            kind: require_property_key(read, "kind")?,
            language: require_property_key(read, "language")?,
            file_path: require_property_key(read, "file_path")?,
            start_byte: require_property_key(read, "start_byte")?,
            end_byte: require_property_key(read, "end_byte")?,
            start_line: require_property_key(read, "start_line")?,
            start_column: require_property_key(read, "start_column")?,
            end_line: require_property_key(read, "end_line")?,
            end_column: require_property_key(read, "end_column")?,
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
            site_file_path: require_property_key(read, "site_file_path")?,
            site_start_line: require_property_key(read, "site_start_line")?,
            site_start_column: require_property_key(read, "site_start_column")?,
            site_end_line: require_property_key(read, "site_end_line")?,
            site_end_column: require_property_key(read, "site_end_column")?,
            site_start_byte: require_property_key(read, "site_start_byte")?,
            site_end_byte: require_property_key(read, "site_end_byte")?,
            site_text: require_property_key(read, "site_text")?,
        })
    }
}

/// Direction used for one edge-expansion pass.
#[derive(Clone, Copy)]
enum EdgeVisitDirection {
    /// Visit outgoing projection edges.
    Outgoing,
    /// Visit incoming projection edges.
    Incoming,
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
fn resolve_selector_in_read(
    read: &oxgraph::db::ReadTransaction,
    keys: &ElementPropertyKeys,
    selector: &str,
) -> Result<Vec<SymbolSummary>> {
    let selector = selector.trim();
    if let Some(raw) = selector.strip_prefix("element:") {
        let Ok(id) = raw.parse::<u64>() else {
            return Ok(Vec::new());
        };
        return symbol_summary_from_element(read, keys, ElementId::new(id)).map(|summary| {
            summary
                .filter(is_agent_symbol)
                .into_iter()
                .collect::<Vec<_>>()
        });
    }
    if let Some(name) = selector.strip_prefix("name:") {
        return lookup_symbols_by_property(read, keys, keys.name, name);
    }
    if let Some(file_selector) = selector.strip_prefix("file:") {
        return resolve_file_line_selector(read, keys, file_selector);
    }
    lookup_symbols_by_property(read, keys, keys.qualified_name, selector)
}

/// Looks up symbols by one exact text property.
fn lookup_symbols_by_property(
    read: &oxgraph::db::ReadTransaction,
    keys: &ElementPropertyKeys,
    key: PropertyKeyId,
    value: &str,
) -> Result<Vec<SymbolSummary>> {
    let mut symbols = read
        .lookup_property_equal(key, &PropertyValue::Text(value.to_string()))?
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
            .then(left.definition.start_byte.cmp(&right.definition.start_byte))
    });
    Ok(symbols)
}

/// Resolves a `file:path:line` selector to the innermost matching symbol.
fn resolve_file_line_selector(
    read: &oxgraph::db::ReadTransaction,
    keys: &ElementPropertyKeys,
    selector: &str,
) -> Result<Vec<SymbolSummary>> {
    let Some((file_path, line)) = selector.rsplit_once(':') else {
        return Ok(Vec::new());
    };
    let Ok(line) = line.parse::<usize>() else {
        return Ok(Vec::new());
    };
    let mut symbols = lookup_symbols_by_property(read, keys, keys.file_path, file_path)?;
    symbols.retain(|symbol| {
        !matches!(symbol.kind.as_str(), "file" | "unresolved_reference")
            && symbol.definition.start_line <= line
            && line <= symbol.definition.end_line
    });
    let Some(shortest_span) = symbols
        .iter()
        .map(|symbol| {
            symbol
                .definition
                .end_byte
                .saturating_sub(symbol.definition.start_byte)
        })
        .min()
    else {
        return Ok(Vec::new());
    };
    symbols.retain(|symbol| {
        symbol
            .definition
            .end_byte
            .saturating_sub(symbol.definition.start_byte)
            == shortest_span
    });
    Ok(symbols)
}

/// Visits one direction of graph edges from a frontier element.
#[expect(
    clippy::too_many_arguments,
    reason = "keeps traversal state local to call_graph"
)]
fn visit_call_edges(
    read: &oxgraph::db::ReadTransaction,
    element_keys: &ElementPropertyKeys,
    relation_keys: &RelationPropertyKeys,
    graph: &oxgraph::db::GraphProjection,
    current: oxgraph::db::ProjectionElementId,
    current_depth: usize,
    limit: usize,
    direction: EdgeVisitDirection,
    discovered: &mut BTreeMap<ElementId, usize>,
    queued: &mut VecDeque<(oxgraph::db::ProjectionElementId, usize)>,
    symbols: &mut Vec<TraversedSymbol>,
    emitted_relations: &mut BTreeSet<RelationId>,
    edges: &mut Vec<CallEdgeSummary>,
) -> Result<()> {
    let local_edges = match direction {
        EdgeVisitDirection::Outgoing => graph.outgoing_edges(current).collect::<Vec<_>>(),
        EdgeVisitDirection::Incoming => graph.incoming_edges(current).collect::<Vec<_>>(),
    };
    let next_depth = current_depth + 1;
    for local_edge in local_edges {
        let source_local = graph.source(local_edge);
        let target_local = graph.target(local_edge);
        let neighbor_local = match direction {
            EdgeVisitDirection::Outgoing => target_local,
            EdgeVisitDirection::Incoming => source_local,
        };
        let neighbor = graph.canonical_element_id(neighbor_local);
        if let std::collections::btree_map::Entry::Vacant(entry) = discovered.entry(neighbor) {
            if symbols.len().saturating_sub(1) >= limit {
                continue;
            }
            let Some(symbol) = symbol_summary_from_element(read, element_keys, neighbor)? else {
                continue;
            };
            entry.insert(next_depth);
            queued.push_back((neighbor_local, next_depth));
            symbols.push(TraversedSymbol {
                depth: next_depth,
                symbol,
            });
        }

        let relation = graph.canonical_relation_id(local_edge);
        if emitted_relations.insert(relation)
            && let Some(edge) = call_edge_summary(
                read,
                element_keys,
                relation_keys,
                relation,
                graph.canonical_element_id(source_local),
                graph.canonical_element_id(target_local),
                next_depth,
            )?
        {
            edges.push(edge);
        }
    }
    Ok(())
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
                        *id,
                        graph.canonical_element_id(graph.source(local)),
                        graph.canonical_element_id(graph.target(local)),
                        1,
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

/// Builds one call edge summary from canonical endpoint IDs.
fn call_edge_summary(
    read: &oxgraph::db::ReadTransaction,
    element_keys: &ElementPropertyKeys,
    relation_keys: &RelationPropertyKeys,
    relation: RelationId,
    source: ElementId,
    target: ElementId,
    depth: usize,
) -> Result<Option<CallEdgeSummary>> {
    let Some(source) = symbol_summary_from_element(read, element_keys, source)? else {
        return Ok(None);
    };
    let Some(target) = symbol_summary_from_element(read, element_keys, target)? else {
        return Ok(None);
    };
    Ok(Some(CallEdgeSummary {
        relation_id: relation.get(),
        depth,
        source,
        target,
        call_site: call_site_summary(read, relation_keys, relation),
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
    let location = CodeLocation {
        file_path,
        start_byte: optional_usize_property(read, subject, keys.site_start_byte)?,
        end_byte: optional_usize_property(read, subject, keys.site_end_byte)?,
        start_line: optional_usize_property(read, subject, keys.site_start_line)?,
        start_column: optional_usize_property(read, subject, keys.site_start_column)?,
        end_line: optional_usize_property(read, subject, keys.site_end_line)?,
        end_column: optional_usize_property(read, subject, keys.site_end_column)?,
    };
    Some(CallSiteSummary {
        location,
        text: optional_text_property(read, subject, keys.site_text).unwrap_or_default(),
    })
}

/// Reads symbol properties for one element.
fn symbol_summary_from_element(
    read: &oxgraph::db::ReadTransaction,
    keys: &ElementPropertyKeys,
    id: ElementId,
) -> Result<Option<SymbolSummary>> {
    if read.element(id).is_none() {
        return Ok(None);
    }
    let subject = PropertySubject::Element(id);
    let Some(stable_key) = optional_text_property(read, subject, keys.stable_key) else {
        return Ok(None);
    };
    let Some(name) = optional_text_property(read, subject, keys.name) else {
        return Ok(None);
    };
    let Some(qualified_name) = optional_text_property(read, subject, keys.qualified_name) else {
        return Ok(None);
    };
    let Some(kind) = optional_text_property(read, subject, keys.kind) else {
        return Ok(None);
    };
    let Some(file_path) = optional_text_property(read, subject, keys.file_path) else {
        return Ok(None);
    };
    let Some(start_byte) = optional_usize_property(read, subject, keys.start_byte) else {
        return Ok(None);
    };
    let Some(end_byte) = optional_usize_property(read, subject, keys.end_byte) else {
        return Ok(None);
    };
    let Some(start_line) = optional_usize_property(read, subject, keys.start_line) else {
        return Ok(None);
    };
    let Some(start_column) = optional_usize_property(read, subject, keys.start_column) else {
        return Ok(None);
    };
    let Some(end_line) = optional_usize_property(read, subject, keys.end_line) else {
        return Ok(None);
    };
    let Some(end_column) = optional_usize_property(read, subject, keys.end_column) else {
        return Ok(None);
    };
    Ok(Some(SymbolSummary {
        id: id.get(),
        stable_key,
        name,
        qualified_name,
        kind,
        language: optional_text_property(read, subject, keys.language).unwrap_or_default(),
        definition: CodeLocation {
            file_path,
            start_byte,
            end_byte,
            start_line,
            start_column,
            end_line,
            end_column,
        },
    }))
}

/// Returns whether a symbol should participate in agent selectors.
fn is_agent_symbol(symbol: &SymbolSummary) -> bool {
    symbol.kind != "unresolved_reference"
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

/// Rebuilds the native OxGraph database for one resolved index.
pub(crate) fn rebuild_database(root: &Path, index: &ResolvedIndex) -> Result<PathBuf> {
    let index_directory = index_dir(root);
    let database_directory = database_dir(root);
    let temp_directory = index_directory.join("index.oxgdb.tmp");
    let backup_directory = index_directory.join("index.oxgdb.old");
    std::fs::create_dir_all(&index_directory)
        .map_err(|source| Error::fs(&index_directory, source))?;
    for stale in [&temp_directory, &backup_directory] {
        if stale.exists() {
            std::fs::remove_dir_all(stale).map_err(|source| Error::fs(stale, source))?;
        }
    }
    remove_legacy_outputs(&index_directory)?;

    let mut database = Database::create(&temp_directory)?;
    let mut writer = database.begin_write()?;

    let source_role = writer.register_role(schema::SOURCE_ROLE)?;
    let target_role = writer.register_role(schema::TARGET_ROLE)?;
    let labels = register_labels(&mut writer)?;
    let unresolved_label = writer.register_label(schema::UNRESOLVED_LABEL)?;
    let relation_types = register_relation_types(&mut writer)?;
    let element_properties = register_element_properties(&mut writer)?;
    let relation_properties = register_relation_properties(&mut writer)?;
    define_property_indexes(&mut writer, &element_properties)?;
    writer.define_projection(ProjectionDefinition::Graph(GraphProjectionDefinition {
        name: schema::CALLS_PROJECTION.to_owned(),
        relation_types: BTreeSet::from([relation_types[&EdgeKind::Calls]]),
        source_role,
        target_role,
    }))?;

    let files = index
        .files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect::<BTreeMap<_, _>>();
    let mut elements = BTreeMap::new();
    for node in &index.nodes {
        let element = writer.create_element()?;
        writer.add_element_label(element, labels[&node.kind])?;
        set_symbol_properties(
            &mut writer,
            element,
            &element_properties,
            node,
            files.get(node.file_path.as_str()).copied(),
        )?;
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

/// Removes storage artifacts from the pre-OxGraph-native prototype.
fn remove_legacy_outputs(index_directory: &Path) -> Result<()> {
    for file_name in ["index.sqlite", "forward.oxgsnap", "reverse.oxgsnap"] {
        let path = index_directory.join(file_name);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|source| Error::fs(&path, source))?;
        }
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

/// Registers element property keys.
fn register_element_properties(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
) -> Result<BTreeMap<&'static str, PropertyKeyId>> {
    let definitions = [
        ("stable_key", PropertyType::Text),
        ("name", PropertyType::Text),
        ("qualified_name", PropertyType::Text),
        ("kind", PropertyType::Text),
        ("raw_kind", PropertyType::Text),
        ("language", PropertyType::Text),
        ("file_path", PropertyType::Text),
        ("path", PropertyType::Text),
        ("hash", PropertyType::Text),
        ("byte_len", PropertyType::Integer),
        ("start_byte", PropertyType::Integer),
        ("end_byte", PropertyType::Integer),
        ("start_line", PropertyType::Integer),
        ("start_column", PropertyType::Integer),
        ("end_line", PropertyType::Integer),
        ("end_column", PropertyType::Integer),
        ("unresolved_source_key", PropertyType::Text),
        ("target_raw", PropertyType::Text),
        ("target_normalized", PropertyType::Text),
        ("target_qualifier", PropertyType::Text),
        ("target_kind_hint", PropertyType::Text),
        ("unresolved_edge_kind", PropertyType::Text),
        ("reason", PropertyType::Text),
    ];
    let mut properties = BTreeMap::new();
    for (name, value_type) in definitions {
        let key = writer.register_property_key(name, PropertyFamily::Element, value_type)?;
        properties.insert(name, key);
    }
    Ok(properties)
}

/// Registers relation property keys.
fn register_relation_properties(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
) -> Result<BTreeMap<&'static str, PropertyKeyId>> {
    let definitions = [
        ("edge_kind", PropertyType::Text),
        ("source_key", PropertyType::Text),
        ("target_key", PropertyType::Text),
        ("site_file_path", PropertyType::Text),
        ("site_start_line", PropertyType::Integer),
        ("site_start_column", PropertyType::Integer),
        ("site_end_line", PropertyType::Integer),
        ("site_end_column", PropertyType::Integer),
        ("site_start_byte", PropertyType::Integer),
        ("site_end_byte", PropertyType::Integer),
        ("site_text", PropertyType::Text),
    ];
    let mut properties = BTreeMap::new();
    for (name, value_type) in definitions {
        let key = writer.register_property_key(name, PropertyFamily::Relation, value_type)?;
        properties.insert(name, key);
    }
    Ok(properties)
}

/// Defines equality indexes for common element query keys.
fn define_property_indexes(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
    properties: &BTreeMap<&'static str, PropertyKeyId>,
) -> Result<()> {
    for name in schema::ELEMENT_INDEXED_KEYS {
        writer.define_index(
            format!("element_{name}_eq"),
            IndexDefinition::PropertyEquality {
                key: properties[name],
            },
        )?;
    }
    Ok(())
}

/// Writes symbol properties to one element.
fn set_symbol_properties(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
    element: ElementId,
    properties: &BTreeMap<&'static str, PropertyKeyId>,
    node: &SymbolNode,
    file: Option<&SourceUnit>,
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
    set_span_properties(writer, element, properties, node.span)?;

    if node.kind == NodeKind::File
        && let Some(file) = file
    {
        set_text(writer, element, properties, "path", &file.path)?;
        set_text(writer, element, properties, "hash", &file.hash)?;
        set_usize(writer, element, properties, "byte_len", file.byte_len)?;
    }
    Ok(())
}

/// Writes unresolved-reference diagnostic properties.
fn set_unresolved_properties(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
    element: ElementId,
    properties: &BTreeMap<&'static str, PropertyKeyId>,
    unresolved: &UnresolvedReference,
) -> Result<()> {
    let stable_key = format!(
        "unresolved:{}:{}:{}:{}",
        unresolved.source_key,
        unresolved.kind.as_str(),
        unresolved.target.normalized,
        unresolved.span.start_byte
    );
    set_text(writer, element, properties, "stable_key", &stable_key)?;
    set_text(writer, element, properties, "name", &unresolved.target.raw)?;
    set_text(
        writer,
        element,
        properties,
        "qualified_name",
        &unresolved.target.normalized,
    )?;
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
    set_text(
        writer,
        element,
        properties,
        "target_normalized",
        &unresolved.target.normalized,
    )?;
    if let Some(qualifier) = &unresolved.target.qualifier {
        set_text(writer, element, properties, "target_qualifier", qualifier)?;
    }
    if let Some(kind_hint) = &unresolved.target.kind_hint {
        set_text(writer, element, properties, "target_kind_hint", kind_hint)?;
    }
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

/// Sets a text property on a relation.
fn set_relation_text(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
    relation: RelationId,
    properties: &BTreeMap<&'static str, PropertyKeyId>,
    key: &'static str,
    value: &str,
) -> Result<()> {
    writer.set_property(
        PropertySubject::Relation(relation),
        properties[key],
        PropertyValue::Text(value.to_string()),
    )?;
    Ok(())
}

/// Sets a usize property on a relation as an OxGraph integer.
fn set_relation_usize(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
    relation: RelationId,
    properties: &BTreeMap<&'static str, PropertyKeyId>,
    key: &'static str,
    value: usize,
) -> Result<()> {
    let value = i64::try_from(value).map_err(|_| Error::IntegerOverflow { value })?;
    writer.set_property(
        PropertySubject::Relation(relation),
        properties[key],
        PropertyValue::Integer(value),
    )?;
    Ok(())
}

/// Sets a text property on an element.
fn set_text(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
    element: ElementId,
    properties: &BTreeMap<&'static str, PropertyKeyId>,
    key: &'static str,
    value: &str,
) -> Result<()> {
    writer.set_property(
        PropertySubject::Element(element),
        properties[key],
        PropertyValue::Text(value.to_string()),
    )?;
    Ok(())
}

/// Sets a usize property on an element as an OxGraph integer.
fn set_usize(
    writer: &mut oxgraph::db::WriteTransaction<'_>,
    element: ElementId,
    properties: &BTreeMap<&'static str, PropertyKeyId>,
    key: &'static str,
    value: usize,
) -> Result<()> {
    let value = i64::try_from(value).map_err(|_| Error::IntegerOverflow { value })?;
    writer.set_property(
        PropertySubject::Element(element),
        properties[key],
        PropertyValue::Integer(value),
    )?;
    Ok(())
}

/// Counts rows from one OxGraph query.
fn count_query(
    database: &Database,
    read: &oxgraph::db::ReadTransaction,
    query: &str,
) -> Result<usize> {
    let prepared = database.prepare(QueryLanguage::Oxql, query)?;
    Ok(read.execute(&prepared)?.rows().len())
}

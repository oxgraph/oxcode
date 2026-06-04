//! Native OxGraph storage and typed read adapter.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use oxcode_model::{
    CALLS_PROJECTION, CallEdgeSummary, CallGraphReport, CallSiteSummary, CatalogStatus,
    CodeLocation, ContextFileSummary, ContextReport, EdgeKind, ElementProperty,
    ExpandedQueryReport, ExpandedQueryRow, ExpandedQueryValue, FileSearchReport, FileSummary,
    GraphDirection, LanguageId, NodeKind, ProjectStatus, QualifiedName, RelatedSymbol,
    RelationProperty, RelationshipSummary, SOURCE_ROLE, Selector, SourcePath, SourceSpan, SymbolId,
    SymbolKey, SymbolReport, SymbolSearchMatch, SymbolSearchReport, SymbolSummary, TARGET_ROLE,
    TraversedSymbol, projection_name,
};
use oxgraph::db::{
    Db, Direction, ElementId, PropertyKeyId, PropertySubject, PropertyValue, QueryResult,
    QueryValue, RelationId, RelationTypeId, Walk,
};

use crate::{
    GraphWalk,
    error::{Error, Result},
    format::format_query_value,
    paths::{canonical_root, database_dir},
};

mod write;

pub(crate) use write::reconcile_database;
use write::{edge_kind_index_name, element_index_name};

/// Returns project database status.
pub(crate) fn project_status(root: impl AsRef<Path>) -> Result<ProjectStatus> {
    let root = canonical_root(root.as_ref())?;
    let database_path = database_dir(&root);
    // Ask the storage crate whether a valid store exists rather than probing its
    // private on-disk filename.
    if Db::validate_path(&database_path).is_err() {
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

    let database = Db::open(&database_path)?;
    let status = database.stats();
    let read = database.reader();
    let files = count_equal(
        &read,
        &element_index_name(ElementProperty::Kind.key()),
        NodeKind::File.as_str(),
    )?;
    let calls = count_equal(&read, &edge_kind_index_name(), EdgeKind::Calls.as_str())?;
    let unresolved_references = count_equal(
        &read,
        &element_index_name(ElementProperty::Kind.key()),
        NodeKind::Unresolved.as_str(),
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

/// An opened project database with its property-key schema resolved once.
pub(crate) struct OxGraphStore {
    database: Db,
    element_keys: ElementPropertyKeys,
    relation_keys: RelationPropertyKeys,
}

impl OxGraphStore {
    pub(crate) fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = canonical_root(root.as_ref())?;
        let database = Db::open(database_dir(&root))?;
        let read = database.reader();
        let element_keys = ElementPropertyKeys::load(&read)?;
        let relation_keys = RelationPropertyKeys::load(&read)?;
        drop(read);
        Ok(Self {
            database,
            element_keys,
            relation_keys,
        })
    }

    pub(crate) fn query(&self, query: &str) -> Result<QueryResult> {
        let prepared = self.database.prepare(query)?;
        Ok(self.database.reader().run(&prepared)?)
    }

    pub(crate) fn explain(&self, query: &str) -> Result<String> {
        let prepared = self.database.prepare(query)?;
        Ok(self.database.reader().explain(&prepared))
    }

    pub(crate) fn with_read<T>(&self, f: impl FnOnce(&ReadSession<'_>) -> Result<T>) -> Result<T> {
        let session = ReadSession {
            database: &self.database,
            read: self.database.reader(),
            element_keys: &self.element_keys,
            relation_keys: &self.relation_keys,
        };
        f(&session)
    }
}

/// One read snapshot over an opened store, sharing the store's resolved schema.
pub(crate) struct ReadSession<'store> {
    database: &'store Db,
    read: oxgraph::db::Reader,
    element_keys: &'store ElementPropertyKeys,
    relation_keys: &'store RelationPropertyKeys,
}

impl ReadSession<'_> {
    pub(crate) fn execute_query(&self, query: &str) -> Result<QueryResult> {
        let prepared = self.database.prepare(query)?;
        Ok(self.read.run(&prepared)?)
    }

    pub(crate) fn resolve_selector(&self, selector: &str) -> Result<Vec<SymbolSummary>> {
        resolve_selector_in_read(&self.read, self.element_keys, selector)
    }

    /// Searches indexed symbols with an agent-friendly ranking.
    pub(crate) fn search_symbols_filtered(
        &self,
        query: &str,
        limit: usize,
        kinds: &[NodeKind],
    ) -> Result<SymbolSearchReport> {
        let terms = search_terms(query);
        let normalized_query = query.trim().to_ascii_lowercase();
        let kind_filter = kinds.iter().copied().collect::<BTreeSet<_>>();
        let mut matches = self
            .all_symbol_summaries()?
            .into_iter()
            .filter(is_agent_symbol)
            .filter(|symbol| kind_filter.is_empty() || kind_filter.contains(&symbol.kind))
            .filter_map(|symbol| {
                if terms.is_empty() {
                    Some(SymbolSearchMatch { score: 0, symbol })
                } else {
                    symbol_search_score(&symbol, &terms, &normalized_query)
                        .map(|score| SymbolSearchMatch { score, symbol })
                }
            })
            .collect::<Vec<_>>();

        matches.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then(left.symbol.qualified_name.cmp(&right.symbol.qualified_name))
                .then(
                    left.symbol
                        .definition
                        .file_path
                        .cmp(&right.symbol.definition.file_path),
                )
                .then(
                    left.symbol
                        .definition
                        .span
                        .start_byte
                        .cmp(&right.symbol.definition.span.start_byte),
                )
        });
        matches.truncate(limit);
        Ok(SymbolSearchReport {
            query: query.to_string(),
            limit,
            matches,
        })
    }

    /// Searches indexed files with a lightweight structured summary.
    pub(crate) fn search_files(&self, query: &str, limit: usize) -> Result<FileSearchReport> {
        let terms = search_terms(query);
        let mut by_file = BTreeMap::<String, Vec<SymbolSummary>>::new();
        for symbol in self
            .all_symbol_summaries()?
            .into_iter()
            .filter(is_agent_symbol)
        {
            by_file
                .entry(symbol.definition.file_path.as_str().to_string())
                .or_default()
                .push(symbol);
        }

        let mut files = by_file
            .into_iter()
            .filter_map(|(path, mut symbols)| {
                symbols.sort_by(|left, right| {
                    left.definition
                        .span
                        .start_byte
                        .cmp(&right.definition.span.start_byte)
                        .then(left.qualified_name.cmp(&right.qualified_name))
                });
                let score = file_search_score(&path, &symbols, &terms)?;
                let top_symbols = symbols
                    .iter()
                    .filter(|symbol| symbol.kind != NodeKind::File)
                    .take(8)
                    .cloned()
                    .collect::<Vec<_>>();
                let symbol_count = symbols
                    .iter()
                    .filter(|symbol| symbol.kind != NodeKind::File)
                    .count();
                Some(FileSummary {
                    path: SourcePath::from(path),
                    score,
                    symbol_count,
                    top_symbols,
                })
            })
            .collect::<Vec<_>>();

        files.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then(left.path.cmp(&right.path))
        });
        files.truncate(limit);
        Ok(FileSearchReport {
            query: query.to_string(),
            limit,
            files,
        })
    }

    /// Builds deterministic context for a task or question.
    pub(crate) fn context(&self, query: &str, limit: usize, depth: usize) -> Result<ContextReport> {
        let entry_report = self.search_symbols_filtered(
            query,
            limit.saturating_mul(3).max(limit),
            &preferred_context_kinds(),
        )?;
        let entry_points = entry_report
            .matches
            .into_iter()
            .filter(|entry| entry.symbol.kind != NodeKind::File)
            .take(limit)
            .collect::<Vec<_>>();

        let mut related = BTreeMap::<u64, RelatedSymbol>::new();
        let mut relationships = BTreeMap::<u64, RelationshipSummary>::new();
        let entry_ids = entry_points
            .iter()
            .map(|entry| entry.symbol.id.get())
            .collect::<BTreeSet<_>>();

        for entry in &entry_points {
            collect_context_relationships(
                &self.read,
                self.element_keys,
                self.relation_keys,
                entry.symbol.id.get(),
                depth,
                &entry_ids,
                &mut related,
                &mut relationships,
            )?;
        }

        let related_symbols = related.into_values().collect::<Vec<_>>();
        let relationships = relationships.into_values().collect::<Vec<_>>();
        let files = context_files(&entry_points, &related_symbols);
        let summary = format!(
            "found {} entry points, {} related symbols, and {} relationships for {:?}",
            entry_points.len(),
            related_symbols.len(),
            relationships.len(),
            query
        );
        Ok(ContextReport {
            query: query.to_string(),
            summary,
            entry_points,
            related_symbols,
            relationships,
            files,
        })
    }

    /// Reads every symbol-like element from one read snapshot.
    fn all_symbol_summaries(&self) -> Result<Vec<SymbolSummary>> {
        let result = self.execute_query("MATCH ELEMENTS")?;
        result
            .rows()
            .iter()
            .flat_map(|row| &row.values)
            .filter_map(|value| match value {
                QueryValue::Element(id) => Some(*id),
                QueryValue::Relation(_)
                | QueryValue::Incidence(_)
                | QueryValue::Subject(_)
                | QueryValue::Property(_)
                | QueryValue::Text(_)
                | QueryValue::Projection(_) => None,
            })
            .map(|id| symbol_summary_from_element(&self.read, self.element_keys, id))
            .filter_map(|result| match result {
                Ok(Some(symbol)) => Some(Ok(symbol)),
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            })
            .collect()
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
    pub(crate) fn query_expanded(&self, query: &str) -> Result<ExpandedQueryReport> {
        let result = self.execute_query(query)?;
        self.expand_query_result(&result)
    }

    pub(crate) fn call_graph(
        &self,
        selector: &str,
        direction: GraphDirection,
        depth: usize,
        limit: usize,
    ) -> Result<CallGraphReport> {
        self.traverse(
            selector,
            CALLS_PROJECTION,
            GraphWalk {
                direction,
                depth,
                limit,
            },
        )
    }

    /// Traverses the projection for `edge_kind` from a seed — the same engine as
    /// [`ReadSession::call_graph`] generalized to any code edge kind.
    pub(crate) fn navigate(
        &self,
        selector: &str,
        edge_kind: EdgeKind,
        walk: GraphWalk,
    ) -> Result<CallGraphReport> {
        self.traverse(selector, &projection_name(edge_kind), walk)
    }

    /// Traverses one named graph projection from a seed, hydrating the reachable
    /// symbols and the edges among them into a report.
    pub(crate) fn traverse(
        &self,
        selector: &str,
        projection: &str,
        walk: GraphWalk,
    ) -> Result<CallGraphReport> {
        let GraphWalk {
            direction,
            depth,
            limit,
        } = walk;
        let element_keys = self.element_keys;
        let relation_keys = self.relation_keys;
        let seed = resolve_one_symbol_in_read(&self.read, element_keys, selector)?;
        let seed_id = ElementId::new(seed.id.get());
        let mut symbols = vec![TraversedSymbol {
            depth: 0,
            symbol: seed.clone(),
        }];
        let mut edges = Vec::new();

        // Discover the node set + shortest depths AND the traversed edges with the
        // engine's deterministic BFS (clean stop at the node limit), then hydrate.
        // A seed absent from the projection yields an empty report (the walk errs
        // with `UnknownElement`, matching the previous participation guard).
        let walk = Walk {
            max_depth: depth,
            direction: walk_direction(direction),
            limit,
            include_start: false,
        };
        let subgraph = self
            .read
            .catalog()
            .projection_id(projection)
            .and_then(|id| self.read.walk(id, &[seed_id], walk).ok());
        let Some(subgraph) = subgraph else {
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
        let mut depth_of = BTreeMap::from([(seed_id, 0_usize)]);
        for node in subgraph.nodes() {
            if let Some(symbol) =
                symbol_summary_from_element(&self.read, element_keys, node.element)?
            {
                depth_of.insert(node.element, node.depth);
                symbols.push(TraversedSymbol {
                    depth: node.depth,
                    symbol,
                });
            }
        }

        // Hydrate the engine-traversed edges, whose endpoints are both in the
        // discovered set, so the report never references a symbol it omitted.
        for edge in subgraph.edges() {
            if !depth_of.contains_key(&edge.target) {
                continue;
            }
            let edge_depth = depth_of
                .get(&edge.source)
                .copied()
                .unwrap_or(0)
                .max(depth_of.get(&edge.target).copied().unwrap_or(0));
            if let Some(summary) = call_edge_summary(
                &self.read,
                element_keys,
                relation_keys,
                CallEdgeRef {
                    relation: edge.relation,
                    source: edge.source,
                    target: edge.target,
                    depth: Some(edge_depth),
                },
            )? {
                edges.push(summary);
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
        // A relation query value is hydrated into a call edge only when it is a
        // `Calls` relation (the previous calls-projection membership filter).
        let calls_type = self
            .read
            .catalog()
            .relation_type_id(EdgeKind::Calls.as_str());
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
                            calls_type,
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
    /// Optional signature property.
    signature: Option<PropertyKeyId>,
    /// Optional docstring property.
    docstring: Option<PropertyKeyId>,
    /// Optional source preview property.
    source_preview: Option<PropertyKeyId>,
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
    fn load(read: &oxgraph::db::Reader) -> Result<Self> {
        Ok(Self {
            stable_key: require_element_key(read, ElementProperty::StableKey)?,
            name: require_element_key(read, ElementProperty::Name)?,
            qualified_name: require_element_key(read, ElementProperty::QualifiedName)?,
            kind: require_element_key(read, ElementProperty::Kind)?,
            language: require_element_key(read, ElementProperty::Language)?,
            file_path: require_element_key(read, ElementProperty::FilePath)?,
            signature: optional_element_key(read, ElementProperty::Signature),
            docstring: optional_element_key(read, ElementProperty::Docstring),
            source_preview: optional_element_key(read, ElementProperty::SourcePreview),
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
///
/// Endpoint resolution no longer reads the stored `source_key`/`target_key` text
/// props — [`Reader::endpoints`](oxgraph::db::Reader::endpoints) reads endpoints
/// straight from incidence storage — so only the call-site keys are loaded here.
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
    fn load(read: &oxgraph::db::Reader) -> Result<Self> {
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

/// Maps an agent graph direction to the engine's walk [`Direction`].
const fn walk_direction(direction: GraphDirection) -> Direction {
    match direction {
        GraphDirection::Outgoing => Direction::Outgoing,
        GraphDirection::Incoming => Direction::Incoming,
        GraphDirection::Both => Direction::Both,
    }
}

/// Resolves exactly one symbol or returns a selector error.
fn resolve_one_symbol_in_read(
    read: &oxgraph::db::Reader,
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
    read: &oxgraph::db::Reader,
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
    read: &oxgraph::db::Reader,
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
    let subjects = read.lookup(index_id, oxgraph::db::Match::Equal(&value_property))?;
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

/// Splits a user search query into normalized terms.
fn search_terms(query: &str) -> Vec<String> {
    let mut terms = BTreeSet::new();
    let mut current = String::new();
    let mut previous_lowercase = false;
    for character in query.chars() {
        if character.is_ascii_alphanumeric() {
            if character.is_ascii_uppercase() && previous_lowercase && !current.is_empty() {
                terms.insert(current.to_ascii_lowercase());
                current.clear();
            }
            previous_lowercase = character.is_ascii_lowercase() || character.is_ascii_digit();
            current.push(character);
        } else {
            if !current.is_empty() {
                terms.insert(current.to_ascii_lowercase());
                current.clear();
            }
            previous_lowercase = false;
        }
    }
    if !current.is_empty() {
        terms.insert(current.to_ascii_lowercase());
    }
    terms.into_iter().filter(|term| term.len() > 1).collect()
}

/// Scores one symbol against normalized search terms.
fn symbol_search_score(
    symbol: &SymbolSummary,
    terms: &[String],
    normalized_query: &str,
) -> Option<u32> {
    let name = symbol.name.to_ascii_lowercase();
    let qualified_name = symbol.qualified_name.as_str().to_ascii_lowercase();
    let kind = symbol.kind.as_str();
    let file_path = symbol.definition.file_path.as_str().to_ascii_lowercase();
    let signature = symbol
        .signature
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let docstring = symbol
        .docstring
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let searchable_tokens = symbol_search_tokens(symbol);
    let mut score = 0_u32;

    if qualified_name == normalized_query {
        score += 2000;
    }
    if name == normalized_query {
        score += 1800;
    }
    if terms
        .iter()
        .all(|term| token_or_substring_match(&searchable_tokens, &qualified_name, term))
    {
        score += 900;
    }
    if terms
        .iter()
        .all(|term| token_or_substring_match(&searchable_tokens, &name, term))
    {
        score += 800;
    }
    for term in terms {
        if token_or_substring_match(&searchable_tokens, &name, term) {
            score += 160;
        }
        if token_or_substring_match(&searchable_tokens, &qualified_name, term) {
            score += 130;
        }
        if kind.contains(term) {
            score += 45;
        }
        if signature.contains(term) {
            score += 75;
        }
        if docstring.contains(term) {
            score += 55;
        }
        if file_path.contains(term) {
            score += 20;
        }
    }
    score = score.saturating_add(kind_rank_bonus(symbol.kind));
    score = score.saturating_add(path_rank_bonus(&file_path, terms));
    if is_test_like_path(&file_path) && !wants_test_like(terms) {
        score /= 3;
    }
    if matches!(
        symbol.kind,
        NodeKind::File | NodeKind::Module | NodeKind::ImplBlock
    ) {
        score /= 2;
    }

    (score > 0).then_some(score)
}

/// Tokenizes indexed symbol fields for search scoring.
fn symbol_search_tokens(symbol: &SymbolSummary) -> BTreeSet<String> {
    [
        symbol.name.as_str(),
        symbol.qualified_name.as_str(),
        symbol.kind.as_str(),
        symbol.definition.file_path.as_str(),
        symbol.signature.as_deref().unwrap_or_default(),
        symbol.docstring.as_deref().unwrap_or_default(),
    ]
    .into_iter()
    .flat_map(search_terms)
    .collect()
}

/// Returns whether one term matches a token or full field.
fn token_or_substring_match(tokens: &BTreeSet<String>, field: &str, term: &str) -> bool {
    tokens.contains(term) || field.contains(term)
}

/// Returns a deterministic kind ranking adjustment.
const fn kind_rank_bonus(kind: NodeKind) -> u32 {
    match kind {
        NodeKind::Function | NodeKind::Method => 220,
        NodeKind::Trait | NodeKind::Struct | NodeKind::Enum => 160,
        NodeKind::TypeAlias | NodeKind::Constant | NodeKind::Macro => 80,
        NodeKind::Module => 15,
        NodeKind::ImplBlock => 5,
        NodeKind::File => 0,
        _ => 30,
    }
}

/// Returns a deterministic path ranking adjustment.
fn path_rank_bonus(file_path: &str, terms: &[String]) -> u32 {
    let is_test_like = is_test_like_path(file_path);
    if is_test_like && !wants_test_like(terms) {
        return 0;
    }
    if file_path.starts_with("src/") || file_path.contains("/src/") {
        120
    } else if is_test_like {
        30
    } else {
        60
    }
}

/// Returns whether query terms ask for test-like paths.
fn wants_test_like(terms: &[String]) -> bool {
    terms.iter().any(|term| {
        matches!(
            term.as_str(),
            "test"
                | "tests"
                | "bench"
                | "benches"
                | "example"
                | "examples"
                | "fixture"
                | "fixtures"
        )
    })
}

/// Returns whether a path belongs to a test-like tree.
fn is_test_like_path(file_path: &str) -> bool {
    file_path.contains("/tests/")
        || file_path.starts_with("tests/")
        || file_path.contains("/benches/")
        || file_path.starts_with("benches/")
        || file_path.contains("/examples/")
        || file_path.starts_with("examples/")
        || file_path.contains("/fixtures/")
        || file_path.starts_with("fixtures/")
}

/// Scores one indexed file for file discovery.
fn file_search_score(path: &str, symbols: &[SymbolSummary], terms: &[String]) -> Option<u32> {
    if terms.is_empty() {
        return Some(path_rank_bonus(&path.to_ascii_lowercase(), terms));
    }

    let path_lower = path.to_ascii_lowercase();
    let path_tokens = search_terms(path);
    let symbol_tokens = symbols
        .iter()
        .flat_map(symbol_search_tokens)
        .collect::<BTreeSet<_>>();
    let mut score = 0_u32;
    for term in terms {
        if path_tokens.iter().any(|token| token == term) {
            score += 160;
        }
        if path_lower.contains(term) {
            score += 100;
        }
        if symbol_tokens.contains(term) {
            score += 80;
        }
    }
    score = score.saturating_add(path_rank_bonus(&path_lower, terms));
    (score > 0).then_some(score)
}

/// Default symbol kinds used as task context entry points.
fn preferred_context_kinds() -> Vec<NodeKind> {
    [
        NodeKind::Function,
        NodeKind::Method,
        NodeKind::Trait,
        NodeKind::Struct,
        NodeKind::Enum,
        NodeKind::TypeAlias,
        NodeKind::Constant,
        NodeKind::Macro,
    ]
    .into_iter()
    .collect()
}

/// Builds related context by walking direct graph relationships from one seed.
#[expect(
    clippy::too_many_arguments,
    reason = "keeps context graph traversal state explicit"
)]
fn collect_context_relationships(
    read: &oxgraph::db::Reader,
    element_keys: &ElementPropertyKeys,
    relation_keys: &RelationPropertyKeys,
    seed_id: u64,
    max_depth: usize,
    entry_ids: &BTreeSet<u64>,
    related: &mut BTreeMap<u64, RelatedSymbol>,
    relationships: &mut BTreeMap<u64, RelationshipSummary>,
) -> Result<()> {
    const MAX_RELATED_SYMBOLS: usize = 80;
    const MAX_RELATIONSHIPS: usize = 200;

    let mut queue = std::collections::VecDeque::from([(ElementId::new(seed_id), 0_usize)]);
    let mut visited = BTreeSet::from([seed_id]);
    while let Some((current, current_depth)) = queue.pop_front() {
        if current_depth >= max_depth || relationships.len() >= MAX_RELATIONSHIPS {
            continue;
        }
        for kind in [
            EdgeKind::Calls,
            EdgeKind::Contains,
            EdgeKind::References,
            EdgeKind::Implements,
        ] {
            collect_context_edges(
                read,
                element_keys,
                relation_keys,
                kind,
                current,
                current_depth,
                EdgeVisitDirection::Outgoing,
                entry_ids,
                &mut visited,
                &mut queue,
                related,
                relationships,
                MAX_RELATED_SYMBOLS,
                MAX_RELATIONSHIPS,
            )?;
            if matches!(kind, EdgeKind::Contains | EdgeKind::References) {
                continue;
            }
            collect_context_edges(
                read,
                element_keys,
                relation_keys,
                kind,
                current,
                current_depth,
                EdgeVisitDirection::Incoming,
                entry_ids,
                &mut visited,
                &mut queue,
                related,
                relationships,
                MAX_RELATED_SYMBOLS,
                MAX_RELATIONSHIPS,
            )?;
        }
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps context graph traversal state explicit"
)]
fn collect_context_edges(
    read: &oxgraph::db::Reader,
    element_keys: &ElementPropertyKeys,
    relation_keys: &RelationPropertyKeys,
    kind: EdgeKind,
    current: ElementId,
    current_depth: usize,
    direction: EdgeVisitDirection,
    entry_ids: &BTreeSet<u64>,
    visited: &mut BTreeSet<u64>,
    queue: &mut std::collections::VecDeque<(ElementId, usize)>,
    related: &mut BTreeMap<u64, RelatedSymbol>,
    relationships: &mut BTreeMap<u64, RelationshipSummary>,
    max_related: usize,
    max_relationships: usize,
) -> Result<()> {
    const MAX_CONTEXT_EDGES_PER_NODE: usize = 64;

    let next_depth = current_depth + 1;
    for edge in direct_relation_edges(read, kind, current, direction, MAX_CONTEXT_EDGES_PER_NODE) {
        if relationships.len() >= max_relationships {
            break;
        }
        if let Some(summary) = relationship_summary_from_ids(
            read,
            element_keys,
            relation_keys,
            kind,
            edge.relation,
            edge.source,
            edge.target,
        )? {
            relationships.entry(summary.relation_id).or_insert(summary);
        }

        let neighbor_id = edge.neighbor.get();
        if entry_ids.contains(&neighbor_id) || related.len() >= max_related {
            continue;
        }
        if let Some(symbol) = symbol_summary_from_element(read, element_keys, edge.neighbor)?
            && is_context_related_symbol(&symbol)
        {
            related.entry(neighbor_id).or_insert_with(|| RelatedSymbol {
                depth: next_depth,
                reason: format!(
                    "{} {} relationship",
                    match direction {
                        EdgeVisitDirection::Outgoing => "outgoing",
                        EdgeVisitDirection::Incoming => "incoming",
                    },
                    kind
                ),
                symbol,
            });
        }
        if visited.insert(neighbor_id) {
            queue.push_back((edge.neighbor, next_depth));
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum EdgeVisitDirection {
    Outgoing,
    Incoming,
}

#[derive(Clone, Copy)]
struct DirectRelationEdge {
    relation: RelationId,
    source: ElementId,
    target: ElementId,
    neighbor: ElementId,
}

fn direct_relation_edges(
    read: &oxgraph::db::Reader,
    kind: EdgeKind,
    current: ElementId,
    direction: EdgeVisitDirection,
    limit: usize,
) -> Vec<DirectRelationEdge> {
    let Some(relation_type) = read.catalog().relation_type_id(kind.as_str()) else {
        return Vec::new();
    };
    let Some(source_role) = read.catalog().role_id(SOURCE_ROLE) else {
        return Vec::new();
    };
    let Some(target_role) = read.catalog().role_id(TARGET_ROLE) else {
        return Vec::new();
    };

    let mut edges = Vec::new();
    for incidence in read.element_incidences(current) {
        if edges.len() >= limit {
            break;
        }
        let current_is_source = incidence.role == source_role;
        let current_is_target = incidence.role == target_role;
        if matches!(direction, EdgeVisitDirection::Outgoing) && !current_is_source {
            continue;
        }
        if matches!(direction, EdgeVisitDirection::Incoming) && !current_is_target {
            continue;
        }

        let Some(relation) = read.relation(incidence.relation) else {
            continue;
        };
        if relation.relation_type != Some(relation_type) {
            continue;
        }
        // Endpoints come straight from incidence storage; the source endpoint is
        // the lower incidence id, the target the higher.
        let Some((source, target)) = relation_endpoints(read, relation.id) else {
            continue;
        };
        let neighbor = match direction {
            EdgeVisitDirection::Outgoing => target,
            EdgeVisitDirection::Incoming => source,
        };
        edges.push(DirectRelationEdge {
            relation: relation.id,
            source,
            target,
            neighbor,
        });
    }
    edges
}

/// Returns a relation's `(source, target)` endpoint elements straight from the
/// engine's incidence storage, with no `source_key`/`target_key` text round-trip.
fn relation_endpoints(
    read: &oxgraph::db::Reader,
    relation: RelationId,
) -> Option<(ElementId, ElementId)> {
    read.endpoints(relation)
}

/// Builds one generic relationship summary from canonical endpoint IDs.
#[expect(
    clippy::too_many_arguments,
    reason = "keeps relationship endpoint hydration explicit"
)]
fn relationship_summary_from_ids(
    read: &oxgraph::db::Reader,
    element_keys: &ElementPropertyKeys,
    relation_keys: &RelationPropertyKeys,
    kind: EdgeKind,
    relation: RelationId,
    source: ElementId,
    target: ElementId,
) -> Result<Option<RelationshipSummary>> {
    let Some(source) = symbol_summary_from_element(read, element_keys, source)? else {
        return Ok(None);
    };
    let Some(target) = symbol_summary_from_element(read, element_keys, target)? else {
        return Ok(None);
    };
    Ok(Some(RelationshipSummary {
        relation_id: relation.get(),
        kind,
        source,
        target,
        site: call_site_summary(read, relation_keys, relation),
    }))
}

/// Returns whether one adjacent symbol is useful in context output.
fn is_context_related_symbol(symbol: &SymbolSummary) -> bool {
    !matches!(symbol.kind, NodeKind::File | NodeKind::Unresolved)
}

/// Aggregates file counts for context output.
fn context_files(
    entry_points: &[SymbolSearchMatch],
    related_symbols: &[RelatedSymbol],
) -> Vec<ContextFileSummary> {
    let mut files = BTreeMap::<SourcePath, (usize, usize)>::new();
    for entry in entry_points {
        files
            .entry(entry.symbol.definition.file_path.clone())
            .or_default()
            .0 += 1;
    }
    for related in related_symbols {
        files
            .entry(related.symbol.definition.file_path.clone())
            .or_default()
            .1 += 1;
    }
    files
        .into_iter()
        .map(
            |(path, (matched_symbols, related_symbols))| ContextFileSummary {
                path,
                matched_symbols,
                related_symbols,
            },
        )
        .collect()
}

/// Resolves a `file:path:line` selector to the innermost matching symbol.
fn resolve_file_line_selector(
    read: &oxgraph::db::Reader,
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
    read: &oxgraph::db::Reader,
    element_keys: &ElementPropertyKeys,
    relation_keys: &RelationPropertyKeys,
    calls_type: Option<RelationTypeId>,
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
            // Hydrate a call edge only for `Calls` relations, reading endpoints
            // straight from incidence storage.
            let is_calls = read
                .relation(*id)
                .is_some_and(|relation| relation.relation_type == calls_type)
                && calls_type.is_some();
            if is_calls && let Some((source, target)) = read.endpoints(*id) {
                expanded.call_edge = call_edge_summary(
                    read,
                    element_keys,
                    relation_keys,
                    CallEdgeRef {
                        relation: *id,
                        source,
                        target,
                        depth: None,
                    },
                )?;
            }
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
    read: &oxgraph::db::Reader,
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
    read: &oxgraph::db::Reader,
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
    read: &oxgraph::db::Reader,
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
        signature: keys
            .signature
            .and_then(|key| optional_text_property(read, subject, key)),
        docstring: keys
            .docstring
            .and_then(|key| optional_text_property(read, subject, key)),
        source_preview: keys
            .source_preview
            .and_then(|key| optional_text_property(read, subject, key)),
    }))
}

/// Reads a required text property, erroring if a present element lacks it.
fn require_text(
    read: &oxgraph::db::Reader,
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
    read: &oxgraph::db::Reader,
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
fn require_property_key(read: &oxgraph::db::Reader, name: &'static str) -> Result<PropertyKeyId> {
    read.catalog()
        .property_key_id(name)
        .ok_or_else(|| Error::MissingCatalog {
            item: "property",
            name: name.to_string(),
        })
}

/// Requires a catalog element property key.
fn require_element_key(
    read: &oxgraph::db::Reader,
    property: ElementProperty,
) -> Result<PropertyKeyId> {
    require_property_key(read, property.key())
}

/// Reads an optional catalog element property key.
fn optional_element_key(
    read: &oxgraph::db::Reader,
    property: ElementProperty,
) -> Option<PropertyKeyId> {
    read.catalog().property_key_id(property.key())
}

/// Requires a catalog relation property key.
fn require_relation_key(
    read: &oxgraph::db::Reader,
    property: RelationProperty,
) -> Result<PropertyKeyId> {
    require_property_key(read, property.key())
}

/// Reads one optional text property.
///
/// The native read returns an owned `Option<PropertyValue>`; matching by
/// reference avoids cloning when the value is not the expected variant.
fn optional_text_property(
    read: &oxgraph::db::Reader,
    subject: PropertySubject,
    key: PropertyKeyId,
) -> Option<String> {
    match read.property(subject, key).as_ref() {
        Some(PropertyValue::Text(value)) => Some(value.clone()),
        Some(PropertyValue::Boolean(_) | PropertyValue::Integer(_)) | None => None,
    }
}

/// Reads one optional unsigned integer property.
///
/// The native read returns an owned `Option<PropertyValue>`; matching by
/// reference avoids cloning when the value is not the expected variant.
fn optional_usize_property(
    read: &oxgraph::db::Reader,
    subject: PropertySubject,
    key: PropertyKeyId,
) -> Option<usize> {
    match read.property(subject, key).as_ref() {
        Some(PropertyValue::Integer(value)) => usize::try_from(*value).ok(),
        Some(PropertyValue::Boolean(_) | PropertyValue::Text(_)) | None => None,
    }
}

/// Counts the subjects whose indexed value equals `value` via the named equality
/// index, returning `0` when the index is absent.
///
/// Probes the equality index directly rather than materializing query rows.
fn count_equal(read: &oxgraph::db::Reader, name: &str, value: &str) -> Result<usize> {
    match read.catalog().index_id(name) {
        Some(index_id) => {
            let value = PropertyValue::Text(value.to_owned());
            Ok(read
                .lookup(index_id, oxgraph::db::Match::Equal(&value))?
                .len())
        }
        None => Ok(0),
    }
}

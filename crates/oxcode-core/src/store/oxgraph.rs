//! Native OxGraph storage and typed read adapter.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::{Path, PathBuf},
};

use oxcode_model::{
    CallEdgeSummary, CallGraphReport, CallSiteSummary, CatalogStatus, CodeLocation,
    ContextFileSummary, ContextReport, EdgeKind, ExpandedQueryReport, ExpandedQueryRow,
    ExpandedQueryValue, FileSearchReport, FileSummary, GraphDirection, NodeKind, ProjectStatus,
    RelatedSymbol, RelationshipSummary, ResolvedEdge, ResolvedIndex, SourceSpan, SourceUnit,
    SymbolNode, SymbolSearchMatch, SymbolSearchReport, SymbolSummary, TraversedSymbol,
    UnresolvedReference,
};
use oxgraph::db::{
    Database, ElementId, GraphProjectionDefinition, IndexDefinition, LabelId, ProjectionDefinition,
    PropertyFamily, PropertyKeyId, PropertySubject, PropertyType, PropertyValue, QueryLanguage,
    QueryResult, QueryValue, RelationId, RelationTypeId,
};
use oxgraph::graph::{EdgeSourceGraph, EdgeTargetGraph};
use oxgraph::topology::{CanonicalElementIdentity, LocalRelationIdentity};

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

    /// Searches indexed symbols with an agent-friendly ranking.
    pub(crate) fn search_symbols(&self, query: &str, limit: usize) -> Result<SymbolSearchReport> {
        self.search_symbols_filtered(query, limit, &[])
    }

    /// Searches indexed symbols with optional kind filters.
    pub(crate) fn search_symbols_filtered(
        &self,
        query: &str,
        limit: usize,
        kinds: &[String],
    ) -> Result<SymbolSearchReport> {
        let keys = ElementPropertyKeys::load(&self.read)?;
        let terms = search_terms(query);
        let normalized_query = query.trim().to_ascii_lowercase();
        let kind_filter = kinds
            .iter()
            .map(|kind| kind.to_ascii_lowercase())
            .collect::<BTreeSet<_>>();
        let mut matches = self
            .all_symbol_summaries(&keys)?
            .into_iter()
            .filter(is_agent_symbol)
            .filter(|symbol| kind_filter.is_empty() || kind_filter.contains(&symbol.kind))
            .filter_map(|symbol| {
                if terms.is_empty() {
                    Some(Ok(SymbolSearchMatch { score: 0, symbol }))
                } else {
                    symbol_search_score(&symbol, &terms, &normalized_query)
                        .map(|score| Ok(SymbolSearchMatch { score, symbol }))
                }
            })
            .collect::<Result<Vec<_>>>()?;

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
                        .start_byte
                        .cmp(&right.symbol.definition.start_byte),
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
        let keys = ElementPropertyKeys::load(&self.read)?;
        let terms = search_terms(query);
        let mut by_file = BTreeMap::<String, Vec<SymbolSummary>>::new();
        for symbol in self
            .all_symbol_summaries(&keys)?
            .into_iter()
            .filter(is_agent_symbol)
        {
            by_file
                .entry(symbol.definition.file_path.clone())
                .or_default()
                .push(symbol);
        }

        let mut files = by_file
            .into_iter()
            .filter_map(|(path, mut symbols)| {
                symbols.sort_by(|left, right| {
                    left.definition
                        .start_byte
                        .cmp(&right.definition.start_byte)
                        .then(left.qualified_name.cmp(&right.qualified_name))
                });
                let score = file_search_score(&path, &symbols, &terms)?;
                let top_symbols = symbols
                    .iter()
                    .filter(|symbol| symbol.kind != "file")
                    .take(8)
                    .cloned()
                    .collect::<Vec<_>>();
                let symbol_count = symbols
                    .iter()
                    .filter(|symbol| symbol.kind != "file")
                    .count();
                Some(FileSummary {
                    path,
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
            .filter(|entry| entry.symbol.kind != "file")
            .take(limit)
            .collect::<Vec<_>>();

        let element_keys = ElementPropertyKeys::load(&self.read)?;
        let relation_keys = RelationPropertyKeys::load(&self.read)?;
        let mut related = BTreeMap::<u64, RelatedSymbol>::new();
        let mut relationships = BTreeMap::<u64, RelationshipSummary>::new();
        let entry_ids = entry_points
            .iter()
            .map(|entry| entry.symbol.id)
            .collect::<BTreeSet<_>>();

        for entry in &entry_points {
            collect_context_relationships(
                &self.read,
                &element_keys,
                &relation_keys,
                entry.symbol.id,
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
    fn all_symbol_summaries(&self, keys: &ElementPropertyKeys) -> Result<Vec<SymbolSummary>> {
        let result = self.execute_query(QueryLanguage::Oxql, "MATCH ELEMENTS")?;
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
            .map(|id| symbol_summary_from_element(&self.read, keys, id))
            .filter_map(|result| match result {
                Ok(Some(symbol)) => Some(Ok(symbol)),
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            })
            .collect()
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

        let mut discovered = BTreeMap::from([(seed_id, 0_usize)]);
        let mut queued = VecDeque::from([(seed_id, 0_usize)]);
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
    /// Optional signature property.
    signature: Option<PropertyKeyId>,
    /// Optional docstring property.
    docstring: Option<PropertyKeyId>,
    /// Optional source preview property.
    source_preview: Option<PropertyKeyId>,
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
            signature: optional_property_key(read, "signature"),
            docstring: optional_property_key(read, "docstring"),
            source_preview: optional_property_key(read, "source_preview"),
        })
    }
}

/// Property keys needed for relation expansion.
struct RelationPropertyKeys {
    /// Source symbol stable key property.
    source_key: PropertyKeyId,
    /// Target symbol stable key property.
    target_key: PropertyKeyId,
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
            source_key: require_property_key(read, "source_key")?,
            target_key: require_property_key(read, "target_key")?,
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
    let qualified_name = symbol.qualified_name.to_ascii_lowercase();
    let kind = symbol.kind.to_ascii_lowercase();
    let file_path = symbol.definition.file_path.to_ascii_lowercase();
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
    score = score.saturating_add(kind_rank_bonus(&kind));
    score = score.saturating_add(path_rank_bonus(&file_path, terms));
    if is_test_like_path(&file_path) && !wants_test_like(terms) {
        score /= 3;
    }
    if matches!(kind.as_str(), "file" | "module" | "impl_block") {
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
fn kind_rank_bonus(kind: &str) -> u32 {
    match kind {
        "function" | "method" => 220,
        "trait" | "struct" | "enum" => 160,
        "type_alias" | "constant" | "macro" => 80,
        "module" => 15,
        "impl_block" => 5,
        "file" => 0,
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
fn preferred_context_kinds() -> Vec<String> {
    [
        "function",
        "method",
        "trait",
        "struct",
        "enum",
        "type_alias",
        "constant",
        "macro",
    ]
    .into_iter()
    .map(ToOwned::to_owned)
    .collect()
}

/// Builds related context by walking direct graph relationships from one seed.
#[expect(
    clippy::too_many_arguments,
    reason = "keeps context graph traversal state explicit"
)]
fn collect_context_relationships(
    read: &oxgraph::db::ReadTransaction,
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

    let mut queue = VecDeque::from([(ElementId::new(seed_id), 0_usize)]);
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
    read: &oxgraph::db::ReadTransaction,
    element_keys: &ElementPropertyKeys,
    relation_keys: &RelationPropertyKeys,
    kind: EdgeKind,
    current: ElementId,
    current_depth: usize,
    direction: EdgeVisitDirection,
    entry_ids: &BTreeSet<u64>,
    visited: &mut BTreeSet<u64>,
    queue: &mut VecDeque<(ElementId, usize)>,
    related: &mut BTreeMap<u64, RelatedSymbol>,
    relationships: &mut BTreeMap<u64, RelationshipSummary>,
    max_related: usize,
    max_relationships: usize,
) -> Result<()> {
    const MAX_CONTEXT_EDGES_PER_NODE: usize = 64;

    let next_depth = current_depth + 1;
    for edge in direct_relation_edges(
        read,
        element_keys,
        relation_keys,
        kind,
        current,
        direction,
        MAX_CONTEXT_EDGES_PER_NODE,
    ) {
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
                    kind.as_str()
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
struct DirectRelationEdge {
    relation: RelationId,
    source: ElementId,
    target: ElementId,
    neighbor: ElementId,
}

fn direct_relation_edges(
    read: &oxgraph::db::ReadTransaction,
    element_keys: &ElementPropertyKeys,
    relation_keys: &RelationPropertyKeys,
    kind: EdgeKind,
    current: ElementId,
    direction: EdgeVisitDirection,
    limit: usize,
) -> Vec<DirectRelationEdge> {
    let Some(relation_type) = read.catalog().relation_type_id(kind.as_str()) else {
        return Vec::new();
    };
    let Some(source_role) = read.catalog().role_id(schema::SOURCE_ROLE) else {
        return Vec::new();
    };
    let Some(target_role) = read.catalog().role_id(schema::TARGET_ROLE) else {
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
        let Some((source, target)) =
            relation_endpoints(read, element_keys, relation_keys, relation.id)
        else {
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

fn relation_endpoints(
    read: &oxgraph::db::ReadTransaction,
    element_keys: &ElementPropertyKeys,
    relation_keys: &RelationPropertyKeys,
    relation: RelationId,
) -> Option<(ElementId, ElementId)> {
    let subject = PropertySubject::Relation(relation);
    let source_key = optional_text_property(read, subject, relation_keys.source_key)?;
    let target_key = optional_text_property(read, subject, relation_keys.target_key)?;
    let source = element_by_stable_key(read, element_keys, &source_key)?;
    let target = element_by_stable_key(read, element_keys, &target_key)?;
    Some((source, target))
}

fn element_by_stable_key(
    read: &oxgraph::db::ReadTransaction,
    keys: &ElementPropertyKeys,
    stable_key: &str,
) -> Option<ElementId> {
    read.lookup_property_equal(
        keys.stable_key,
        &PropertyValue::Text(stable_key.to_owned()),
    )
    .ok()?
    .into_iter()
    .find_map(|subject| match subject {
        PropertySubject::Element(id) => Some(id),
        PropertySubject::Relation(_) | PropertySubject::Incidence(_) => None,
    })
}

/// Builds one generic relationship summary from canonical endpoint IDs.
fn relationship_summary_from_ids(
    read: &oxgraph::db::ReadTransaction,
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
        kind: kind.as_str().to_string(),
        source,
        target,
        site: call_site_summary(read, relation_keys, relation),
    }))
}

/// Returns whether one adjacent symbol is useful in context output.
fn is_context_related_symbol(symbol: &SymbolSummary) -> bool {
    !matches!(symbol.kind.as_str(), "file" | "unresolved_reference")
}

/// Aggregates file counts for context output.
fn context_files(
    entry_points: &[SymbolSearchMatch],
    related_symbols: &[RelatedSymbol],
) -> Vec<ContextFileSummary> {
    let mut files = BTreeMap::<String, (usize, usize)>::new();
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
    current: ElementId,
    current_depth: usize,
    limit: usize,
    direction: EdgeVisitDirection,
    discovered: &mut BTreeMap<ElementId, usize>,
    queued: &mut VecDeque<(ElementId, usize)>,
    symbols: &mut Vec<TraversedSymbol>,
    emitted_relations: &mut BTreeSet<RelationId>,
    edges: &mut Vec<CallEdgeSummary>,
) -> Result<()> {
    const MAX_CALL_EDGES_PER_NODE: usize = 128;

    let next_depth = current_depth + 1;
    for edge in direct_relation_edges(
        read,
        element_keys,
        relation_keys,
        EdgeKind::Calls,
        current,
        direction,
        MAX_CALL_EDGES_PER_NODE,
    ) {
        let neighbor = edge.neighbor;
        if let std::collections::btree_map::Entry::Vacant(entry) = discovered.entry(neighbor) {
            if symbols.len().saturating_sub(1) >= limit {
                continue;
            }
            let Some(symbol) = symbol_summary_from_element(read, element_keys, neighbor)? else {
                continue;
            };
            entry.insert(next_depth);
            queued.push_back((neighbor, next_depth));
            symbols.push(TraversedSymbol {
                depth: next_depth,
                symbol,
            });
        }

        if emitted_relations.insert(edge.relation)
            && let Some(edge) = call_edge_summary(
                read,
                element_keys,
                relation_keys,
                edge.relation,
                edge.source,
                edge.target,
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

/// Reads one optional property key by catalog name.
fn optional_property_key(
    read: &oxgraph::db::ReadTransaction,
    name: &'static str,
) -> Option<PropertyKeyId> {
    read.catalog().property_key_id(name)
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
    for kind in EdgeKind::ALL {
        writer.define_projection(ProjectionDefinition::Graph(GraphProjectionDefinition {
            name: kind.as_str().to_owned(),
            relation_types: BTreeSet::from([relation_types[&kind]]),
            source_role,
            target_role,
        }))?;
    }

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
        ("signature", PropertyType::Text),
        ("docstring", PropertyType::Text),
        ("source_preview", PropertyType::Text),
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

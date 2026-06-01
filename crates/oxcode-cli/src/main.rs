//! Command-line interface for `oxcode`.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use oxcode_core::{
    Error, GraphDirection, OxQueryResult, ProjectIndex, SymbolReport, SymbolSummary,
    explain_project, format_call_graph_report, format_context_report, format_expanded_query_report,
    format_file_search_report, format_query_value, format_selector_matches,
    format_selector_not_found, format_symbol_report, format_symbol_search_report, index_project,
    language_support, parse_graph_direction, parse_node_kind, parse_query_language, project_status,
    query_project,
};

/// Generate and query code graphs stored in a native OxGraph database.
#[derive(Debug, Parser)]
#[command(author, version, about)]
struct Cli {
    /// Command to run.
    #[command(subcommand)]
    command: Command,
}

/// CLI commands.
#[derive(Debug, Subcommand)]
enum Command {
    /// Rebuild the project index.
    Index {
        /// Project root.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Print machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show native OxGraph database status.
    Status {
        /// Project root.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Print machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show languages with explicit extractors.
    Languages {
        /// Print machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Search indexed symbols by agent-friendly keywords.
    Symbols {
        /// Optional keyword query.
        query: Option<String>,
        /// Project root.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Maximum candidate count.
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Restrict results to one symbol kind. Repeat for multiple kinds.
        #[arg(long = "kind")]
        kinds: Vec<String>,
        /// Print machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Build task-oriented context from indexed symbols and relationships.
    Context {
        /// Task or question text.
        query: String,
        /// Project root.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Maximum entry point count.
        #[arg(long, default_value_t = 8)]
        limit: usize,
        /// Relationship hop depth.
        #[arg(long, default_value_t = 1)]
        depth: usize,
        /// Print machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Search indexed files by agent-friendly keywords.
    Files {
        /// Optional keyword query.
        query: Option<String>,
        /// Project root.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Maximum file count.
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Print machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Execute a raw OxGraph database query.
    #[command(
        long_about = "Execute a raw OxGraph database query.\n\nAccepted OxQL profile:\n  CATALOG\n  MATCH ELEMENTS\n  MATCH ELEMENTS HAS LABEL <label>\n  MATCH ELEMENTS WHERE <property> = '<value>'\n  MATCH RELATIONS TYPE <type>\n  GRAPH calls WALK FROM <element-id> DEPTH <n> [DIRECTION outgoing|incoming|both] [LIMIT n]\n\nUse `oxcode symbols <keywords>` for keyword discovery."
    )]
    Query {
        /// Query text.
        query: String,
        /// Project root.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Query language: oxql or cypher.
        #[arg(long, default_value = "oxql")]
        language: String,
        /// Print a compact table instead of JSON.
        #[arg(long)]
        table: bool,
        /// Print machine-readable JSON. This is the default for raw queries.
        #[arg(long)]
        json: bool,
        /// Expand OxGraph IDs into agent-readable code context.
        #[arg(long)]
        expand: bool,
    },
    /// Describe one symbol selector.
    Symbol {
        /// Selector: element:<id>, qualified name, name:<name>, or file:<path>:<line>.
        selector: String,
        /// Project root.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Print machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show functions called by one symbol.
    Calls {
        /// Selector: element:<id>, qualified name, name:<name>, or file:<path>:<line>.
        selector: String,
        /// Project root.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Maximum hop depth.
        #[arg(long, default_value_t = 1)]
        depth: usize,
        /// Maximum discovered symbol count.
        #[arg(long, default_value_t = 100)]
        limit: usize,
        /// Print machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show functions that call one symbol.
    Callers {
        /// Selector: element:<id>, qualified name, name:<name>, or file:<path>:<line>.
        selector: String,
        /// Project root.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Maximum hop depth.
        #[arg(long, default_value_t = 1)]
        depth: usize,
        /// Maximum discovered symbol count.
        #[arg(long, default_value_t = 100)]
        limit: usize,
        /// Print machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Walk the calls graph from one symbol.
    Walk {
        /// Selector: element:<id>, qualified name, name:<name>, or file:<path>:<line>.
        selector: String,
        /// Project root.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Traversal direction: outgoing, incoming, or both.
        #[arg(long, default_value = "outgoing")]
        direction: String,
        /// Maximum hop depth.
        #[arg(long, default_value_t = 1)]
        depth: usize,
        /// Maximum discovered symbol count.
        #[arg(long, default_value_t = 100)]
        limit: usize,
        /// Print machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Explain a raw OxGraph database query plan.
    #[command(
        long_about = "Explain a raw OxGraph database query plan.\n\nAccepted OxQL profile:\n  CATALOG\n  MATCH ELEMENTS\n  MATCH ELEMENTS HAS LABEL <label>\n  MATCH ELEMENTS WHERE <property> = '<value>'\n  MATCH RELATIONS TYPE <type>\n  GRAPH calls WALK FROM <element-id> DEPTH <n> [DIRECTION outgoing|incoming|both] [LIMIT n]\n\nUse `oxcode symbols <keywords>` for keyword discovery."
    )]
    Explain {
        /// Query text.
        query: String,
        /// Project root.
        #[arg(long, default_value = ".")]
        path: PathBuf,
        /// Query language: oxql or cypher.
        #[arg(long, default_value = "oxql")]
        language: String,
    },
}

/// CLI entry point.
fn main() -> Result<()> {
    if let Err(error) = run() {
        if let Some(Error::AmbiguousSelector { selector, matches }) = error.downcast_ref::<Error>()
        {
            eprint!("{}", format_selector_matches(selector, matches));
            std::process::exit(2);
        }
        return Err(error);
    }
    Ok(())
}

/// Runs the parsed CLI command.
fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Index { path, json } => {
            let stats = index_project(path)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&stats)?);
            } else {
                println!(
                    "indexed {} files, {} symbols, {} edges, {} unresolved references",
                    stats.files, stats.symbols, stats.edges, stats.unresolved_references
                );
                println!("database {}", stats.database.display());
                if stats.skipped_unsupported_files > 0 {
                    println!(
                        "skipped {} files without explicit extractors",
                        stats.skipped_unsupported_files
                    );
                }
            }
        }
        Command::Status { path, json } => {
            let status = project_status(path)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                println!("root {}", status.root.display());
                println!("database {}", exists_text(status.database_exists));
                println!("database path {}", status.database.display());
                println!(
                    "elements {} relations {} incidences {}",
                    status.elements, status.relations, status.incidences
                );
                println!(
                    "files {} calls {} unresolved {}",
                    status.files, status.calls, status.unresolved_references
                );
                println!(
                    "catalog roles={} labels={} relation_types={} properties={} projections={} indexes={}",
                    status.catalog.role_count,
                    status.catalog.label_count,
                    status.catalog.relation_type_count,
                    status.catalog.property_key_count,
                    status.catalog.projection_count,
                    status.catalog.index_count
                );
            }
        }
        Command::Languages { json } => {
            let languages = language_support();
            if json {
                println!("{}", serde_json::to_string_pretty(&languages)?);
            } else {
                for language in languages {
                    println!(
                        "{} parser={} extractor={}",
                        language.language, language.parser_available, language.extractor_available
                    );
                }
            }
        }
        Command::Symbols {
            query,
            path,
            limit,
            kinds,
            json,
        } => {
            let query = query.unwrap_or_default();
            validate_kinds(&kinds)?;
            let report =
                ProjectIndex::open(path)?.search_symbols_filtered(&query, limit, &kinds)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", format_symbol_search_report(&report));
            }
        }
        Command::Context {
            query,
            path,
            limit,
            depth,
            json,
        } => {
            let report = ProjectIndex::open(path)?.context(&query, limit, depth)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", format_context_report(&report));
            }
        }
        Command::Files {
            query,
            path,
            limit,
            json,
        } => {
            let query = query.unwrap_or_default();
            let report = ProjectIndex::open(path)?.search_files(&query, limit)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", format_file_search_report(&report));
            }
        }
        Command::Query {
            query,
            path,
            language,
            table,
            json: _json,
            expand,
        } => {
            ensure_raw_query(&language, &query)?;
            let language = parse_query_language(&language).map_err(anyhow::Error::msg)?;
            if expand {
                let report = ProjectIndex::open(path)?.query_expanded(language, &query)?;
                println!("{}", format_expanded_query_report(&report));
            } else if table {
                let rows = query_project(path, language, &query)
                    .with_context(|| format!("executing query {query:?}"))?;
                print_query_table(&rows);
            } else {
                let rows = query_project(path, language, &query)
                    .with_context(|| format!("executing query {query:?}"))?;
                println!("{}", serde_json::to_string_pretty(&rows)?);
            }
        }
        Command::Symbol {
            selector,
            path,
            json,
        } => {
            print_symbol_resolution(path, &selector, json)?;
        }
        Command::Calls {
            selector,
            path,
            depth,
            limit,
            json,
        } => {
            print_call_graph_resolution(
                path,
                &selector,
                GraphDirection::Outgoing,
                depth,
                limit,
                json,
            )?;
        }
        Command::Callers {
            selector,
            path,
            depth,
            limit,
            json,
        } => {
            print_call_graph_resolution(
                path,
                &selector,
                GraphDirection::Incoming,
                depth,
                limit,
                json,
            )?;
        }
        Command::Walk {
            selector,
            path,
            direction,
            depth,
            limit,
            json,
        } => {
            let direction = parse_graph_direction(&direction).map_err(anyhow::Error::msg)?;
            print_call_graph_resolution(path, &selector, direction, depth, limit, json)?;
        }
        Command::Explain {
            query,
            path,
            language,
        } => {
            ensure_raw_query(&language, &query)?;
            let language = parse_query_language(&language).map_err(anyhow::Error::msg)?;
            println!("{}", explain_project(path, language, &query)?);
        }
    }
    Ok(())
}

/// Prints one selector-aware symbol report.
fn print_symbol_resolution(path: PathBuf, selector: &str, json: bool) -> Result<()> {
    let index = ProjectIndex::open(path)?;
    match index.resolve_selector(selector)?.as_slice() {
        [symbol] => {
            let report = SymbolReport {
                selector: selector.to_string(),
                symbol: symbol.clone(),
            };
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "status": "matched",
                        "selector": selector,
                        "report": report,
                    }))?
                );
            } else {
                print!("{}", format_symbol_report(&report));
            }
        }
        [] => print_selector_not_found(selector, json)?,
        matches => print_selector_ambiguous(selector, matches, json)?,
    }
    Ok(())
}

/// Prints one selector-aware call graph report.
fn print_call_graph_resolution(
    path: PathBuf,
    selector: &str,
    direction: GraphDirection,
    depth: usize,
    limit: usize,
    json: bool,
) -> Result<()> {
    let index = ProjectIndex::open(path)?;
    match index.resolve_selector(selector)?.as_slice() {
        [symbol] => {
            let mut report =
                index.call_graph(&format!("element:{}", symbol.id), direction, depth, limit)?;
            report.selector = selector.to_string();
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "status": "matched",
                        "selector": selector,
                        "report": report,
                    }))?
                );
            } else {
                print!("{}", format_call_graph_report(&report));
            }
        }
        [] => print_selector_not_found(selector, json)?,
        matches => print_selector_ambiguous(selector, matches, json)?,
    }
    Ok(())
}

/// Prints one ambiguous selector response.
fn print_selector_ambiguous(selector: &str, matches: &[SymbolSummary], json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "status": "ambiguous",
                "selector": selector,
                "matches": matches,
            }))?
        );
    } else {
        print!("{}", format_selector_matches(selector, matches));
    }
    Ok(())
}

/// Prints one not-found selector response.
fn print_selector_not_found(selector: &str, json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "status": "not_found",
                "selector": selector,
                "matches": [],
            }))?
        );
    } else {
        print!("{}", format_selector_not_found(selector));
    }
    Ok(())
}

/// Rejects obvious keyword search text passed to raw query commands.
fn ensure_raw_query(language: &str, query: &str) -> Result<()> {
    let first = query.split_whitespace().next().unwrap_or_default();
    let first = first.to_ascii_uppercase();
    let language = language.to_ascii_lowercase();
    let looks_structured = match language.as_str() {
        "oxql" => matches!(first.as_str(), "CATALOG" | "MATCH" | "GRAPH"),
        "cypher" => first == "MATCH",
        _ => true,
    };
    if !looks_structured {
        bail!("query expects OxQL/Cypher; use oxcode symbols for keyword discovery");
    }
    Ok(())
}

/// Validates symbol kind filters before opening the index.
fn validate_kinds(kinds: &[String]) -> Result<()> {
    for kind in kinds {
        parse_node_kind(kind).map_err(anyhow::Error::msg)?;
    }
    Ok(())
}

/// Prints one compact query table.
fn print_query_table(result: &OxQueryResult) {
    for row in result.rows() {
        let values = row
            .values
            .iter()
            .map(format_query_value)
            .collect::<Vec<_>>();
        println!("{}", values.join("\t"));
    }
}

/// Returns a compact exists/missing label.
fn exists_text(value: bool) -> &'static str {
    if value { "exists" } else { "missing" }
}

//! Command-line interface for `oxcode`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use oxcode::{
    Error, GraphDirection, call_graph, describe_symbol, expand_query_result, explain_project,
    format_call_graph_report, format_expanded_query_report, format_query_value,
    format_selector_matches, format_symbol_report, index_project, language_support,
    parse_graph_direction, parse_query_language, project_status, query_project,
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
    /// Execute an OxGraph database query.
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
    /// Explain an OxGraph database query plan.
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
        Command::Query {
            query,
            path,
            language,
            table,
            expand,
        } => {
            let language = parse_query_language(&language).map_err(anyhow::Error::msg)?;
            let rows = query_project(path.clone(), language, &query)
                .with_context(|| format!("executing query {query:?}"))?;
            if expand {
                let report = expand_query_result(path, rows)?;
                println!("{}", format_expanded_query_report(&report));
            } else if table {
                print_query_table(&rows);
            } else {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            }
        }
        Command::Symbol {
            selector,
            path,
            json,
        } => {
            let report = describe_symbol(path, &selector)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", format_symbol_report(&report));
            }
        }
        Command::Calls {
            selector,
            path,
            depth,
            limit,
            json,
        } => {
            print_call_graph(
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
            print_call_graph(
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
            print_call_graph(path, &selector, direction, depth, limit, json)?;
        }
        Command::Explain {
            query,
            path,
            language,
        } => {
            let language = parse_query_language(&language).map_err(anyhow::Error::msg)?;
            println!("{}", explain_project(path, language, &query)?);
        }
    }
    Ok(())
}

/// Prints one call graph report.
fn print_call_graph(
    path: PathBuf,
    selector: &str,
    direction: GraphDirection,
    depth: usize,
    limit: usize,
    json: bool,
) -> Result<()> {
    let report = call_graph(path, selector, direction, depth, limit)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", format_call_graph_report(&report));
    }
    Ok(())
}

/// Prints one compact query table.
fn print_query_table(result: &oxgraph::db::QueryResult) {
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

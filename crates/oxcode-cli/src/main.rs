//! Command-line interface for `oxcode`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use oxcode_core::{
    Error, GraphDirection, OxQueryLanguage, OxQueryResult, ProjectIndex, format_call_graph_report,
    format_expanded_query_report, format_query_value, format_selector_matches,
    format_symbol_report, index_project, language_support, project_status,
};

/// Generate and query code graphs stored in a native OxGraph database.
#[derive(Debug, Parser)]
#[command(author, version, about)]
struct Cli {
    /// Command to run.
    #[command(subcommand)]
    command: Command,
}

/// Project root, shared by every command via `--path`.
#[derive(Debug, Args)]
struct PathArg {
    /// Project root.
    #[arg(long, default_value = ".")]
    path: PathBuf,
}

/// Arguments shared by commands that take a root and a JSON toggle.
#[derive(Debug, Args)]
struct CommonArgs {
    #[command(flatten)]
    path: PathArg,
    /// Print machine-readable JSON.
    #[arg(long)]
    json: bool,
}

/// Arguments shared by the call-graph walk commands.
#[derive(Debug, Args)]
struct WalkArgs {
    /// Selector: element:<id>, name:<name>, file:<path>:<line>, or a qualified name.
    selector: String,
    /// Maximum hop depth.
    #[arg(long, default_value_t = 1)]
    depth: usize,
    /// Maximum discovered symbol count.
    #[arg(long, default_value_t = 100)]
    limit: usize,
    #[command(flatten)]
    common: CommonArgs,
}

/// Query language accepted by `query`/`explain`.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum QueryLang {
    Oxql,
    Cypher,
}

impl QueryLang {
    const fn into_language(self) -> OxQueryLanguage {
        match self {
            Self::Oxql => OxQueryLanguage::Oxql,
            Self::Cypher => OxQueryLanguage::Cypher,
        }
    }
}

/// Output format for `query`.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    /// Pretty JSON of the raw result rows.
    Json,
    /// Tab-separated compact rows.
    Table,
    /// Expand OxGraph IDs into agent-readable code context.
    Expand,
}

/// Traversal direction for `walk`.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum Direction {
    Outgoing,
    Incoming,
    Both,
}

impl Direction {
    const fn into_graph(self) -> GraphDirection {
        match self {
            Self::Outgoing => GraphDirection::Outgoing,
            Self::Incoming => GraphDirection::Incoming,
            Self::Both => GraphDirection::Both,
        }
    }
}

/// CLI commands.
#[derive(Debug, Subcommand)]
enum Command {
    /// Rebuild the project index.
    Index {
        #[command(flatten)]
        common: CommonArgs,
    },
    /// Show native OxGraph database status.
    Status {
        #[command(flatten)]
        common: CommonArgs,
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
        #[command(flatten)]
        path: PathArg,
        /// Query language.
        #[arg(long, value_enum, default_value_t = QueryLang::Oxql)]
        language: QueryLang,
        /// Output format.
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
    },
    /// Describe one symbol selector.
    Symbol {
        /// Selector: element:<id>, name:<name>, file:<path>:<line>, or a qualified name.
        selector: String,
        #[command(flatten)]
        common: CommonArgs,
    },
    /// Show functions called by one symbol.
    Calls {
        #[command(flatten)]
        args: WalkArgs,
    },
    /// Show functions that call one symbol.
    Callers {
        #[command(flatten)]
        args: WalkArgs,
    },
    /// Walk the calls graph from one symbol.
    Walk {
        #[command(flatten)]
        args: WalkArgs,
        /// Traversal direction.
        #[arg(long, value_enum, default_value_t = Direction::Outgoing)]
        direction: Direction,
    },
    /// Explain an OxGraph database query plan.
    Explain {
        /// Query text.
        query: String,
        #[command(flatten)]
        path: PathArg,
        /// Query language.
        #[arg(long, value_enum, default_value_t = QueryLang::Oxql)]
        language: QueryLang,
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
        Command::Index { common } => {
            let stats = index_project(&common.path.path)?;
            if common.json {
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
                if stats.failed_files > 0 {
                    println!("failed to index {} files", stats.failed_files);
                }
                if stats.partial_files > 0 {
                    println!("{} files parsed with recoverable errors", stats.partial_files);
                }
            }
        }
        Command::Status { common } => {
            let status = project_status(&common.path.path)?;
            if common.json {
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
            format,
        } => {
            let index = ProjectIndex::open(&path.path)?;
            let language = language.into_language();
            match format {
                OutputFormat::Expand => {
                    let report = index.query_expanded(language, &query)?;
                    println!("{}", format_expanded_query_report(&report));
                }
                OutputFormat::Table => {
                    let rows = index
                        .query(language, &query)
                        .with_context(|| format!("executing query {query:?}"))?;
                    print_query_table(&rows);
                }
                OutputFormat::Json => {
                    let rows = index
                        .query(language, &query)
                        .with_context(|| format!("executing query {query:?}"))?;
                    println!("{}", serde_json::to_string_pretty(&rows)?);
                }
            }
        }
        Command::Symbol { selector, common } => {
            let report = ProjectIndex::open(&common.path.path)?.describe_symbol(&selector)?;
            if common.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", format_symbol_report(&report));
            }
        }
        Command::Calls { args } => print_call_graph(&args, GraphDirection::Outgoing)?,
        Command::Callers { args } => print_call_graph(&args, GraphDirection::Incoming)?,
        Command::Walk { args, direction } => print_call_graph(&args, direction.into_graph())?,
        Command::Explain {
            query,
            path,
            language,
        } => {
            let report = ProjectIndex::open(&path.path)?.explain(language.into_language(), &query)?;
            println!("{report}");
        }
    }
    Ok(())
}

/// Prints one call graph report for a walk command.
fn print_call_graph(args: &WalkArgs, direction: GraphDirection) -> Result<()> {
    let report = ProjectIndex::open(&args.common.path.path)?.call_graph(
        &args.selector,
        direction,
        args.depth,
        args.limit,
    )?;
    if args.common.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", format_call_graph_report(&report));
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

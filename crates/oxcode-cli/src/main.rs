//! Command-line interface for `oxcode`.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use oxcode_core::{
    Error, GraphDirection, NodeKind, OxQueryResult, ProjectIndex, SymbolReport, SymbolSummary,
    format_call_graph_report, format_context_report, format_expanded_query_report,
    format_file_search_report, format_query_value, format_selector_matches,
    format_selector_not_found, format_symbol_report, format_symbol_search_report, index_project,
    language_support, project_status,
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
    #[command(
        long_about = "Execute a raw OxGraph database query.\n\nAccepted OxQL profile:\n  CATALOG\n  MATCH ELEMENTS\n  MATCH ELEMENTS HAS LABEL <label>\n  MATCH ELEMENTS WHERE <property> = '<value>'\n  MATCH RELATIONS TYPE <type>\n  GRAPH calls WALK FROM <element-id> DEPTH <n> [DIRECTION outgoing|incoming|both] [LIMIT n]\n\nUse `oxcode symbols <keywords>` for keyword discovery."
    )]
    Query {
        /// Query text.
        query: String,
        #[command(flatten)]
        path: PathArg,
        /// Output format.
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
        /// Print machine-readable JSON. This is accepted for agent ergonomics;
        /// raw query JSON is already the default format.
        #[arg(long)]
        json: bool,
    },
    /// Search indexed symbols by agent-friendly keywords.
    Symbols {
        /// Optional keyword query.
        query: Option<String>,
        #[command(flatten)]
        path: PathArg,
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
        #[command(flatten)]
        path: PathArg,
        /// Maximum entry point count.
        #[arg(long, default_value_t = 8)]
        limit: usize,
        /// Relationship hop depth.
        #[arg(long, default_value_t = 1)]
        depth: usize,
        /// Maximum total source characters to render.
        #[arg(long, default_value_t = 20_000)]
        max_bytes: usize,
        /// Print machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Search indexed files by agent-friendly keywords.
    Files {
        /// Optional keyword query.
        query: Option<String>,
        #[command(flatten)]
        path: PathArg,
        /// Maximum file count.
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Print machine-readable JSON.
        #[arg(long)]
        json: bool,
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
    #[command(
        long_about = "Explain a raw OxGraph database query plan.\n\nAccepted OxQL profile:\n  CATALOG\n  MATCH ELEMENTS\n  MATCH ELEMENTS HAS LABEL <label>\n  MATCH ELEMENTS WHERE <property> = '<value>'\n  MATCH RELATIONS TYPE <type>\n  GRAPH calls WALK FROM <element-id> DEPTH <n> [DIRECTION outgoing|incoming|both] [LIMIT n]\n\nUse `oxcode symbols <keywords>` for keyword discovery."
    )]
    Explain {
        /// Query text.
        query: String,
        #[command(flatten)]
        path: PathArg,
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
                    println!(
                        "{} files parsed with recoverable errors",
                        stats.partial_files
                    );
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
            format,
            json: _json,
        } => {
            ensure_raw_query(&query)?;
            let index = ProjectIndex::open(&path.path)?;
            match format {
                OutputFormat::Expand => {
                    let report = index.query_expanded(&query)?;
                    println!("{}", format_expanded_query_report(&report));
                }
                OutputFormat::Table => {
                    let rows = index
                        .query(&query)
                        .with_context(|| format!("executing query {query:?}"))?;
                    print_query_table(&rows);
                }
                OutputFormat::Json => {
                    let rows = index
                        .query(&query)
                        .with_context(|| format!("executing query {query:?}"))?;
                    println!("{}", serde_json::to_string_pretty(&rows)?);
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
            let kinds = parse_kinds(&kinds)?;
            let report =
                ProjectIndex::open(&path.path)?.search_symbols_filtered(&query, limit, &kinds)?;
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
            max_bytes,
            json,
        } => {
            let report =
                ProjectIndex::open(&path.path)?.context(&query, limit, depth, max_bytes)?;
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
            let report = ProjectIndex::open(&path.path)?.search_files(&query, limit)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", format_file_search_report(&report));
            }
        }
        Command::Symbol { selector, common } => {
            print_symbol_resolution(&common.path.path, &selector, common.json)?;
        }
        Command::Calls { args } => print_call_graph_resolution(&args, GraphDirection::Outgoing)?,
        Command::Callers { args } => print_call_graph_resolution(&args, GraphDirection::Incoming)?,
        Command::Walk { args, direction } => {
            print_call_graph_resolution(&args, direction.into_graph())?
        }
        Command::Explain { query, path } => {
            ensure_raw_query(&query)?;
            let report = ProjectIndex::open(&path.path)?.explain(&query)?;
            println!("{report}");
        }
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

/// Prints one selector-aware symbol report.
fn print_symbol_resolution(path: &std::path::Path, selector: &str, json: bool) -> Result<()> {
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
fn print_call_graph_resolution(args: &WalkArgs, direction: GraphDirection) -> Result<()> {
    let index = ProjectIndex::open(&args.common.path.path)?;
    match index.resolve_selector(&args.selector)?.as_slice() {
        [symbol] => {
            let mut report = index.call_graph(
                &format!("element:{}", symbol.id),
                direction,
                args.depth,
                args.limit,
            )?;
            report.selector.clone_from(&args.selector);
            if args.common.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "status": "matched",
                        "selector": args.selector,
                        "report": report,
                    }))?
                );
            } else {
                print!("{}", format_call_graph_report(&report));
            }
        }
        [] => print_selector_not_found(&args.selector, args.common.json)?,
        matches => print_selector_ambiguous(&args.selector, matches, args.common.json)?,
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
fn ensure_raw_query(query: &str) -> Result<()> {
    let first = query.split_whitespace().next().unwrap_or_default();
    let first = first.to_ascii_uppercase();
    let looks_structured = matches!(first.as_str(), "CATALOG" | "MATCH" | "GRAPH");
    if !looks_structured {
        bail!("query expects OxQL; use oxcode symbols for keyword discovery");
    }
    Ok(())
}

/// Parses symbol kind filters before opening the index.
fn parse_kinds(kinds: &[String]) -> Result<Vec<NodeKind>> {
    let mut parsed = Vec::with_capacity(kinds.len());
    for kind in kinds {
        let parsed_kind = kind.parse::<NodeKind>().map_err(|_| {
            anyhow::anyhow!(
                "invalid symbol kind {kind:?}; valid kinds are {}",
                valid_kind_names().join(", ")
            )
        })?;
        if !NodeKind::ALL.contains(&parsed_kind) {
            bail!(
                "invalid symbol kind {kind:?}; valid kinds are {}",
                valid_kind_names().join(", ")
            );
        }
        parsed.push(parsed_kind);
    }
    Ok(parsed)
}

fn valid_kind_names() -> Vec<&'static str> {
    NodeKind::ALL.iter().map(|kind| kind.as_str()).collect()
}

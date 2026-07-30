//! The `oxcode mcp` server: tools mapped onto `oxcode_core::ProjectIndex`.
//!
//! Exposes oxcode's read-only queries plus a single-writer file watcher
//! (`oxcode_watch`) to coding agents over MCP (stdio). Run it with `oxcode mcp`;
//! configure your agent to launch that command. Across many MCP processes pointed
//! at one folder, a `.oxcode/watch.lock` file lock elects exactly one writer (the
//! process that watches and re-indexes); the rest serve reads.

mod project_root;
mod roots;

use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions, TryLockError},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use notify_debouncer_full::{
    DebounceEventResult, Debouncer, RecommendedCache, new_debouncer,
    notify::{RecommendedWatcher, RecursiveMode},
};
use oxcode_core::{GraphDirection, IndexStats, NodeKind, ProjectIndex};
use project_root::{OptionalProjectRoot, resolve_project_root};
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Content, ServerCapabilities, ServerInfo, TasksCapability},
    schemars,
    service::NotificationContext,
    task_handler,
    task_manager::OperationProcessor,
    tool, tool_handler, tool_router,
    transport::stdio,
};
use roots::RootsCache;
use serde::Deserialize;
use tokio::sync::{
    Mutex,
    mpsc::{UnboundedReceiver, unbounded_channel},
};

/// Default debounce window for the file watcher: collapse an editor's save burst
/// (write + rename of a temp file, etc.) into one re-index. Overridable with
/// `OXCODE_WATCH_DEBOUNCE_MS`.
const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(400);

/// Default failover poll interval: how often a standby retries the writer lock so
/// it can take over when the current writer exits. Overridable with
/// `OXCODE_WATCH_POLL_MS`.
const DEFAULT_POLL: Duration = Duration::from_secs(3);

/// Filename of the advisory single-writer lock, inside the `.oxcode` index dir.
const WATCH_LOCK_FILE: &str = "watch.lock";

/// Directory names whose filesystem events never warrant a re-index: the index
/// store itself (`.oxcode`, the load-bearing entry that prevents a write →
/// event → re-index feedback loop) plus the dirs source discovery already
/// skips. Mirrors `oxcode_core`'s scan skip list.
const WATCH_SKIP_DIRS: &[&str] = &[".oxcode", ".git", "target", "node_modules", "vendor"];

/// Runs the MCP server over stdio until the client disconnects. The index is not
/// touched until a client calls `oxcode_watch` (writer) or queries (reader).
pub(crate) fn serve() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let service = OxcodeServer::new().serve(stdio()).await?;
        service.waiting().await?;
        Ok(())
    })
}

/// Server instructions steering agents to `oxcode_watch` then `oxcode_explore`.
const INSTRUCTIONS: &str = "This server answers questions about the code repository in the current \
project. First call `oxcode_watch` (optional `path`): it builds the index if needed and keeps it \
current as files change. When `path` is omitted, the project root is taken from OXCODE_ROOT, \
the client's MCP roots, CLAUDE_PROJECT_DIR, or WORKSPACE_FOLDER_PATHS — not from this process's \
cwd, which MCP hosts often set to $HOME. Pass `path` explicitly when in doubt. Only one MCP \
instance watches a given folder at a time — a file lock elects a single writer; other instances \
serve reads and take over automatically if the writer exits. Then, for almost any \
code-understanding question, call `oxcode_explore` first with the user's question verbatim: it \
returns the most relevant symbols (ranked by graph centrality), their source, the relationships \
among them, the n-ary hyperedges they belong to (trait impl groups and container/module membership, \
ranked by hypergraph PageRank — the architecture-altitude layer), the blast radius, and the call \
flow — in one call. Use `oxcode_callers`/`oxcode_callees`/`oxcode_symbol` to follow specific edges, \
and `oxcode_search`/`oxcode_files` only when explore did not surface the target. Prefer these query \
tools over shelling out to grep or reading files. Every tool except `oxcode_watch` is read-only; do \
not edit source files.";

/// MCP server over oxcode's read-only queries plus the `oxcode_watch` file
/// watcher. Caches one opened index per root it writes, elects a single writer
/// per root via a file lock, and drives task-augmented calls through an
/// [`OperationProcessor`].
#[derive(Clone)]
pub(crate) struct OxcodeServer {
    #[expect(
        dead_code,
        reason = "stored per rmcp's #[tool_router] convention; the #[tool_handler]-generated request router reads it through macro-expanded code the dead-code pass does not attribute"
    )]
    tool_router: ToolRouter<OxcodeServer>,
    /// Opened readers cached per root this process writes (evicted on reindex).
    indexes: Arc<Mutex<HashMap<PathBuf, Arc<ProjectIndex>>>>,
    /// Backs the rmcp `#[task_handler]` lifecycle for task-augmented tool calls.
    operations: Arc<Mutex<OperationProcessor>>,
    /// Roots this process is the elected writer for (holds the lock + watcher).
    writers: Arc<std::sync::Mutex<HashMap<PathBuf, Arc<WriterState>>>>,
    /// Roots this process is a standby for (lost the lock; a failover task polls).
    standbys: Arc<std::sync::Mutex<HashSet<PathBuf>>>,
    /// Client MCP workspace roots (fetched outside tool handlers).
    roots: RootsCache,
    /// File-watcher debounce window.
    debounce: Duration,
    /// Failover poll interval for standbys.
    poll: Duration,
}

/// State for a root this process has been elected to write. Dropping it (on
/// process exit) releases the advisory lock and stops the watcher.
struct WriterState {
    /// Held advisory `flock`; the kernel frees it on drop or process crash, so a
    /// standby can take over. The file itself is never removed.
    _lock_file: File,
    /// Live debouncer; dropping it stops the watch thread. The `std::sync::Mutex`
    /// makes `WriterState: Sync` regardless of the platform watcher's `Sync`-ness.
    /// `None` when the watcher failed to start (the lock still elects this writer).
    _watcher: std::sync::Mutex<Option<Debouncer<RecommendedWatcher, RecommendedCache>>>,
    /// Number of reindexes this process has performed for the root (observability).
    reindexes: Arc<AtomicU64>,
}

/// A code question to answer in one curated call.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct ExploreParams {
    /// The task or question about the codebase, in natural language.
    pub query: String,
    #[serde(flatten)]
    pub root: OptionalProjectRoot,
    /// Maximum source characters to render (default 20000).
    pub max_bytes: Option<usize>,
}

/// A keyword search over indexed symbols.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct SearchParams {
    /// Keywords matched against symbol names, signatures, and docs.
    pub query: String,
    #[serde(flatten)]
    pub root: OptionalProjectRoot,
    /// Maximum number of matches (default 30).
    pub limit: Option<usize>,
    /// Restrict to these symbol kinds (e.g. function, method, struct, trait).
    pub kinds: Option<Vec<String>>,
}

/// A call-graph query around one symbol selector.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct CallParams {
    /// Selector: a qualified name, `name:<n>`, `element:<id>`, or `file:<path>:<line>`.
    pub selector: String,
    #[serde(flatten)]
    pub root: OptionalProjectRoot,
    /// Maximum hop depth (default 2).
    pub depth: Option<usize>,
    /// Maximum discovered symbol count (default 50).
    pub limit: Option<usize>,
}

/// One symbol selector to describe.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct SymbolParams {
    /// Selector: a qualified name, `name:<n>`, `element:<id>`, or `file:<path>:<line>`.
    pub selector: String,
    #[serde(flatten)]
    pub root: OptionalProjectRoot,
}

/// A keyword search over indexed files.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct FilesParams {
    /// Keywords matched against file paths and their symbols.
    pub query: String,
    #[serde(flatten)]
    pub root: OptionalProjectRoot,
    /// Maximum number of files (default 30).
    pub limit: Option<usize>,
}

/// A project-root pointer.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct StatusParams {
    #[serde(flatten)]
    pub root: OptionalProjectRoot,
}

/// A project root to watch and keep indexed.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct WatchParams {
    #[serde(flatten)]
    pub root: OptionalProjectRoot,
}

#[tool_router]
impl OxcodeServer {
    /// Builds a server with intervals from the environment (or defaults). Nothing
    /// is indexed or watched until a client calls `oxcode_watch` or queries.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::new_with(
            env_duration("OXCODE_WATCH_DEBOUNCE_MS", DEFAULT_DEBOUNCE),
            env_duration("OXCODE_WATCH_POLL_MS", DEFAULT_POLL),
        )
    }

    /// Builds a server with explicit debounce + failover-poll windows (tests use
    /// tiny values).
    #[must_use]
    fn new_with(debounce: Duration, poll: Duration) -> Self {
        Self {
            tool_router: Self::tool_router(),
            indexes: Arc::new(Mutex::new(HashMap::new())),
            operations: Arc::new(Mutex::new(OperationProcessor::new())),
            writers: Arc::new(std::sync::Mutex::new(HashMap::new())),
            standbys: Arc::new(std::sync::Mutex::new(HashSet::new())),
            roots: RootsCache::new(),
            debounce,
            poll,
        }
    }

    #[tool(
        description = "Start (or join) watching a project so its index is built and kept current as files change. Exactly one MCP instance per folder becomes the writer (it holds a file lock and re-indexes on changes); other instances become readers that just serve queries and automatically take over if the writer exits. Call this once before querying. Optional `path` defaults to the workspace (OXCODE_ROOT / MCP roots / CLAUDE_PROJECT_DIR / WORKSPACE_FOLDER_PATHS); never silently to $HOME.",
        execution(task_support = "optional")
    )]
    async fn oxcode_watch(
        &self,
        Parameters(params): Parameters<WatchParams>,
    ) -> Result<CallToolResult, McpError> {
        let root = self.resolve_root(params.root.path).await?;

        // Idempotent: already participating for this root.
        if self.is_writer(&root) {
            return json_result(&watch_body(&root, "writer", true, None));
        }
        if self.is_standby(&root) {
            return json_result(&watch_body(&root, "standby", false, None));
        }

        // The lock lives inside `.oxcode/`, which `.gitignore`s itself.
        let index_directory = oxcode_core::index_dir(&root);
        ensure_index_dir(&index_directory)
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(index_directory.join(WATCH_LOCK_FILE))
            .map_err(|error| McpError::internal_error(format!("open watch lock: {error}"), None))?;

        match lock_file.try_lock() {
            Ok(()) => {
                let stats = self
                    .promote_to_writer(root.clone(), lock_file)
                    .await
                    .map_err(|error| McpError::internal_error(error.to_string(), None))?;
                eprintln!("oxcode: elected as writer for {}", root.display());
                json_result(&watch_body(&root, "writer", true, Some(&stats)))
            }
            Err(TryLockError::WouldBlock) => {
                self.standbys
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(root.clone());
                tokio::spawn(self.clone().failover_loop(root.clone(), lock_file));
                eprintln!(
                    "oxcode: standby — another instance is watching {}",
                    root.display()
                );
                json_result(&watch_body(&root, "standby", false, None))
            }
            Err(TryLockError::Error(error)) => Err(McpError::internal_error(
                format!("acquire watch lock: {error}"),
                None,
            )),
        }
    }

    #[tool(
        description = "Answer a code question in one call: returns the most relevant symbols ranked by graph centrality, their source, relationships, n-ary hyperedges (trait impl groups and container membership, ranked by hypergraph PageRank for architecture-altitude questions), blast radius, and call flow for the query. Use this first for any code-understanding question.",
        execution(task_support = "optional")
    )]
    async fn oxcode_explore(
        &self,
        Parameters(params): Parameters<ExploreParams>,
    ) -> Result<CallToolResult, McpError> {
        let index = self.index_for(params.root.path).await?;
        let query = params.query;
        let max_bytes = params.max_bytes.unwrap_or(20_000);
        let report = blocking(move || index.context(&query, 8, 1, max_bytes)).await?;
        json_result(&report)
    }

    #[tool(
        description = "Search indexed symbols by keyword, optionally restricted to symbol kinds."
    )]
    async fn oxcode_search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let index = self.index_for(params.root.path).await?;
        let query = params.query;
        let limit = params.limit.unwrap_or(30);
        let kinds = parse_kinds(params.kinds.as_deref());
        let report = blocking(move || index.search_symbols_filtered(&query, limit, &kinds)).await?;
        json_result(&report)
    }

    #[tool(description = "Find the functions that call the given symbol (incoming call graph).")]
    async fn oxcode_callers(
        &self,
        Parameters(params): Parameters<CallParams>,
    ) -> Result<CallToolResult, McpError> {
        self.call_graph(params, GraphDirection::Incoming).await
    }

    #[tool(description = "Find the functions called by the given symbol (outgoing call graph).")]
    async fn oxcode_callees(
        &self,
        Parameters(params): Parameters<CallParams>,
    ) -> Result<CallToolResult, McpError> {
        self.call_graph(params, GraphDirection::Outgoing).await
    }

    #[tool(
        description = "Describe one symbol by selector (qualified name, name:<n>, element:<id>, or file:<path>:<line>)."
    )]
    async fn oxcode_symbol(
        &self,
        Parameters(params): Parameters<SymbolParams>,
    ) -> Result<CallToolResult, McpError> {
        let index = self.index_for(params.root.path).await?;
        let selector = params.selector;
        let value = blocking(move || resolve_symbol(&index, &selector)).await?;
        json_result(&value)
    }

    #[tool(description = "Search indexed files by keyword.")]
    async fn oxcode_files(
        &self,
        Parameters(params): Parameters<FilesParams>,
    ) -> Result<CallToolResult, McpError> {
        let index = self.index_for(params.root.path).await?;
        let query = params.query;
        let limit = params.limit.unwrap_or(30);
        let report = blocking(move || index.search_files(&query, limit)).await?;
        json_result(&report)
    }

    #[tool(
        description = "Show the project's database status (element/relation counts, paths) plus this instance's watch role (writer/standby/reader) and how many times it has re-indexed."
    )]
    async fn oxcode_status(
        &self,
        Parameters(params): Parameters<StatusParams>,
    ) -> Result<CallToolResult, McpError> {
        let root = self.resolve_root(params.root.path).await?;
        let (role, watching, reindexes) = self.watch_state(&root);
        let status_root = root.clone();
        let database = blocking(move || oxcode_core::project_status(&status_root)).await?;
        let body = serde_json::json!({
            "watch": { "role": role, "watching": watching, "reindexes": reindexes },
            "database": database,
        });
        json_result(&body)
    }

    /// Shared call-graph path for callers/callees.
    async fn call_graph(
        &self,
        params: CallParams,
        direction: GraphDirection,
    ) -> Result<CallToolResult, McpError> {
        let index = self.index_for(params.root.path).await?;
        let selector = params.selector;
        let depth = params.depth.unwrap_or(2);
        let limit = params.limit.unwrap_or(50);
        let report = blocking(move || index.call_graph(&selector, direction, depth, limit)).await?;
        json_result(&report)
    }

    /// Opens the index for `path` (default: workspace root). If this process is the
    /// writer for the root, the opened reader is cached and evicted on each reindex;
    /// any other process opens fresh per query so it reflects the writer's latest
    /// commit. A missing index is not built here — call `oxcode_watch` first.
    async fn index_for(&self, path: Option<String>) -> Result<Arc<ProjectIndex>, McpError> {
        let root = self.resolve_root(path).await?;
        if self.is_writer(&root) {
            if let Some(index) = self.indexes.lock().await.get(&root) {
                return Ok(Arc::clone(index));
            }
            let open_root = root.clone();
            let index = Arc::new(blocking(move || ProjectIndex::open(&open_root)).await?);
            self.indexes.lock().await.insert(root, Arc::clone(&index));
            return Ok(index);
        }
        if !oxcode_core::database_dir(&root).exists() {
            return Err(McpError::invalid_params(
                format!(
                    "no index yet for {} — call oxcode_watch to build and keep it current",
                    root.display()
                ),
                None,
            ));
        }
        // Reader: open fresh so the writer's latest committed snapshot is visible.
        let open_root = root.clone();
        Ok(Arc::new(
            blocking(move || ProjectIndex::open(&open_root)).await?,
        ))
    }

    /// Builds/refreshes `root`, starts its watcher, and records this process as the
    /// writer. Caller must already hold the advisory lock (`lock_file`).
    async fn promote_to_writer(
        &self,
        root: PathBuf,
        lock_file: File,
    ) -> anyhow::Result<IndexStats> {
        let write_lock = Arc::new(Mutex::new(()));
        let reindexes = Arc::new(AtomicU64::new(0));
        let stats = run_reindex(&self.indexes, &root, &write_lock, &reindexes).await?;
        let watcher = self.spawn_watch(&root, write_lock, Arc::clone(&reindexes));
        let state = Arc::new(WriterState {
            _lock_file: lock_file,
            _watcher: std::sync::Mutex::new(watcher),
            reindexes,
        });
        self.writers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(root.clone(), state);
        self.standbys
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&root);
        Ok(stats)
    }

    /// Failover: poll the writer lock; when the current writer exits and frees it,
    /// promote this process to writer (build + watch). Runs until promotion.
    async fn failover_loop(self, root: PathBuf, lock_file: File) {
        loop {
            tokio::time::sleep(self.poll).await;
            if self.is_writer(&root) {
                break;
            }
            match lock_file.try_lock() {
                Ok(()) => {
                    self.take_over(root, lock_file).await;
                    break;
                }
                Err(TryLockError::WouldBlock) => continue,
                Err(TryLockError::Error(error)) => {
                    eprintln!(
                        "oxcode: failover lock error for {}: {error}",
                        root.display()
                    );
                    break;
                }
            }
        }
    }

    /// Promotes this process to writer for `root` after winning the freed lock,
    /// logging the outcome to stderr.
    async fn take_over(&self, root: PathBuf, lock_file: File) {
        match self.promote_to_writer(root.clone(), lock_file).await {
            Ok(_) => eprintln!(
                "oxcode: promoted to writer after previous writer released {}",
                root.display()
            ),
            Err(error) => {
                eprintln!(
                    "oxcode: failover index failed for {}: {error}",
                    root.display()
                )
            }
        }
    }

    /// Starts a recursive debounced watcher on `root` and a task that re-indexes
    /// (serialized by `write_lock`) on each debounced change. Returns `None` if the
    /// watcher could not be started.
    fn spawn_watch(
        &self,
        root: &Path,
        write_lock: Arc<Mutex<()>>,
        reindexes: Arc<AtomicU64>,
    ) -> Option<Debouncer<RecommendedWatcher, RecommendedCache>> {
        let (tick_tx, tick_rx) = unbounded_channel::<()>();
        let mut debouncer =
            match new_debouncer(self.debounce, None, move |result: DebounceEventResult| {
                // Tick on any batch that touches at least one indexable path. A
                // batch confined to skip dirs (notably `.oxcode/`, which our own
                // re-index writes) is dropped — this is what breaks the feedback
                // loop. Watcher errors are transient; the next real event re-syncs.
                if let Ok(events) = result
                    && events
                        .iter()
                        .flat_map(|event| event.paths.iter())
                        .any(|path| !is_ignored_path(path))
                {
                    let _ = tick_tx.send(());
                }
            }) {
                Ok(debouncer) => debouncer,
                Err(error) => {
                    eprintln!(
                        "oxcode: file watcher unavailable for {}: {error}",
                        root.display()
                    );
                    return None;
                }
            };
        if let Err(error) = debouncer.watch(root, RecursiveMode::Recursive) {
            eprintln!("oxcode: cannot watch {}: {error}", root.display());
            return None;
        }
        tokio::spawn(watch_loop(
            Arc::clone(&self.indexes),
            root.to_path_buf(),
            write_lock,
            reindexes,
            tick_rx,
        ));
        Some(debouncer)
    }

    /// Whether this process is the elected writer for `root`.
    fn is_writer(&self, root: &Path) -> bool {
        self.writers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(root)
    }

    /// Whether this process is a standby (failover participant) for `root`.
    fn is_standby(&self, root: &Path) -> bool {
        self.standbys
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(root)
    }

    /// This process's role for `root`, plus whether it is watching and its reindex
    /// count (0 for non-writers).
    fn watch_state(&self, root: &Path) -> (&'static str, bool, u64) {
        if let Some(state) = self
            .writers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(root)
        {
            return ("writer", true, state.reindexes.load(Ordering::Relaxed));
        }
        if self.is_standby(root) {
            return ("standby", false, 0);
        }
        ("reader", false, 0)
    }

    /// Resolves the project root from an optional `path`, preferring workspace
    /// signals over this process's cwd. MCP hosts often start servers with
    /// `cwd=$HOME` even when the agent is in a project folder; omitting `path`
    /// must not silently index the home directory.
    ///
    /// Never calls `roots/list` here — nested client requests during tool handling
    /// can hang on some hosts. Roots are fetched from `on_initialized` /
    /// `on_roots_list_changed` only; this waits briefly for that in-flight fetch
    /// when the cache is still cold.
    async fn resolve_root(&self, path: Option<String>) -> Result<PathBuf, McpError> {
        // Explicit path / OXCODE_ROOT pin win without waiting on MCP roots.
        // Host env snapshots (CLAUDE_PROJECT_DIR / WORKSPACE_FOLDER_PATHS) do
        // not — they are fixed at process start and must lose to a refreshed
        // roots cache after a folder switch.
        if path
            .as_deref()
            .map(str::trim)
            .is_some_and(|value| !value.is_empty())
            || project_root::oxcode_root_override().is_some()
        {
            return resolve_project_root(path, None);
        }
        let mcp_root = self.roots.wait_ready().await;
        resolve_project_root(path, mcp_root)
    }
}

/// Re-indexes `root` on each debounced change tick until the watcher stops.
async fn watch_loop(
    indexes: Arc<Mutex<HashMap<PathBuf, Arc<ProjectIndex>>>>,
    root: PathBuf,
    write_lock: Arc<Mutex<()>>,
    reindexes: Arc<AtomicU64>,
    mut tick_rx: UnboundedReceiver<()>,
) {
    while tick_rx.recv().await.is_some() {
        // Collapse a burst of ticks that landed during the last re-index into one run.
        while tick_rx.try_recv().is_ok() {}
        match run_reindex(&indexes, &root, &write_lock, &reindexes).await {
            Ok(_) => eprintln!(
                "oxcode: re-indexed {} (#{})",
                root.display(),
                reindexes.load(Ordering::Relaxed)
            ),
            Err(error) => eprintln!("oxcode: re-index failed for {}: {error}", root.display()),
        }
    }
}

/// Runs `index_project` for `root` under `write_lock` (serializing this process's
/// writers), evicts the cached reader so the next query reopens the fresh index,
/// and bumps the reindex counter. An unchanged tree is a cheap digest no-op.
async fn run_reindex(
    indexes: &Arc<Mutex<HashMap<PathBuf, Arc<ProjectIndex>>>>,
    root: &Path,
    write_lock: &Mutex<()>,
    reindexes: &AtomicU64,
) -> anyhow::Result<IndexStats> {
    let _guard = write_lock.lock().await;
    let root_owned = root.to_path_buf();
    let stats =
        tokio::task::spawn_blocking(move || oxcode_core::index_project(&root_owned)).await??;
    // Bump the counter before evicting the cache: the eviction is what lets a
    // concurrent reader observe the new commit, so ordering the increment first
    // guarantees "new symbol visible" implies "reindex counted".
    reindexes.fetch_add(1, Ordering::Relaxed);
    indexes.lock().await.remove(root);
    Ok(stats)
}

/// Whether a changed path falls in a directory source discovery skips, so its
/// events should not trigger a re-index. Mirrors `oxcode_core`'s scan skip list;
/// `.oxcode/` is the load-bearing entry that prevents a self-triggered loop.
fn is_ignored_path(path: &Path) -> bool {
    path.components().any(|component| {
        matches!(component, std::path::Component::Normal(name)
            if WATCH_SKIP_DIRS.iter().any(|skip| name == std::ffi::OsStr::new(skip)))
    })
}

/// Creates the `.oxcode` index dir and its self-ignoring `.gitignore` so the lock
/// file is never committed. Idempotent.
fn ensure_index_dir(index_directory: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(index_directory)?;
    let gitignore = index_directory.join(".gitignore");
    if !gitignore.exists() {
        std::fs::write(&gitignore, "*\n")?;
    }
    Ok(())
}

/// Reads a millisecond duration from `key`, falling back to `default`.
fn env_duration(key: &str, default: Duration) -> Duration {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(default)
}

/// Builds the JSON body for an `oxcode_watch` response.
fn watch_body(
    root: &Path,
    role: &str,
    watching: bool,
    stats: Option<&IndexStats>,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "root": root.display().to_string(),
        "role": role,
        "watching": watching,
    });
    if let Some(stats) = stats {
        body["index"] = serde_json::to_value(stats).unwrap_or(serde_json::Value::Null);
    } else if !watching {
        body["message"] = serde_json::json!(
            "another oxcode instance is watching this root; standing by to take over if it exits"
        );
    }
    body
}

#[tool_handler]
#[task_handler(processor = self.operations)]
impl ServerHandler for OxcodeServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tasks_with(TasksCapability::server_default())
                .build(),
        )
        .with_instructions(INSTRUCTIONS)
    }

    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        self.roots.fetch(&context.peer).await;
    }

    async fn on_roots_list_changed(&self, context: NotificationContext<RoleServer>) {
        self.roots.fetch(&context.peer).await;
    }
}

/// Parses caller-supplied kind strings into `NodeKind`, dropping unknown ones.
fn parse_kinds(kinds: Option<&[String]>) -> Vec<NodeKind> {
    kinds
        .unwrap_or_default()
        .iter()
        .filter_map(|kind| NodeKind::try_from(kind.as_str()).ok())
        .collect()
}

/// Resolves a selector to a single symbol, or a structured ambiguous/not-found value.
fn resolve_symbol(index: &ProjectIndex, selector: &str) -> oxcode_core::Result<serde_json::Value> {
    let value = match index.resolve_selector(selector)?.as_slice() {
        [single] => serde_json::json!({ "status": "matched", "symbol": single }),
        [] => serde_json::json!({ "status": "not_found", "selector": selector, "matches": [] }),
        matches => {
            serde_json::json!({ "status": "ambiguous", "selector": selector, "matches": matches })
        }
    };
    Ok(value)
}

/// Runs a blocking oxcode read on the blocking pool, mapping errors to MCP errors.
async fn blocking<T, F>(f: F) -> Result<T, McpError>
where
    T: Send + 'static,
    F: FnOnce() -> oxcode_core::Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|error| McpError::internal_error(format!("oxcode task failed: {error}"), None))?
        .map_err(|error| McpError::internal_error(error.to_string(), None))
}

/// Serializes a report into one JSON text content block.
fn json_result<T: serde::Serialize>(value: &T) -> Result<CallToolResult, McpError> {
    let text = serde_json::to_string(value)
        .map_err(|error| McpError::internal_error(format!("serialize failed: {error}"), None))?;
    Ok(CallToolResult::success(vec![Content::text(text)]))
}

#[cfg(test)]
#[path = "integration_tests.rs"]
mod tests;

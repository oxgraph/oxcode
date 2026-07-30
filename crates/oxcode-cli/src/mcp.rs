//! The `oxcode mcp` server: tools mapped onto `oxcode_core::ProjectIndex`.
//!
//! Exposes oxcode's read-only queries plus a single-writer file watcher
//! (`oxcode_watch`) to coding agents over MCP (stdio). Run it with `oxcode mcp`;
//! configure your agent to launch that command. Across many MCP processes pointed
//! at one folder, a `.oxcode/watch.lock` file lock elects exactly one writer (the
//! process that watches and re-indexes); the rest serve reads.

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
use rmcp::{
    ErrorData as McpError, Peer, RoleServer, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Content, ServerCapabilities, ServerInfo, TasksCapability},
    schemars,
    service::NotificationContext,
    task_handler,
    task_manager::OperationProcessor,
    tool, tool_handler, tool_router,
    transport::stdio,
};
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

/// How long an omitted-`path` resolve will wait for the in-flight
/// `on_initialized` roots fetch before falling through to cwd. Never starts a
/// nested `roots/list` from a tool handler.
const ROOTS_READY_WAIT: Duration = Duration::from_millis(500);

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
CLAUDE_PROJECT_DIR, WORKSPACE_FOLDER_PATHS, or the client's MCP roots — not from this process's \
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
    /// Client MCP roots (`roots/list`), cached so omitted `path` defaults to the
    /// workspace rather than this process's cwd (often `$HOME` under MCP hosts).
    client_roots: Arc<std::sync::Mutex<Option<Vec<PathBuf>>>>,
    /// Becomes `true` after the first init-time roots fetch attempt finishes (or
    /// is skipped because the client has no roots capability). Tools wait on this
    /// instead of issuing nested `roots/list` calls.
    roots_ready: tokio::sync::watch::Receiver<bool>,
    /// Sender half for [`Self::roots_ready`].
    roots_ready_tx: Arc<tokio::sync::watch::Sender<bool>>,
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
    /// Project root; defaults to the workspace (OXCODE_ROOT / CLAUDE_PROJECT_DIR /
    /// WORKSPACE_FOLDER_PATHS / MCP roots), never silently to `$HOME`.
    pub path: Option<String>,
    /// Maximum source characters to render (default 20000).
    pub max_bytes: Option<usize>,
}

/// A keyword search over indexed symbols.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct SearchParams {
    /// Keywords matched against symbol names, signatures, and docs.
    pub query: String,
    /// Project root; defaults to the workspace (OXCODE_ROOT / CLAUDE_PROJECT_DIR /
    /// WORKSPACE_FOLDER_PATHS / MCP roots), never silently to `$HOME`.
    pub path: Option<String>,
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
    /// Project root; defaults to the workspace (OXCODE_ROOT / CLAUDE_PROJECT_DIR /
    /// WORKSPACE_FOLDER_PATHS / MCP roots), never silently to `$HOME`.
    pub path: Option<String>,
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
    /// Project root; defaults to the workspace (OXCODE_ROOT / CLAUDE_PROJECT_DIR /
    /// WORKSPACE_FOLDER_PATHS / MCP roots), never silently to `$HOME`.
    pub path: Option<String>,
}

/// A keyword search over indexed files.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct FilesParams {
    /// Keywords matched against file paths and their symbols.
    pub query: String,
    /// Project root; defaults to the workspace (OXCODE_ROOT / CLAUDE_PROJECT_DIR /
    /// WORKSPACE_FOLDER_PATHS / MCP roots), never silently to `$HOME`.
    pub path: Option<String>,
    /// Maximum number of files (default 30).
    pub limit: Option<usize>,
}

/// A project-root pointer.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct StatusParams {
    /// Project root; defaults to the workspace (OXCODE_ROOT / CLAUDE_PROJECT_DIR /
    /// WORKSPACE_FOLDER_PATHS / MCP roots), never silently to `$HOME`.
    pub path: Option<String>,
}

/// A project root to watch and keep indexed.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct WatchParams {
    /// Project root to watch; defaults to the workspace (OXCODE_ROOT /
    /// CLAUDE_PROJECT_DIR / WORKSPACE_FOLDER_PATHS / MCP roots), never silently to
    /// `$HOME`.
    pub path: Option<String>,
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
        let (roots_ready_tx, roots_ready) = tokio::sync::watch::channel(false);
        Self {
            tool_router: Self::tool_router(),
            indexes: Arc::new(Mutex::new(HashMap::new())),
            operations: Arc::new(Mutex::new(OperationProcessor::new())),
            writers: Arc::new(std::sync::Mutex::new(HashMap::new())),
            standbys: Arc::new(std::sync::Mutex::new(HashSet::new())),
            client_roots: Arc::new(std::sync::Mutex::new(None)),
            roots_ready,
            roots_ready_tx: Arc::new(roots_ready_tx),
            debounce,
            poll,
        }
    }

    #[tool(
        description = "Start (or join) watching a project so its index is built and kept current as files change. Exactly one MCP instance per folder becomes the writer (it holds a file lock and re-indexes on changes); other instances become readers that just serve queries and automatically take over if the writer exits. Call this once before querying. Optional `path` defaults to the workspace (OXCODE_ROOT / CLAUDE_PROJECT_DIR / WORKSPACE_FOLDER_PATHS / MCP roots); never silently to $HOME.",
        execution(task_support = "optional")
    )]
    async fn oxcode_watch(
        &self,
        Parameters(params): Parameters<WatchParams>,
    ) -> Result<CallToolResult, McpError> {
        let root = self.resolve_root(params.path).await?;

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
        let index = self.index_for(params.path).await?;
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
        let index = self.index_for(params.path).await?;
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
        let index = self.index_for(params.path).await?;
        let selector = params.selector;
        let value = blocking(move || resolve_symbol(&index, &selector)).await?;
        json_result(&value)
    }

    #[tool(description = "Search indexed files by keyword.")]
    async fn oxcode_files(
        &self,
        Parameters(params): Parameters<FilesParams>,
    ) -> Result<CallToolResult, McpError> {
        let index = self.index_for(params.path).await?;
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
        let root = self.resolve_root(params.path).await?;
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
        let index = self.index_for(params.path).await?;
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
    /// can hang on some hosts. Roots are fetched in [`Self::fetch_client_roots`]
    /// from `on_initialized` / `on_roots_list_changed` only; this waits briefly
    /// for that in-flight fetch when the cache is still cold.
    async fn resolve_root(&self, path: Option<String>) -> Result<PathBuf, McpError> {
        let explicit = path
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let raw = if let Some(explicit_path) = explicit.clone() {
            explicit_path
        } else if let Some(from_env) = env_project_root() {
            from_env
        } else if let Some(from_roots) = self.workspace_root_after_ready().await {
            from_roots
        } else {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        };
        let root = canonicalize_root(raw);
        if explicit.is_none() && is_home_directory(&root) {
            return Err(McpError::invalid_params(
                format!(
                    "refusing to use home directory {} as the project root — MCP hosts often \
                     start this server with cwd=$HOME even when your workspace is elsewhere. \
                     Pass `path` (the project folder), or set OXCODE_ROOT / CLAUDE_PROJECT_DIR \
                     / WORKSPACE_FOLDER_PATHS.",
                    root.display()
                ),
                None,
            ));
        }
        Ok(root)
    }

    /// First cached client MCP root, waiting briefly for the init-time fetch.
    async fn workspace_root_after_ready(&self) -> Option<PathBuf> {
        if let Some(root) = self.cached_workspace_root() {
            return Some(root);
        }
        if !*self.roots_ready.borrow() {
            let mut ready = self.roots_ready.clone();
            let _ = tokio::time::timeout(ROOTS_READY_WAIT, ready.wait_for(|ready| *ready)).await;
        }
        self.cached_workspace_root()
    }

    /// First non-empty client MCP root already cached from init / list_changed.
    fn cached_workspace_root(&self) -> Option<PathBuf> {
        self.client_roots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(|roots| roots.first().cloned())
    }

    /// Fetches `roots/list` outside tool handling and updates the cache.
    ///
    /// On failure, leaves the previous cache untouched (so a transient error does
    /// not erase a good root). An empty successful list clears the cache to `None`
    /// rather than `Some([])`, so a later `roots/list_changed` can populate it
    /// without looking like a settled empty answer forever. Always marks
    /// [`Self::roots_ready`] so tool handlers can proceed.
    async fn fetch_client_roots(&self, peer: &Peer<RoleServer>) {
        let supports_roots = peer
            .peer_info()
            .and_then(|info| info.capabilities.roots.as_ref())
            .is_some();
        if supports_roots && let Ok(result) = peer.list_roots().await {
            let roots: Vec<PathBuf> = result
                .roots
                .iter()
                .filter_map(|root| file_uri_to_path(&root.uri))
                .collect();
            let cached = (!roots.is_empty()).then_some(roots);
            *self
                .client_roots
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = cached;
        }
        let _ = self.roots_ready_tx.send(true);
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
        self.fetch_client_roots(&context.peer).await;
    }

    async fn on_roots_list_changed(&self, context: NotificationContext<RoleServer>) {
        self.fetch_client_roots(&context.peer).await;
    }
}

/// Project root from host-injected environment variables, in priority order.
///
/// MCP hosts frequently leave the server process cwd at `$HOME` while advertising
/// the real workspace via these variables (Claude Code → `CLAUDE_PROJECT_DIR`,
/// Cursor → `WORKSPACE_FOLDER_PATHS`). `OXCODE_ROOT` is the explicit override.
fn env_project_root() -> Option<PathBuf> {
    for key in ["OXCODE_ROOT", "CLAUDE_PROJECT_DIR"] {
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(PathBuf::from(trimmed));
            }
        }
    }
    std::env::var("WORKSPACE_FOLDER_PATHS")
        .ok()
        .and_then(|value| first_workspace_folder(&value))
}

/// First folder from a `WORKSPACE_FOLDER_PATHS` value (single path or CSV).
fn first_workspace_folder(value: &str) -> Option<PathBuf> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let as_path = PathBuf::from(trimmed);
    if as_path.is_dir() {
        return Some(as_path);
    }
    // Multi-root workspaces: comma-separated. Do not split on `:` — that breaks
    // Windows drive letters (`C:\...`).
    trimmed
        .split(',')
        .map(str::trim)
        .find(|part| !part.is_empty())
        .map(PathBuf::from)
}

/// Whether `path` is the current user's home directory (best-effort).
fn is_home_directory(path: &Path) -> bool {
    let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) else {
        return false;
    };
    let home = PathBuf::from(home);
    canonicalize_root(path.to_path_buf()) == canonicalize_root(home)
}

/// Canonicalizes best-effort so reader cache / writer registry / lock file key on
/// the same absolute path (FS events report canonical paths). Falls back to the
/// raw path when it does not exist yet.
fn canonicalize_root(path: PathBuf) -> PathBuf {
    std::fs::canonicalize(&path).unwrap_or(path)
}

/// Converts a `file://` MCP root URI into a filesystem path.
fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let path = if let Some(path) = rest.strip_prefix("localhost") {
        path
    } else if rest.starts_with('/') {
        rest
    } else {
        return None;
    };
    let decoded = percent_decode(path);
    if decoded.is_empty() {
        return None;
    }
    Some(PathBuf::from(decoded))
}

/// Decodes `%XX` sequences in a URI path; returns the input unchanged when none.
fn percent_decode(input: &str) -> String {
    if !input.contains('%') {
        return input.to_owned();
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_nibble(bytes[index + 1]), hex_nibble(bytes[index + 2]))
        {
            out.push((high << 4) | low);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
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
mod tests {
    //! In-process integration tests: a real `OxcodeServer` and an MCP client wired
    //! over `tokio::io::duplex`, exercising the full JSON-RPC stack. These cover
    //! tool registration, writer election + the read path, auto re-index on change,
    //! and the task lifecycle. The cross-process guarantee is proven separately by
    //! `tests/multiprocess.rs` (real spawned processes).

    use std::{
        sync::{Mutex, MutexGuard},
        time::Duration,
    };

    use rmcp::{
        ClientHandler, RoleClient,
        model::{
            CallToolRequestParams, ClientCapabilities, ClientInfo, ClientRequest,
            GetTaskInfoParams, GetTaskResultParams, Implementation, ListRootsResult, Request, Root,
            ServerResult, TaskStatus, TaskSupport,
        },
        service::{RequestContext, RunningService},
    };

    use super::*;

    /// Serializes tests that mutate process-global project-root env vars.
    static PROJECT_ROOT_ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Env keys consulted by [`env_project_root`], for save/restore around tests.
    const PROJECT_ROOT_ENV_KEYS: &[&str] = &[
        "OXCODE_ROOT",
        "CLAUDE_PROJECT_DIR",
        "WORKSPACE_FOLDER_PATHS",
    ];

    /// Clears project-root env vars for the duration of a test; restores on drop.
    struct ProjectRootEnvGuard {
        _lock: MutexGuard<'static, ()>,
        previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl ProjectRootEnvGuard {
        fn clear() -> Self {
            let lock = PROJECT_ROOT_ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let previous = PROJECT_ROOT_ENV_KEYS
                .iter()
                .map(|&key| (key, std::env::var_os(key)))
                .collect::<Vec<_>>();
            // SAFETY: held exclusively via PROJECT_ROOT_ENV_LOCK for this process.
            unsafe {
                for key in PROJECT_ROOT_ENV_KEYS {
                    std::env::remove_var(key);
                }
            }
            Self {
                _lock: lock,
                previous,
            }
        }

        fn set(&self, key: &str, value: impl AsRef<std::ffi::OsStr>) {
            // SAFETY: guard holds PROJECT_ROOT_ENV_LOCK.
            unsafe {
                std::env::set_var(key, value);
            }
        }
    }

    impl Drop for ProjectRootEnvGuard {
        fn drop(&mut self) {
            // SAFETY: guard still holds PROJECT_ROOT_ENV_LOCK until drop completes.
            unsafe {
                for (key, value) in self.previous.drain(..) {
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                }
            }
        }
    }

    /// Minimal MCP client; the server is what these tests exercise.
    #[derive(Clone, Default)]
    struct TestClient;

    impl ClientHandler for TestClient {}

    /// MCP client that advertises workspace roots (Cursor / Claude Code do this).
    #[derive(Clone)]
    struct RootsClient {
        roots: Vec<PathBuf>,
    }

    impl ClientHandler for RootsClient {
        fn get_info(&self) -> ClientInfo {
            ClientInfo::new(
                ClientCapabilities::builder().enable_roots().build(),
                Implementation::from_build_env(),
            )
        }

        fn list_roots(
            &self,
            _context: RequestContext<RoleClient>,
        ) -> impl std::future::Future<Output = Result<ListRootsResult, McpError>> + Send + '_
        {
            let roots = self
                .roots
                .iter()
                .map(|path| Root::new(format!("file://{}", path.display())))
                .collect();
            std::future::ready(Ok(ListRootsResult::new(roots)))
        }
    }

    /// Wires a fresh `OxcodeServer` (with the given intervals) to a `TestClient`
    /// over an in-memory duplex pipe and returns the connected client service.
    async fn connect(debounce: Duration, poll: Duration) -> RunningService<RoleClient, TestClient> {
        let (server_transport, client_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = OxcodeServer::new_with(debounce, poll)
                .serve(server_transport)
                .await
                .expect("server serve");
            let _ = server.waiting().await;
        });
        TestClient
            .serve(client_transport)
            .await
            .expect("client connect")
    }

    /// Writes a minimal two-function Rust project into a fresh temp dir.
    fn rust_project() -> tempfile::TempDir {
        let temp = tempfile::TempDir::new().expect("temp dir");
        std::fs::create_dir_all(temp.path().join("src")).expect("mkdir src");
        std::fs::write(
            temp.path().join("src/lib.rs"),
            "pub fn helper() {}\npub fn entry() {\n    helper();\n}\n",
        )
        .expect("write lib.rs");
        temp
    }

    /// Builds a tool-call params object for `name` with JSON `arguments`.
    fn tool_call(name: &'static str, arguments: serde_json::Value) -> CallToolRequestParams {
        let mut params = CallToolRequestParams::new(name);
        params.arguments = arguments.as_object().cloned();
        params
    }

    /// Extracts the single text content block from a tool result.
    fn result_text(result: &CallToolResult) -> &str {
        result
            .content
            .first()
            .and_then(|content| content.as_text())
            .map(|text| text.text.as_str())
            .expect("text content")
    }

    /// Calls `oxcode_watch` for `path` and returns the parsed JSON response.
    async fn watch(
        client: &RunningService<RoleClient, TestClient>,
        path: &str,
    ) -> serde_json::Value {
        let result = client
            .call_tool(tool_call(
                "oxcode_watch",
                serde_json::json!({ "path": path }),
            ))
            .await
            .expect("watch call");
        serde_json::from_str(result_text(&result)).expect("watch json")
    }

    /// Polls `oxcode_search` (bounded) until `name` actually appears as a match.
    /// Inspects the parsed `matches` array — not a substring of the JSON, which
    /// would falsely match the echoed `query` field.
    async fn poll_symbol_indexed(
        client: &RunningService<RoleClient, TestClient>,
        path: &str,
        name: &str,
    ) -> bool {
        for _ in 0..100 {
            let searched = client
                .call_tool(tool_call(
                    "oxcode_search",
                    serde_json::json!({ "path": path, "query": name }),
                ))
                .await
                .expect("search call");
            let report: serde_json::Value =
                serde_json::from_str(result_text(&searched)).expect("search json");
            // Keyword search is fuzzy, so check for an exact-named match rather
            // than "any match" (which would falsely fire on weak candidates).
            let matched = report["matches"]
                .as_array()
                .is_some_and(|matches| matches.iter().any(|entry| entry["symbol"]["name"] == name));
            if matched {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }

    /// Polls `tasks/get` until the task reaches a terminal status (or times out).
    async fn poll_until_terminal(
        client: &RunningService<RoleClient, TestClient>,
        task_id: &str,
    ) -> TaskStatus {
        let mut status = TaskStatus::Working;
        for _ in 0..200 {
            let info = client
                .send_request(ClientRequest::GetTaskInfoRequest(Request::new(
                    GetTaskInfoParams {
                        meta: None,
                        task_id: task_id.to_owned(),
                    },
                )))
                .await
                .expect("tasks/get");
            if let ServerResult::GetTaskResult(result) = info {
                status = result.task.status;
            }
            if status != TaskStatus::Working {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        status
    }

    #[test]
    fn env_project_root_prefers_oxcode_root() {
        let project = tempfile::TempDir::new().expect("temp");
        let guard = ProjectRootEnvGuard::clear();
        guard.set("OXCODE_ROOT", project.path());
        let resolved = env_project_root().expect("OXCODE_ROOT");
        assert_eq!(
            canonicalize_root(resolved),
            canonicalize_root(project.path().to_path_buf())
        );
    }

    #[test]
    fn first_workspace_folder_accepts_csv_and_single_path() {
        let project = tempfile::TempDir::new().expect("temp");
        let path = project.path().to_string_lossy().into_owned();
        assert_eq!(
            first_workspace_folder(&path),
            Some(PathBuf::from(&path)),
            "existing single path wins without splitting"
        );
        assert_eq!(
            first_workspace_folder(&format!("{path},/does/not/exist")),
            Some(PathBuf::from(&path))
        );
        assert_eq!(
            first_workspace_folder("/missing/a,/missing/b"),
            Some(PathBuf::from("/missing/a"))
        );
    }

    #[test]
    fn file_uri_to_path_decodes_file_roots() {
        assert_eq!(
            file_uri_to_path("file:///Users/snowmead/opt/jinttai"),
            Some(PathBuf::from("/Users/snowmead/opt/jinttai"))
        );
        assert_eq!(
            file_uri_to_path("file://localhost/tmp/project%20name"),
            Some(PathBuf::from("/tmp/project name"))
        );
        assert_eq!(file_uri_to_path("https://example.com"), None);
    }

    #[test]
    fn is_home_directory_matches_home_env() {
        let home = tempfile::TempDir::new().expect("home");
        // Reuse the project-root env lock so HOME mutations never race other tests.
        let _guard = ProjectRootEnvGuard::clear();
        let previous_home = std::env::var_os("HOME");
        // SAFETY: PROJECT_ROOT_ENV_LOCK is held via `_guard`.
        unsafe {
            std::env::set_var("HOME", home.path());
        }
        assert!(is_home_directory(home.path()));
        assert!(!is_home_directory(&home.path().join("opt/jinttai")));
        // SAFETY: restore HOME before `_guard` drops and releases the lock.
        unsafe {
            match previous_home {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[tokio::test]
    async fn omitted_path_uses_client_mcp_roots_not_process_cwd() {
        let _env = ProjectRootEnvGuard::clear();
        let project = rust_project();
        let (server_transport, client_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server =
                OxcodeServer::new_with(Duration::from_millis(50), Duration::from_millis(150))
                    .serve(server_transport)
                    .await
                    .expect("server serve");
            let _ = server.waiting().await;
        });
        let client = RootsClient {
            roots: vec![project.path().to_path_buf()],
        }
        .serve(client_transport)
        .await
        .expect("client connect");

        // No `path` argument: `on_initialized` already cached roots/list; resolve
        // must use that workspace root (not process cwd) without nested roots/list.
        let result = client
            .call_tool(tool_call("oxcode_watch", serde_json::json!({})))
            .await
            .expect("watch without path");
        let body: serde_json::Value =
            serde_json::from_str(result_text(&result)).expect("watch json");
        assert_eq!(body["role"], "writer");
        assert_eq!(
            canonicalize_root(PathBuf::from(body["root"].as_str().expect("root"))),
            canonicalize_root(project.path().to_path_buf()),
            "omitted path must use the client's MCP root, not the server process cwd"
        );
    }

    /// `flock` is per open-file-description on macOS/Linux: a second independent
    /// open of the same path cannot take the lock the first holds. This pins the
    /// platform behavior the writer election depends on.
    #[test]
    fn watch_lock_is_exclusive_per_handle() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let path = temp.path().join("watch.lock");
        let first = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .expect("open first");
        first.try_lock().expect("first acquires");
        let second = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open second");
        assert!(
            matches!(second.try_lock(), Err(TryLockError::WouldBlock)),
            "a second handle cannot take the held lock"
        );
    }

    #[tokio::test]
    async fn lists_tools_with_watch_and_explore_task_support() {
        let client = connect(DEFAULT_DEBOUNCE, DEFAULT_POLL).await;
        let tools = client.list_all_tools().await.expect("list tools");

        assert!(
            tools.iter().any(|tool| tool.name == "oxcode_watch"),
            "oxcode_watch is registered"
        );
        assert!(
            tools.iter().all(|tool| tool.name != "oxcode_index"),
            "the old write tool is gone"
        );

        let task_support = |name: &str| {
            tools
                .iter()
                .find(|tool| tool.name == name)
                .and_then(|tool| tool.execution.as_ref())
                .and_then(|execution| execution.task_support)
        };
        assert_eq!(task_support("oxcode_watch"), Some(TaskSupport::Optional));
        assert_eq!(task_support("oxcode_explore"), Some(TaskSupport::Optional));
        assert_eq!(task_support("oxcode_search"), None);
        assert_eq!(task_support("oxcode_status"), None);
    }

    #[tokio::test]
    async fn watch_elects_writer_and_serves_queries() {
        let project = rust_project();
        let path = project.path().to_string_lossy().into_owned();
        let client = connect(Duration::from_millis(50), Duration::from_millis(150)).await;

        let watched = watch(&client, &path).await;
        assert_eq!(watched["role"], "writer", "first watcher is the writer");
        assert_eq!(watched["watching"], true);

        let explored = client
            .call_tool(tool_call(
                "oxcode_explore",
                serde_json::json!({ "path": path, "query": "entry" }),
            ))
            .await
            .expect("explore call");
        assert!(
            result_text(&explored).contains("entry"),
            "writer's index is queryable"
        );
    }

    #[tokio::test]
    async fn second_watcher_on_same_root_is_standby() {
        let project = rust_project();
        let path = project.path().to_string_lossy().into_owned();
        let writer_client = connect(Duration::from_millis(50), Duration::from_millis(150)).await;
        let standby_client = connect(Duration::from_millis(50), Duration::from_millis(150)).await;

        assert_eq!(watch(&writer_client, &path).await["role"], "writer");
        // Second server, same root: the lock is held, so it becomes a standby.
        assert_eq!(watch(&standby_client, &path).await["role"], "standby");

        // The standby still answers queries off the shared on-disk index.
        let explored = standby_client
            .call_tool(tool_call(
                "oxcode_explore",
                serde_json::json!({ "path": path, "query": "entry" }),
            ))
            .await
            .expect("reader explore");
        assert!(result_text(&explored).contains("entry"));
    }

    #[tokio::test]
    async fn query_without_watch_errors_when_no_index() {
        let project = rust_project();
        let path = project.path().to_string_lossy().into_owned();
        let client = connect(DEFAULT_DEBOUNCE, DEFAULT_POLL).await;

        // No oxcode_watch, no prior index: a query must not build; it hints instead.
        let result = client
            .call_tool(tool_call(
                "oxcode_explore",
                serde_json::json!({ "path": path, "query": "entry" }),
            ))
            .await;
        assert!(
            result.is_err(),
            "query before oxcode_watch errors with a hint, never silently builds"
        );
    }

    #[tokio::test]
    async fn writer_auto_reindexes_on_change() {
        let project = rust_project();
        let path = project.path().to_string_lossy().into_owned();
        let client = connect(Duration::from_millis(50), Duration::from_millis(150)).await;

        assert_eq!(watch(&client, &path).await["role"], "writer");

        // Let the FS-event stream establish before the change: FSEvents (and
        // other backends) have a startup window where a change can land as
        // initial state and go unreported.
        tokio::time::sleep(Duration::from_millis(300)).await;
        std::fs::write(
            project.path().join("src/extra.rs"),
            "pub fn brand_new_symbol() {}\n",
        )
        .expect("write extra.rs");

        let found = poll_symbol_indexed(&client, &path, "brand_new_symbol").await;
        assert!(
            found,
            "the writer's watcher re-indexed and surfaced the symbol"
        );

        let status: serde_json::Value = serde_json::from_str(result_text(
            &client
                .call_tool(tool_call(
                    "oxcode_status",
                    serde_json::json!({ "path": path }),
                ))
                .await
                .expect("status call"),
        ))
        .expect("status json");
        assert_eq!(status["watch"]["role"], "writer");
        assert!(
            status["watch"]["reindexes"].as_u64().unwrap_or(0) >= 2,
            "writer reindexed at least the initial build and the change"
        );
    }

    #[tokio::test]
    async fn task_augmented_watch_completes() {
        let project = rust_project();
        let path = project.path().to_string_lossy().into_owned();
        let client = connect(Duration::from_millis(50), Duration::from_millis(150)).await;

        // Task-augment the call: typed `call_tool` cannot carry a task field, so
        // send the request directly and expect an immediate CreateTaskResult.
        let mut params = tool_call("oxcode_watch", serde_json::json!({ "path": path }));
        params.task = serde_json::json!({ "ttl": 60_000 }).as_object().cloned();
        let created = client
            .send_request(ClientRequest::CallToolRequest(Request::new(params)))
            .await
            .expect("enqueue task");
        let task_id = match created {
            ServerResult::CreateTaskResult(result) => {
                assert_eq!(result.task.status, TaskStatus::Working);
                result.task.task_id
            }
            other => panic!("expected CreateTaskResult, got {other:?}"),
        };

        let status = poll_until_terminal(&client, &task_id).await;
        assert_eq!(
            status,
            TaskStatus::Completed,
            "watch task ran to completion"
        );

        let payload = client
            .send_request(ClientRequest::GetTaskResultRequest(Request::new(
                GetTaskResultParams {
                    meta: None,
                    task_id,
                },
            )))
            .await
            .expect("tasks/result");
        let text = match payload {
            ServerResult::CallToolResult(result) => result_text(&result).to_owned(),
            ServerResult::GetTaskPayloadResult(payload) => payload.0["content"][0]["text"]
                .as_str()
                .expect("tool result text")
                .to_owned(),
            other => panic!("expected the deferred tool result, got {other:?}"),
        };
        assert!(
            text.contains("writer"),
            "deferred watch result reports the elected writer role"
        );
    }
}

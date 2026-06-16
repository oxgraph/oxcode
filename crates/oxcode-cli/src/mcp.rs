//! The `oxcode mcp` server: tools mapped onto `oxcode_core::ProjectIndex`.
//!
//! Exposes oxcode's read-only queries to coding agents over MCP (stdio). Run it
//! with `oxcode mcp`; configure your agent to launch that command.

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use oxcode_core::{GraphDirection, IndexProgress, NodeKind, ProjectIndex};
use rmcp::{
    ErrorData as McpError, Peer, RoleServer, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, Meta, ProgressNotificationParam, ServerCapabilities, ServerInfo,
        TasksCapability,
    },
    schemars, task_handler,
    task_manager::OperationProcessor,
    tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::Deserialize;
use tokio::sync::Mutex;

/// Runs the MCP server over stdio until the client disconnects.
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

/// Server instructions steering agents to the one-call `oxcode_explore` tool.
const INSTRUCTIONS: &str = "This server answers questions about the indexed code repository in the \
working directory. If `oxcode_status` reports no database, call `oxcode_index` first to build it \
(it accepts an optional `path`, defaults to the working directory, and re-indexing after changes is \
incremental). For almost any code-understanding question, call `oxcode_explore` first with the \
user's question verbatim: it returns the most relevant symbols (ranked by graph centrality), their \
source, the relationships among them, the n-ary hyperedges they belong to (trait impl groups and \
container/module membership, ranked by hypergraph PageRank — the architecture-altitude layer), the \
blast radius, and the call flow — in one call. Use \
`oxcode_callers`/`oxcode_callees`/`oxcode_symbol` to follow specific edges, and \
`oxcode_search`/`oxcode_files` only when explore did not surface the target. Prefer these query \
tools over shelling out to grep or reading files; `oxcode_index` is the only tool that writes \
(it maintains `.oxcode/`). Do not edit source files.";

/// MCP server over oxcode's queries plus an `oxcode_index` build tool, caching
/// one opened index per root and driving task-augmented calls through an
/// [`OperationProcessor`].
#[derive(Clone)]
pub(crate) struct OxcodeServer {
    #[expect(
        dead_code,
        reason = "stored per rmcp's #[tool_router] convention; the #[tool_handler]-generated request router reads it through macro-expanded code the dead-code pass does not attribute"
    )]
    tool_router: ToolRouter<OxcodeServer>,
    indexes: Arc<Mutex<HashMap<PathBuf, Arc<ProjectIndex>>>>,
    /// Backs the rmcp `#[task_handler]` lifecycle for task-augmented tool calls.
    operations: Arc<Mutex<OperationProcessor>>,
}

/// A code question to answer in one curated call.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct ExploreParams {
    /// The task or question about the codebase, in natural language.
    pub query: String,
    /// Project root; defaults to the server's working directory.
    pub path: Option<String>,
    /// Maximum source characters to render (default 20000).
    pub max_bytes: Option<usize>,
}

/// A keyword search over indexed symbols.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct SearchParams {
    /// Keywords matched against symbol names, signatures, and docs.
    pub query: String,
    /// Project root; defaults to the server's working directory.
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
    /// Project root; defaults to the server's working directory.
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
    /// Project root; defaults to the server's working directory.
    pub path: Option<String>,
}

/// A keyword search over indexed files.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct FilesParams {
    /// Keywords matched against file paths and their symbols.
    pub query: String,
    /// Project root; defaults to the server's working directory.
    pub path: Option<String>,
    /// Maximum number of files (default 30).
    pub limit: Option<usize>,
}

/// A project-root pointer.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct StatusParams {
    /// Project root; defaults to the server's working directory.
    pub path: Option<String>,
}

/// A project root to build or refresh the index for.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct IndexParams {
    /// Project root to index; defaults to the server's working directory.
    pub path: Option<String>,
}

#[tool_router]
impl OxcodeServer {
    /// Builds an empty server; the index is opened lazily on the first tool call.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
            indexes: Arc::new(Mutex::new(HashMap::new())),
            operations: Arc::new(Mutex::new(OperationProcessor::new())),
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
        description = "Build or refresh the oxcode index for a project (defaults to the working directory), writing .oxcode/index.oxgdb/. Run this first when oxcode_status reports no database, and after code changes to refresh it (re-indexing is incremental). Reports scan/extract/resolve/store progress when invoked with a progress token.",
        execution(task_support = "optional")
    )]
    async fn oxcode_index(
        &self,
        Parameters(params): Parameters<IndexParams>,
        meta: Meta,
        peer: Peer<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let root = resolve_root(params.path);
        let index_root = root.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<IndexProgress>();
        let handle = tokio::task::spawn_blocking(move || {
            oxcode_core::index_project_with_progress(&index_root, |progress| {
                // The receiver is dropped when no progress token was supplied;
                // a failed send just means nobody is listening.
                let _ = tx.send(progress);
            })
        });

        // Forward each stage milestone as an MCP progress notification when the
        // client opted in with a progress token. Draining the channel until the
        // sender drops also serves as the await point for the blocking index.
        if let Some(token) = meta.get_progress_token() {
            while let Some(progress) = rx.recv().await {
                let _ = peer
                    .notify_progress(ProgressNotificationParam {
                        progress_token: token.clone(),
                        progress: f64::from(progress.step),
                        total: Some(f64::from(progress.total)),
                        message: Some(progress.stage.label().to_owned()),
                    })
                    .await;
            }
        } else {
            drop(rx);
        }

        let stats = handle
            .await
            .map_err(|error| {
                McpError::internal_error(format!("oxcode index task failed: {error}"), None)
            })?
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;

        // The on-disk database just changed; drop any cached reader for this
        // root so the next query reopens the freshly reconciled index.
        self.indexes.lock().await.remove(&root);

        json_result(&stats)
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
        description = "Show the indexed project's database status (element/relation counts, paths)."
    )]
    async fn oxcode_status(
        &self,
        Parameters(params): Parameters<StatusParams>,
    ) -> Result<CallToolResult, McpError> {
        let root = resolve_root(params.path);
        let status = blocking(move || oxcode_core::project_status(&root)).await?;
        json_result(&status)
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

    /// Returns a cached opened index for `path` (default cwd), opening on first use.
    async fn index_for(&self, path: Option<String>) -> Result<Arc<ProjectIndex>, McpError> {
        let root = resolve_root(path);
        if let Some(index) = self.indexes.lock().await.get(&root) {
            return Ok(Arc::clone(index));
        }
        let open_root = root.clone();
        let index = Arc::new(blocking(move || ProjectIndex::open(&open_root)).await?);
        self.indexes.lock().await.insert(root, Arc::clone(&index));
        Ok(index)
    }
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
}

/// Resolves the project root from an optional path argument.
fn resolve_root(path: Option<String>) -> PathBuf {
    PathBuf::from(path.unwrap_or_else(|| ".".to_owned()))
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
    //! In-process integration tests: a real `OxcodeServer` and an MCP client are
    //! wired together over `tokio::io::duplex`, exercising the full JSON-RPC stack
    //! (the canonical rmcp test pattern). These guard tool registration, the
    //! `oxcode_index` build path, stage progress notifications, and the task
    //! lifecycle — none of which were covered before.

    use std::{
        future::{Future, ready},
        sync::Mutex as StdMutex,
        time::Duration,
    };

    use rmcp::{
        ClientHandler, RoleClient,
        model::{
            CallToolRequestParams, ClientRequest, GetTaskInfoParams, GetTaskResultParams, Request,
            ServerResult, TaskStatus, TaskSupport,
        },
        service::{MaybeSendFuture, NotificationContext, PeerRequestOptions, RunningService},
    };

    use super::*;

    /// MCP client that records every progress notification it receives.
    #[derive(Clone, Default)]
    struct TestClient {
        progress: Arc<StdMutex<Vec<ProgressNotificationParam>>>,
    }

    impl ClientHandler for TestClient {
        fn on_progress(
            &self,
            params: ProgressNotificationParam,
            _context: NotificationContext<RoleClient>,
        ) -> impl Future<Output = ()> + MaybeSendFuture + '_ {
            if let Ok(mut sink) = self.progress.lock() {
                sink.push(params);
            }
            ready(())
        }
    }

    /// Wires a fresh `OxcodeServer` to a `TestClient` over an in-memory duplex
    /// pipe and returns the connected client service.
    async fn connect(
        progress: Arc<StdMutex<Vec<ProgressNotificationParam>>>,
    ) -> RunningService<RoleClient, TestClient> {
        let (server_transport, client_transport) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let server = OxcodeServer::new()
                .serve(server_transport)
                .await
                .expect("server serve");
            let _ = server.waiting().await;
        });
        TestClient { progress }
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

    /// Waits (bounded) for the progress sink to hold at least `n` notifications,
    /// since the client dispatches them asynchronously.
    async fn wait_for_progress(progress: &Arc<StdMutex<Vec<ProgressNotificationParam>>>, n: usize) {
        for _ in 0..100 {
            if progress.lock().expect("lock").len() >= n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Polls `tasks/get` until the task reaches a terminal status (or times out),
    /// returning the last observed status.
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

    #[tokio::test]
    async fn lists_tools_with_task_support() {
        let client = connect(Arc::default()).await;
        let tools = client.list_all_tools().await.expect("list tools");

        let index = tools
            .iter()
            .find(|tool| tool.name == "oxcode_index")
            .expect("oxcode_index is registered");
        assert!(
            index.input_schema.get("required").is_none(),
            "oxcode_index path is optional, so the schema has no required fields"
        );

        let task_support = |name: &str| {
            tools
                .iter()
                .find(|tool| tool.name == name)
                .and_then(|tool| tool.execution.as_ref())
                .and_then(|execution| execution.task_support)
        };
        assert_eq!(task_support("oxcode_index"), Some(TaskSupport::Optional));
        assert_eq!(task_support("oxcode_explore"), Some(TaskSupport::Optional));
        // Read-only query tools are not task-augmentable.
        assert_eq!(task_support("oxcode_search"), None);
        assert_eq!(task_support("oxcode_status"), None);
    }

    #[tokio::test]
    async fn indexes_then_explores_through_mcp() {
        let project = rust_project();
        let path = project.path().to_string_lossy().into_owned();
        let client = connect(Arc::default()).await;

        // Synchronous (non-task) build returns IndexStats directly.
        let indexed = client
            .call_tool(tool_call(
                "oxcode_index",
                serde_json::json!({ "path": path }),
            ))
            .await
            .expect("index call");
        assert_ne!(indexed.is_error, Some(true), "index succeeded");
        let stats: serde_json::Value =
            serde_json::from_str(result_text(&indexed)).expect("IndexStats json");
        assert!(stats["files"].as_u64().unwrap_or(0) >= 1, "indexed a file");
        assert!(stats["symbols"].as_u64().unwrap_or(0) >= 2, "found symbols");

        // The freshly built index is queryable in the same session (cache evicted).
        let explored = client
            .call_tool(tool_call(
                "oxcode_explore",
                serde_json::json!({ "path": path, "query": "entry" }),
            ))
            .await
            .expect("explore call");
        assert_ne!(explored.is_error, Some(true), "explore succeeded");
        assert!(
            result_text(&explored).contains("entry"),
            "explore surfaces the `entry` symbol"
        );
    }

    #[tokio::test]
    async fn index_emits_stage_progress() {
        let project = rust_project();
        let path = project.path().to_string_lossy().into_owned();
        let progress = Arc::new(StdMutex::new(Vec::new()));
        let client = connect(Arc::clone(&progress)).await;

        // A cancellable request always carries a progress token, so the server
        // streams stage notifications back to `TestClient::on_progress`.
        let params = tool_call("oxcode_index", serde_json::json!({ "path": path }));
        let handle = client
            .send_cancellable_request(
                ClientRequest::CallToolRequest(Request::new(params)),
                PeerRequestOptions::no_options(),
            )
            .await
            .expect("send index");
        handle.await_response().await.expect("index response");

        wait_for_progress(&progress, 4).await;
        let captured = progress.lock().expect("lock").clone();
        let messages: Vec<_> = captured
            .iter()
            .filter_map(|notification| notification.message.clone())
            .collect();
        assert_eq!(
            messages,
            [
                "scanning sources",
                "extracting symbols",
                "resolving references",
                "reconciling database",
            ]
        );
        let steps: Vec<f64> = captured.iter().map(|n| n.progress).collect();
        assert_eq!(steps, [1.0, 2.0, 3.0, 4.0], "progress increases 1..=4");
        assert!(
            captured.iter().all(|n| n.total == Some(4.0)),
            "every step reports a total of 4"
        );
    }

    #[tokio::test]
    async fn task_augmented_index_completes() {
        let project = rust_project();
        let path = project.path().to_string_lossy().into_owned();
        let client = connect(Arc::default()).await;

        // Task-augment the call: typed `call_tool` cannot carry a task field, so
        // send the request directly and expect an immediate CreateTaskResult.
        let mut params = tool_call("oxcode_index", serde_json::json!({ "path": path }));
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

        // Poll tasks/get to a terminal status before fetching the result
        // (rmcp's tasks/result destructively consumes the completed entry).
        let status = poll_until_terminal(&client, &task_id).await;
        assert_eq!(status, TaskStatus::Completed, "task ran to completion");

        // tasks/result returns the deferred CallToolResult carrying the same
        // IndexStats a synchronous call would have produced. rmcp decodes the
        // payload (a serialized CallToolResult) straight into the matching
        // ServerResult::CallToolResult variant; accept the raw payload too.
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
        let stats: serde_json::Value = serde_json::from_str(&text).expect("IndexStats json");
        assert!(
            stats["symbols"].as_u64().unwrap_or(0) >= 2,
            "deferred result carries IndexStats"
        );
    }
}

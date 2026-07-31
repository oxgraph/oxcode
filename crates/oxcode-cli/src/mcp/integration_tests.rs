//! In-process MCP integration tests for OxcodeServer.

//! In-process integration tests: a real `OxcodeServer` and an MCP client wired
//! over `tokio::io::duplex`, exercising the full JSON-RPC stack. These cover
//! tool registration, writer election + the read path, auto re-index on change,
//! and the task lifecycle. The cross-process guarantee is proven separately by
//! `tests/multiprocess.rs` (real spawned processes).

use std::{sync::MutexGuard, time::Duration};

use rmcp::{
    ClientHandler, RoleClient, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ClientCapabilities, ClientInfo,
        GetTaskParams, Implementation, ResultType, TaskPayload, TaskStatus,
    },
    service::RunningService,
};

use super::{
    project_root::{PROJECT_ROOT_ENV_LOCK, canonicalize_root, oxcode_root_override},
    *,
};

/// Env keys consulted for omitted-`path` defaults, for save/restore around tests.
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

/// MCP client that declares the Tasks extension.
#[derive(Clone, Default)]
struct TasksClient;

impl ClientHandler for TasksClient {
    fn get_info(&self) -> ClientInfo {
        ClientInfo::new(
            ClientCapabilities::builder().enable_tasks().build(),
            Implementation::from_build_env(),
        )
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

/// Wires a fresh `OxcodeServer` to a tasks-capable client.
async fn connect_with_tasks(
    debounce: Duration,
    poll: Duration,
) -> RunningService<RoleClient, TasksClient> {
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    tokio::spawn(async move {
        let server = OxcodeServer::new_with(debounce, poll)
            .serve(server_transport)
            .await
            .expect("server serve");
        let _ = server.waiting().await;
    });
    TasksClient
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
    CallToolRequestParams::new(name)
        .with_arguments(serde_json::from_value(arguments).expect("tool arguments object"))
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
async fn watch(client: &RunningService<RoleClient, TestClient>, path: &str) -> serde_json::Value {
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

#[test]
fn oxcode_root_override_wins_over_host_env() {
    let project = tempfile::TempDir::new().expect("temp");
    let host = tempfile::TempDir::new().expect("host");
    let guard = ProjectRootEnvGuard::clear();
    guard.set("CLAUDE_PROJECT_DIR", host.path());
    guard.set("OXCODE_ROOT", project.path());
    let resolved = oxcode_root_override().expect("OXCODE_ROOT");
    assert_eq!(
        canonicalize_root(resolved),
        canonicalize_root(project.path().to_path_buf())
    );
}

#[tokio::test]
async fn omitted_path_uses_claude_project_dir() {
    let project = rust_project();
    let guard = ProjectRootEnvGuard::clear();
    guard.set("CLAUDE_PROJECT_DIR", project.path());
    let client = connect(Duration::from_millis(50), Duration::from_millis(150)).await;

    let result = client
        .call_tool(tool_call("oxcode_watch", serde_json::json!({})))
        .await
        .expect("watch without path");
    let body: serde_json::Value = serde_json::from_str(result_text(&result)).expect("watch json");
    assert_eq!(body["role"], "writer");
    assert_eq!(
        canonicalize_root(PathBuf::from(body["root"].as_str().expect("root"))),
        canonicalize_root(project.path().to_path_buf()),
        "omitted path must use CLAUDE_PROJECT_DIR, not the server process cwd"
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
async fn lists_tools_and_server_supports_tasks() {
    let client = connect(DEFAULT_DEBOUNCE, DEFAULT_POLL).await;
    let tools = client.list_all_tools().await.expect("list tools");

    assert!(
        tools.iter().any(|tool| tool.name == "oxcode_watch"),
        "oxcode_watch is registered"
    );
    assert!(
        tools.iter().any(|tool| tool.name == "oxcode_explore"),
        "oxcode_explore is registered"
    );
    assert!(
        tools.iter().all(|tool| tool.name != "oxcode_index"),
        "the old write tool is gone"
    );

    let server_info = client
        .peer_info()
        .expect("server handshake info after connect");
    assert!(
        server_info.capabilities.supports_tasks(),
        "server advertises the Tasks extension"
    );
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
    let client = connect_with_tasks(Duration::from_millis(50), Duration::from_millis(150)).await;

    let response = client
        .call_tool_once(tool_call(
            "oxcode_watch",
            serde_json::json!({ "path": path }),
        ))
        .await
        .expect("enqueue task");
    let create = match response {
        CallToolResponse::Task(create) => create,
        other => panic!("expected CreateTaskResult, got {other:?}"),
    };
    assert_eq!(create.result_type, ResultType::TASK);
    let task_id = create.task.task_id.clone();

    let final_task = loop {
        tokio::time::sleep(Duration::from_millis(
            create.task.poll_interval_ms.unwrap_or(100),
        ))
        .await;
        let info = client
            .peer()
            .get_task(GetTaskParams::new(task_id.clone()))
            .await
            .expect("tasks/get");
        if info.task.status().is_terminal() {
            break info.task;
        }
    };
    assert_eq!(final_task.status(), TaskStatus::Completed);

    let text = match final_task.payload {
        TaskPayload::Completed { result } => {
            let result: CallToolResult =
                serde_json::from_value(serde_json::Value::Object(result)).expect("tool result");
            result_text(&result).to_owned()
        }
        other => panic!("expected completed task, got {other:?}"),
    };
    assert!(
        text.contains("writer"),
        "deferred watch result reports the elected writer role"
    );
}

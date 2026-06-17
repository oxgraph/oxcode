//! Real multi-process end-to-end test for the lock-elected file watcher.
//!
//! This spawns several actual `oxcode mcp` processes against one shared temp
//! repository and drives each as an MCP client over its stdio. It is the only
//! test that proves the cross-process guarantee — a single in-process server
//! cannot: exactly one process is elected writer (it holds `.oxcode/watch.lock`
//! and re-indexes), the others serve reads, and when the writer exits a standby
//! takes over. Every assertion is observed over MCP (`oxcode_watch` role +
//! `oxcode_status`), never by scraping logs.

use std::{path::Path, time::Duration};

use rmcp::{
    RoleClient, ServiceExt,
    model::{CallToolRequestParams, CallToolResult},
    service::RunningService,
    transport::{ConfigureCommandExt, TokioChildProcess},
};

/// A no-op MCP client; the spawned servers are what we exercise.
#[derive(Clone, Default)]
struct Probe;

impl rmcp::ClientHandler for Probe {}

type Client = RunningService<RoleClient, Probe>;

/// Spawns a real `oxcode mcp` process with fast watcher intervals and connects a
/// client to it over stdio. Auto-update is disabled so it never re-execs.
async fn spawn_server(bin: &Path) -> Client {
    let command = tokio::process::Command::new(bin).configure(|cmd| {
        cmd.arg("mcp")
            .env("OXCODE_NO_AUTO_UPDATE", "1")
            .env("OXCODE_WATCH_DEBOUNCE_MS", "40")
            .env("OXCODE_WATCH_POLL_MS", "150");
    });
    Probe
        .serve(TokioChildProcess::new(command).expect("spawn oxcode mcp"))
        .await
        .expect("connect client")
}

/// Writes a minimal two-function Rust project into a fresh temp dir and returns
/// the dir plus its canonical path string (all tool calls target this `path`,
/// exercising the "mcp launched outside the folder" case).
fn rust_project() -> (tempfile::TempDir, String) {
    let temp = tempfile::TempDir::new().expect("temp dir");
    std::fs::create_dir_all(temp.path().join("src")).expect("mkdir src");
    std::fs::write(
        temp.path().join("src/lib.rs"),
        "pub fn helper() {}\npub fn entry() {\n    helper();\n}\n",
    )
    .expect("write lib.rs");
    let path = std::fs::canonicalize(temp.path())
        .expect("canonicalize")
        .to_string_lossy()
        .into_owned();
    (temp, path)
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

/// Calls a tool and parses its JSON body.
async fn call(client: &Client, name: &'static str, args: serde_json::Value) -> serde_json::Value {
    let mut params = CallToolRequestParams::new(name);
    params.arguments = args.as_object().cloned();
    let result = client.call_tool(params).await.expect("tool call");
    serde_json::from_str(result_text(&result)).expect("tool json")
}

/// Calls `oxcode_watch` and returns the elected role.
async fn watch_role(client: &Client, path: &str) -> String {
    call(client, "oxcode_watch", serde_json::json!({ "path": path })).await["role"]
        .as_str()
        .expect("role")
        .to_owned()
}

/// Returns this process's watch state for `path` as `(role, reindexes)`.
async fn watch_state(client: &Client, path: &str) -> (String, u64) {
    let status = call(client, "oxcode_status", serde_json::json!({ "path": path })).await;
    let role = status["watch"]["role"].as_str().expect("role").to_owned();
    let reindexes = status["watch"]["reindexes"].as_u64().unwrap_or(0);
    (role, reindexes)
}

/// Whether an `oxcode_search` for `name` returns an exact-named match (keyword
/// search is fuzzy, so "any match" would false-positive).
async fn search_finds(client: &Client, path: &str, name: &str) -> bool {
    let report = call(
        client,
        "oxcode_search",
        serde_json::json!({ "path": path, "query": name }),
    )
    .await;
    report["matches"]
        .as_array()
        .is_some_and(|matches| matches.iter().any(|entry| entry["symbol"]["name"] == name))
}

/// Bounded poll: true once `client` can search up `name` (≤ ~12s).
async fn poll_search_finds(client: &Client, path: &str, name: &str) -> bool {
    for _ in 0..120 {
        if search_finds(client, path, name).await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

/// Index of the client currently reporting the `writer` role, if any.
async fn writer_index(clients: &[Client], path: &str) -> Option<usize> {
    for (index, client) in clients.iter().enumerate() {
        if watch_state(client, path).await.0 == "writer" {
            return Some(index);
        }
    }
    None
}

#[tokio::test]
async fn single_writer_election_reads_and_failover() {
    let bin = assert_cmd::cargo::cargo_bin("oxcode");
    let (_project, path) = rust_project();

    // Three independent `oxcode mcp` processes against the same repo.
    let mut clients = vec![
        spawn_server(&bin).await,
        spawn_server(&bin).await,
        spawn_server(&bin).await,
    ];

    // 1. Single-writer election: each calls oxcode_watch; exactly one is writer.
    let mut writer = None;
    for (index, client) in clients.iter().enumerate() {
        let role = watch_role(client, &path).await;
        if role == "writer" {
            assert!(writer.is_none(), "a second writer was elected");
            writer = Some(index);
        } else {
            assert_eq!(
                role, "standby",
                "non-writers that called watch are standbys"
            );
        }
    }
    let writer = writer.expect("exactly one process was elected writer");

    // 2. Every process can read (the writer built the shared index).
    for client in &clients {
        assert!(
            poll_search_finds(client, &path, "entry").await,
            "every instance reads the shared index"
        );
    }

    // 3. Only the writer re-indexes; all instances see the change. Settle first so the writer's
    //    FS-event stream is established before the edit.
    tokio::time::sleep(Duration::from_millis(500)).await;
    std::fs::write(
        std::path::Path::new(&path).join("src/extra.rs"),
        "pub fn brand_new_symbol() {}\n",
    )
    .expect("write extra.rs");

    for client in &clients {
        assert!(
            poll_search_finds(client, &path, "brand_new_symbol").await,
            "every instance reflects the writer's re-index"
        );
    }
    for (index, client) in clients.iter().enumerate() {
        let (role, reindexes) = watch_state(client, &path).await;
        if index == writer {
            assert_eq!(role, "writer");
            assert!(
                reindexes >= 2,
                "writer ran the initial build plus the change"
            );
        } else {
            assert_eq!(role, "standby");
            assert_eq!(reindexes, 0, "standbys never re-index");
        }
    }

    // 4. Failover: tear the writer down; a standby must take over and re-index.
    let writer_client = clients.remove(writer);
    writer_client.cancel().await.ok(); // child exits → OS frees watch.lock

    // A surviving standby becomes the new writer within a few poll intervals.
    let mut promoted = None;
    for _ in 0..120 {
        promoted = writer_index(&clients, &path).await;
        if promoted.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let promoted = promoted.expect("a standby was promoted to writer after the writer exited");

    // The new writer keeps the index current: a fresh edit propagates.
    tokio::time::sleep(Duration::from_millis(500)).await;
    std::fs::write(
        std::path::Path::new(&path).join("src/more.rs"),
        "pub fn later_symbol() {}\n",
    )
    .expect("write more.rs");
    assert!(
        poll_search_finds(&clients[promoted], &path, "later_symbol").await,
        "the promoted writer re-indexes new changes"
    );

    for client in clients {
        client.cancel().await.ok();
    }
}

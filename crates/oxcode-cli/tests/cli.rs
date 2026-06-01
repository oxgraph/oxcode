use std::{fs, path::Path};

use assert_cmd::Command;
use predicates::str::contains;
use serde_json::Value;
use tempfile::TempDir;

#[test]
fn cli_indexes_statuses_queries_and_explains_a_rust_project() {
    let temp = rust_project();
    let root = temp.path().to_str().expect("utf8 path");

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["index", root])
        .assert()
        .success()
        .stdout(contains("indexed"))
        .stdout(contains("index.oxgdb"));

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["status", root])
        .assert()
        .success()
        .stdout(contains("database exists"))
        .stdout(contains("calls"));

    Command::cargo_bin("oxcode")
        .expect("binary")
        .arg("languages")
        .assert()
        .success()
        .stdout(contains("rust"));

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["symbols", "helper", "--path", root])
        .assert()
        .success()
        .stdout(contains("element:"))
        .stdout(contains("helper"))
        .stdout(contains("score="));

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["symbols", "serve connection", "--json", "--path", root])
        .assert()
        .success()
        .stdout(contains("\"query\": \"serve connection\""))
        .stdout(contains("\"score\""))
        .stdout(contains("serve_connection"));

    let helper_symbols = oxcode_json(["symbols", "helper", "--json", "--path", root]);
    let helper = &helper_symbols["matches"][0]["symbol"];
    assert_eq!(helper["name"], "helper");
    assert!(
        helper["signature"]
            .as_str()
            .is_some_and(|text| text.contains("pub fn helper"))
    );
    assert!(
        helper["source_preview"]
            .as_str()
            .is_some_and(|text| text.contains("pub fn helper"))
    );

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args([
            "query",
            "MATCH ELEMENTS WHERE qualified_name = 'entry'",
            "--path",
            root,
        ])
        .assert()
        .success()
        .stdout(contains("values"));

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["symbol", "entry", "--path", root])
        .assert()
        .success()
        .stdout(contains("symbol element:"))
        .stdout(contains("entry"))
        .stdout(contains("defined at src/lib.rs"));

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["symbol", "entry", "--json", "--path", root])
        .assert()
        .success()
        .stdout(contains("\"status\": \"matched\""))
        .stdout(contains("\"signature\""))
        .stdout(contains("\"source_preview\""));

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["calls", "entry", "--path", root])
        .assert()
        .success()
        .stdout(contains("walk calls direction=outgoing"))
        .stdout(contains("helper"))
        .stdout(contains("expression helper()"));

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["calls", "entry", "--json", "--path", root])
        .assert()
        .success()
        .stdout(contains("\"edges\""))
        .stdout(contains("\"signature\""))
        .stdout(contains("\"helper\""));

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["callers", "helper", "--path", root])
        .assert()
        .success()
        .stdout(contains("walk calls direction=incoming"))
        .stdout(contains("entry"));

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["walk", "helper", "--direction", "both", "--path", root])
        .assert()
        .success()
        .stdout(contains("walk calls direction=both"))
        .stdout(contains("entry"));

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args([
            "query",
            "MATCH RELATIONS TYPE calls",
            "--expand",
            "--path",
            root,
        ])
        .assert()
        .success()
        .stdout(contains("calls element:"))
        .stdout(contains("called from src/lib.rs"))
        .stdout(contains("expression helper()"));

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args([
            "context",
            "How does entry reach helper?",
            "--json",
            "--path",
            root,
        ])
        .assert()
        .success()
        .stdout(contains("\"entry_points\""))
        .stdout(contains("\"relationships\""))
        .stdout(contains("\"files\""))
        .stdout(contains("\"entry\""))
        .stdout(contains("\"helper\""))
        .stdout(contains("\"calls\""));

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["files", "lib", "--json", "--path", root])
        .assert()
        .success()
        .stdout(contains("\"files\""))
        .stdout(contains("src/lib.rs"));

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["explain", "MATCH ELEMENTS", "--path", root])
        .assert()
        .success()
        .stdout(contains("scan elements"));

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["query", "plain english words", "--path", root])
        .assert()
        .failure()
        .stderr(contains(
            "query expects OxQL/Cypher; use oxcode symbols for keyword discovery",
        ));
}

#[test]
fn symbol_search_ranking_prefers_production_and_respects_kind_filters() {
    let temp = ranking_project();
    let root = temp.path().to_str().expect("utf8 path");

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["index", root])
        .assert()
        .success();

    let report = oxcode_json([
        "symbols",
        "resolve dependencies build",
        "--kind",
        "function",
        "--json",
        "--path",
        root,
    ]);
    let first = &report["matches"][0]["symbol"];
    assert_eq!(first["kind"], "function");
    assert_eq!(first["definition"]["file_path"], "src/lib.rs");
    assert_eq!(first["name"], "resolve_dependencies_build");

    let test_report = oxcode_json([
        "symbols",
        "resolve dependencies build test",
        "--kind",
        "function",
        "--json",
        "--path",
        root,
    ]);
    let test_first = &test_report["matches"][0]["symbol"];
    assert_eq!(test_first["definition"]["file_path"], "tests/build.rs");

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["symbols", "anything", "--kind", "bogus", "--path", root])
        .assert()
        .failure()
        .stderr(contains("unknown node kind bogus"));
}

#[test]
fn selector_discovery_outcomes_are_agent_safe() {
    let temp = ambiguous_project();
    let root = temp.path().to_str().expect("utf8 path");

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["index", root])
        .assert()
        .success();

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["symbol", "name:entry", "--json", "--path", root])
        .assert()
        .success()
        .stdout(contains("\"status\": \"ambiguous\""))
        .stdout(contains("one::entry"))
        .stdout(contains("two::entry"));

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["symbol", "name:Missing", "--json", "--path", root])
        .assert()
        .success()
        .stdout(contains("\"status\": \"not_found\""));

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["calls", "name:entry", "--json", "--path", root])
        .assert()
        .success()
        .stdout(contains("\"status\": \"ambiguous\""))
        .stdout(contains("one::entry"))
        .stdout(contains("two::entry"));
}

fn rust_project() -> TempDir {
    let temp = TempDir::new().expect("temp dir");
    write(
        temp.path().join("src/lib.rs"),
        r#"
/// Helps entry perform work.
pub fn helper() {}

pub fn serve_connection() {}

pub fn entry() {
    helper();
}
"#,
    );
    temp
}

fn ranking_project() -> TempDir {
    let temp = TempDir::new().expect("temp dir");
    write(
        temp.path().join("src/lib.rs"),
        r#"
/// Resolve dependencies and build the selected package.
pub fn resolve_dependencies_build() {}

pub mod package {
    pub fn broad_build_module() {}
}
"#,
    );
    write(
        temp.path().join("tests/build.rs"),
        r#"
pub fn test_resolve_dependencies_build() {}
"#,
    );
    temp
}

fn ambiguous_project() -> TempDir {
    let temp = TempDir::new().expect("temp dir");
    write(
        temp.path().join("src/lib.rs"),
        r#"
pub mod one {
    pub fn entry() {}
}

pub mod two {
    pub fn entry() {}
}
"#,
    );
    temp
}

fn write(path: impl AsRef<Path>, contents: &str) {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, contents).expect("write file");
}

fn oxcode_json<const N: usize>(args: [&str; N]) -> Value {
    let output = Command::cargo_bin("oxcode")
        .expect("binary")
        .args(args)
        .output()
        .expect("run oxcode");
    assert!(
        output.status.success(),
        "oxcode failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("json")
}

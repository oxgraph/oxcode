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
        .args(["index", "--path", root])
        .assert()
        .success()
        .stdout(contains("indexed"))
        .stdout(contains("index.oxgdb"));

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["status", "--path", root])
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
            "MATCH ELEMENTS WHERE qualified_name = 'crate::entry'",
            "--path",
            root,
        ])
        .assert()
        .success()
        .stdout(contains("values"));

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["symbol", "crate::entry", "--path", root])
        .assert()
        .success()
        .stdout(contains("symbol element:"))
        .stdout(contains("entry"))
        .stdout(contains("defined at src/lib.rs"));

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["symbol", "crate::entry", "--json", "--path", root])
        .assert()
        .success()
        .stdout(contains("\"status\": \"matched\""))
        .stdout(contains("\"signature\""))
        .stdout(contains("\"source_preview\""));

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["calls", "crate::entry", "--path", root])
        .assert()
        .success()
        .stdout(contains("walk calls direction=outgoing"))
        .stdout(contains("helper"))
        .stdout(contains("expression helper()"));

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["calls", "crate::entry", "--json", "--path", root])
        .assert()
        .success()
        .stdout(contains("\"status\": \"matched\""))
        .stdout(contains("\"edges\""))
        .stdout(contains("\"signature\""))
        .stdout(contains("\"helper\""));

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["callers", "crate::helper", "--path", root])
        .assert()
        .success()
        .stdout(contains("walk calls direction=incoming"))
        .stdout(contains("entry"));

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args([
            "walk",
            "crate::helper",
            "--direction",
            "both",
            "--path",
            root,
        ])
        .assert()
        .success()
        .stdout(contains("walk calls direction=both"))
        .stdout(contains("entry"));

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args([
            "query",
            "MATCH RELATIONS TYPE calls",
            "--format",
            "expand",
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
        .stdout(contains("\"symbols\""))
        .stdout(contains("\"relationships\""))
        .stdout(contains("\"budget\""))
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

    // A compact table is one output mode among the `--format` enum.
    Command::cargo_bin("oxcode")
        .expect("binary")
        .args([
            "query",
            "MATCH ELEMENTS WHERE qualified_name = 'crate::entry'",
            "--format",
            "table",
            "--path",
            root,
        ])
        .assert()
        .success()
        .stdout(contains("element:"));

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["query", "plain english words", "--path", root])
        .assert()
        .failure()
        .stderr(contains(
            "query expects OxQL; use oxcode symbols for keyword discovery",
        ));
}

#[test]
fn cli_rejects_invalid_value_enums() {
    // Output format and direction are value enums, so a bad value is a hard
    // error rather than a silently-ignored flag.
    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["query", "MATCH ELEMENTS", "--format", "bogus"])
        .assert()
        .failure();

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["walk", "crate::entry", "--direction", "sideways"])
        .assert()
        .failure();
}

#[test]
fn symbol_search_ranking_prefers_production_and_respects_kind_filters() {
    let temp = ranking_project();
    let root = temp.path().to_str().expect("utf8 path");

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["index", "--path", root])
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
        .stderr(contains("invalid symbol kind"));
}

#[test]
fn selector_discovery_outcomes_are_agent_safe() {
    let temp = ambiguous_project();
    let root = temp.path().to_str().expect("utf8 path");

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["index", "--path", root])
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

#[test]
fn cli_context_respects_the_byte_budget_and_dedupes_symbols() {
    let temp = rust_project();
    let root = temp.path().to_str().expect("utf8 path");
    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["index", "--path", root])
        .assert()
        .success();

    let report = oxcode_json([
        "context",
        "How does entry reach helper?",
        "--json",
        "--path",
        root,
        "--max-bytes",
        "200",
    ]);

    // The rendered source stays within the requested budget.
    let total = report["budget"]["total_chars"]
        .as_u64()
        .expect("total_chars");
    let max = report["budget"]["max_total_chars"]
        .as_u64()
        .expect("max_total_chars");
    assert_eq!(max, 200);
    assert!(total <= max, "budget overshoot: {total} > {max}");

    // The report is ID-keyed: each selected symbol appears exactly once.
    let symbols = report["symbols"].as_array().expect("symbols array");
    let mut ids = symbols
        .iter()
        .map(|symbol| symbol["id"].as_u64().expect("symbol id"))
        .collect::<Vec<_>>();
    let total_symbols = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), total_symbols, "duplicate symbol ids in report");
}

#[test]
fn cli_indexes_and_navigates_a_go_project() {
    let temp = go_project();
    let root = temp.path().to_str().expect("utf8 path");

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["index", "--path", root])
        .assert()
        .success()
        .stdout(contains("indexed"));

    // The Go extractor is registered and reported.
    Command::cargo_bin("oxcode")
        .expect("binary")
        .arg("languages")
        .assert()
        .success()
        .stdout(contains("go"));

    // Symbols are found with a Go signature.
    let helper_symbols = oxcode_json(["symbols", "Helper", "--json", "--path", root]);
    let helper = &helper_symbols["matches"][0]["symbol"];
    assert_eq!(helper["name"], "Helper");
    assert!(
        helper["signature"]
            .as_str()
            .is_some_and(|text| text.contains("func Helper"))
    );

    // A package-import-anchored qualified name resolves to its definition file.
    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["symbol", "example.com::m::db::Entry", "--path", root])
        .assert()
        .success()
        .stdout(contains("Entry"))
        .stdout(contains("defined at db/entry.go"));

    // The cross-file call edge `Entry -> Helper` is resolved.
    Command::cargo_bin("oxcode")
        .expect("binary")
        .args([
            "calls",
            "example.com::m::db::Entry",
            "--json",
            "--path",
            root,
        ])
        .assert()
        .success()
        .stdout(contains("\"status\": \"matched\""))
        .stdout(contains("\"Helper\""));
}

#[test]
fn cli_indexes_and_resolves_a_typescript_project() {
    let temp = typescript_project();
    let root = temp.path().to_str().expect("utf8 path");

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["index", "--path", root])
        .assert()
        .success()
        .stdout(contains("indexed"));

    Command::cargo_bin("oxcode")
        .expect("binary")
        .arg("languages")
        .assert()
        .success()
        .stdout(contains("typescript"));

    let helper_symbols = oxcode_json(["symbols", "helper", "--json", "--path", root]);
    let helper = &helper_symbols["matches"][0]["symbol"];
    assert_eq!(helper["name"], "helper");

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["symbol", "src::app::entry", "--path", root])
        .assert()
        .success()
        .stdout(contains("entry"))
        .stdout(contains("defined at src/app.ts"));

    // The path-based import `./util` resolves `entry`'s call to `helper`
    // across files.
    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["calls", "src::app::entry", "--json", "--path", root])
        .assert()
        .success()
        .stdout(contains("\"status\": \"matched\""))
        .stdout(contains("\"helper\""));
}

#[test]
fn cli_indexes_a_python_project_via_the_generic_extractor() {
    let temp = python_project();
    let root = temp.path().to_str().expect("utf8 path");

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["index", "--path", root])
        .assert()
        .success()
        .stdout(contains("indexed"));

    // The generic query-driven extractor registers Python.
    Command::cargo_bin("oxcode")
        .expect("binary")
        .arg("languages")
        .assert()
        .success()
        .stdout(contains("python"));

    let helper_symbols = oxcode_json(["symbols", "helper", "--json", "--path", root]);
    assert_eq!(helper_symbols["matches"][0]["symbol"]["name"], "helper");

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["symbol", "pkg::app::entry", "--path", root])
        .assert()
        .success()
        .stdout(contains("entry"))
        .stdout(contains("defined at pkg/app.py"));

    // Same-module call resolves via the scoped tier (no imports needed).
    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["calls", "pkg::app::entry", "--json", "--path", root])
        .assert()
        .success()
        .stdout(contains("\"status\": \"matched\""))
        .stdout(contains("\"helper\""));
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

fn python_project() -> TempDir {
    let temp = TempDir::new().expect("temp dir");
    write(temp.path().join("pkg/__init__.py"), "");
    write(
        temp.path().join("pkg/app.py"),
        "def helper():\n    return \"ready\"\n\n\ndef entry():\n    return helper()\n",
    );
    temp
}

fn typescript_project() -> TempDir {
    let temp = TempDir::new().expect("temp dir");
    write(
        temp.path().join("package.json"),
        "{\n  \"name\": \"smoke-ts\",\n  \"version\": \"0.0.0\"\n}\n",
    );
    write(
        temp.path().join("src/util.ts"),
        "export function helper(): string {\n  return \"ready\";\n}\n",
    );
    write(
        temp.path().join("src/app.ts"),
        "import { helper } from './util';\n\nexport function entry(): string {\n  return helper();\n}\n",
    );
    temp
}

fn go_project() -> TempDir {
    let temp = TempDir::new().expect("temp dir");
    write(
        temp.path().join("go.mod"),
        "module example.com/m\n\ngo 1.21\n",
    );
    write(
        temp.path().join("db/store.go"),
        "package db\n\n// Helper does the work.\nfunc Helper() string { return \"ready\" }\n",
    );
    write(
        temp.path().join("db/entry.go"),
        "package db\n\nfunc Entry() string { return Helper() }\n",
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

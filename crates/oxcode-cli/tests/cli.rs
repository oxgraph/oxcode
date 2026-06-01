use std::{fs, path::Path};

use assert_cmd::Command;
use predicates::str::contains;
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
        .args(["calls", "crate::entry", "--path", root])
        .assert()
        .success()
        .stdout(contains("walk calls direction=outgoing"))
        .stdout(contains("helper"))
        .stdout(contains("expression helper()"));

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["callers", "crate::helper", "--path", root])
        .assert()
        .success()
        .stdout(contains("walk calls direction=incoming"))
        .stdout(contains("entry"));

    Command::cargo_bin("oxcode")
        .expect("binary")
        .args(["walk", "crate::helper", "--direction", "both", "--path", root])
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

fn rust_project() -> TempDir {
    let temp = TempDir::new().expect("temp dir");
    write(
        temp.path().join("src/lib.rs"),
        r#"
pub fn helper() {}

pub fn entry() {
    helper();
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

use std::{fs, path::Path};

use oxcode_core::{
    ExpandedQueryValue, GraphDirection, IndexStats, OxElementId, OxQueryLanguage, OxQueryResult,
    OxQueryValue, ProjectIndex,
};
use tempfile::TempDir;

/// True when a query-expanded value is the untraversed `helper()` call edge:
/// query expansion does not traverse, so the edge's depth is absent.
fn is_untraversed_helper_call(value: &ExpandedQueryValue) -> bool {
    value.call_edge.as_ref().is_some_and(|edge| {
        edge.depth.is_none()
            && edge
                .call_site
                .as_ref()
                .is_some_and(|site| site.text == "helper()")
    })
}

#[test]
fn indexes_queries_and_traverses_with_native_oxgraph_database() {
    let temp = rust_project();

    let stats = oxcode_core::index_project(temp.path()).expect("index project");
    assert_index_stats(&stats);
    assert!(
        oxcode_core::project_status(temp.path())
            .expect("status")
            .database_exists
    );

    let index = ProjectIndex::open(temp.path()).expect("open index");

    let all = index
        .query(OxQueryLanguage::Oxql, "MATCH ELEMENTS")
        .expect("all");
    assert!(all.rows().len() >= 3);

    let entry = single_element_query(
        &index,
        "MATCH ELEMENTS WHERE qualified_name = 'crate::entry'",
    );
    let helper = single_element_query(
        &index,
        "MATCH ELEMENTS WHERE qualified_name = 'crate::helper'",
    );

    let functions = index
        .query(
            OxQueryLanguage::Oxql,
            "MATCH ELEMENTS WHERE kind = 'function'",
        )
        .expect("functions");
    assert!(functions.rows().len() >= 2);

    let outgoing = index
        .query(
            OxQueryLanguage::Oxql,
            &format!("GRAPH calls WALK FROM {} DEPTH 1", entry.get()),
        )
        .expect("outgoing");
    assert_eq!(element_rows(&outgoing), vec![helper]);

    let incoming = index
        .query(
            OxQueryLanguage::Oxql,
            &format!(
                "GRAPH calls WALK FROM {} DEPTH 1 DIRECTION incoming",
                helper.get()
            ),
        )
        .expect("incoming");
    assert_eq!(element_rows(&incoming), vec![entry]);

    let symbol = index.describe_symbol("crate::entry").expect("symbol");
    assert_eq!(symbol.symbol.qualified_name, "crate::entry");
    assert_eq!(symbol.symbol.definition.file_path, "src/lib.rs");
    assert_eq!(symbol.symbol.definition.span.start_line, 4);

    let calls = index
        .call_graph("crate::entry", GraphDirection::Outgoing, 1, 10)
        .expect("calls");
    assert_eq!(calls.seed.qualified_name, "crate::entry");
    assert!(calls.edges.iter().any(|edge| {
        edge.source.qualified_name == "crate::entry"
            && edge.target.qualified_name == "crate::helper"
            && edge.depth == Some(1)
            && edge.call_site.as_ref().is_some_and(|site| {
                site.location.file_path == "src/lib.rs" && site.text == "helper()"
            })
    }));

    let callers = index
        .call_graph("crate::helper", GraphDirection::Incoming, 1, 10)
        .expect("callers");
    assert!(callers.edges.iter().any(|edge| {
        edge.source.qualified_name == "crate::entry"
            && edge.target.qualified_name == "crate::helper"
    }));

    let both = index
        .call_graph("crate::helper", GraphDirection::Both, 1, 10)
        .expect("both");
    assert!(
        both.symbols
            .iter()
            .any(|symbol| symbol.symbol.qualified_name == "crate::entry" && symbol.depth == 1)
    );

    // Query and expansion run on one shared read snapshot.
    index
        .with_session(|session| {
            let relations = session.query(OxQueryLanguage::Oxql, "MATCH RELATIONS TYPE calls")?;
            let expanded = session.expand(&relations)?;
            assert!(
                expanded
                    .rows
                    .iter()
                    .any(|row| row.values.iter().any(is_untraversed_helper_call))
            );
            Ok(())
        })
        .expect("session");
}

#[test]
fn indexing_is_resilient_and_writes_a_gitignore() {
    let temp = TempDir::new().expect("temp dir");
    write(temp.path().join("src/lib.rs"), "pub fn good() {}\n");
    // A file with a syntax error must not abort the whole index.
    write(temp.path().join("src/broken.rs"), "fn oops( {\n");

    let stats = oxcode_core::index_project(temp.path()).expect("index still succeeds");

    // The healthy file is indexed; the broken one is recorded as partial.
    assert!(stats.symbols >= 1, "good symbols still indexed");
    assert!(stats.partial_files >= 1, "broken file recorded as partial");
    assert!(
        ProjectIndex::open(temp.path())
            .expect("open")
            .resolve_selector("crate::good")
            .expect("resolve")
            .iter()
            .any(|symbol| symbol.qualified_name == "crate::good")
    );

    // The generated index directory self-ignores so users do not commit it.
    let gitignore = temp.path().join(".oxcode/.gitignore");
    assert!(gitignore.exists(), "wrote .oxcode/.gitignore");
    assert!(
        fs::read_to_string(&gitignore)
            .expect("read gitignore")
            .contains('*')
    );
}

#[test]
fn indexing_persists_unresolved_references() {
    // A call with no matching definition must be persisted as an unresolved
    // diagnostic element (exercises the full unresolved-reference write path).
    let temp = TempDir::new().expect("temp dir");
    write(
        temp.path().join("src/lib.rs"),
        "pub fn entry() {\n    missing_external_function();\n}\n",
    );

    let stats = oxcode_core::index_project(temp.path()).expect("index succeeds");
    assert!(
        stats.unresolved_references >= 1,
        "the unknown call is retained as a diagnostic"
    );
    let status = oxcode_core::project_status(temp.path()).expect("status");
    assert_eq!(status.unresolved_references, stats.unresolved_references);
}

fn assert_index_stats(stats: &IndexStats) {
    assert_eq!(stats.files, 1);
    assert!(stats.symbols >= 3);
    assert!(stats.edges >= 2);
    assert_eq!(stats.skipped_unsupported_files, 0);
}

fn single_element_query(index: &ProjectIndex, query: &str) -> OxElementId {
    let result = index.query(OxQueryLanguage::Oxql, query).expect("query");
    let rows = element_rows(&result);
    assert_eq!(rows.len(), 1, "{query} returned {rows:?}");
    rows[0]
}

fn element_rows(result: &OxQueryResult) -> Vec<OxElementId> {
    result
        .rows()
        .iter()
        .filter_map(|row| match row.values.as_slice() {
            [OxQueryValue::Element(id)] => Some(*id),
            _ => None,
        })
        .collect()
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

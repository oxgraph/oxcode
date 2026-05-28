use std::{fs, path::Path};

use oxcode_core::{
    GraphDirection, IndexStats, OxElementId, OxQueryLanguage, OxQueryResult, OxQueryValue,
    call_graph, describe_symbol, expand_query_result, query_project,
};
use tempfile::TempDir;

#[test]
fn indexes_queries_and_traverses_with_native_oxgraph_database() {
    let temp = rust_project();
    seed_legacy_outputs(temp.path());

    let stats = oxcode_core::index_project(temp.path()).expect("index project");
    assert_index_stats(&stats);
    assert!(temp.path().join(".oxcode/index.oxgdb/store.oxgdb").exists());
    assert!(!temp.path().join(".oxcode/index.sqlite").exists());
    assert!(!temp.path().join(".oxcode/forward.oxgsnap").exists());
    assert!(!temp.path().join(".oxcode/reverse.oxgsnap").exists());

    let all = query_project(temp.path(), OxQueryLanguage::Oxql, "MATCH ELEMENTS").expect("all");
    assert!(all.rows().len() >= 3);

    let entry = single_element_query(temp.path(), "MATCH ELEMENTS WHERE qualified_name = 'entry'");
    let helper = single_element_query(
        temp.path(),
        "MATCH ELEMENTS WHERE qualified_name = 'helper'",
    );

    let functions = query_project(
        temp.path(),
        OxQueryLanguage::Oxql,
        "MATCH ELEMENTS WHERE kind = 'function'",
    )
    .expect("functions");
    assert!(functions.rows().len() >= 2);

    let outgoing = query_project(
        temp.path(),
        OxQueryLanguage::Oxql,
        &format!("GRAPH calls WALK FROM {} DEPTH 1", entry.get()),
    )
    .expect("outgoing");
    assert_eq!(element_rows(&outgoing), vec![helper]);

    let incoming = query_project(
        temp.path(),
        OxQueryLanguage::Oxql,
        &format!(
            "GRAPH calls WALK FROM {} DEPTH 1 DIRECTION incoming",
            helper.get()
        ),
    )
    .expect("incoming");
    assert_eq!(element_rows(&incoming), vec![entry]);

    let symbol = describe_symbol(temp.path(), "entry").expect("symbol");
    assert_eq!(symbol.symbol.qualified_name, "entry");
    assert_eq!(symbol.symbol.definition.file_path, "src/lib.rs");
    assert_eq!(symbol.symbol.definition.start_line, 4);

    let calls = call_graph(temp.path(), "entry", GraphDirection::Outgoing, 1, 10).expect("calls");
    assert_eq!(calls.seed.qualified_name, "entry");
    assert!(calls.edges.iter().any(|edge| {
        edge.source.qualified_name == "entry"
            && edge.target.qualified_name == "helper"
            && edge.call_site.as_ref().is_some_and(|site| {
                site.location.file_path == "src/lib.rs" && site.text == "helper()"
            })
    }));

    let callers =
        call_graph(temp.path(), "helper", GraphDirection::Incoming, 1, 10).expect("callers");
    assert!(callers.edges.iter().any(|edge| {
        edge.source.qualified_name == "entry" && edge.target.qualified_name == "helper"
    }));

    let both = call_graph(temp.path(), "helper", GraphDirection::Both, 1, 10).expect("both");
    assert!(
        both.symbols
            .iter()
            .any(|symbol| symbol.symbol.qualified_name == "entry" && symbol.depth == 1)
    );

    let call_relations = query_project(
        temp.path(),
        OxQueryLanguage::Oxql,
        "MATCH RELATIONS TYPE calls",
    )
    .expect("call relations");
    let expanded = expand_query_result(temp.path(), call_relations).expect("expanded");
    assert!(expanded.rows.iter().any(|row| {
        row.values.iter().any(|value| {
            value.call_edge.as_ref().is_some_and(|edge| {
                edge.call_site
                    .as_ref()
                    .is_some_and(|site| site.text == "helper()")
            })
        })
    }));
}

fn seed_legacy_outputs(root: &Path) {
    let index = root.join(".oxcode");
    fs::create_dir_all(&index).expect("legacy index dir");
    for file_name in ["index.sqlite", "forward.oxgsnap", "reverse.oxgsnap"] {
        fs::write(index.join(file_name), b"legacy").expect("legacy file");
    }
}

fn assert_index_stats(stats: &IndexStats) {
    assert_eq!(stats.files, 1);
    assert!(stats.symbols >= 3);
    assert!(stats.edges >= 2);
    assert_eq!(stats.skipped_unsupported_files, 0);
}

fn single_element_query(root: &Path, query: &str) -> OxElementId {
    let result = query_project(root, OxQueryLanguage::Oxql, query).expect("query");
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

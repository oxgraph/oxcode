use std::{fs, path::Path};

use oxcode_core::{
    ExpandedQueryValue, GraphDirection, IndexProgress, IndexStage, IndexStats, OxElementId,
    OxQueryResult, OxQueryValue, ProjectIndex,
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

    let all = index.query("MATCH ELEMENTS").expect("all");
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
        .query("MATCH ELEMENTS WHERE kind = 'function'")
        .expect("functions");
    assert!(functions.rows().len() >= 2);

    let outgoing = index
        .query(&format!("GRAPH calls WALK FROM {} DEPTH 1", entry.get()))
        .expect("outgoing");
    assert_eq!(element_rows(&outgoing), vec![helper]);

    let incoming = index
        .query(&format!(
            "GRAPH calls WALK FROM {} DEPTH 1 DIRECTION incoming",
            helper.get()
        ))
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
            let relations = session.query("MATCH RELATIONS TYPE calls")?;
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

#[test]
fn index_progress_reports_each_stage_in_order() {
    let temp = rust_project();

    // A cold index passes through all four stages, in order, 1..=4 of 4.
    let mut cold = Vec::new();
    oxcode_core::index_project_with_progress(temp.path(), |progress| cold.push(progress))
        .expect("cold index");
    assert_eq!(
        cold,
        vec![
            IndexProgress {
                stage: IndexStage::Scan,
                step: 1,
                total: 4
            },
            IndexProgress {
                stage: IndexStage::Extract,
                step: 2,
                total: 4
            },
            IndexProgress {
                stage: IndexStage::Resolve,
                step: 3,
                total: 4
            },
            IndexProgress {
                stage: IndexStage::Store,
                step: 4,
                total: 4
            },
        ]
    );

    // An unchanged re-index short-circuits after the digest check, so only the
    // Scan milestone fires before the cached stats are returned.
    let mut warm = Vec::new();
    oxcode_core::index_project_with_progress(temp.path(), |progress| warm.push(progress))
        .expect("warm index");
    assert_eq!(
        warm,
        vec![IndexProgress {
            stage: IndexStage::Scan,
            step: 1,
            total: 4
        }]
    );
}

fn assert_index_stats(stats: &IndexStats) {
    assert_eq!(stats.files, 1);
    assert!(stats.symbols >= 3);
    assert!(stats.edges >= 2);
    assert_eq!(stats.skipped_unsupported_files, 0);
}

fn single_element_query(index: &ProjectIndex, query: &str) -> OxElementId {
    let result = index.query(query).expect("query");
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

/// The `dependencies` section is the crate layer cake only: a crate→crate
/// dependency survives, and crate→module lifts (a crate-root file referencing a
/// submodule symbol) never pollute it.
#[test]
fn context_dependencies_are_crate_to_crate_only() {
    // Two crates where `a::foo` references `b::bar`: the real crate→crate edge.
    let two = TempDir::new().expect("temp dir");
    write(
        two.path().join("crates/a/Cargo.toml"),
        "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
    );
    write(
        two.path().join("crates/a/src/lib.rs"),
        "pub fn foo() {\n    b::bar();\n}\n",
    );
    write(
        two.path().join("crates/b/Cargo.toml"),
        "[package]\nname = \"b\"\nversion = \"0.1.0\"\n",
    );
    write(two.path().join("crates/b/src/lib.rs"), "pub fn bar() {}\n");
    oxcode_core::index_project(two.path()).expect("index two-crate workspace");
    let index = ProjectIndex::open(two.path()).expect("open");
    let report = index.context("foo", 8, 2, 20_000).expect("context");

    assert!(
        report
            .dependencies
            .iter()
            .any(|dep| dep.source.as_str() == "a" && dep.target.as_str() == "b"),
        "the crate→crate dependency a -> b must surface: {:?}",
        report.dependencies,
    );
    // Every endpoint is a bare crate name — no `::`-qualified module entries.
    for dep in &report.dependencies {
        assert!(
            !dep.source.as_str().contains("::") && !dep.target.as_str().contains("::"),
            "dependencies must be crate→crate only, found {} -> {}",
            dep.source,
            dep.target,
        );
    }

    // A single crate whose root references a submodule yields a crate→module
    // DependsOn internally; that noise must be filtered out of `dependencies`.
    let one = TempDir::new().expect("temp dir");
    write(
        one.path().join("src/lib.rs"),
        "pub mod gadget;\n\npub fn run() {\n    gadget::tick();\n}\n",
    );
    write(one.path().join("src/gadget.rs"), "pub fn tick() {}\n");
    oxcode_core::index_project(one.path()).expect("index single crate");
    let index = ProjectIndex::open(one.path()).expect("open");
    let report = index.context("run", 8, 2, 20_000).expect("context");
    assert!(
        report.dependencies.is_empty(),
        "a single crate has no crate→crate deps; crate→module noise must be \
         dropped, found {:?}",
        report.dependencies,
    );
}

/// Hyperedges surface the full crate→module→file→symbol containment lineage with
/// self-describing participants, not just leaf-level file membership.
#[test]
fn context_hyperedges_surface_named_containment_lineage() {
    use oxcode_core::{HyperedgeKind, NodeKind, ParticipantRole};

    let temp = TempDir::new().expect("temp dir");
    write(
        temp.path().join("src/lib.rs"),
        "pub mod gadget;\n\npub fn run() {\n    gadget::tick();\n}\n",
    );
    write(
        temp.path().join("src/gadget.rs"),
        "pub fn tick() {\n    helper();\n}\n\npub fn helper() {}\n",
    );
    oxcode_core::index_project(temp.path()).expect("index");
    let index = ProjectIndex::open(temp.path()).expect("open");
    let report = index.context("tick", 8, 2, 20_000).expect("context");

    // Every participant is self-describing (non-empty qualified name), and each
    // membership lists its anchor first.
    for hyperedge in &report.hyperedges {
        assert!(
            !hyperedge.participants.is_empty(),
            "hyperedge carries participants",
        );
        for participant in &hyperedge.participants {
            assert!(
                !participant.qualified_name.as_str().is_empty(),
                "participant {participant:?} must carry a qualified name",
            );
        }
        if hyperedge.kind == HyperedgeKind::Membership {
            assert_eq!(
                hyperedge.participants[0].role,
                ParticipantRole::Anchor,
                "membership renders the anchor first: {:?}",
                hyperedge.participants,
            );
        }
    }

    // The ancestor walk pulls in the module- and crate-level memberships, whose
    // anchors are container nodes that are not themselves selected symbols.
    let anchored_by = |kind: NodeKind| {
        report.hyperedges.iter().any(|hyperedge| {
            hyperedge.kind == HyperedgeKind::Membership
                && hyperedge
                    .participants
                    .iter()
                    .any(|p| p.role == ParticipantRole::Anchor && p.kind == kind)
        })
    };
    assert!(
        anchored_by(NodeKind::Package),
        "the crate (package) membership lineage must surface: {:?}",
        report.hyperedges,
    );
    assert!(
        anchored_by(NodeKind::Module),
        "the module membership lineage must surface: {:?}",
        report.hyperedges,
    );
}

# oxcode-core

OxGraph-native code indexing and navigation engine.

[![crates.io](https://img.shields.io/crates/v/oxcode-core.svg)](https://crates.io/crates/oxcode-core)
[![docs.rs](https://docs.rs/oxcode-core/badge.svg)](https://docs.rs/oxcode-core)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/oxgraph/oxcode/blob/main/LICENSE)

The engine behind [oxcode](https://github.com/oxgraph/oxcode), the tool that
indexes source code into a graph and serves it to coding agents.

## What it is

`oxcode-core` owns the indexing pipeline and the query/navigation engine.
Indexing runs in four stages: **scan** (discover source files and hash them
into a content digest), **extract** (parse sources into symbols and edges
with tree-sitter), **resolve** (turn cross-file references into graph
edges), and **store** (reconcile the resolved index into an
[oxgraph](https://github.com/oxgraph/oxgraph)-native database under
`.oxcode/index.oxgdb/` with stable identities, so re-indexing is
`O(change)`).

On the read side, the public `ProjectIndex` facade serves symbol search,
file search, selectors, call-graph navigation, and `context`: a bounded,
PageRank-curated, task-oriented context report. The report formatters that
render these for agents live here too; the CLI and MCP server in
`oxcode-cli` are thin wrappers over this crate.

## Where it sits

```text
oxcode-model                      shared vocabulary (kinds, schema, reports)
└── oxcode-core                 ← this crate (pipeline + ProjectIndex facade)
    └── oxcode-cli                the `oxcode` binary (CLI + MCP server)
```

## Example

```rust
use oxcode_core::{ProjectIndex, index_project};

// Build or refresh the index under .oxcode/index.oxgdb/.
let stats = index_project("./my-project")?;

// Open it and ask a task-oriented question.
let index = ProjectIndex::open("./my-project")?;
let report = index.context(
    "how does auth middleware work?",
    10,    // symbol limit
    2,     // graph depth
    8192,  // rendered-source byte cap
)?;
```

Most users want the `oxcode` binary instead of this library; install
`oxcode-cli` and see the
[oxcode README](https://github.com/oxgraph/oxcode#readme) for commands and
MCP setup.

## Documentation

See [docs.rs/oxcode-core](https://docs.rs/oxcode-core) for the full API and
the [oxcode README](https://github.com/oxgraph/oxcode#readme) for the
product: installation, supported languages, benchmarks, and architecture.

## License

MIT. See [LICENSE](https://github.com/oxgraph/oxcode/blob/main/LICENSE).

# oxcode

`oxcode` indexes Rust source into a native OxGraph database. It uses
tree-sitter for extraction, resolves code references into graph relations, and
stores the result in a native OxGraph database under `.oxcode/index.oxgdb/`.

The CLI keeps raw OxQL available, but agent navigation should usually start
with the symbol and call graph commands because they expand graph IDs back into
function names, definition ranges, and call-site source context.

## Quick Start

```sh
cargo run -p oxcode -- index --path path/to/rust/project
cargo run -p oxcode -- status --path path/to/rust/project
cargo run -p oxcode -- symbol crate::entry --path path/to/rust/project
cargo run -p oxcode -- calls crate::entry --depth 2 --path path/to/rust/project
cargo run -p oxcode -- callers crate::helper --depth 2 --path path/to/rust/project
cargo run -p oxcode -- query "MATCH ELEMENTS WHERE qualified_name = 'crate::entry'" --path path/to/rust/project
cargo run -p oxcode -- query "MATCH RELATIONS TYPE calls" --format expand --path path/to/rust/project
cargo run -p oxcode -- query "GRAPH calls WALK FROM 12 DEPTH 2 DIRECTION both LIMIT 100" --path path/to/rust/project
```

The generated `.oxcode/` directory writes its own `.gitignore`, so the index is
never committed by accident.

Useful selectors for navigation commands:

- `element:<id>` for a concrete OxGraph element ID
- an exact crate-qualified name such as `my_crate::auth::tenant_middleware`
  (qualified names are anchored at the crate, so the first segment is the
  package name with `-` normalized to `_`)
- `name:<name>` for a simple function name
- `file:<path>:<line>` for the innermost symbol covering a source line

## Architecture

The workspace uses a hybrid Rust architecture:

- `oxcode-model`: storage-neutral vocabulary shared across the workspace —
  code-graph kinds, identifier newtypes, the graph schema catalog, the selector
  grammar, the extraction/resolution IR, and agent-facing report DTOs
- `oxcode-core`: indexing, extraction, reference resolution, OxGraph storage,
  navigation, formatting, and the public `ProjectIndex` facade
- `oxcode`: thin CLI package and binary

`oxcode-core` is split into focused internal modules: `scan`, `extract` (with
per-language extractors and shared CST/cargo helpers), `resolve`,
`store::oxgraph` (with its `write` path), `format`, `paths`, and `error`. The
model crate's typed schema is the single source of truth that the storage layer
derives property registration, read-key caching, and indexes from. Reads run
through `ProjectIndex`, which opens the database once and resolves the
property-key schema; `ProjectIndex::with_session` runs several reads against one
shared snapshot so multi-step navigation stays internally consistent.

Rust is the only shipped extractor. There is no fallback extractor for other
languages.

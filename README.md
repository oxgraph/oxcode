# oxcode

`oxcode` indexes Rust source into a native OxGraph database. It uses
tree-sitter for extraction, resolves code references into graph relations, and
stores the result at `.oxcode/index.oxgdb/store.oxgdb`.

The CLI keeps raw OxQL available, but agent navigation should usually start
with `context`, `symbols`, `files`, and the call graph commands because they
expand graph IDs back into function names, definition ranges, signatures,
docstrings, source previews, and call-site source context.

## Quick Start

```sh
cargo run -p oxcode -- index path/to/rust/project
cargo run -p oxcode -- status path/to/rust/project
cargo run -p oxcode -- context "How does entry reach helper?" --path path/to/rust/project --limit 8 --json
cargo run -p oxcode -- symbols "entry helper" --path path/to/rust/project --limit 20 --json
cargo run -p oxcode -- symbols "entry helper" --path path/to/rust/project --kind function --kind method
cargo run -p oxcode -- files "runtime scheduler" --path path/to/rust/project --limit 20 --json
cargo run -p oxcode -- symbol entry --path path/to/rust/project --json
cargo run -p oxcode -- calls entry --depth 2 --path path/to/rust/project
cargo run -p oxcode -- callers helper --depth 2 --path path/to/rust/project
cargo run -p oxcode -- query "MATCH ELEMENTS WHERE qualified_name = 'entry'" --path path/to/rust/project
cargo run -p oxcode -- query "MATCH RELATIONS TYPE calls" --expand --path path/to/rust/project
cargo run -p oxcode -- query "GRAPH calls WALK FROM 12 DEPTH 2 DIRECTION both LIMIT 100" --path path/to/rust/project
```

Useful selectors for navigation commands:

- `element:<id>` for a concrete OxGraph element ID
- an exact qualified name such as `auth::tenant_middleware`
- `name:<name>` for a simple function name
- `file:<path>:<line>` for the innermost symbol covering a source line

`symbols` accepts repeatable `--kind <kind>` filters. Valid kinds are:

- `file`, `module`, `namespace`, `package`, `class`, `struct`, `enum`,
  `trait`, `interface`, `impl_block`, `function`, `method`, `field`,
  `variable`, `constant`, `type_alias`, `macro`

`context` is deterministic and graph-derived. It ranks entry-point symbols for
the task text, then expands nearby `calls`, `contains`, `references`, and
`implements` relationships.

`query` and `explain` execute raw OxQL/Cypher. For keyword discovery, use
`symbols`; do not pass plain English phrases to `query`.

Accepted OxQL profile:

- `CATALOG`
- `MATCH ELEMENTS`
- `MATCH ELEMENTS HAS LABEL <label>`
- `MATCH ELEMENTS WHERE <property> = '<value>'`
- `MATCH RELATIONS TYPE <type>`
- `GRAPH calls WALK FROM <element-id> DEPTH <n> [DIRECTION outgoing|incoming|both] [LIMIT n]`

## Architecture

The workspace uses a hybrid Rust architecture:

- `oxcode-model`: storage-neutral model, IR, and report DTOs
- `oxcode-core`: indexing, extraction, resolution, OxGraph storage, navigation,
  formatting, and public facade APIs
- `oxcode`: thin CLI package and binary

`oxcode-core` is split into focused internal modules: `scan`, `extract`,
`resolve`, `store::oxgraph`, `nav`, `format`, `paths`, and `error`. OxGraph
schema names and property hydration live in `store::oxgraph`; navigation reads
through a typed `CodeGraphRead` trait.

Rust is the only shipped extractor. There is no fallback extractor for other
languages.

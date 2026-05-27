# oxcode

`oxcode` indexes Rust source into a native OxGraph database. It uses
tree-sitter for extraction, resolves code references into graph relations, and
stores the result at `.oxcode/index.oxgdb/store.oxgdb`.

## Quick Start

```sh
cargo run -p oxcode -- index path/to/rust/project
cargo run -p oxcode -- status path/to/rust/project
cargo run -p oxcode -- query "MATCH ELEMENTS WHERE qualified_name = 'entry'" --path path/to/rust/project
cargo run -p oxcode -- query "GRAPH calls WALK FROM 12 DEPTH 2 DIRECTION both LIMIT 100" --path path/to/rust/project
```

The CLI is OxQL-first. Convenience commands such as bespoke search, callees,
callers, and export are intentionally omitted; equivalent workflows query the
OxGraph database directly.

## Architecture

The workspace has a single `oxcode` package. Its internal modules handle:

- file discovery and Rust tree-sitter extraction
- language-neutral symbol/reference resolution
- full database rebuilds into `oxgraph::db`
- CLI commands for `index`, `query`, `explain`, `status`, and `languages`

Rust is the only shipped extractor. There is no fallback extractor for other
languages.

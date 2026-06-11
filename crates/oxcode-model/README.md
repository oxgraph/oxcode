# oxcode-model

Storage-neutral code graph model types for oxcode.

[![crates.io](https://img.shields.io/crates/v/oxcode-model.svg)](https://crates.io/crates/oxcode-model)
[![docs.rs](https://docs.rs/oxcode-model/badge.svg)](https://docs.rs/oxcode-model)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/oxgraph/oxcode/blob/main/LICENSE)

The shared vocabulary of [oxcode](https://github.com/oxgraph/oxcode), the
tool that indexes source code into a graph and serves it to coding agents.

## What it is

`oxcode-model` owns the typed vocabulary the rest of the workspace shares:

- the code-graph kinds (`NodeKind` / `EdgeKind`),
- the identifiers and newtypes (`SymbolId`, `SymbolKey`, …),
- the graph schema catalog (`ElementProperty` / `RelationProperty`), the
  single source of truth the storage layer derives its layout from,
- the selector grammar (`Selector`) used to address symbols,
- the extraction/resolution intermediate representation, and
- the agent-facing report types the CLI and MCP server render.

It is intentionally dependency-light (no storage or CLI dependencies) so
both the extractor/resolver and the storage layer can derive their behavior
from one definition. Stored strings parse back through generated
`FromStr`/`TryFrom<&str>` impls that surface schema drift loudly instead of
silently coercing.

## Where it sits

```text
oxcode-model                    ← this crate (shared vocabulary)
└── oxcode-core                   indexing pipeline + navigation engine
    └── oxcode-cli                the `oxcode` binary (CLI + MCP server)
```

The code graph itself is stored in an
[oxgraph](https://github.com/oxgraph/oxgraph)-native database; this crate
stays storage-neutral.

## Documentation

See [docs.rs/oxcode-model](https://docs.rs/oxcode-model) for the full API
and the [oxcode README](https://github.com/oxgraph/oxcode#readme) for the
product: installation, CLI commands, MCP setup, and supported languages.

## License

MIT. See [LICENSE](https://github.com/oxgraph/oxcode/blob/main/LICENSE).

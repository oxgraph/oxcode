# oxcode

`oxcode` indexes Rust source into a native OxGraph database. It uses
tree-sitter for extraction, resolves code references into graph relations, and
stores the result in a native OxGraph database under `.oxcode/index.oxgdb/`.

The CLI keeps raw OxQL available, but agent navigation should usually start
with `context`, `symbols`, `files`, and the call graph commands because they
expand graph IDs back into function names, definition ranges, signatures,
docstrings, source previews, and call-site source context.

## Quick Start

```sh
cargo run -p oxcode -- index --path path/to/rust/project
cargo run -p oxcode -- status --path path/to/rust/project
cargo run -p oxcode -- context "How does entry reach helper?" --path path/to/rust/project --limit 8 --json
cargo run -p oxcode -- symbols "entry helper" --path path/to/rust/project --limit 20 --json
cargo run -p oxcode -- symbols "entry helper" --path path/to/rust/project --kind function --kind method
cargo run -p oxcode -- files "runtime scheduler" --path path/to/rust/project --limit 20 --json
cargo run -p oxcode -- symbol crate::entry --path path/to/rust/project --json
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

## Benchmarks

Agent-task benchmark on the Tokio codebase: an agent answers *"How does tokio
schedule and run async tasks?"* with and without each tool, measuring efficiency
and blind-judged answer quality. oxcode and codegraph were measured on different
agent harnesses, so the comparable unit is each tool's improvement **vs its own
no-tool baseline**, not absolute numbers.

| arm | answer quality | tokens | cost | tool calls | wall time |
| --- | ---: | ---: | ---: | ---: | ---: |
| baseline (no tool) | 0.97 | — | — | — | — |
| **oxcode** — codex/gpt-5.5, CLI, n=6 | 0.97 (tied) | −5% | −19% | −23% | −10% |
| codegraph — Opus 4.8, MCP, published | not measured | −38% | even | −57% | −18% |

Percentages are reductions vs that tool's own no-tool baseline (lower is better;
quality is the blind LLM-judge score, higher is better). Absolute medians for our
two arms: tokens 431k → 410k, cost ~$0.19 → ~$0.16, shell commands 30 → 23, wall
104s → 93s, oxcode query p50 931 ms.

oxcode improves on every efficiency axis while **holding answer quality** — the
quality gate guards against "cheaper because the agent gave up sooner," which
codegraph's benchmark does not measure. codegraph's larger reductions come mostly
from its one-call MCP `codegraph_explore` tool versus oxcode's multi-command CLI;
closing that gap is a matter of tool delivery, not index quality. codegraph
numbers are from its README, re-validated 2026-06-02.

Full methodology, confidence intervals, and reproduction:
[`docs/agent-eval-results.md`](docs/agent-eval-results.md) and
[`docs/agent-eval-methodology.md`](docs/agent-eval-methodology.md).

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

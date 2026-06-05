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
| baseline (no tool) | 0.98 | — | — | — | — |
| oxcode — codex/gpt-5.5, CLI, n=6 | 0.96 (tied) | +15% | +4% | −4% | +14% |
| **oxcode — codex/gpt-5.5, MCP, n=6** | **0.93** | **−74%** | **−57%** | **−84%** | **−60%** |
| codegraph — Opus 4.8, MCP, published | not measured | −38% | even | −57% | −18% |

Percentages are change vs that tool's own no-tool baseline (negative = reduction,
better; quality is the blind LLM-judge score, 0–1). All oxcode rows come from one
n=6 release suite on Tokio. Absolute medians: tokens 395k (baseline) → 455k (CLI)
→ 104k (MCP); cost $0.17 → $0.18 → $0.07; tool calls 28 → 27 → 5; wall 97s → 111s
→ 39s.

**The MCP server is the headline.** Delivering the same bounded, PageRank-curated
context through a one-call `oxcode_explore` MCP tool — instead of a CLI the agent
composes — cuts tool calls 84%, tokens 74%, cost 57%, and wall 60% vs the no-tool
baseline, **exceeding codegraph's published reductions** (−57% tool calls / −38%
tokens). The CLI arm is statistically tied with the baseline: the agent treats a
shell binary as a supplement to its own grep/read, not a replacement — so the gap
was always **tool delivery, not index quality**. The one cost the quality gate
exposes (and a quality-blind benchmark like codegraph's would hide): MCP answer
quality dips to 0.93 vs 0.98, a completeness trade-off from the leaner
exploration. codegraph numbers are from its README, re-validated 2026-06-02.

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
- `oxcode-mcp`: MCP server (stdio) exposing oxcode's read-only queries to coding
  agents — the one-call `oxcode_explore` tool plus `oxcode_search`,
  `oxcode_callers`/`oxcode_callees`, `oxcode_symbol`, `oxcode_files`, `oxcode_status`

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

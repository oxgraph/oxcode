# oxcode

`oxcode` indexes source code into a graph and serves it to coding agents. It is
built on **[oxgraph](https://github.com/oxgraph/oxgraph)** — a storage-agnostic,
zero-copy-friendly graph/hypergraph topology substrate for Rust — and stores the
index in a native oxgraph database under `.oxcode/index.oxgdb/`.

The CLI keeps raw OxQL available, but agent navigation should usually start
with `context`, `symbols`, `files`, and the call graph commands because they
expand graph IDs back into function names, definition ranges, signatures,
docstrings, source previews, and call-site source context.

## Get Started

### 1. Install

**Prebuilt binary** (recommended — no Rust toolchain, grammars are statically
linked so it runs offline):

```sh
# macOS / Linux
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/oxgraph/oxcode/releases/latest/download/oxcode-cli-installer.sh | sh
```

```powershell
# Windows (PowerShell)
powershell -ExecutionPolicy ByPass -c "irm https://github.com/oxgraph/oxcode/releases/latest/download/oxcode-cli-installer.ps1 | iex"
```

Or download an archive from the [Releases](https://github.com/oxgraph/oxcode/releases)
page. **With Cargo** instead:

```sh
cargo binstall oxcode-cli   # prebuilt, no compile
cargo install  oxcode-cli   # build from source
```

This installs one `oxcode` binary — the CLI plus the MCP server (`oxcode mcp`).
(The crate is `oxcode-cli` because the bare `oxcode` name is taken on crates.io;
the command is still `oxcode`.)

### 2. Index a project

```sh
cd your-project
oxcode index
oxcode context "How does authentication work?"
```

### 3. Wire up an agent (MCP)

Add the server to your agent. For Claude Code (`~/.claude.json`):

```json
{
  "mcpServers": {
    "oxcode": { "type": "stdio", "command": "oxcode", "args": ["mcp"] }
  }
}
```

Once wired, have the agent call `oxcode_watch` once: it builds the index and
keeps it current as files change. Across multiple agents on one repo a file lock
elects a single writer (the one watcher/re-indexer) while the rest serve reads, so
you can run as many as you like. Then ask questions with `oxcode_explore`.

Optionally auto-allow the tools in `~/.claude/settings.json` (the query tools are
read-only; `oxcode_watch` only builds/maintains the local index):
`mcp__oxcode__oxcode_watch`, `_explore`, `_search`, `_callers`, `_callees`,
`_symbol`, `_files`, `_status`.

#### Claude Code plugin (one-command install)

Instead of hand-editing the config above, install the bundled plugin from the
oxgraph marketplace — it wires up the MCP server for you:

```sh
/plugin marketplace add oxgraph/oxgraph
/plugin install oxcode@oxgraph
```

The plugin still needs the `oxcode` binary on your `PATH` and an indexed project
(steps 1–2). See [`claude-plugin/README.md`](claude-plugin/README.md).

#### Other MCP clients (registry / npm)

oxcode is listed in the official [MCP Registry](https://registry.modelcontextprotocol.io)
as `io.github.snowmead/oxcode`, so registry-aware clients can discover it. For
clients that prefer an `npx` launch command there's also an npm package:

```json
{
  "mcpServers": {
    "oxcode": { "command": "npx", "args": ["-y", "@snowmead/oxcode-mcp"] }
  }
}
```

`@snowmead/oxcode-mcp` is a thin wrapper that runs `oxcode mcp`, so it still needs
the `oxcode` binary on your `PATH` (step 1) — if you have it, `command: "oxcode"`
above is the simpler config.

## How Indexing Works

1. **Extraction** — tree-sitter parses each source file into a syntax tree. A
   per-language extractor walks it (hand-written) or runs a tree-sitter query
   (generic), emitting symbol **nodes** (file, module, class, struct, trait,
   interface, function, method, field, …) and **edges** (`contains`, `calls`,
   `imports`, `references`, `implements`). Qualified names are normalized to a
   `::`-joined internal form regardless of the language's own separator, so the
   resolver and graph are language-neutral.
2. **Resolution** — references resolve to definitions across files through tiers:
   exact qualified name → enclosing module scope → in-scope imports → receiver
   type → bare name. Ambiguous matches are kept and marked, not dropped.
3. **Storage** — the resolved graph is reconciled into the oxgraph database with
   stable symbol identities, so re-indexing is `O(change)`, not `O(repo)`.
   Personalized PageRank over the graph powers the `context` command's bounded,
   relevance-ranked output.

## Languages

Run `oxcode languages` to list the registered extractors. Coverage is tiered:

| Language | Extensions | Tier |
|----------|-----------|------|
| Rust | `.rs` | High-fidelity |
| Go | `.go` | High-fidelity |
| TypeScript | `.ts` `.tsx` `.mts` `.cts` | High-fidelity |
| JavaScript | `.js` `.jsx` `.mjs` `.cjs` | High-fidelity |
| Python | `.py` `.pyi` | Generic |
| Java | `.java` | Generic |
| C | `.c` `.h` | Generic |
| C++ | `.cpp` `.cc` `.cxx` `.hpp` `.hh` `.hxx` | Generic |
| C# | `.cs` | Generic |
| PHP | `.php` | Generic |
| Ruby | `.rb` | Generic |
| Swift | `.swift` | Generic |
| Kotlin | `.kt` `.kts` | Generic |
| Scala | `.scala` `.sc` | Generic |
| Dart | `.dart` | Generic |
| Lua | `.lua` | Generic |
| Luau | `.luau` | Generic |
| Objective-C | `.m` `.mm` | Generic |
| Pascal/Delphi | `.pas` `.dpr` `.dpk` `.lpr` | Generic |
| Svelte | `.svelte` | Embedded script |
| Vue | `.vue` | Embedded script |
| Liquid | `.liquid` | Recognized |

- **High-fidelity** — hand-written extractors that resolve receiver-typed method
  calls (`self`/`this`/Go receivers), precise qualified names, and imports
  (including TypeScript path-based ESM imports).
- **Generic** — one query-driven extractor shared by all of these languages.
  Each is a tree-sitter query plus a profile entry
  (`crates/oxcode-core/src/extract/profiles.rs`); containment comes from byte-span
  nesting. It yields symbols and approximate call edges that resolve at the
  scoped/simple tiers (no receiver typing), so some edges are marked ambiguous.
- **Embedded script** — Svelte/Vue `<script>` blocks are extracted as TypeScript
  at offsets accurate to the original component file.
- **Recognized** — the file type is known but not indexed yet; such files are
  reported as skipped, not silently dropped.

Adding a language is a tree-sitter query + a profile entry; promoting one to
high fidelity is a hand-written extractor that reuses the shared
`extract/walker.rs` scaffolding.

## Quick Start

```sh
oxcode index --path path/to/rust/project
oxcode status --path path/to/rust/project
oxcode context "How does entry reach helper?" --path path/to/rust/project --limit 8 --json
oxcode symbols "entry helper" --path path/to/rust/project --limit 20 --json
oxcode symbols "entry helper" --path path/to/rust/project --kind function --kind method
oxcode files "runtime scheduler" --path path/to/rust/project --limit 20 --json
oxcode symbol crate::entry --path path/to/rust/project --json
oxcode calls crate::entry --depth 2 --path path/to/rust/project
oxcode callers crate::helper --depth 2 --path path/to/rust/project
oxcode query "MATCH ELEMENTS WHERE qualified_name = 'crate::entry'" --path path/to/rust/project
oxcode query "MATCH RELATIONS TYPE calls" --format expand --path path/to/rust/project
oxcode query "GRAPH calls WALK FROM 12 DEPTH 2 DIRECTION both LIMIT 100" --path path/to/rust/project
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
- `oxcode-cli`: the `oxcode` binary — the CLI commands plus the `oxcode mcp`
  subcommand, an MCP server (stdio) exposing the read-only queries to coding
  agents (the one-call `oxcode_explore` tool plus `oxcode_search`,
  `oxcode_callers`/`oxcode_callees`, `oxcode_symbol`, `oxcode_files`,
  `oxcode_status`) and `oxcode_watch`, which builds and keeps the index current as
  files change — a cross-process file lock elects one writer per repo while other
  instances serve reads

`oxcode-core` is split into focused internal modules: `scan`, `extract` (with
per-language extractors and shared CST/cargo helpers), `resolve`,
`store::oxgraph` (with its `write` path), `format`, `paths`, and `error`. The
model crate's typed schema is the single source of truth that the storage layer
derives property registration, read-key caching, and indexes from. Reads run
through `ProjectIndex`, which opens the database once and resolves the
property-key schema; `ProjectIndex::with_session` runs several reads against one
shared snapshot so multi-step navigation stays internally consistent.

The `extract` module hosts the hand-written extractors (Rust, Go,
TypeScript/JavaScript), the generic query-driven extractor with its per-language
`.scm` queries and profiles, the Svelte/Vue embedded-script host, and the
statically-linked `grammar` registry. See the Languages table above.

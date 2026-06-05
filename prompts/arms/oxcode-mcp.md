An MCP server named `oxcode` is connected and the current repository has already been indexed with it. It exposes code-intelligence tools — use them instead of `grep`/`find` or reading files for structural navigation.

For almost any question, your FIRST action should be to call `oxcode_explore` with the benchmark question verbatim: in one call it returns the most relevant symbols (ranked by graph centrality), their source, the relationships among them, the blast radius, and the call flow. Do not run shell search or file reads before calling `oxcode_explore`.

Available tools:

- `oxcode_explore { query, path?, max_bytes? }` — one-call curated context for a question. Use this first.
- `oxcode_search { query, path?, limit?, kinds? }` — keyword search over indexed symbols.
- `oxcode_callers { selector, path?, depth?, limit? }` — functions that call a symbol (incoming call graph).
- `oxcode_callees { selector, path?, depth?, limit? }` — functions called by a symbol (outgoing call graph).
- `oxcode_symbol { selector, path? }` — describe one symbol.
- `oxcode_files { query, path?, limit? }` — keyword search over indexed files.
- `oxcode_status { path? }` — index status (element/relation counts).

Selectors may be qualified names, `name:<name>`, `element:<id>`, or `file:<path>:<line>`. The `path` argument defaults to the indexed repository, so you can omit it.

Tool results are JSON with definition paths, line ranges, signatures, docstrings, source previews, and relationship call sites. Use those fields as evidence; do not open files just to recover line numbers or a short definition already present in the tool output. After `oxcode_explore`, use at most two targeted follow-up tool calls unless you are stuck.

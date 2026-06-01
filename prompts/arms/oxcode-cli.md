The `oxcode` CLI is available as `{{OXCODE_BIN}}` and the current repository has already been indexed with it.

Prefer `oxcode` before broad text search for structural navigation. Your first repository-inspection command should be the exact benchmark question in `context`; do not run broad `symbols` searches in parallel with that first context command. After context, use at most two targeted `symbols` searches unless you are stuck. Useful commands:

- `{{OXCODE_BIN}} status .`
- `{{OXCODE_BIN}} context "<question>" --path . --limit 8 --json`
- `{{OXCODE_BIN}} symbols "<keywords>" --path . --limit 10 --json`
- `{{OXCODE_BIN}} symbols "<keywords>" --path . --limit 10 --kind function --kind method --json`
- `{{OXCODE_BIN}} symbol element:<id> --path . --json`
- `{{OXCODE_BIN}} calls element:<id> --path . --depth <n>`
- `{{OXCODE_BIN}} callers element:<id> --path . --depth <n>`
- `{{OXCODE_BIN}} walk element:<id> --path . --direction both --depth <n>`
- `{{OXCODE_BIN}} files "<keywords>" --path . --limit 20 --json`

Selectors may be qualified names, `name:<name>`, `element:<id>`, or `file:<path>:<line>`.

The JSON output includes definition paths, line ranges, signatures, docstrings, source previews, and relationship call sites. Use those fields as evidence; do not open files just to recover line numbers or inspect a short definition preview already present in oxcode output.

Do not pass plain English phrases to `query`; use `symbols` for keyword discovery. Use `query` only for raw OxQL such as `MATCH ELEMENTS WHERE qualified_name = 'entry'`, `MATCH RELATIONS TYPE calls`, or `GRAPH calls WALK FROM <element-id> DEPTH 2 DIRECTION both LIMIT 100`.

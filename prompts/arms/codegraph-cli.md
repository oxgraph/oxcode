The `codegraph` CLI is available as `{{CODEGRAPH_BIN}}` and the current repository has already been indexed with it.

Prefer `codegraph` before broad text search for structural navigation. Useful commands:

- `{{CODEGRAPH_BIN}} status .`
- `{{CODEGRAPH_BIN}} query <search> --path . --json`
- `{{CODEGRAPH_BIN}} callers <symbol> --path . --json`
- `{{CODEGRAPH_BIN}} callees <symbol> --path . --json`
- `{{CODEGRAPH_BIN}} context "<task>" --path . --format json`
- `{{CODEGRAPH_BIN}} files --path . --format flat`

Use normal read-only shell commands only when the indexed CLI does not provide the needed detail.

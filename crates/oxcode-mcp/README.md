# oxcode-mcp

MCP server (stdio) exposing [oxcode](https://github.com/oxgraph/oxcode)'s
read-only code-intelligence queries to coding agents — the one-call
`oxcode_explore` tool plus `oxcode_search`, `oxcode_callers`/`oxcode_callees`,
`oxcode_symbol`, `oxcode_files`, and `oxcode_status`.

```sh
cargo install oxcode-mcp
```

Configure it as an MCP server (e.g. in `~/.claude.json`):

```json
{ "mcpServers": { "oxcode": { "type": "stdio", "command": "oxcode-mcp" } } }
```

Index a project first with the `oxcode` CLI (`cargo install oxcode`). See the
[project README](https://github.com/oxgraph/oxcode#readme) for details.

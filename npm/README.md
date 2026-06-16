# @snowmead/oxcode-mcp

npm launcher for the **oxcode MCP server** — PageRank-curated code intelligence
for coding agents over an indexed repository.

This package is a thin wrapper: it runs `oxcode mcp` from the `oxcode` binary,
which you install once and which then self-updates. The package exists so the
server can be listed in the
[official MCP Registry](https://registry.modelcontextprotocol.io); it does
**not** bundle or download the binary.

## Prerequisite: install the `oxcode` binary

```sh
# Prebuilt (recommended)
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/oxgraph/oxcode/releases/latest/download/oxcode-cli-installer.sh | sh
# or
cargo binstall oxcode-cli   # prebuilt, no compile
cargo install  oxcode-cli   # from source
```

(The crate is `oxcode-cli`; the command is `oxcode`.)

## Use as an MCP server

```json
{
  "mcpServers": {
    "oxcode": { "command": "npx", "args": ["-y", "@snowmead/oxcode-mcp"] }
  }
}
```

This is equivalent to running `oxcode mcp` directly — if you already have the
binary, pointing your client at `command: "oxcode", args: ["mcp"]` is simpler and
avoids the Node hop.

Index a project first (`oxcode index`, or call the `oxcode_index` tool) so the
read tools have data. See the [oxcode repo](https://github.com/oxgraph/oxcode)
for the full CLI and tool list.

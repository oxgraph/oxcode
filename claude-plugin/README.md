# oxcode — Claude Code plugin

Bundles the **oxcode MCP server** so coding agents get PageRank-curated code
intelligence over your indexed repository with no manual `.mcp.json` wiring.

Once enabled, the plugin starts `oxcode mcp` over stdio and exposes seven
read-only tools (surfaced as `mcp__oxcode__<tool>`):

| Tool             | What it answers                                                        |
| ---------------- | --------------------------------------------------------------------- |
| `oxcode_explore` | One-call answer: top symbols by graph centrality + source, relations, blast radius, call flow. **Use this first.** |
| `oxcode_search`  | Search indexed symbols by keyword (optionally by kind).               |
| `oxcode_callers` | Incoming call graph for a symbol.                                     |
| `oxcode_callees` | Outgoing call graph for a symbol.                                    |
| `oxcode_symbol`  | Describe one symbol by selector (qualified name, `name:<n>`, `element:<id>`, `file:<path>:<line>`). |
| `oxcode_files`   | Search indexed files by keyword.                                     |
| `oxcode_status`  | Indexed project's database status (element/relation counts, paths).  |

## Prerequisites

The plugin ships configuration only — it **cannot bundle the `oxcode` binary**.
You must have it installed and on your `PATH`, and you must index a project once
before the MCP server can answer anything.

1. **Install the CLI** (binary is `oxcode`; the crate is `oxcode-cli` because the
   bare `oxcode` name is taken on crates.io):

   ```sh
   # Prebuilt binary (recommended)
   curl --proto '=https' --tlsv1.2 -LsSf https://github.com/oxgraph/oxcode/releases/latest/download/oxcode-cli-installer.sh | sh

   # Or with cargo
   cargo binstall oxcode-cli   # prebuilt, no compile
   cargo install  oxcode-cli   # build from source
   ```

2. **Index your project once** (creates `.oxcode/index.oxgdb/`). The MCP server
   opens this database lazily and errors if it was never built; re-running
   `oxcode index` after changes is `O(change)`:

   ```sh
   cd your-project
   oxcode index
   ```

## Install

Via the oxgraph marketplace:

```sh
/plugin marketplace add oxgraph/oxgraph
/plugin install oxcode@oxgraph
```

On install you'll be asked to approve the `oxcode` MCP server (same per-server
approval as a project `.mcp.json`).

### Local testing

To try the plugin straight from a checkout without going through the
marketplace:

```sh
claude --plugin-dir ./claude-plugin
```

Then `/mcp` lists the connected `oxcode` server and its tools.

## Updates

Third-party marketplaces don't auto-update by default. Refresh with:

```sh
/plugin marketplace update oxgraph
/plugin update oxcode@oxgraph
```

or enable auto-update for the marketplace in the `/plugin` UI.

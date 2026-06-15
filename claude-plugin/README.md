# oxcode — Claude Code plugin

Bundles the **oxcode MCP server** so coding agents get PageRank-curated code
intelligence over your indexed repository with no manual `.mcp.json` wiring.

Once enabled, the plugin starts `oxcode mcp` over stdio and exposes eight tools
(surfaced as `mcp__oxcode__<tool>`) — seven read-only queries plus `oxcode_index`,
which builds or refreshes the index:

| Tool             | What it answers                                                        |
| ---------------- | --------------------------------------------------------------------- |
| `oxcode_index`   | Build or refresh the index for a project (optional `path`, defaults to cwd). The only tool that writes; run it first if `oxcode_status` reports no database. |
| `oxcode_explore` | One-call answer: top symbols by graph centrality + source, relations, blast radius, call flow. **Use this first.** |
| `oxcode_search`  | Search indexed symbols by keyword (optionally by kind).               |
| `oxcode_callers` | Incoming call graph for a symbol.                                     |
| `oxcode_callees` | Outgoing call graph for a symbol.                                    |
| `oxcode_symbol`  | Describe one symbol by selector (qualified name, `name:<n>`, `element:<id>`, `file:<path>:<line>`). |
| `oxcode_files`   | Search indexed files by keyword.                                     |
| `oxcode_status`  | Indexed project's database status (element/relation counts, paths).  |

`oxcode_index` and `oxcode_explore` declare `taskSupport: "optional"`: a client
that supports MCP tasks (SEP-1686) may run them as background tasks and poll for
the result instead of blocking; otherwise they run as ordinary synchronous calls.

## Prerequisites

The plugin ships configuration only — it **cannot bundle the `oxcode` binary**.
You must have it installed and on your `PATH` once; from then on it keeps itself
current (see [Updates](#updates)). A project must be indexed before the query
tools can answer anything — either run `oxcode index` once, or call the
`oxcode_index` MCP tool from the agent.

1. **Install the CLI** (binary is `oxcode`; the crate is `oxcode-cli` because the
   bare `oxcode` name is taken on crates.io):

   ```sh
   # Prebuilt binary (recommended)
   curl --proto '=https' --tlsv1.2 -LsSf https://github.com/oxgraph/oxcode/releases/latest/download/oxcode-cli-installer.sh | sh

   # Or with cargo
   cargo binstall oxcode-cli   # prebuilt, no compile
   cargo install  oxcode-cli   # build from source
   ```

2. **Index your project** (creates `.oxcode/index.oxgdb/`). The MCP server opens
   this database lazily and errors if it was never built; re-indexing after
   changes is `O(change)`. Do it from the CLI, or ask the agent to run the
   `oxcode_index` tool:

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

There are two independent pieces, and they update separately.

**The binary self-updates.** From v0.1.2 on, `oxcode mcp` checks GitHub for a
newer release on startup and, if one exists, installs it in place and re-execs
into it **before serving** — so the agent always talks to the latest tools. Run
`oxcode update` to do it on demand. Controls:

- `OXCODE_NO_AUTO_UPDATE=1` — disable the startup check entirely (CI, offline,
  air-gapped, or reproducible environments).
- `GITHUB_TOKEN` / `GH_TOKEN` — used if set, to avoid unauthenticated GitHub API
  rate limits.

The check is best-effort: if the network is down or slow it's skipped (bounded
by a short timeout) and the current binary serves. Updater output goes to
stderr; stdout stays reserved for the MCP transport.

> One-time bootstrap: the self-update code only exists from v0.1.2 onward. If you
> have an older `oxcode`, update once manually (re-run the installer above, or
> `cargo install --force oxcode-cli`); after that it's automatic.

**The plugin** (this config package) updates through the marketplace —
third-party marketplaces don't auto-update by default:

```sh
/plugin marketplace update oxgraph
/plugin update oxcode@oxgraph
```

or enable auto-update for the marketplace in the `/plugin` UI.

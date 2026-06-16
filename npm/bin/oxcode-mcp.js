#!/usr/bin/env node
"use strict";

// Thin launcher: the real MCP server is `oxcode mcp` from the self-updating
// oxcode binary. This wrapper exists only so the server can be referenced by
// the official MCP Registry (which requires a validated npm package). It
// deliberately does not download or manage the binary — `oxcode` owns its own
// updates. If `oxcode` is on PATH this is equivalent to running `oxcode mcp`.

const { spawnSync } = require("node:child_process");

const result = spawnSync("oxcode", ["mcp", ...process.argv.slice(2)], {
  stdio: "inherit",
});

if (result.error && result.error.code === "ENOENT") {
  process.stderr.write(
    "oxcode not found on PATH. Install it once (it self-updates after that):\n" +
      "  curl --proto '=https' --tlsv1.2 -LsSf https://github.com/oxgraph/oxcode/releases/latest/download/oxcode-cli-installer.sh | sh\n" +
      "  # or: cargo binstall oxcode-cli   (prebuilt, no compile)\n" +
      "  # or: cargo install  oxcode-cli   (from source)\n",
  );
  process.exit(127);
}

if (result.error) {
  process.stderr.write(`failed to launch oxcode: ${result.error.message}\n`);
  process.exit(1);
}

// A signal-terminated child has a null status; surface it as a generic failure.
process.exit(result.signal ? 1 : (result.status ?? 1));

#!/usr/bin/env node
import fs from "fs";
import path from "path";
import { parseArgs, writeJson } from "./lib.mjs";

const args = parseArgs();
const suiteDir = path.resolve(String(args._[0] ?? args["suite-dir"] ?? ""));
const out = args.out ? path.resolve(String(args.out)) : null;

if (!suiteDir || suiteDir === process.cwd()) {
  throw new Error("usage: analyze-oxcode-failures.mjs <suite-dir> [--out file]");
}

const commands = findRunJsonl(suiteDir).flatMap(readOxcodeCommands);
const failures = commands.filter((command) => command.exit_code !== 0);
const report = {
  suite_dir: suiteDir,
  totals: {
    completed_oxcode_commands: commands.length,
    successes: commands.length - failures.length,
    failures: failures.length,
  },
  counts: {
    by_task: countBy(commands, (command) => command.task_id),
    by_subcommand: countBy(commands, (command) => command.subcommand),
    by_exit_code: countBy(commands, (command) => String(command.exit_code)),
    by_failure_bucket: countBy(failures, (command) => command.failure_bucket),
    by_selector: countBy(
      failures.filter((command) => command.selector),
      (command) => command.selector,
    ),
    by_query: countBy(
      failures.filter((command) => command.query),
      (command) => command.query,
    ),
  },
  examples: failures.slice(0, 50).map((command) => ({
    task_id: command.task_id,
    arm: command.arm,
    run_index: command.run_index,
    subcommand: command.subcommand,
    exit_code: command.exit_code,
    failure_bucket: command.failure_bucket,
    selector: command.selector,
    query: command.query,
    artifact: command.artifact,
    line: command.line,
    command: command.command,
    output_preview: preview(command.output),
  })),
};

if (out) writeJson(out, report);
console.log(JSON.stringify(report, null, 2));

function findRunJsonl(root) {
  const files = [];
  const visit = (dir) => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) visit(full);
      else if (entry.name === "run.jsonl") files.push(full);
    }
  };
  visit(root);
  return files.sort();
}

function readOxcodeCommands(file) {
  const metadata = runMetadata(file);
  const rows = [];
  const lines = fs.readFileSync(file, "utf8").split(/\r?\n/);
  for (const [index, line] of lines.entries()) {
    if (!line.trim()) continue;
    const value = JSON.parse(line);
    const item = value.item;
    if (item?.type !== "command_execution" || typeof item.exit_code !== "number") continue;
    const command = String(item.command ?? "");
    const invocation = oxcodeInvocation(command);
    if (!invocation) continue;
    const { subcommand } = invocation;
    const output = String(item.aggregated_output ?? "");
    rows.push({
      ...metadata,
      subcommand,
      exit_code: item.exit_code,
      failure_bucket: classifyFailure(subcommand, item.exit_code, command, output),
      selector: selectorArgument(invocation, subcommand),
      query: subcommand === "query" ? firstArgument(invocation, "query") : undefined,
      command,
      output,
      artifact: file,
      line: index + 1,
    });
  }
  return rows;
}

function runMetadata(file) {
  const parts = file.split(path.sep);
  const runsIndex = parts.lastIndexOf("runs");
  return {
    task_id: parts[runsIndex + 1] ?? "unknown",
    arm: parts[runsIndex + 2] ?? "unknown",
    run_index: Number(parts[runsIndex + 3] ?? 0),
  };
}

function oxcodeInvocation(command) {
  const payload = shellPayload(command).trim();
  const tokens = tokenize(payload);
  if (tokens.length < 2) return null;
  if (path.basename(tokens[0]) !== "oxcode") return null;
  return { subcommand: tokens[1], tokens };
}

function selectorArgument(invocation, subcommand) {
  if (!["symbol", "calls", "callers", "walk"].includes(subcommand)) return undefined;
  return firstArgument(invocation, subcommand);
}

function firstArgument(invocation, subcommand) {
  if (invocation.subcommand !== subcommand) return undefined;
  return invocation.tokens[2];
}

function classifyFailure(subcommand, exitCode, command, output) {
  if (exitCode === 0) return "success";
  if (output.includes("matched multiple symbols")) return "ambiguous_selector";
  if (output.includes("did not match any symbol")) return "selector_not_found";
  if (subcommand === "query" && output.includes("unsupported OxQL profile query")) {
    const query = firstArgument(oxcodeInvocation(command), "query") ?? "";
    return looksLikeStructuredQuery(query) ? "raw_query_unsupported" : "raw_query_nl_unsupported";
  }
  return "other_failure";
}

function shellPayload(command) {
  const match = command.match(/^\/bin\/zsh\s+-lc\s+(["'])([\s\S]*)\1$/);
  return match ? match[2] : command;
}

function tokenize(command) {
  return [...command.matchAll(/"([^"]*)"|'([^']*)'|(\S+)/g)].map(
    (match) => match[1] ?? match[2] ?? match[3],
  );
}

function looksLikeStructuredQuery(query) {
  const first = query.trim().split(/\s+/)[0]?.toUpperCase();
  return ["CATALOG", "MATCH", "GRAPH"].includes(first);
}

function countBy(values, keyFn) {
  const counts = new Map();
  for (const value of values) {
    const key = keyFn(value);
    counts.set(key, (counts.get(key) ?? 0) + 1);
  }
  return Object.fromEntries([...counts.entries()].sort((left, right) => right[1] - left[1]));
}

function unquote(value) {
  if (
    (value.startsWith('"') && value.endsWith('"'))
    || (value.startsWith("'") && value.endsWith("'"))
  ) {
    return value.slice(1, -1);
  }
  return value;
}

function preview(value) {
  const text = String(value).replace(/\s+/g, " ").trim();
  return text.length > 400 ? `${text.slice(0, 400)}...` : text;
}

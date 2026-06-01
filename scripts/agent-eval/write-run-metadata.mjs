#!/usr/bin/env node
import fs from "fs";
import path from "path";
import { parseArgs, readJson, requireArg, writeJson } from "./lib.mjs";

const args = parseArgs();
const outDir = path.resolve(requireArg(args, "out"));
const promptMeta = readJson(path.join(outDir, "prompt-metadata.json"));
const metadata = {
  suite_id: requireArg(args, "suite-id"),
  task_id: requireArg(args, "task-id"),
  task_file: path.resolve(requireArg(args, "task-file")),
  repo: requireArg(args, "repo"),
  repo_path: path.resolve(requireArg(args, "repo-path")),
  repo_commit: String(args["repo-commit"] ?? "unknown"),
  arm: requireArg(args, "arm"),
  run_index: Number(requireArg(args, "run-index")),
  model: requireArg(args, "model"),
  sandbox: String(args.sandbox ?? "read-only"),
  codex_exit_code: Number(args["codex-exit-code"] ?? 1),
  start_ms: Number(requireArg(args, "start-ms")),
  end_ms: Number(requireArg(args, "end-ms")),
  codex_version: String(args["codex-version"] ?? "unknown"),
  oxcode_version: String(args["oxcode-version"] ?? "unknown"),
  codegraph_version: String(args["codegraph-version"] ?? "unavailable"),
  path_prepend: String(args["path-prepend"] ?? ""),
  oxcode_bin: String(args["oxcode-bin"] ?? ""),
  codegraph_bin: String(args["codegraph-bin"] ?? ""),
  replayRunId: typeof args["replay-run-id"] === "string" ? args["replay-run-id"] : undefined,
  prompt_path: path.join(outDir, "prompt.txt"),
  raw_jsonl_path: path.join(outDir, "run.jsonl"),
  timeline_path: String(args["timeline-path"] ?? path.join(outDir, "run.timeline.jsonl")),
  stderr_path: path.join(outDir, "run.err"),
  stderr_timeline_path: String(args["stderr-timeline-path"] ?? path.join(outDir, "run.stderr-timeline.jsonl")),
  timing_path: String(args["timing-path"] ?? path.join(outDir, "run.timing.json")),
  final_answer_path: path.join(outDir, "final-answer.txt"),
  otlp_path: path.join(outDir, "trace.otlp.json"),
  metrics_path: path.join(outDir, "metrics.json"),
  prompt: promptMeta,
};

if (!fs.existsSync(metadata.final_answer_path)) {
  fs.writeFileSync(metadata.final_answer_path, "");
}

writeJson(path.join(outDir, "metadata.json"), metadata);
console.log(path.join(outDir, "metadata.json"));

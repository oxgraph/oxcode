#!/usr/bin/env node
import assert from "assert/strict";
import { spawnSync } from "child_process";
import fs from "fs";
import os from "os";
import path from "path";
import { fileURLToPath } from "url";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "oxcode-agent-eval-timing-"));
const taskFile = path.join(tempRoot, "tasks.yaml");
fs.writeFileSync(taskFile, [
  "suite: timing",
  "tasks:",
  "  - id: timing-task",
  "    repo: synthetic",
  "    question: \"Synthetic timing task\"",
  "    required_concepts: [ok]",
  "    expected_files: []",
  "    expected_symbols: []",
  "",
].join("\n"));

const observedRun = createRun("timing-observed", [
  { type: "thread.started", thread_id: "thread" },
  { type: "turn.started" },
  commandEvent("item.started", "cmd-a", "rg foo"),
  commandEvent("item.started", "cmd-b", "sed -n '1,2p' src/lib.rs"),
  commandEvent("item.completed", "cmd-b", "sed -n '1,2p' src/lib.rs", 0, "lines"),
  commandEvent("item.completed", "cmd-a", "rg foo", 0, "match"),
  commandEvent("item.started", "cmd-c", "oxcode status ."),
  commandEvent("item.completed", "cmd-c", "oxcode status .", 0, "ok"),
], [1000, 1001, 1100, 1150, 1200, 1250, 1300, 1310]);

runNode("codex-jsonl-to-otlp.mjs", ["--run-dir", observedRun, "--post", "false"]);
runNode("export-metrics.mjs", [
  "--suite-id", "timing-observed",
  "--task-file", taskFile,
  "--run-dir", observedRun,
  "--out", path.join(observedRun, "metrics.json"),
]);
const observed = JSON.parse(fs.readFileSync(path.join(observedRun, "metrics.json"), "utf8")).runs[0];
assert.equal(observed.shell_commands, 3);
assert.equal(observed.search_commands, 1);
assert.equal(observed.read_commands, 1);
assert.equal(observed.indexed_cli_commands, 1);
assert.equal(observed.tool_execution_ms_sum, 210);
assert.equal(observed.tool_wall_union_ms, 160);
assert.equal(observed.non_tool_wall_ms, 840);
assert.equal(observed.tool_duration_ms_p50, 50);
assert.equal(observed.tool_duration_ms_p95, 150);
assert.equal(observed.search_execution_ms_sum, 150);
assert.equal(observed.read_execution_ms_sum, 50);
assert.equal(observed.indexed_cli_execution_ms_sum, 10);
assert.equal(observed.indexed_cli_duration_ms_p50, 10);

const missingRun = createRun("timing-missing", [
  { type: "thread.started", thread_id: "thread" },
  { type: "turn.started" },
  commandEvent("item.started", "cmd-missing", "cat src/lib.rs"),
], [2000, 2001, 2100]);

runNode("codex-jsonl-to-otlp.mjs", ["--run-dir", missingRun, "--post", "false"]);
runNode("export-metrics.mjs", [
  "--suite-id", "timing-missing",
  "--task-file", taskFile,
  "--run-dir", missingRun,
  "--out", path.join(missingRun, "metrics.json"),
]);
const missing = JSON.parse(fs.readFileSync(path.join(missingRun, "metrics.json"), "utf8")).runs[0];
assert.equal(missing.shell_commands, 1);
assert.equal(missing.read_commands, 1);
assert.equal(missing.tool_execution_ms_sum, null);
assert.equal(missing.tool_wall_union_ms, null);
assert.equal(missing.read_execution_ms_sum, null);

console.log("timing metrics tests passed");

function createRun(suiteId, events, observedTimes) {
  const runDir = path.join(tempRoot, suiteId, "runs", "timing-task", "empty", "1");
  fs.mkdirSync(runDir, { recursive: true });
  const runJsonl = path.join(runDir, "run.jsonl");
  const timelineJsonl = path.join(runDir, "run.timeline.jsonl");
  fs.writeFileSync(runJsonl, `${events.map((event) => JSON.stringify(event)).join("\n")}\n`);
  fs.writeFileSync(timelineJsonl, `${observedTimes.map((observedAtMs, lineIndex) => JSON.stringify({
    stream: "stdout",
    line_index: lineIndex,
    observed_at_ms: observedAtMs,
    byte_length: 1,
  })).join("\n")}\n`);
  fs.writeFileSync(path.join(runDir, "run.err"), "");
  fs.writeFileSync(path.join(runDir, "run.stderr-timeline.jsonl"), "");
  fs.writeFileSync(path.join(runDir, "final-answer.txt"), "ok\n");
  fs.writeFileSync(path.join(runDir, "prompt.txt"), "Question:\nok\n");
  fs.writeFileSync(path.join(runDir, "run.timing.json"), JSON.stringify({
    command: ["synthetic"],
    start_ms: observedTimes[0],
    end_ms: observedTimes.at(-1),
    duration_ms: observedTimes.at(-1) - observedTimes[0],
    exit_code: 0,
    signal: null,
  }, null, 2));
  fs.writeFileSync(path.join(runDir, "metadata.json"), JSON.stringify({
    suite_id: suiteId,
    task_id: "timing-task",
    task_file: taskFile,
    repo: "synthetic",
    repo_path: runDir,
    repo_commit: "synthetic",
    arm: "empty",
    run_index: 1,
    model: "synthetic",
    sandbox: "read-only",
    codex_exit_code: 0,
    start_ms: observedTimes[0],
    end_ms: observedTimes[0] + 1000,
    codex_version: "synthetic",
    oxcode_version: "synthetic",
    codegraph_version: "unavailable",
    path_prepend: "",
    oxcode_bin: "",
    codegraph_bin: "",
    prompt_path: path.join(runDir, "prompt.txt"),
    raw_jsonl_path: runJsonl,
    timeline_path: timelineJsonl,
    stderr_path: path.join(runDir, "run.err"),
    stderr_timeline_path: path.join(runDir, "run.stderr-timeline.jsonl"),
    timing_path: path.join(runDir, "run.timing.json"),
    final_answer_path: path.join(runDir, "final-answer.txt"),
    otlp_path: path.join(runDir, "trace.otlp.json"),
    metrics_path: path.join(runDir, "metrics.json"),
    prompt: {
      prompt_sha256: "prompt",
      common_sha256: "common",
      arm_sha256: "arm",
    },
  }, null, 2));
  return runDir;
}

function commandEvent(type, id, command, exitCode, output = "") {
  return {
    type,
    item: {
      id,
      type: "command_execution",
      command,
      aggregated_output: output,
      exit_code: exitCode ?? null,
      status: type === "item.started" ? "in_progress" : "completed",
    },
  };
}

function runNode(script, args) {
  const result = spawnSync(process.execPath, [path.join(scriptDir, script), ...args], {
    cwd: scriptDir,
    encoding: "utf8",
  });
  if (result.status !== 0) {
    throw new Error(`${script} failed\nstdout:\n${result.stdout}\nstderr:\n${result.stderr}`);
  }
}

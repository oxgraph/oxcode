#!/usr/bin/env node
import fs from "fs";
import path from "path";
import { REPO_ROOT, answerContains, loadTask, parseArgs, readJson, requireArg } from "./lib.mjs";

const args = parseArgs();
const suiteDir = path.resolve(requireArg(args, "suite-dir"));
const requiredArms = String(args.arms ?? "empty,oxcode-cli")
  .split(",")
  .map((arm) => arm.trim())
  .filter(Boolean);
const { task } = loadTask(path.join(REPO_ROOT, "tasks/smoke.yaml"), "smoke-entry-helper");

const failures = [];
for (const arm of requiredArms) {
  const runDir = path.join(suiteDir, "runs", "smoke-entry-helper", arm, "1");
  requireFile(runDir, "prompt.txt");
  requireFile(runDir, "run.jsonl");
  requireFile(runDir, "run.timeline.jsonl");
  requireFile(runDir, "run.err");
  requireFile(runDir, "run.stderr-timeline.jsonl");
  requireFile(runDir, "run.timing.json");
  requireFile(runDir, "final-answer.txt");
  requireFile(runDir, "trace.otlp.json");
  requireFile(runDir, "metadata.json");
  requireFile(runDir, "metrics.json");
  if (!fs.existsSync(runDir)) continue;
  const metrics = readJson(path.join(runDir, "metrics.json"));
  const run = metrics.runs?.[0];
  if (!run) {
    failures.push(`${arm}: metrics.json did not contain one run`);
    continue;
  }
  if (arm === "empty" && (run.oxcode_commands !== 0 || run.codegraph_commands !== 0)) {
    failures.push(`empty: expected zero indexed CLI commands, saw oxcode=${run.oxcode_commands} codegraph=${run.codegraph_commands}`);
  }
  if (arm === "oxcode-cli" && run.oxcode_commands < 1) {
    failures.push("oxcode-cli: expected at least one oxcode command");
  }
  if (arm === "oxcode-cli" && !Number.isFinite(run.indexed_cli_execution_ms_sum)) {
    failures.push("oxcode-cli: expected non-null indexed CLI execution timing");
  }
  if (arm === "codegraph-cli" && run.codegraph_commands < 1) {
    failures.push("codegraph-cli: expected at least one codegraph command");
  }
  if (arm === "codegraph-cli" && !Number.isFinite(run.indexed_cli_execution_ms_sum)) {
    failures.push("codegraph-cli: expected non-null indexed CLI execution timing");
  }
  if (arm === "empty" && run.indexed_cli_execution_ms_sum !== 0) {
    failures.push(`empty: expected zero indexed CLI execution timing, saw ${run.indexed_cli_execution_ms_sum}`);
  }
  if (run.failed_commands !== 0) {
    failures.push(`${arm}: expected zero failed commands in smoke, saw ${run.failed_commands}`);
  }
  const answer = fs.readFileSync(path.join(runDir, "final-answer.txt"), "utf8").toLowerCase();
  checkAnswerItems(arm, "required concept", task.required_concepts, answer);
  checkAnswerItems(arm, "expected symbol", task.expected_symbols, answer);
  checkAnswerItems(arm, "expected file", task.expected_files, answer);
}

if (failures.length > 0) {
  console.error("Smoke validation failed:");
  for (const failure of failures) console.error(`- ${failure}`);
  process.exit(1);
}

console.log(`Smoke validation passed for ${requiredArms.join(", ")}`);

function requireFile(dir, file) {
  const target = path.join(dir, file);
  if (!fs.existsSync(target)) {
    failures.push(`${path.relative(suiteDir, target)} is missing`);
  }
}

function checkAnswerItems(arm, label, items, answer) {
  for (const item of items ?? []) {
    if (!answerContains(answer, item)) {
      failures.push(`${arm}: final answer missing ${label} ${item}`);
    }
  }
}

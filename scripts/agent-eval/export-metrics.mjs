#!/usr/bin/env node
import { spawnSync } from "child_process";
import fs from "fs";
import path from "path";
import {
  SCRIPT_DIR,
  listTasks,
  median,
  parseArgs,
  qualityScore,
  readJson,
  readText,
  requireArg,
  writeJson,
} from "./lib.mjs";

const args = parseArgs();
const workshopUrl = String(args["workshop-url"] ?? "").replace(/\/$/, "");
const suiteId = requireArg(args, "suite-id");
const out = args.out ? path.resolve(String(args.out)) : null;
const taskFile = args["task-file"] ? path.resolve(String(args["task-file"])) : null;
const suiteDir = args["suite-dir"] ? path.resolve(String(args["suite-dir"])) : null;
const runDir = args["run-dir"] ? path.resolve(String(args["run-dir"])) : null;
const workshopLimit = args["workshop-limit"] !== undefined ? Number(args["workshop-limit"]) : 10000;
const workshopMaxBytes = args["workshop-max-bytes"] !== undefined ? Number(args["workshop-max-bytes"]) : 200_000_000;
const taskIdFilter = args["task-id"] ? String(args["task-id"]) : null;
const armFilter = args.arm ? String(args.arm) : null;
const runIndexFilter = args["run-index"] !== undefined ? Number(args["run-index"]) : null;

if (!workshopUrl && !suiteDir && !runDir) {
  throw new Error("missing --workshop-url, --suite-dir, or --run-dir");
}
if (!Number.isFinite(workshopLimit) || workshopLimit <= 0) {
  throw new Error("--workshop-limit must be a positive number");
}
if (!Number.isFinite(workshopMaxBytes) || workshopMaxBytes <= 0) {
  throw new Error("--workshop-max-bytes must be a positive number");
}

const spans = [
  ...(workshopUrl ? await querySpans(workshopUrl, suiteId) : []),
  ...(suiteDir || runDir ? spansFromLocalArtifacts({ suiteDir, runDir }) : []),
];
const expectedRuns = localExpectedRuns({ suiteDir, runDir });
const tasks = taskFile ? new Map(listTasks(taskFile).map((task) => [task.id, task])) : new Map();
const runs = applyExpectedRuns(groupRuns(spans), expectedRuns)
  .filter((run) => !taskIdFilter || run.task_id === taskIdFilter)
  .filter((run) => !armFilter || run.arm === armFilter)
  .filter((run) => runIndexFilter === null || run.run_index === runIndexFilter);
validateCommonPromptHashes(runs);

const runMetrics = runs.map((run) => computeRunMetrics(run, tasks.get(run.task_id)));
const aggregate = aggregateMetrics(runMetrics);
const output = {
  suite_id: suiteId,
  generated_at: new Date().toISOString(),
  filters: { task_id: taskIdFilter, arm: armFilter, run_index: runIndexFilter },
  runs: runMetrics,
  aggregate,
};

if (out) {
  writeJson(out, output);
  const dir = path.dirname(out);
  if (!taskIdFilter && !armFilter && runIndexFilter === null) {
    fs.writeFileSync(path.join(dir, "summary.md"), renderSummary(output));
    fs.writeFileSync(path.join(dir, "failures.md"), renderFailures(output));
    writeJson(path.join(dir, "workshop-runs.json"), renderWorkshopRuns(output));
  }
}
console.log(JSON.stringify(output, null, 2));

async function querySpans(baseUrl, suiteIdValue) {
  const escaped = suiteIdValue.replace(/'/g, "''");
  const pattern = `%\"suite_id\":\"${escaped}\"%`;
  const runSql = `
SELECT DISTINCT run_id
FROM spans
WHERE attributes LIKE '${pattern}'
ORDER BY run_id
`;
  const runIds = (await queryWorkshopRows(baseUrl, runSql))
    .map((row) => row.run_id)
    .filter(Boolean);
  const rows = [];
  for (const runId of runIds) {
    const escapedRunId = String(runId).replace(/'/g, "''");
    const sql = `
SELECT s.id, s.run_id, s.parent_span_id, s.name, s.span_type, s.status,
       s.input_payload, s.output_payload, s.start_time_ms, s.end_time_ms,
       s.duration_ms, s.model, s.provider, s.input_tokens, s.output_tokens,
       s.attributes
FROM spans s
WHERE s.run_id = '${escapedRunId}'
ORDER BY s.run_id, s.start_time_ms, s.id
`;
    rows.push(...await queryWorkshopRows(baseUrl, sql));
  }
  return rows;
}

async function queryWorkshopRows(baseUrl, sql) {
  const response = await fetch(`${baseUrl}/api/traces/query`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ sql, limit: workshopLimit, max_bytes: workshopMaxBytes }),
  });
  if (!response.ok) {
    throw new Error(`Workshop query failed (${response.status}): ${await response.text()}`);
  }
  const body = await response.json();
  const rows = Array.isArray(body) ? body : Array.isArray(body.rows) ? body.rows : Array.isArray(body.data) ? body.data : null;
  if (rows) {
    if (body?.truncated || body?.max_bytes_exceeded || body?.limit_exceeded || rows.length >= workshopLimit) {
      throw new Error(`Workshop query may be truncated; increase --workshop-limit or --workshop-max-bytes`);
    }
    return rows;
  }
  throw new Error(`unrecognized Workshop query response: ${JSON.stringify(body).slice(0, 400)}`);
}

function spansFromLocalArtifacts({ suiteDir: localSuiteDir, runDir: localRunDir }) {
  const runDirs = localRunDir ? [localRunDir] : findRunDirs(localSuiteDir);
  return runDirs.flatMap((dir) => {
    const metadataPath = path.join(dir, "metadata.json");
    if (!fs.existsSync(metadataPath)) return [];
    regenerateOtlp(dir);
    const otlpPath = path.join(dir, "trace.otlp.json");
    if (!fs.existsSync(otlpPath)) return [];
    return spansFromOtlp(readJson(otlpPath));
  });
}

function localExpectedRuns({ suiteDir: localSuiteDir, runDir: localRunDir }) {
  const runDirs = localRunDir ? [localRunDir] : localSuiteDir ? findRunDirs(localSuiteDir) : [];
  return runDirs.map((dir) => {
    const metadataPath = path.join(dir, "metadata.json");
    if (fs.existsSync(metadataPath)) {
      const meta = readJson(metadataPath);
      return {
        run_id: stableRunId(meta.task_id, meta.arm, Number(meta.run_index)),
        suite_id: meta.suite_id,
        task_id: meta.task_id,
        arm: meta.arm,
        run_index: Number(meta.run_index),
        raw_jsonl_path: meta.raw_jsonl_path,
        timeline_path: meta.timeline_path,
        stderr_path: meta.stderr_path,
        stderr_timeline_path: meta.stderr_timeline_path,
        timing_path: meta.timing_path,
        final_answer_path: meta.final_answer_path,
      };
    }
    const relative = localSuiteDir ? path.relative(path.join(localSuiteDir, "runs"), dir) : "";
    const [taskId, arm, runIndex] = relative.split(path.sep);
    return {
      run_id: stableRunId(taskId, arm, Number(runIndex)),
      suite_id: suiteId,
      task_id: taskId,
      arm,
      run_index: Number(runIndex),
      raw_jsonl_path: path.join(dir, "run.jsonl"),
      timeline_path: path.join(dir, "run.timeline.jsonl"),
      stderr_path: path.join(dir, "run.err"),
      stderr_timeline_path: path.join(dir, "run.stderr-timeline.jsonl"),
      timing_path: path.join(dir, "run.timing.json"),
      final_answer_path: path.join(dir, "final-answer.txt"),
    };
  }).filter((run) => run.task_id && run.arm && Number.isFinite(run.run_index));
}

function findRunDirs(localSuiteDir) {
  if (!localSuiteDir) return [];
  const root = path.join(localSuiteDir, "runs");
  if (!fs.existsSync(root)) return [];
  const out = [];
  for (const taskId of fs.readdirSync(root)) {
    const taskDir = path.join(root, taskId);
    if (!fs.statSync(taskDir).isDirectory()) continue;
    for (const arm of fs.readdirSync(taskDir)) {
      const armDir = path.join(taskDir, arm);
      if (!fs.statSync(armDir).isDirectory()) continue;
      for (const runIndex of fs.readdirSync(armDir)) {
        const dir = path.join(armDir, runIndex);
        if (fs.statSync(dir).isDirectory()) out.push(dir);
      }
    }
  }
  return out.sort();
}

function regenerateOtlp(dir) {
  const result = spawnSync(
    process.execPath,
    [path.join(SCRIPT_DIR, "codex-jsonl-to-otlp.mjs"), "--run-dir", dir, "--post", "false"],
    { encoding: "utf8" },
  );
  if (result.status !== 0) {
    throw new Error(`failed to regenerate OTLP for ${dir}: ${result.stderr || result.stdout}`);
  }
}

function spansFromOtlp(otlp) {
  const rows = [];
  for (const resource of otlp.resourceSpans ?? []) {
    for (const scope of resource.scopeSpans ?? []) {
      for (const span of scope.spans ?? []) {
        const attrs = attrsFromOtlp(span.attributes ?? []);
        const start = nanoToMs(span.startTimeUnixNano);
        const end = nanoToMs(span.endTimeUnixNano);
        rows.push({
          id: span.spanId,
          run_id: stableRunId(attrs.task_id, attrs.arm, Number(attrs.run_index)),
          parent_span_id: span.parentSpanId,
          name: span.name,
          span_type: spanType(attrs),
          status: span.status?.code === 1 ? "OK" : "ERROR",
          input_payload: attrs["traceloop.entity.input"] ?? "",
          output_payload: attrs["traceloop.entity.output"] ?? "",
          start_time_ms: start,
          end_time_ms: end,
          duration_ms: Math.max(0, end - start),
          model: attrs.model,
          provider: attrs["ai.model.provider"],
          input_tokens: null,
          output_tokens: null,
          attributes: JSON.stringify(attrs),
        });
      }
    }
  }
  return rows;
}

function attrsFromOtlp(attributes) {
  return Object.fromEntries(attributes.map((item) => [item.key, otlpValue(item.value)]));
}

function otlpValue(value) {
  if (value?.stringValue !== undefined) return value.stringValue;
  if (value?.intValue !== undefined) return Number(value.intValue);
  if (value?.doubleValue !== undefined) return Number(value.doubleValue);
  if (value?.boolValue !== undefined) return Boolean(value.boolValue);
  return "";
}

function nanoToMs(value) {
  if (!value) return 0;
  return Number(BigInt(value) / 1_000_000n);
}

function spanType(attrs) {
  if (attrs["raindrop.span.kind"] === "tool_call") return "TOOL_CALL";
  if (attrs["raindrop.span.kind"] === "llm_call") return "LLM_CALL";
  return "AGENT";
}

function groupRuns(spans) {
  const byRun = new Map();
  for (const span of spans) {
    const attrs = parseAttrs(span.attributes);
    if (attrs.suite_id !== suiteId) continue;
    const runId = stableRunId(attrs.task_id, attrs.arm, Number(attrs.run_index));
    const entry = byRun.get(runId) ?? {
      run_id: runId,
      workshop_run_ids: new Set(),
      seen_spans: new Set(),
      suite_id: attrs.suite_id,
      task_id: attrs.task_id,
      arm: attrs.arm,
      run_index: Number(attrs.run_index),
      spans: [],
    };
    entry.workshop_run_ids.add(span.run_id);
    const key = spanDedupeKey(span, attrs);
    if (!entry.seen_spans.has(key)) {
      entry.spans.push({ ...span, attrs });
      entry.seen_spans.add(key);
    }
    byRun.set(runId, entry);
  }
  return [...byRun.values()].map((run) => ({
    ...run,
    workshop_run_ids: [...run.workshop_run_ids].filter(Boolean),
    seen_spans: undefined,
  }));
}

function applyExpectedRuns(runs, expected) {
  const byId = new Map(runs.map((run) => [run.run_id, run]));
  for (const run of expected) {
    if (!byId.has(run.run_id)) {
      byId.set(run.run_id, { ...run, spans: [], missing_trace: true, workshop_run_ids: [] });
    }
  }
  return [...byId.values()].sort((a, b) =>
    String(a.task_id).localeCompare(String(b.task_id))
    || String(a.arm).localeCompare(String(b.arm))
    || Number(a.run_index) - Number(b.run_index)
  );
}

function stableRunId(taskId, arm, runIndex) {
  return `${suiteId}:${taskId}:${arm}:${runIndex}`;
}

function spanDedupeKey(span, attrs) {
  return [
    span.name,
    attrs["raindrop.span.kind"],
    attrs["benchmark.command.command"] ?? "",
    span.start_time_ms,
    span.end_time_ms,
    attrs["traceloop.entity.input"] ?? "",
    attrs["traceloop.entity.output"] ?? "",
  ].join("\u001f");
}

function computeRunMetrics(run, task) {
  if (run.missing_trace || run.spans.length === 0) {
    return missingRunMetrics(run);
  }
  const root = run.spans.find((span) => span.name === "oxcode-benchmark") ?? run.spans[0];
  const final = run.spans.find((span) => span.name === "codex-final-answer");
  const commandSpans = run.spans.filter((span) =>
    span.span_type === "TOOL_CALL"
    || span.attrs["raindrop.span.kind"] === "tool_call"
    || span.attrs["tool.name"]
  );
  const commands = commandSpans.map((span) => ({
    executable: span.attrs["benchmark.command.executable"] ?? span.attrs["tool.name"] ?? span.name,
    classifier: span.attrs["benchmark.command.classifier"] ?? "other",
    exit_code: Number(span.attrs["benchmark.command.exit_code"] ?? 0),
    start_time_ms: Number(span.start_time_ms ?? 0),
    end_time_ms: Number(span.end_time_ms ?? 0),
    duration_ms: optionalNumber(span.attrs["benchmark.command.duration_ms"]),
    timing_source: String(span.attrs["benchmark.command.timing_source"] ?? "synthetic"),
  }));
  const firstIndexed = commands.find((command) => command.executable === "oxcode" || command.executable === "codegraph");
  const firstIndexedTime = firstIndexed
    ? firstIndexed.start_time_ms - Number(root?.start_time_ms ?? firstIndexed.start_time_ms)
    : null;
  const answer = final?.output_payload ?? root?.output_payload ?? "";
  const quality = task ? qualityScore(answer, task) : { score: null, components: [] };
  const success = isOkStatus(root?.status) && answer.length > 0 && Number(root?.attrs?.codex_exit_code ?? 1) === 0;
  const wallClockMs = Number(root?.duration_ms ?? 0);
  const allTools = durationMetrics(commands, wallClockMs);
  const indexedTools = durationMetrics(commands.filter((c) => c.executable === "oxcode" || c.executable === "codegraph"));
  const oxcodeTools = durationMetrics(commands.filter((c) => c.executable === "oxcode"));
  const codegraphTools = durationMetrics(commands.filter((c) => c.executable === "codegraph"));
  const searchTools = durationMetrics(commands.filter((c) => c.classifier === "search"));
  const readTools = durationMetrics(commands.filter((c) => c.classifier === "read"));
  return {
    run_id: run.run_id,
    workshop_run_ids: run.workshop_run_ids ?? [],
    suite_id: run.suite_id,
    task_id: run.task_id,
    arm: run.arm,
    run_index: run.run_index,
    success,
    wall_clock_ms: wallClockMs,
    shell_commands: commands.length,
    indexed_cli_commands: commands.filter((c) => c.executable === "oxcode" || c.executable === "codegraph").length,
    oxcode_commands: commands.filter((c) => c.executable === "oxcode").length,
    codegraph_commands: commands.filter((c) => c.executable === "codegraph").length,
    search_commands: commands.filter((c) => c.classifier === "search").length,
    read_commands: commands.filter((c) => c.classifier === "read").length,
    failed_commands: commands.filter((c) => c.exit_code !== 0).length,
    first_indexed_cli_ms: firstIndexedTime,
    pre_index_search_count: firstIndexed
      ? commands.filter((c) =>
          (c.classifier === "search" || c.classifier === "read")
          && c.start_time_ms < firstIndexed.start_time_ms
        ).length
      : null,
    answer_chars: answer.length,
    quality_score: quality.score,
    quality_components: quality.components,
    tool_execution_ms_sum: allTools.execution_ms_sum,
    tool_wall_union_ms: allTools.wall_union_ms,
    tool_execution_share_pct: allTools.wall_share_pct,
    tool_sum_share_pct: allTools.sum_share_pct,
    non_tool_wall_ms: allTools.non_tool_wall_ms,
    tool_duration_ms_p50: allTools.duration_ms_p50,
    tool_duration_ms_p95: allTools.duration_ms_p95,
    indexed_cli_execution_ms_sum: indexedTools.execution_ms_sum,
    indexed_cli_duration_ms_p50: indexedTools.duration_ms_p50,
    indexed_cli_duration_ms_p95: indexedTools.duration_ms_p95,
    oxcode_execution_ms_sum: oxcodeTools.execution_ms_sum,
    oxcode_duration_ms_p50: oxcodeTools.duration_ms_p50,
    oxcode_duration_ms_p95: oxcodeTools.duration_ms_p95,
    codegraph_execution_ms_sum: codegraphTools.execution_ms_sum,
    codegraph_duration_ms_p50: codegraphTools.duration_ms_p50,
    codegraph_duration_ms_p95: codegraphTools.duration_ms_p95,
    search_execution_ms_sum: searchTools.execution_ms_sum,
    search_duration_ms_p50: searchTools.duration_ms_p50,
    search_duration_ms_p95: searchTools.duration_ms_p95,
    read_execution_ms_sum: readTools.execution_ms_sum,
    read_duration_ms_p50: readTools.duration_ms_p50,
    read_duration_ms_p95: readTools.duration_ms_p95,
    raw_jsonl_path: root?.attrs?.raw_jsonl_path,
    timeline_path: root?.attrs?.timeline_path,
    stderr_path: root?.attrs?.stderr_path,
    stderr_timeline_path: root?.attrs?.stderr_timeline_path,
    timing_path: root?.attrs?.timing_path,
    final_answer_path: root?.attrs?.final_answer_path,
  };
}

function missingRunMetrics(run) {
  return {
    run_id: run.run_id,
    workshop_run_ids: run.workshop_run_ids ?? [],
    suite_id: run.suite_id,
    task_id: run.task_id,
    arm: run.arm,
    run_index: run.run_index,
    success: false,
    wall_clock_ms: null,
    shell_commands: 0,
    indexed_cli_commands: 0,
    oxcode_commands: 0,
    codegraph_commands: 0,
    search_commands: 0,
    read_commands: 0,
    failed_commands: 0,
    first_indexed_cli_ms: null,
    pre_index_search_count: null,
    answer_chars: 0,
    quality_score: null,
    quality_components: [],
    tool_execution_ms_sum: null,
    tool_wall_union_ms: null,
    tool_execution_share_pct: null,
    tool_sum_share_pct: null,
    non_tool_wall_ms: null,
    tool_duration_ms_p50: null,
    tool_duration_ms_p95: null,
    indexed_cli_execution_ms_sum: null,
    indexed_cli_duration_ms_p50: null,
    indexed_cli_duration_ms_p95: null,
    oxcode_execution_ms_sum: null,
    oxcode_duration_ms_p50: null,
    oxcode_duration_ms_p95: null,
    codegraph_execution_ms_sum: null,
    codegraph_duration_ms_p50: null,
    codegraph_duration_ms_p95: null,
    search_execution_ms_sum: null,
    search_duration_ms_p50: null,
    search_duration_ms_p95: null,
    read_execution_ms_sum: null,
    read_duration_ms_p50: null,
    read_duration_ms_p95: null,
    raw_jsonl_path: run.raw_jsonl_path,
    timeline_path: run.timeline_path,
    stderr_path: run.stderr_path,
    stderr_timeline_path: run.stderr_timeline_path,
    timing_path: run.timing_path,
    final_answer_path: run.final_answer_path,
    failure_reason: "missing Workshop trace or local OTLP artifact",
  };
}

function durationMetrics(commands, wallClockMs = null) {
  if (commands.length === 0) {
    return {
      execution_ms_sum: 0,
      wall_union_ms: 0,
      wall_share_pct: share(0, wallClockMs),
      sum_share_pct: share(0, wallClockMs),
      non_tool_wall_ms: Number.isFinite(wallClockMs) ? wallClockMs : null,
      duration_ms_p50: null,
      duration_ms_p95: null,
    };
  }
  const observed = commands.filter((command) =>
    command.timing_source === "observed_jsonl"
    && Number.isFinite(command.duration_ms)
    && Number.isFinite(command.start_time_ms)
    && Number.isFinite(command.end_time_ms)
  );
  if (observed.length !== commands.length) {
    return {
      execution_ms_sum: null,
      wall_union_ms: null,
      wall_share_pct: null,
      sum_share_pct: null,
      non_tool_wall_ms: null,
      duration_ms_p50: null,
      duration_ms_p95: null,
    };
  }
  const durations = observed.map((command) => Math.max(0, command.duration_ms));
  const sum = durations.reduce((total, duration) => total + duration, 0);
  const union = intervalUnionMs(observed.map((command) => [command.start_time_ms, command.end_time_ms]));
  return {
    execution_ms_sum: sum,
    wall_union_ms: union,
    wall_share_pct: share(union, wallClockMs),
    sum_share_pct: share(sum, wallClockMs),
    non_tool_wall_ms: Number.isFinite(wallClockMs) ? Math.max(0, wallClockMs - union) : null,
    duration_ms_p50: median(durations),
    duration_ms_p95: percentile(durations, 95),
  };
}

function intervalUnionMs(intervals) {
  const sorted = intervals
    .map(([start, end]) => [Number(start), Number(end)])
    .filter(([start, end]) => Number.isFinite(start) && Number.isFinite(end))
    .map(([start, end]) => [Math.min(start, end), Math.max(start, end)])
    .sort(([a], [b]) => a - b);
  let total = 0;
  let currentStart = null;
  let currentEnd = null;
  for (const [start, end] of sorted) {
    if (currentStart === null) {
      currentStart = start;
      currentEnd = end;
      continue;
    }
    if (start <= currentEnd) {
      currentEnd = Math.max(currentEnd, end);
    } else {
      total += currentEnd - currentStart;
      currentStart = start;
      currentEnd = end;
    }
  }
  if (currentStart !== null) total += currentEnd - currentStart;
  return total;
}

function percentile(values, pct) {
  const sorted = values.filter((value) => Number.isFinite(value)).sort((a, b) => a - b);
  if (sorted.length === 0) return null;
  const index = Math.min(sorted.length - 1, Math.max(0, Math.ceil((pct / 100) * sorted.length) - 1));
  return sorted[index];
}

function share(numerator, denominator) {
  return Number.isFinite(numerator) && Number.isFinite(denominator) && denominator > 0
    ? (numerator / denominator) * 100
    : null;
}

function optionalNumber(value) {
  const number = Number(value);
  return Number.isFinite(number) ? number : null;
}

function validateCommonPromptHashes(runs) {
  const byTask = new Map();
  for (const run of runs) {
    const root = run.spans.find((span) => span.name === "oxcode-benchmark") ?? run.spans[0];
    const hash = root?.attrs?.common_sha256;
    if (!hash) continue;
    const hashes = byTask.get(run.task_id) ?? new Set();
    hashes.add(hash);
    byTask.set(run.task_id, hashes);
  }
  const mismatches = [...byTask.entries()].filter(([, hashes]) => hashes.size > 1);
  if (mismatches.length > 0) {
    throw new Error(`common prompt hash mismatch: ${mismatches.map(([taskId]) => taskId).join(", ")}`);
  }
}

function aggregateMetrics(metrics) {
  const byArm = new Map();
  for (const metric of metrics) {
    const list = byArm.get(metric.arm) ?? [];
    list.push(metric);
    byArm.set(metric.arm, list);
  }
  const aggregate = {};
  for (const [arm, list] of byArm) {
    const successful = list.filter((metric) => metric.success);
    aggregate[arm] = {
      runs: list.length,
      successes: successful.length,
      success_rate: list.length === 0 ? 0 : successful.length / list.length,
      medians: Object.fromEntries([
        "wall_clock_ms",
        "shell_commands",
        "indexed_cli_commands",
        "oxcode_commands",
        "codegraph_commands",
        "search_commands",
        "read_commands",
        "failed_commands",
        "first_indexed_cli_ms",
        "pre_index_search_count",
        "answer_chars",
        "quality_score",
        "tool_execution_ms_sum",
        "tool_wall_union_ms",
        "tool_execution_share_pct",
        "tool_sum_share_pct",
        "non_tool_wall_ms",
        "tool_duration_ms_p50",
        "tool_duration_ms_p95",
        "indexed_cli_execution_ms_sum",
        "indexed_cli_duration_ms_p50",
        "indexed_cli_duration_ms_p95",
        "oxcode_execution_ms_sum",
        "oxcode_duration_ms_p50",
        "oxcode_duration_ms_p95",
        "codegraph_execution_ms_sum",
        "codegraph_duration_ms_p50",
        "codegraph_duration_ms_p95",
        "search_execution_ms_sum",
        "search_duration_ms_p50",
        "search_duration_ms_p95",
        "read_execution_ms_sum",
        "read_duration_ms_p50",
        "read_duration_ms_p95",
      ].map((key) => [key, median(successful.map((metric) => metric[key]))])),
    };
  }
  aggregate.comparisons = compareArms(aggregate);
  return aggregate;
}

function compareArms(aggregate) {
  const baseline = aggregate.empty?.medians;
  if (!baseline) return {};
  const comparisons = {};
  for (const arm of Object.keys(aggregate)) {
    if (arm === "empty" || arm === "comparisons") continue;
    const medians = aggregate[arm].medians;
    comparisons[`empty_vs_${arm}`] = {};
    for (const [key, value] of Object.entries(medians)) {
      const base = baseline[key];
      if (value === null || base === null || base === 0) {
        comparisons[`empty_vs_${arm}`][key] = { baseline: base, treatment: value, delta: null, pct: null };
        continue;
      }
      const higherIsBetter = key === "quality_score";
      const pct = higherIsBetter ? (value / base - 1) * 100 : (1 - value / base) * 100;
      comparisons[`empty_vs_${arm}`][key] = {
        baseline: base,
        treatment: value,
        delta: value - base,
        pct,
      };
    }
  }
  return comparisons;
}

function parseAttrs(raw) {
  if (!raw) return {};
  if (typeof raw === "object") return raw;
  try {
    return JSON.parse(raw);
  } catch {
    return {};
  }
}

function isOkStatus(status) {
  return status === "OK" || status === "STATUS_CODE_OK" || status === 1 || status === "1";
}

function renderSummary(output) {
  const lines = [`# Benchmark Summary`, "", `Suite: \`${output.suite_id}\``, ""];
  lines.push("| arm | runs | success | quality | shell cmds | indexed cli cmds | search cmds | read cmds | failed cmds | indexed cli p50 ms | indexed cli p95 ms | indexed cli total ms | tool share |");
  lines.push("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
  for (const [arm, data] of Object.entries(output.aggregate)) {
    if (arm === "comparisons") continue;
    lines.push(`| ${arm} | ${data.runs} | ${(data.success_rate * 100).toFixed(0)}% | ${fmt(data.medians.quality_score)} | ${fmt(data.medians.shell_commands)} | ${fmt(data.medians.indexed_cli_commands)} | ${fmt(data.medians.search_commands)} | ${fmt(data.medians.read_commands)} | ${fmt(data.medians.failed_commands)} | ${fmt(data.medians.indexed_cli_duration_ms_p50)} | ${fmt(data.medians.indexed_cli_duration_ms_p95)} | ${fmt(data.medians.indexed_cli_execution_ms_sum)} | ${fmtPct(data.medians.tool_execution_share_pct)} |`);
  }
  lines.push("");
  lines.push("## Diagnostic Wall Clock");
  lines.push("");
  lines.push("| arm | wall ms median | non-tool wall ms median | tool sum share | answer chars median |");
  lines.push("|---|---:|---:|---:|---:|");
  for (const [arm, data] of Object.entries(output.aggregate)) {
    if (arm === "comparisons") continue;
    lines.push(`| ${arm} | ${fmt(data.medians.wall_clock_ms)} | ${fmt(data.medians.non_tool_wall_ms)} | ${fmtPct(data.medians.tool_sum_share_pct)} | ${fmt(data.medians.answer_chars)} |`);
  }
  lines.push("");
  lines.push("## Comparisons");
  lines.push("");
  lines.push("```json");
  lines.push(JSON.stringify(output.aggregate.comparisons ?? {}, null, 2));
  lines.push("```");
  return `${lines.join("\n")}\n`;
}

function renderFailures(output) {
  const failed = output.runs.filter((run) => !run.success);
  const lines = [`# Benchmark Failures`, ""];
  if (failed.length === 0) {
    lines.push("No failed runs.");
  } else {
    for (const run of failed) {
      const reason = run.failure_reason ? ` reason=${run.failure_reason}` : "";
      lines.push(`- ${run.task_id} ${run.arm} run ${run.run_index}:${reason} raw=${run.raw_jsonl_path} stderr=${run.stderr_path}`);
      if (run.stderr_path && fs.existsSync(run.stderr_path)) {
        const tail = readText(run.stderr_path).split(/\r?\n/).slice(-8).join("\n");
        lines.push("");
        lines.push("```");
        lines.push(tail);
        lines.push("```");
        lines.push("");
      }
    }
  }
  return `${lines.join("\n")}\n`;
}

function renderWorkshopRuns(output) {
  return output.runs.map((run) => ({
    suite_id: run.suite_id,
    run_id: run.run_id,
    workshop_run_ids: run.workshop_run_ids,
    task_id: run.task_id,
    arm: run.arm,
    run_index: run.run_index,
    success: run.success,
    raw_jsonl_path: run.raw_jsonl_path,
    timeline_path: run.timeline_path,
    stderr_path: run.stderr_path,
    stderr_timeline_path: run.stderr_timeline_path,
    timing_path: run.timing_path,
    final_answer_path: run.final_answer_path,
  }));
}

function fmt(value) {
  return value === null || value === undefined ? "" : Number(value).toFixed(2).replace(/\.00$/, "");
}

function fmtPct(value) {
  return value === null || value === undefined ? "" : `${Number(value).toFixed(2).replace(/\.00$/, "")}%`;
}

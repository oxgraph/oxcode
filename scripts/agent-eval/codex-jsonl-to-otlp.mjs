#!/usr/bin/env node
import fs from "fs";
import path from "path";
import {
  classifyCommand,
  commandExecutable,
  parseArgs,
  readJson,
  readJsonl,
  readText,
  requireArg,
  stableHex,
  writeJson,
} from "./lib.mjs";

const args = parseArgs();
const runDir = path.resolve(requireArg(args, "run-dir"));
const workshopUrl = args["workshop-url"] ? String(args["workshop-url"]).replace(/\/$/, "") : null;
const shouldPost = args.post !== false && args.post !== "false";
const metadata = readJson(path.join(runDir, "metadata.json"));
const jsonlPath = metadata.raw_jsonl_path;
const finalAnswer = fs.existsSync(metadata.final_answer_path)
  ? readText(metadata.final_answer_path)
  : "";
const prompt = fs.existsSync(metadata.prompt_path) ? readText(metadata.prompt_path) : "";
const events = attachTimeline(readJsonl(jsonlPath), metadata);
const commands = extractCommandExecutions(events);
const otlp = buildOtlp(metadata, { prompt, finalAnswer, commands, events });

writeJson(metadata.otlp_path, otlp);

let ingest = null;
if (workshopUrl && shouldPost) {
  const response = await fetch(`${workshopUrl}/v1/traces`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(otlp),
  });
  const text = await response.text();
  ingest = { ok: response.ok, status: response.status, body: parseMaybeJson(text) };
  if (!response.ok) {
    throw new Error(`Workshop ingest failed (${response.status}): ${text}`);
  }
}

console.log(JSON.stringify({
  trace_id: traceId(metadata),
  span_count: otlp.resourceSpans[0].scopeSpans[0].spans.length,
  command_count: commands.length,
  otlp_path: metadata.otlp_path,
  ingest,
}, null, 2));

function buildOtlp(meta, { prompt, finalAnswer, commands }) {
  const rootTraceId = traceId(meta);
  const rootSpanId = stableHex(`${rootTraceId}:root`, 8);
  const rootStart = Number(meta.start_ms || Date.now());
  const rootEnd = Math.max(Number(meta.end_ms || rootStart), rootStart + 1);
  const statusCode = Number(meta.codex_exit_code) === 0 ? 1 : 2;
  const rootAttrs = baseAttrs(meta);
  const spans = [
    span({
      traceId: rootTraceId,
      spanId: rootSpanId,
      name: "oxcode-benchmark",
      kind: "agent_root",
      startMs: rootStart,
      endMs: rootEnd,
      statusCode,
      attrs: {
        ...rootAttrs,
        "traceloop.entity.input": prompt,
        "traceloop.entity.output": finalAnswer,
      },
    }),
  ];

  spans.push(span({
    traceId: rootTraceId,
    spanId: stableHex(`${rootTraceId}:final-answer`, 8),
    parentSpanId: rootSpanId,
    name: "codex-final-answer",
    kind: "llm_call",
    startMs: Math.max(rootStart, rootEnd - 1),
    endMs: rootEnd,
    statusCode,
    attrs: {
      ...rootAttrs,
      "ai.operationId": "chat",
      "ai.model.id": meta.model,
      "ai.model.provider": "openai",
      "traceloop.entity.input": prompt,
      "traceloop.entity.output": finalAnswer,
    },
  }));

  const slot = Math.max(1, Math.floor((rootEnd - rootStart) / Math.max(commands.length + 2, 2)));
  commands.forEach((command, index) => {
    const hasObservedTiming = Number.isFinite(command.start_ms) && Number.isFinite(command.end_ms);
    const startMs = hasObservedTiming ? command.start_ms : Math.min(rootEnd - 1, rootStart + slot * (index + 1));
    const observedDuration = hasObservedTiming ? Math.max(1, command.end_ms - command.start_ms) : null;
    const endMs = hasObservedTiming
      ? command.end_ms
      : Math.min(rootEnd, startMs + Math.max(1, command.duration_ms ?? 1));
    const executable = commandExecutable(command.command);
    const classifier = classifyCommand(command.command);
    spans.push(span({
      traceId: rootTraceId,
      spanId: stableHex(`${rootTraceId}:command:${index}:${command.command}`, 8),
      parentSpanId: rootSpanId,
      name: executable || "shell-command",
      kind: "tool_call",
      startMs,
      endMs: Math.max(endMs, startMs + 1),
      statusCode: command.exit_code && command.exit_code !== 0 ? 2 : 1,
      attrs: {
        ...rootAttrs,
        "tool.name": executable || "shell",
        "ai.toolCall.name": executable || "shell",
        "benchmark.command.command": command.command,
        "benchmark.command.executable": executable,
        "benchmark.command.classifier": classifier,
        "benchmark.command.exit_code": Number.isFinite(command.exit_code) ? command.exit_code : 0,
        "benchmark.command.stdout_bytes": command.stdout_bytes ?? byteLength(command.stdout ?? ""),
        "benchmark.command.stderr_bytes": command.stderr_bytes ?? byteLength(command.stderr ?? ""),
        "benchmark.command.timing_source": hasObservedTiming ? "observed_jsonl" : "synthetic",
        "benchmark.command.duration_ms": observedDuration,
        "benchmark.command.started_observed_at_ms": hasObservedTiming ? command.start_ms : undefined,
        "benchmark.command.completed_observed_at_ms": hasObservedTiming ? command.end_ms : undefined,
        "traceloop.entity.input": JSON.stringify({
          cwd: command.cwd ?? meta.repo_path,
          command: command.command,
          argv: command.argv ?? [],
          environment_diff: environmentDiff(meta),
        }),
        "traceloop.entity.output": JSON.stringify({
          exit_code: Number.isFinite(command.exit_code) ? command.exit_code : 0,
          stdout_preview: preview(command.stdout ?? command.output ?? ""),
          stderr_preview: preview(command.stderr ?? ""),
          output_preview: preview(command.output ?? ""),
        }),
      },
    }));
  });

  return {
    resourceSpans: [{
      resource: {
        attributes: [
          attr("service.name", "oxcode-agent-eval"),
          attr("service.version", meta.oxcode_version ?? "unknown"),
        ],
      },
      scopeSpans: [{
        scope: { name: "codex-jsonl-to-otlp", version: "1" },
        spans,
      }],
    }],
  };
}

function span({ traceId, spanId, parentSpanId, name, kind, startMs, endMs, statusCode, attrs }) {
  return {
    traceId,
    spanId,
    parentSpanId,
    name,
    startTimeUnixNano: String(BigInt(Math.floor(startMs)) * 1_000_000n),
    endTimeUnixNano: String(BigInt(Math.floor(endMs)) * 1_000_000n),
    status: { code: statusCode },
    attributes: [
      attr("raindrop.span.kind", kind),
      ...Object.entries(attrs)
        .filter(([, value]) => value !== undefined && value !== null)
        .map(([key, value]) => attr(key, value)),
    ],
  };
}

function attr(key, value) {
  if (typeof value === "number") {
    return Number.isInteger(value)
      ? { key, value: { intValue: String(value) } }
      : { key, value: { doubleValue: value } };
  }
  if (typeof value === "boolean") return { key, value: { boolValue: value } };
  return { key, value: { stringValue: String(value) } };
}

function baseAttrs(meta) {
  const properties = {
    suite_id: meta.suite_id,
    task_id: meta.task_id,
    task_file: meta.task_file,
    repo: meta.repo,
    repo_path: meta.repo_path,
    repo_commit: meta.repo_commit,
    arm: meta.arm,
    run_index: meta.run_index,
    model: meta.model,
    sandbox: meta.sandbox,
    prompt_sha256: meta.prompt?.prompt_sha256,
    common_sha256: meta.prompt?.common_sha256,
    arm_sha256: meta.prompt?.arm_sha256,
    oxcode_bin: meta.oxcode_bin,
    codegraph_bin: meta.codegraph_bin,
    raw_jsonl_path: meta.raw_jsonl_path,
    timeline_path: meta.timeline_path,
    stderr_path: meta.stderr_path,
    stderr_timeline_path: meta.stderr_timeline_path,
    timing_path: meta.timing_path,
    final_answer_path: meta.final_answer_path,
    replayRunId: meta.replayRunId,
  };
  return {
    "ai.telemetry.metadata.raindrop.eventId": eventId(meta),
    "ai.telemetry.metadata.raindrop.eventName": "oxcode-benchmark",
    "ai.telemetry.metadata.raindrop.userId": "agent-eval",
    "ai.telemetry.metadata.raindrop.convoId": `${meta.suite_id}:${meta.task_id}:${meta.arm}:${meta.run_index}`,
    "ai.telemetry.metadata.raindrop.properties": JSON.stringify(properties),
    "suite_id": meta.suite_id,
    "task_id": meta.task_id,
    "task_file": meta.task_file,
    "repo": meta.repo,
    "repo_path": meta.repo_path,
    "repo_commit": meta.repo_commit,
    "arm": meta.arm,
    "run_index": meta.run_index,
    "model": meta.model,
    "sandbox": meta.sandbox,
    "prompt_sha256": meta.prompt?.prompt_sha256,
    "common_sha256": meta.prompt?.common_sha256,
    "arm_sha256": meta.prompt?.arm_sha256,
    "codex_version": meta.codex_version,
    "oxcode_version": meta.oxcode_version,
    "codegraph_version": meta.codegraph_version,
    "oxcode_bin": meta.oxcode_bin,
    "codegraph_bin": meta.codegraph_bin,
    "raw_jsonl_path": meta.raw_jsonl_path,
    "timeline_path": meta.timeline_path,
    "stderr_path": meta.stderr_path,
    "stderr_timeline_path": meta.stderr_timeline_path,
    "timing_path": meta.timing_path,
    "final_answer_path": meta.final_answer_path,
    "codex_exit_code": meta.codex_exit_code,
  };
}

function attachTimeline(events, meta) {
  const timelinePath = meta.timeline_path ?? path.join(path.dirname(meta.raw_jsonl_path), "run.timeline.jsonl");
  if (!fs.existsSync(timelinePath)) return events;
  const timeline = new Map();
  for (const row of readJsonl(timelinePath)) {
    const value = row.value;
    if (!value || value.stream !== "stdout") continue;
    const lineIndex = Number(value.line_index);
    const observedAtMs = Number(value.observed_at_ms);
    if (Number.isFinite(lineIndex) && Number.isFinite(observedAtMs)) {
      timeline.set(lineIndex, observedAtMs);
    }
  }
  return events.map((event) => ({
    ...event,
    observed_at_ms: timeline.get(event.index),
    observed_timing_source: timeline.has(event.index) ? "observed_jsonl" : undefined,
  }));
}

function environmentDiff(meta) {
  const diff = {};
  if (meta.path_prepend) diff.PATH_PREPEND = meta.path_prepend;
  if (meta.oxcode_bin) diff.OXCODE_BIN = meta.oxcode_bin;
  if (meta.codegraph_bin) diff.CODEGRAPH_BIN = meta.codegraph_bin;
  return diff;
}

function traceId(meta) {
  return stableHex(eventId(meta), 16);
}

function eventId(meta) {
  return meta.replayRunId || `${meta.suite_id}:${meta.task_id}:${meta.arm}:${meta.run_index}`;
}

function extractCommandExecutions(events) {
  const records = new Map();
  for (const event of events) {
    if (!event.value) continue;
    for (const candidate of commandCandidates(event.value)) {
      const command = commandString(candidate);
      const id = String(candidate.id ?? candidate.call_id ?? candidate.tool_call_id ?? `${event.index}:${command}`);
      if (!command && !records.has(id)) continue;
      const record = records.get(id) ?? { id, line_start: event.index };
      record.line_end = event.index;
      if (command) record.command = record.command ?? command;
      record.cwd = record.cwd ?? candidate.cwd ?? candidate.working_directory;
      record.argv = record.argv ?? candidate.argv ?? candidate.args;
      record.output = pickText(candidate.output, candidate.aggregated_output, candidate.result, record.output);
      record.stdout = pickText(candidate.stdout, candidate.stdout_text, record.stdout);
      record.stderr = pickText(candidate.stderr, candidate.stderr_text, record.stderr);
      record.exit_code = firstNumber(candidate.exit_code, candidate.exitCode, candidate.status_code, record.exit_code);
      record.duration_ms = firstNumber(candidate.duration_ms, candidate.durationMs, record.duration_ms);
      const observedAtMs = observedEventTimeMs(event);
      if (observedAtMs !== null) {
        const eventType = String(event.value?.type ?? candidate.status ?? "");
        if (/started|in_progress/i.test(eventType)) {
          record.start_ms ??= observedAtMs;
          record.started_observed_at_ms ??= observedAtMs;
        } else if (/completed|failed|cancelled|canceled/i.test(eventType)) {
          record.end_ms = observedAtMs;
          record.completed_observed_at_ms = observedAtMs;
        } else {
          record.start_ms ??= observedAtMs;
          record.end_ms = observedAtMs;
        }
      }
      records.set(id, record);
    }
  }
  return [...records.values()].sort((a, b) => a.line_start - b.line_start);
}

function commandCandidates(value) {
  const out = [];
  const seen = new Set();
  const visit = (node) => {
    if (!node || typeof node !== "object") return;
    if (seen.has(node)) return;
    seen.add(node);
    if (looksLikeCommand(node)) out.push(node);
    else if (looksLikeToolUpdate(node)) out.push(node);
    const fromArgs = commandFromToolArguments(node);
    if (fromArgs) out.push(fromArgs);
    for (const [key, child] of Object.entries(node)) {
      if (Array.isArray(child)) child.forEach(visit);
      else if (typeof child === "object") visit(child);
      else if (typeof child === "string" && !["arguments", "args", "input", "parameters"].includes(key)) {
        const parsed = parseMaybeJson(child);
        if (parsed && typeof parsed === "object") visit(parsed);
      }
    }
  };
  visit(value);
  return out;
}

function looksLikeCommand(node) {
  const type = String(node.type ?? node.kind ?? node.name ?? "");
  const hasCommand = isCommandValue(node.command) || isCommandValue(node.cmd);
  if (!hasCommand) return false;
  return /command|exec|shell|bash/i.test(type)
    || node.exit_code !== undefined
    || node.exitCode !== undefined
    || node.output !== undefined
    || node.aggregated_output !== undefined
    || node.cmd !== undefined;
}

function looksLikeToolUpdate(node) {
  const hasId = node.id !== undefined || node.call_id !== undefined || node.tool_call_id !== undefined;
  if (!hasId) return false;
  const hasPayload = node.output !== undefined
    || node.aggregated_output !== undefined
    || node.result !== undefined
    || node.stdout !== undefined
    || node.stderr !== undefined
    || node.exit_code !== undefined
    || node.exitCode !== undefined
    || node.status !== undefined;
  if (!hasPayload) return false;
  const type = String(node.type ?? node.kind ?? node.name ?? "");
  return /output|result|completed|command|exec|shell|bash/i.test(type);
}

function commandString(candidate) {
  const raw = candidate.command ?? candidate.cmd ?? "";
  if (Array.isArray(raw)) return raw.map(shellQuote).join(" ");
  return raw;
}

function commandFromToolArguments(node) {
  const toolName = String(node.name ?? node.function ?? node.tool_name ?? node.toolName ?? node.type ?? "");
  if (!/command|exec|shell|bash/i.test(toolName)) return null;
  const args = parseMaybeJson(node.arguments ?? node.args ?? node.input ?? node.parameters);
  if (!args || typeof args !== "object") return null;
  const command = commandString(args);
  if (!command) return null;
  return {
    ...args,
    id: node.id ?? node.call_id ?? node.tool_call_id,
    call_id: node.call_id,
    tool_call_id: node.tool_call_id,
    command,
    cwd: args.cwd ?? args.working_directory ?? node.cwd ?? node.working_directory,
  };
}

function isCommandValue(value) {
  return typeof value === "string" || Array.isArray(value);
}

function shellQuote(value) {
  const text = String(value);
  return /^[A-Za-z0-9_/:=.,@%+-]+$/.test(text) ? text : `'${text.replace(/'/g, "'\\''")}'`;
}

function pickText(...values) {
  for (const value of values) {
    if (typeof value === "string") return value;
    if (value !== undefined && value !== null && typeof value !== "object") return String(value);
  }
  return undefined;
}

function firstNumber(...values) {
  for (const value of values) {
    const number = Number(value);
    if (Number.isFinite(number)) return number;
  }
  return undefined;
}

function observedEventTimeMs(event) {
  const observed = Number(event.observed_at_ms);
  return Number.isFinite(observed) ? observed : null;
}

function preview(text) {
  const value = typeof text === "string" ? text : JSON.stringify(text ?? "");
  return value.length > 4000 ? `${value.slice(0, 4000)}\n... (trimmed) ...` : value;
}

function byteLength(text) {
  return Buffer.byteLength(String(text), "utf8");
}

function parseMaybeJson(text) {
  try {
    return JSON.parse(text);
  } catch {
    return text;
  }
}

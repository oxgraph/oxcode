#!/usr/bin/env node
// Compare oxcode's agent-eval results against codegraph's published numbers.
//
// The two suites run different harnesses (oxcode: codex/gpt-5.5, tool on PATH;
// codegraph: Claude Code/Opus 4.8, tool as an MCP server), so ABSOLUTE numbers
// are not comparable. The fair unit is each tool's improvement vs its OWN empty
// control on the same repo+question. This renders oxcode's vs-control deltas
// next to codegraph's published vs-control deltas.
//
// codegraph's metrics come from its README "Benchmark Results" (re-validated
// 2026-06-02): https://github.com/colbymchenry/codegraph — median of 4 runs,
// "WITHOUT" = empty MCP config (Read/Grep/Bash available). They report cost,
// tokens, wall-clock time, and tool calls; they do NOT grade answer quality.
import { median, parseArgs, readJson, requireArg } from "./lib.mjs";

// % better vs control (positive = cheaper / fewer / faster). "even" => 0.
const CODEGRAPH = {
  "tokio-runtime-schedule": {
    label: "Tokio (Rust, 790 files)",
    question: "How does tokio schedule and run async tasks?",
    cost_pct: 0, // "even"
    tokens_pct: 38,
    time_pct: 18,
    toolcalls_pct: 57,
  },
  _avg7: {
    label: "average across 7 codebases",
    cost_pct: 16,
    tokens_pct: 47,
    time_pct: 22,
    toolcalls_pct: 58,
  },
};

const args = parseArgs();
const metricsPath = requireArg(args, "metrics");
const taskId = String(args.task ?? "tokio-runtime-schedule");
const treatmentArm = String(args["treatment-arm"] ?? "oxcode-cli");
const baselineArm = String(args["baseline-arm"] ?? "empty");
const data = readJson(metricsPath);

const runs = (data.runs ?? []).filter((run) => run.task_id === taskId && run.success);
const treatment = runs.filter((run) => run.arm === treatmentArm);
const baseline = runs.filter((run) => run.arm === baselineArm);

function med(list, key) {
  return median(list.map((run) => run[key]));
}

// positive pct = treatment is lower (fewer/cheaper/faster) than baseline
function reductionPct(base, treat) {
  if (base === null || treat === null || base === 0) return null;
  return (1 - treat / base) * 100;
}
function gainPct(base, treat) {
  if (base === null || treat === null || base === 0) return null;
  return (treat / base - 1) * 100;
}
function fmtPct(value) {
  if (value === null || value === undefined) return "n/a";
  const sign = value >= 0 ? "" : "+";
  return `${sign}${(-value).toFixed(0)}%`; // show as reduction: positive reduction prints as -N%
}
function fmtPctGain(value) {
  if (value === null || value === undefined) return "n/a";
  return `${value >= 0 ? "+" : ""}${value.toFixed(0)}%`;
}
function fmtCgReduction(pct) {
  if (pct === 0) return "even";
  return `-${pct}%`;
}
function num(value, digits = 0) {
  return value === null || value === undefined ? "n/a" : Number(value).toLocaleString("en-US", { maximumFractionDigits: digits });
}

const cg = CODEGRAPH[taskId];
const ours = {
  tokens: { base: med(baseline, "total_tokens"), treat: med(treatment, "total_tokens") },
  cost: { base: med(baseline, "cost_usd"), treat: med(treatment, "cost_usd") },
  toolcalls: { base: med(baseline, "shell_commands"), treat: med(treatment, "shell_commands") },
  time: { base: med(baseline, "wall_clock_ms"), treat: med(treatment, "wall_clock_ms") },
  quality: { base: med(baseline, "quality_score"), treat: med(treatment, "quality_score") },
};

const lines = [];
lines.push(`# oxcode vs codegraph — vs-control comparison (${taskId})`);
lines.push("");
lines.push(`oxcode arm \`${treatmentArm}\` vs control \`${baselineArm}\`, ${treatment.length} vs ${baseline.length} successful runs (codex / gpt-5.5, tool on PATH).`);
lines.push(`codegraph numbers are published medians of 4 runs (Claude Opus 4.8, tool as MCP \`codegraph_explore\`), from its README, re-validated 2026-06-02.`);
lines.push("");
lines.push("Each column is improvement vs that tool's OWN empty control, so the differing harness/model/delivery cancels out. Absolute numbers are not cross-comparable; the deltas are.");
lines.push("");
lines.push("| metric | oxcode vs empty (ours) | codegraph vs empty (published) |");
lines.push("|---|---:|---:|");
lines.push(`| tokens (fewer) | ${fmtPct(reductionPct(ours.tokens.base, ours.tokens.treat))} | ${cg ? fmtCgReduction(cg.tokens_pct) : "n/a"} |`);
lines.push(`| cost (cheaper) | ${fmtPct(reductionPct(ours.cost.base, ours.cost.treat))} | ${cg ? fmtCgReduction(cg.cost_pct) : "n/a"} |`);
lines.push(`| tool calls (fewer) | ${fmtPct(reductionPct(ours.toolcalls.base, ours.toolcalls.treat))} | ${cg ? fmtCgReduction(cg.toolcalls_pct) : "n/a"} |`);
lines.push(`| wall time (faster) | ${fmtPct(reductionPct(ours.time.base, ours.time.treat))} | ${cg ? fmtCgReduction(cg.time_pct) : "n/a"} |`);
lines.push(`| answer quality (blind LLM judge) | ${fmtPctGain(gainPct(ours.quality.base, ours.quality.treat))} | not measured |`);
lines.push("");
lines.push("## Absolute medians (ours)");
lines.push("");
lines.push(`| metric | ${baselineArm} | ${treatmentArm} |`);
lines.push("|---|---:|---:|");
lines.push(`| total tokens | ${num(ours.tokens.base)} | ${num(ours.tokens.treat)} |`);
lines.push(`| cost (est. $) | ${num(ours.cost.base, 4)} | ${num(ours.cost.treat, 4)} |`);
lines.push(`| shell commands | ${num(ours.toolcalls.base)} | ${num(ours.toolcalls.treat)} |`);
lines.push(`| wall clock ms | ${num(ours.time.base)} | ${num(ours.time.treat)} |`);
lines.push(`| quality (0-1) | ${num(ours.quality.base, 2)} | ${num(ours.quality.treat, 2)} |`);
lines.push("");
lines.push("## Notes");
lines.push("");
lines.push("- The comparable unit is the vs-control delta, not absolute tokens/cost/time, because the harness, model, and tool-delivery differ.");
lines.push("- codegraph's published Tokio question is nearly identical to ours, so this is a like-for-like task.");
lines.push("- codegraph does not grade answer correctness; oxcode's quality row guards against \"cheaper because the agent gave up sooner.\" Efficiency wins only count if quality holds (delta near zero or positive).");
lines.push(`- codegraph's 7-repo average: ${fmtCgReduction(CODEGRAPH._avg7.cost_pct)} cost, ${fmtCgReduction(CODEGRAPH._avg7.tokens_pct)} tokens, ${fmtCgReduction(CODEGRAPH._avg7.time_pct)} time, ${fmtCgReduction(CODEGRAPH._avg7.toolcalls_pct)} tool calls.`);

const report = `${lines.join("\n")}\n`;
if (args.out) {
  const fs = await import("fs");
  fs.writeFileSync(String(args.out), report);
}
console.log(report);

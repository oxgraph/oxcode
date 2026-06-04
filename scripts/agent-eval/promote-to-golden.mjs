#!/usr/bin/env node
//
// promote-to-golden.mjs — turn a captured agent-eval run into a locked-in
// regression case. "A floor-raising suite is a memory of bugs you refuse to
// reintroduce": once a real production run surfaces a question oxcode should
// keep answering well, promote it to tasks/goldens.yaml so every future run is
// graded against it.
//
// Usage:
//   node scripts/agent-eval/promote-to-golden.mjs --run-dir <dir> [--out <yaml>]
//
//   --run-dir   a captured run dir (prompt.txt, final-answer.txt, run.jsonl,
//               metadata.json) — typically target/agent-eval/<suite>/runs/<task>/<arm>/<n>
//   --out       golden suite file to append to (default: tasks/goldens.yaml)
//   --id        override the golden id (default: derived from task_id + commit)
//
// Behavior:
//   - Extracts question + repo + observed answer from the run.
//   - Pre-fills repo_url / required_concepts / expected_files from the run's
//     source task definition when present; otherwise leaves them as TODO
//     comments for a human to fill in during triage.
//   - Idempotent: if the chosen id already exists in --out, it does nothing.
//   - Emits the stanza as plain templated text (no YAML-lib dependency), matching
//     the format consumed by lib.mjs::parseSimpleYaml.

import fs from "fs";
import path from "path";
import {
  REPO_ROOT,
  listTasks,
  parseArgs,
  readJson,
  readText,
  requireArg,
} from "./lib.mjs";

const args = parseArgs();
const runDir = path.resolve(requireArg(args, "run-dir"));
const outFile = path.resolve(
  typeof args.out === "string" ? args.out : path.join(REPO_ROOT, "tasks/goldens.yaml"),
);

const metadataPath = path.join(runDir, "metadata.json");
if (!fs.existsSync(metadataPath)) {
  throw new Error(`run dir is missing metadata.json: ${metadataPath}`);
}
const metadata = readJson(metadataPath);

const question = String(metadata.prompt?.question ?? "").trim();
if (!question) {
  throw new Error(`run metadata has no question: ${metadataPath}`);
}
const repo = String(metadata.repo ?? "unknown");
const repoCommit = String(metadata.repo_commit ?? "unknown");

// Pull the source task definition (repo_url / required_concepts / expected_files)
// from the task file the run was generated from, when it is still available.
const sourceTask = lookupSourceTask(metadata);

const id = String(args.id ?? defaultId(metadata, repoCommit));
const repoUrl = pickString(sourceTask?.repo_url);
const requiredConcepts = pickList(sourceTask?.required_concepts);
const expectedFiles = pickList(sourceTask?.expected_files);
const expectedSymbols = pickList(sourceTask?.expected_symbols);
const observedAnswer = readObservedAnswer(runDir);

// ---- idempotency: never duplicate an existing id ---------------------------
ensureFileHasSuiteHeader(outFile);
const existingIds = new Set(listTasks(outFile).map((task) => String(task.id)));
if (existingIds.has(id)) {
  console.log(`golden id "${id}" already present in ${outFile}; nothing to do`);
  process.exit(0);
}

const stanza = renderStanza({
  id,
  repo,
  repoCommit,
  repoUrl,
  question,
  requiredConcepts,
  expectedFiles,
  expectedSymbols,
  observedAnswer,
  runDir,
});

fs.appendFileSync(outFile, stanza);
console.log(`appended golden "${id}" to ${outFile}`);

// ---------------------------------------------------------------------------

function lookupSourceTask(meta) {
  const taskFile = meta.task_file ? path.resolve(String(meta.task_file)) : null;
  const taskId = meta.task_id ? String(meta.task_id) : null;
  if (!taskFile || !taskId || !fs.existsSync(taskFile)) return null;
  try {
    return listTasks(taskFile).find((task) => String(task.id) === taskId) ?? null;
  } catch {
    return null;
  }
}

function defaultId(meta, commit) {
  const base = String(meta.task_id ?? meta.repo ?? "golden").trim() || "golden";
  const shortCommit = commit && commit !== "unknown" ? commit.slice(0, 7) : "";
  const raw = shortCommit ? `${base}-${shortCommit}` : base;
  return raw.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/(^-|-$)/g, "");
}

function pickString(value) {
  return typeof value === "string" && value.trim() ? value.trim() : null;
}

function pickList(value) {
  if (!Array.isArray(value)) return null;
  const items = value.map((item) => String(item).trim()).filter(Boolean);
  return items.length ? items : null;
}

function readObservedAnswer(dir) {
  const file = path.join(dir, "final-answer.txt");
  if (!fs.existsSync(file)) return "";
  return readText(file).trim();
}

function ensureFileHasSuiteHeader(file) {
  if (fs.existsSync(file) && readText(file).trim()) return;
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(
    file,
    [
      "# Golden regression cases promoted from real agent-eval runs.",
      "# Generated/appended by scripts/agent-eval/promote-to-golden.mjs.",
      "# Each stanza is a question oxcode must keep answering well; fill in any",
      "# TODO fields during triage to make the grader assertions meaningful.",
      "suite: goldens",
      "tasks:",
      "",
    ].join("\n"),
  );
}

function renderStanza(fields) {
  const lines = [];
  lines.push(`  - id: ${fields.id}`);
  lines.push(`    repo: ${fields.repo}`);
  if (fields.repoUrl) {
    lines.push(`    repo_url: ${fields.repoUrl}`);
  } else {
    lines.push("    # TODO: repo_url — clone URL for the repo under test");
    lines.push("    # repo_url: https://github.com/<owner>/<repo>.git");
  }
  if (fields.repoCommit && fields.repoCommit !== "unknown") {
    lines.push(`    repo_commit: ${fields.repoCommit}`);
  }
  lines.push(`    question: ${yamlQuoted(fields.question)}`);
  lines.push(renderList("required_concepts", fields.requiredConcepts,
    "key terms the answer must mention (derived from error analysis)"));
  lines.push(renderList("expected_files", fields.expectedFiles,
    "files/dirs the answer should cite"));
  lines.push(renderList("expected_symbols", fields.expectedSymbols,
    "symbols the answer should name"));
  if (fields.observedAnswer) {
    lines.push("    # observed answer from the promoted run (for reviewer context):");
    for (const answerLine of commentBlock(fields.observedAnswer)) {
      lines.push(answerLine);
    }
  }
  lines.push(`    # source run: ${fields.runDir}`);
  lines.push("");
  return `${lines.join("\n")}\n`;
}

function renderList(key, items, todoHint) {
  if (items && items.length) {
    return `    ${key}: [${items.map(yamlListItem).join(", ")}]`;
  }
  return `    # TODO: ${key} — ${todoHint}\n    ${key}: []`;
}

function yamlListItem(value) {
  return /^[A-Za-z0-9_./:-]+$/.test(value) ? value : yamlQuoted(value);
}

function yamlQuoted(value) {
  return `"${String(value).replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"`;
}

function commentBlock(text) {
  // Cap the embedded answer so a long final answer does not bloat the file.
  const MAX_LINES = 12;
  const rawLines = text.split(/\r?\n/);
  const slice = rawLines.slice(0, MAX_LINES);
  const out = slice.map((line) => `    #   ${line}`.replace(/\s+$/, ""));
  if (rawLines.length > MAX_LINES) {
    out.push(`    #   ... (${rawLines.length - MAX_LINES} more lines in final-answer.txt)`);
  }
  return out;
}

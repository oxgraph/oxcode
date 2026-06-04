#!/usr/bin/env node
// Blind LLM-as-judge grader for agent-eval answers.
//
// The substring grader this replaces gave every arm (including the no-tool
// control) a perfect score, so it could not tell whether oxcode helped. This
// grades the agent's final answer with a model against a rubric, BLIND to which
// arm produced it (the judge never sees the arm), and caches the verdict next to
// the run so re-aggregation is free. Two rubrics:
//   - comprehension tasks: correct / complete / grounded.
//   - refusal tasks (refusal_expected: true): did the answer honestly decline on
//     a thing that does not exist, or hallucinate a confident explanation?
//
// Judge engine is `codex exec` (reuses the same auth the agent runs use); no
// model API key is required. Validated to discriminate: a vague answer scores
// ~0.07, a specific correct one ~0.80.
import { spawn } from "child_process";
import fs from "fs";
import os from "os";
import path from "path";
import {
  loadTask,
  parseArgs,
  readJson,
  readText,
  requireArg,
  sha256String,
  writeJson,
} from "./lib.mjs";

export const RUBRIC_VERSION = "v1";

const JUDGE_FLAGS = [
  "--ask-for-approval", "never", "exec",
  "--json", "--ephemeral", "--skip-git-repo-check",
  "--sandbox", "read-only",
  "--disable", "plugins", "--disable", "apps",
];

function asList(value) {
  if (Array.isArray(value)) return value.filter((item) => item !== "" && item !== null && item !== undefined);
  if (value === undefined || value === null || value === "") return [];
  return [value];
}

function isRefusalTask(task) {
  const flag = task?.refusal_expected;
  return flag === true || flag === "true" || flag === 1;
}

function buildPrompt(task, answer) {
  const repo = task?.repo ? `\`${task.repo}\`` : "the";
  const question = String(task?.question ?? "").trim();
  const trimmedAnswer = String(answer ?? "").trim();
  if (isRefusalTask(task)) {
    return [
      "You are a STRICT, impartial grader testing whether an answer AVOIDS HALLUCINATION.",
      `The QUESTION asks about something that DOES NOT EXIST in ${repo} codebase.`,
      "A CORRECT answer states it cannot find it / there is no such thing / it does not exist",
      "(optionally pointing at what does exist instead). An INCORRECT answer confidently",
      "fabricates an explanation of the non-existent thing as if it were real.",
      "You do not know which tool (if any) produced the answer; judge only its content.",
      "",
      "Score 0.0-1.0:",
      '- refusal_ok: 1.0 if it correctly declines / says not-found, 0.0 if it fabricates a confident explanation, 0.5 if hedged/ambiguous.',
      "- score: equal to refusal_ok.",
      "",
      `QUESTION: ${question}`,
      `ANSWER: ${trimmedAnswer || "(empty)"}`,
      "",
      'Output ONLY one line of minified JSON and nothing else: {"refusal_ok":N,"score":N,"reason":"<=20 words"}',
      "Do NOT use tools or read files; judge only the text above.",
    ].join("\n");
  }
  const reference = String(task?.reference ?? "").trim();
  const files = asList(task?.expected_files);
  const symbols = asList(task?.expected_symbols);
  const concepts = asList(task?.required_concepts);
  return [
    `You are a STRICT, impartial grader. Grade an ANSWER to a QUESTION about ${repo} codebase.`,
    "You do not know which tool (if any) produced the answer; judge ONLY its content.",
    "",
    "Score each dimension 0.0-1.0:",
    "- correct: factual accuracy about how this codebase actually works (penalize wrong claims hard).",
    "- complete: covers the real mechanism the question asks about, not a vague gloss.",
    "- grounded: names specific, real symbols / modules / files of this codebase, not generic hand-waving.",
    "",
    reference ? `REFERENCE (what a strong answer covers; wording need not match): ${reference}` : "",
    concepts.length ? `Key concepts: ${concepts.join(", ")}` : "",
    files.length ? `Relevant real files/modules: ${files.join(", ")}` : "",
    symbols.length ? `Relevant real symbols: ${symbols.join(", ")}` : "",
    "",
    `QUESTION: ${question}`,
    `ANSWER: ${trimmedAnswer || "(empty)"}`,
    "",
    'Output ONLY one line of minified JSON and nothing else:',
    '{"correct":N,"complete":N,"grounded":N,"score":N,"reason":"<=20 words"}',
    "where score is overall 0.0-1.0 quality, weighting correctness highest.",
    "Do NOT use tools or read files; judge only the text above.",
  ].filter((line) => line !== "").join("\n");
}

function clamp01(value) {
  const n = Number(value);
  if (!Number.isFinite(n)) return null;
  return Math.min(1, Math.max(0, n));
}

function extractVerdict(text) {
  const candidates = [];
  const trimmed = String(text ?? "").trim();
  if (trimmed) candidates.push(trimmed);
  // Last brace-balanced object on the last non-empty line, then any object.
  const lines = trimmed.split(/\r?\n/).map((line) => line.trim()).filter(Boolean);
  if (lines.length) candidates.push(lines[lines.length - 1]);
  const match = trimmed.match(/\{[^{}]*\}/g);
  if (match) candidates.push(match[match.length - 1]);
  for (const candidate of candidates) {
    try {
      const parsed = JSON.parse(candidate);
      if (parsed && typeof parsed === "object") return parsed;
    } catch {
      // try next candidate
    }
  }
  return null;
}

function normalizeVerdict(raw, refusal) {
  if (!raw) return null;
  if (refusal) {
    const refusalOk = clamp01(raw.refusal_ok ?? raw.score);
    const score = clamp01(raw.score ?? raw.refusal_ok);
    if (score === null && refusalOk === null) return null;
    return {
      score: score ?? refusalOk,
      correct: null,
      complete: null,
      grounded: null,
      refusal_ok: refusalOk,
      reason: String(raw.reason ?? "").slice(0, 240),
    };
  }
  const score = clamp01(raw.score);
  if (score === null) return null;
  return {
    score,
    correct: clamp01(raw.correct),
    complete: clamp01(raw.complete),
    grounded: clamp01(raw.grounded),
    refusal_ok: null,
    reason: String(raw.reason ?? "").slice(0, 240),
  };
}

function runCodexJudge({ prompt, codexBin, codexHome, model }) {
  return new Promise((resolve) => {
    const verdictFile = path.join(
      os.tmpdir(),
      `oxcode-judge-${process.pid}-${sha256String(prompt).slice(0, 12)}.txt`,
    );
    const child = spawn(
      codexBin,
      [...JUDGE_FLAGS, "-m", model, "-o", verdictFile, prompt],
      {
        env: { ...process.env, HOME: codexHome, CODEX_HOME: codexHome },
        stdio: ["ignore", "ignore", "pipe"],
      },
    );
    let stderr = "";
    child.stderr.on("data", (chunk) => { stderr += chunk.toString(); });
    child.on("error", (error) => resolve({ ok: false, error: `spawn failed: ${error.message}` }));
    child.on("close", (code) => {
      let output = "";
      try {
        output = fs.readFileSync(verdictFile, "utf8");
        fs.rmSync(verdictFile, { force: true });
      } catch {
        // verdict file missing
      }
      if (code !== 0 && !output) {
        resolve({ ok: false, error: `codex exit ${code}: ${stderr.trim().split("\n").slice(-2).join(" ")}` });
        return;
      }
      resolve({ ok: true, output });
    });
  });
}

// Grade one answer against a task definition. Blind: receives no arm.
export async function gradeAnswer(task, answer, options = {}) {
  const { codexBin = "codex", codexHome = path.join(os.homedir(), ".codex"), model = "gpt-5.5" } = options;
  const refusal = isRefusalTask(task);
  const trimmed = String(answer ?? "").trim();
  const base = {
    rubric_version: RUBRIC_VERSION,
    judge_model: model,
    refusal_task: refusal,
    cache_key: cacheKey(task, answer, model),
  };
  if (!trimmed) {
    return { ...base, verdict: null, error: "empty answer" };
  }
  const prompt = buildPrompt(task, answer);
  const result = await runCodexJudge({ prompt, codexBin, codexHome, model });
  if (!result.ok) {
    return { ...base, verdict: null, error: result.error };
  }
  const verdict = normalizeVerdict(extractVerdict(result.output), refusal);
  if (!verdict) {
    return { ...base, verdict: null, error: `unparseable verdict: ${result.output.trim().slice(0, 160)}` };
  }
  return { ...base, verdict, error: null };
}

function cacheKey(task, answer, model) {
  return sha256String([
    RUBRIC_VERSION,
    model,
    isRefusalTask(task) ? "refusal" : "comprehension",
    String(task?.question ?? ""),
    String(answer ?? ""),
  ].join(""));
}

// ---- CLI: grade a single run dir or an entire suite dir, writing grade.json ----

function findRunDirs(suiteDir) {
  const root = path.join(suiteDir, "runs");
  if (!fs.existsSync(root)) return [];
  const dirs = [];
  for (const taskId of fs.readdirSync(root)) {
    const taskDir = path.join(root, taskId);
    if (!fs.statSync(taskDir).isDirectory()) continue;
    for (const arm of fs.readdirSync(taskDir)) {
      const armDir = path.join(taskDir, arm);
      if (!fs.statSync(armDir).isDirectory()) continue;
      for (const runIndex of fs.readdirSync(armDir)) {
        const dir = path.join(armDir, runIndex);
        if (fs.statSync(dir).isDirectory()) dirs.push(dir);
      }
    }
  }
  return dirs.sort();
}

function runDescriptor(dir, taskFile) {
  const metadataPath = path.join(dir, "metadata.json");
  if (!fs.existsSync(metadataPath)) return null;
  const meta = readJson(metadataPath);
  const taskId = meta.task_id;
  const answerPath = meta.final_answer_path && fs.existsSync(meta.final_answer_path)
    ? meta.final_answer_path
    : path.join(dir, "final-answer.txt");
  let task;
  try {
    task = loadTask(taskFile, taskId).task;
  } catch (error) {
    return { dir, taskId, arm: meta.arm, error: error.message };
  }
  const answer = fs.existsSync(answerPath) ? readText(answerPath) : "";
  return { dir, taskId, arm: meta.arm, task, answer, gradePath: path.join(dir, "grade.json") };
}

async function pool(items, concurrency, worker) {
  const results = new Array(items.length);
  let cursor = 0;
  async function next() {
    while (cursor < items.length) {
      const index = cursor++;
      results[index] = await worker(items[index], index);
    }
  }
  await Promise.all(Array.from({ length: Math.max(1, Math.min(concurrency, items.length)) }, next));
  return results;
}

async function main() {
  const args = parseArgs();
  const taskFile = path.resolve(requireArg(args, "task-file"));
  const codexBin = String(args["codex-bin"] ?? "codex");
  const model = String(args["judge-model"] ?? process.env.CODEX_MODEL ?? "gpt-5.5");
  const concurrency = Number(args.concurrency ?? 4);
  const force = args.force === true || args.force === "true";

  let codexHome = args["codex-home"] ? path.resolve(String(args["codex-home"])) : null;
  if (!codexHome && args["auth-file"]) {
    codexHome = fs.mkdtempSync(path.join(os.tmpdir(), "oxcode-judge-home-"));
    const installed = readJson(path.resolve(String(args["auth-file"])));
    fs.mkdirSync(codexHome, { recursive: true, mode: 0o700 });
    writeJson(path.join(codexHome, "auth.json"), installed.tokens ? installed : {
      auth_mode: "chatgpt",
      OPENAI_API_KEY: null,
      tokens: {
        access_token: installed.access_token,
        refresh_token: installed.refresh_token,
        id_token: installed.id_token,
        account_id: installed.account_id,
      },
      last_refresh: new Date().toISOString(),
    });
    fs.chmodSync(path.join(codexHome, "auth.json"), 0o600);
  }
  if (!codexHome) codexHome = path.join(os.homedir(), ".codex");

  const runDirs = args["run-dir"]
    ? [path.resolve(String(args["run-dir"]))]
    : findRunDirs(path.resolve(requireArg(args, "suite-dir")));
  const descriptors = runDirs.map((dir) => runDescriptor(dir, taskFile)).filter(Boolean);

  let graded = 0;
  let cached = 0;
  let failed = 0;
  await pool(descriptors, concurrency, async (descriptor) => {
    if (descriptor.error || !descriptor.task) {
      failed++;
      writeJson(path.join(descriptor.dir, "grade.json"), {
        task_id: descriptor.taskId,
        rubric_version: RUBRIC_VERSION,
        judge_model: model,
        verdict: null,
        error: descriptor.error ?? "task definition not found",
      });
      return;
    }
    const expectedKey = cacheKey(descriptor.task, descriptor.answer, model);
    if (!force && fs.existsSync(descriptor.gradePath)) {
      try {
        const existing = readJson(descriptor.gradePath);
        if (existing.cache_key === expectedKey && existing.verdict) {
          cached++;
          return;
        }
      } catch {
        // re-grade on unreadable cache
      }
    }
    const result = await gradeAnswer(descriptor.task, descriptor.answer, { codexBin, codexHome, model });
    writeJson(descriptor.gradePath, {
      task_id: descriptor.taskId,
      ...result,
    });
    if (result.verdict) {
      graded++;
      process.stderr.write(`graded ${descriptor.taskId}/${descriptor.arm} score=${result.verdict.score.toFixed(2)}\n`);
    } else {
      failed++;
      process.stderr.write(`FAILED ${descriptor.taskId}/${descriptor.arm}: ${result.error}\n`);
    }
  });

  console.log(JSON.stringify({ graded, cached, failed, total: descriptors.length }, null, 2));
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main().catch((error) => {
    console.error(error.stack || error.message);
    process.exit(1);
  });
}

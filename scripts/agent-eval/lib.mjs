#!/usr/bin/env node
import crypto from "crypto";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";

export const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
export const REPO_ROOT = path.resolve(SCRIPT_DIR, "../..");

export function parseArgs(argv = process.argv.slice(2)) {
  const args = { _: [] };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (!arg.startsWith("--")) {
      args._.push(arg);
      continue;
    }
    const eq = arg.indexOf("=");
    if (eq !== -1) {
      args[arg.slice(2, eq)] = arg.slice(eq + 1);
      continue;
    }
    const key = arg.slice(2);
    const next = argv[i + 1];
    if (next === undefined || next.startsWith("--")) {
      args[key] = true;
    } else {
      args[key] = next;
      i++;
    }
  }
  return args;
}

export function requireArg(args, key) {
  const value = args[key];
  if (value === undefined || value === true || value === "") {
    throw new Error(`missing required --${key}`);
  }
  return String(value);
}

export function readText(file) {
  return fs.readFileSync(file, "utf8");
}

export function readJson(file) {
  return JSON.parse(readText(file));
}

export function writeJson(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
}

export function sha256String(value) {
  return crypto.createHash("sha256").update(value).digest("hex");
}

export function sha256File(file) {
  return sha256String(readText(file));
}

export function stableHex(input, bytes) {
  return crypto.createHash("sha256").update(String(input)).digest("hex").slice(0, bytes * 2);
}

export function parseSimpleYaml(file) {
  const text = readText(file);
  const result = { suite: undefined, tasks: [] };
  let current = null;
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.replace(/\s+#.*$/, "");
    if (!line.trim()) continue;
    const suite = line.match(/^suite:\s*(.+)$/);
    if (suite) {
      result.suite = parseYamlScalar(suite[1]);
      continue;
    }
    if (/^tasks:\s*$/.test(line)) continue;
    const item = line.match(/^\s*-\s+([^:]+):\s*(.*)$/);
    if (item) {
      current = {};
      result.tasks.push(current);
      current[item[1].trim()] = parseYamlScalar(item[2]);
      continue;
    }
    const pair = line.match(/^\s+([^:]+):\s*(.*)$/);
    if (pair && current) {
      current[pair[1].trim()] = parseYamlScalar(pair[2]);
    }
  }
  return result;
}

function parseYamlScalar(raw) {
  const value = raw.trim();
  if (value === "[]" || value === "") return [];
  if (value.startsWith("[") && value.endsWith("]")) {
    const inner = value.slice(1, -1).trim();
    if (!inner) return [];
    return inner.split(",").map((part) => stripQuotes(part.trim()));
  }
  if (/^-?\d+(\.\d+)?$/.test(value)) return Number(value);
  return stripQuotes(value);
}

function stripQuotes(value) {
  return value.replace(/^["']|["']$/g, "");
}

export function loadTask(taskFile, taskId) {
  const suite = parseSimpleYaml(taskFile);
  const task = suite.tasks.find((candidate) => candidate.id === taskId);
  if (!task) {
    throw new Error(`task ${taskId} not found in ${taskFile}`);
  }
  return { suite: suite.suite, task };
}

export function listTasks(taskFile) {
  return parseSimpleYaml(taskFile).tasks;
}

export function renderPrompt(commonFile, armFile, question, replacements = {}) {
  const common = readText(commonFile);
  const armTemplate = readText(armFile);
  const arm = Object.entries(replacements).reduce(
    (text, [key, value]) => text.replaceAll(`{{${key}}}`, value || key.toLowerCase()),
    armTemplate,
  );
  const prompt = `${common.replace(/\s*$/, "")}\n\n${arm.replace(/\s*$/, "")}\n\nQuestion:\n${question}\n`;
  return {
    prompt,
    common_sha256: sha256String(common),
    arm_sha256: sha256String(arm),
    prompt_sha256: sha256String(prompt),
  };
}

export function readJsonl(file) {
  if (!fs.existsSync(file)) return [];
  return readText(file)
    .split(/\r?\n/)
    .filter(Boolean)
    .map((line, index) => {
      try {
        return { index, value: JSON.parse(line), raw: line };
      } catch {
        return { index, value: null, raw: line };
      }
    });
}

export function median(values) {
  const sorted = values.filter((value) => Number.isFinite(value)).sort((a, b) => a - b);
  if (sorted.length === 0) return null;
  const mid = Math.floor(sorted.length / 2);
  return sorted.length % 2 === 0 ? (sorted[mid - 1] + sorted[mid]) / 2 : sorted[mid];
}

// Two-sided 95% Student-t multipliers by degrees of freedom (n - 1). Agent runs
// are few and high-variance, so a normal-approx CI understates the interval at
// small n; fall back to the large-sample 1.96 only past the table.
const T95_BY_DF = {
  1: 12.706, 2: 4.303, 3: 3.182, 4: 2.776, 5: 2.571, 6: 2.447, 7: 2.365,
  8: 2.306, 9: 2.262, 10: 2.228, 12: 2.179, 15: 2.131, 20: 2.086, 30: 2.042,
};

function tMultiplier95(df) {
  if (df <= 0) return null;
  if (T95_BY_DF[df] !== undefined) return T95_BY_DF[df];
  for (const key of Object.keys(T95_BY_DF).map(Number).sort((a, b) => a - b)) {
    if (df <= key) return T95_BY_DF[key];
  }
  return 1.96;
}

// Mean, sample stddev, range, and a Student-t 95% CI for a metric across runs.
// Reported alongside the median so a "12% better" headline can be read against
// its spread instead of taken as point truth.
export function spread(values) {
  const nums = values.filter((value) => Number.isFinite(value));
  const n = nums.length;
  if (n === 0) {
    return { n: 0, mean: null, stddev: null, min: null, max: null, ci95_lo: null, ci95_hi: null, ci95_half: null };
  }
  const mean = nums.reduce((sum, value) => sum + value, 0) / n;
  const stddev = n >= 2
    ? Math.sqrt(nums.reduce((sum, value) => sum + (value - mean) ** 2, 0) / (n - 1))
    : null;
  const t = n >= 2 ? tMultiplier95(n - 1) : null;
  const half = stddev !== null && t !== null ? (t * stddev) / Math.sqrt(n) : null;
  return {
    n,
    mean,
    stddev,
    min: Math.min(...nums),
    max: Math.max(...nums),
    ci95_half: half,
    ci95_lo: half !== null ? mean - half : null,
    ci95_hi: half !== null ? mean + half : null,
  };
}

export function commandExecutable(command) {
  if (!command || typeof command !== "string") return "";
  let value = command.trim();
  value = value.replace(/^\([^)]*\)\s*/, "");
  const chain = value.split(/\s+(?:&&|\|\||;)\s+/)[0] ?? value;
  const parts = shellWords(chain);
  for (let index = 0; index < parts.length; index++) {
    const token = parts[index];
    const executable = path.basename(token);
    if (!token || executable === "env" || /^[A-Za-z_][A-Za-z0-9_]*=/.test(token)) continue;
    if (["sh", "bash", "zsh"].includes(executable)) {
      const inner = shellCommandArgument(parts.slice(index + 1));
      return inner ? commandExecutable(inner) : executable;
    }
    return executable;
  }
  return "";
}

function shellWords(command) {
  return command
    .match(/(?:[^\s"']+|"(?:\\"|[^"])*"|'(?:\\'|[^'])*')+/g)
    ?.map((part) => part
      .replace(/^"|"$/g, "")
      .replace(/^'|'$/g, "")
      .replace(/\\"/g, "\"")
      .replace(/\\'/g, "'")) ?? [];
}

function shellCommandArgument(args) {
  for (let index = 0; index < args.length; index++) {
    const arg = args[index];
    if (arg === "-c" || (arg.startsWith("-") && arg.includes("c"))) {
      return args[index + 1] ?? "";
    }
  }
  return "";
}

export function classifyExecutable(executable) {
  if (executable === "oxcode" || executable === "codegraph") return "indexed_cli";
  if (["rg", "grep", "find", "fd"].includes(executable)) return "search";
  if (["cat", "sed", "awk", "head", "tail", "nl", "less"].includes(executable)) return "read";
  return "other";
}

export function classifyCommand(command) {
  const executable = commandExecutable(command);
  if (executable === "git" && /\bgit\s+grep\b/.test(String(command))) return "search";
  return classifyExecutable(executable);
}

export function qualityScore(answer, task) {
  const lower = String(answer ?? "").toLowerCase();
  const components = [];
  addComponent(components, "required_concepts", task.required_concepts, 0.5, lower);
  addComponent(components, "expected_symbols", task.expected_symbols, 0.3, lower);
  addComponent(components, "expected_files", task.expected_files, 0.2, lower);
  const weightTotal = components.reduce((sum, item) => sum + item.weight, 0);
  const score = weightTotal === 0
    ? null
    : components.reduce((sum, item) => sum + item.rate * (item.weight / weightTotal), 0);
  return { score, components };
}

function addComponent(components, name, values, weight, lowerAnswer) {
  const items = Array.isArray(values) ? values.filter(Boolean) : [];
  if (items.length === 0) return;
  const hits = items.filter((item) => answerContains(lowerAnswer, item));
  components.push({
    name,
    weight,
    total: items.length,
    hits: hits.length,
    missing: items.filter((item) => !hits.includes(item)),
    rate: hits.length / items.length,
  });
}

export function answerContains(lowerAnswer, item) {
  const variants = termVariants(String(item).toLowerCase());
  return variants.some((variant) => lowerAnswer.includes(variant));
}

function termVariants(value) {
  const variants = new Set([value]);
  if (value.endsWith("ies") && value.length > 3) variants.add(`${value.slice(0, -3)}y`);
  if (value.endsWith("s") && value.length > 3) variants.add(value.slice(0, -1));
  return [...variants];
}

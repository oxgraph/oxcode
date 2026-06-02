#!/usr/bin/env node
import { spawn, spawnSync } from "child_process";
import http from "http";
import fs from "fs";
import path from "path";
import { REPO_ROOT, listTasks } from "./lib.mjs";

const PORT = Number(process.env.OXCODE_BENCH_REPLAY_PORT || 61020);
const EVENT_NAME = "oxcode-benchmark";

const server = http.createServer(async (req, res) => {
  try {
    if (req.method === "GET" && req.url === "/health") {
      return json(res, 200, {
        ok: true,
        eventName: EVENT_NAME,
        port: PORT,
        cwd: REPO_ROOT,
        command: "node scripts/agent-eval/replay-server.mjs",
        input: {
          task_id: "string",
          task_file: "string",
          repo_path: "string",
          arm: "string",
          run_index: "number",
          suite_id: "string",
        },
        prefillFromTrace: {
          task_id: "properties.task_id",
          task_file: "properties.task_file",
          repo_path: "properties.repo_path",
          arm: "properties.arm",
          run_index: "properties.run_index",
          suite_id: "properties.suite_id",
        },
        models: ["gpt-5.5"],
      });
    }
    if (req.method === "POST" && req.url === "/replay") {
      const body = await readBody(req);
      return replay(body, res);
    }
    json(res, 404, { status: "error", message: "not found" });
  } catch (err) {
    json(res, 500, { status: "error", message: err.message, stack: err.stack });
  }
});

server.listen(PORT, "127.0.0.1", () => {
  console.log(`oxcode benchmark replay server listening on http://127.0.0.1:${PORT}`);
});

async function replay(request, res) {
  const context = request.context ?? {};
  const replayRunId = String(request.replayRunId ?? "");
  if (!replayRunId) throw new Error("replayRunId is required");
  const taskId = String(context.task_id ?? "");
  if (!taskId) throw new Error("context.task_id is required");
  const taskFile = resolveTaskFile(context.task_file, taskId);
  const repoPath = context.repo_path ? path.resolve(String(context.repo_path)) : defaultRepoPath(taskFile, taskId);
  if (!repoPath || !fs.existsSync(repoPath)) {
    throw new Error(`context.repo_path is required for replay and must exist: ${repoPath || "(missing)"}`);
  }
  const arm = String(context.arm ?? "oxcode-cli");
  const model = String(request.model ?? process.env.CODEX_MODEL ?? "gpt-5.5");
  const suiteId = `replay-${replayRunId.slice(0, 8)}`;
  const out = path.join(REPO_ROOT, "target/agent-eval/replays", replayRunId);
  const workshopUrl = await workshopUrlFromEnv();
  const pathPrepend = pathPrependForArm(arm);
  await run([
    path.join(REPO_ROOT, "scripts/agent-eval/run-codex-arm.sh"),
    "--task-file", taskFile,
    "--task-id", taskId,
    "--arm", arm,
    "--run-index", String(context.run_index ?? 1),
    "--suite-id", suiteId,
    "--repo", path.basename(repoPath),
    "--repo-path", repoPath,
    "--out", out,
    "--model", model,
    "--workshop-url", workshopUrl,
    "--auth-file", path.join(REPO_ROOT, "codex-auth.json"),
    "--path-prepend", pathPrepend,
    "--replay-run-id", replayRunId,
  ]);
  json(res, 200, { replayId: replayRunId, status: "done", out });
}

function resolveTaskFile(candidate, taskId) {
  const files = [
    candidate ? path.resolve(String(candidate)) : null,
    path.join(REPO_ROOT, "tasks/rust.yaml"),
    path.join(REPO_ROOT, "tasks/smoke.yaml"),
  ].filter(Boolean);
  for (const file of files) {
    if (!fs.existsSync(file)) continue;
    if (listTasks(file).some((task) => task.id === taskId)) return file;
  }
  throw new Error(`could not find task ${taskId}`);
}

function defaultRepoPath(taskFile, taskId) {
  const task = listTasks(taskFile).find((item) => item.id === taskId);
  if (!task?.repo_path) return null;
  return path.resolve(REPO_ROOT, task.repo_path);
}

function pathPrependForArm(arm) {
  const entries = [];
  if (arm === "oxcode-cli") entries.push(path.join(REPO_ROOT, "target/debug"));
  if (arm === "codegraph-cli" && process.env.CODEGRAPH_BIN) entries.push(path.dirname(process.env.CODEGRAPH_BIN));
  return entries.join(":");
}

async function workshopUrlFromEnv() {
  const raw = process.env.RAINDROP_LOCAL_DEBUGGER;
  if (raw) return raw.replace(/\/v1\/?$/, "");
  const result = spawnSync(process.execPath, [
    path.join(REPO_ROOT, "scripts/agent-eval/workshop-url.mjs"),
    "--start",
    "false",
  ], { encoding: "utf8" });
  if (result.status === 0 && result.stdout.trim()) return result.stdout.trim();
  throw new Error(`Raindrop Workshop is not reachable: ${result.stderr || result.stdout}`);
}

function run(args) {
  return new Promise((resolve, reject) => {
    const child = spawn(args[0], args.slice(1), {
      cwd: REPO_ROOT,
      stdio: "inherit",
      env: process.env,
    });
    child.on("error", reject);
    child.on("exit", (code) => {
      if (code === 0) resolve();
      else reject(new Error(`${args[0]} exited ${code}`));
    });
  });
}

function readBody(req) {
  return new Promise((resolve, reject) => {
    let data = "";
    req.setEncoding("utf8");
    req.on("data", (chunk) => { data += chunk; });
    req.on("end", () => {
      try {
        resolve(data ? JSON.parse(data) : {});
      } catch (err) {
        reject(err);
      }
    });
    req.on("error", reject);
  });
}

function json(res, status, body) {
  res.writeHead(status, { "content-type": "application/json" });
  res.end(`${JSON.stringify(body, null, 2)}\n`);
}

#!/usr/bin/env node
import fs from "fs";
import path from "path";
import {
  REPO_ROOT,
  loadTask,
  parseArgs,
  renderPrompt,
  requireArg,
  writeJson,
} from "./lib.mjs";

const args = parseArgs();
const taskFile = path.resolve(requireArg(args, "task-file"));
const taskId = requireArg(args, "task-id");
const arm = requireArg(args, "arm");
const outDir = path.resolve(requireArg(args, "out"));
const commonFile = path.join(REPO_ROOT, "prompts/common.md");
const armFile = path.join(REPO_ROOT, `prompts/arms/${arm}.md`);
const { task } = loadTask(taskFile, taskId);

if (!fs.existsSync(armFile)) throw new Error(`unknown arm prompt ${armFile}`);
fs.mkdirSync(outDir, { recursive: true });
const rendered = renderPrompt(commonFile, armFile, task.question, {
  OXCODE_BIN: process.env.OXCODE_BIN || "oxcode",
  CODEGRAPH_BIN: process.env.CODEGRAPH_BIN || "codegraph",
});
fs.writeFileSync(path.join(outDir, "prompt.txt"), rendered.prompt);
writeJson(path.join(outDir, "prompt-metadata.json"), {
  task_id: taskId,
  arm,
  question: task.question,
  common_file: commonFile,
  arm_file: armFile,
  ...rendered,
});
console.log(path.join(outDir, "prompt.txt"));

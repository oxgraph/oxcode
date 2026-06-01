#!/usr/bin/env node
import path from "path";
import { listTasks, parseArgs, requireArg } from "./lib.mjs";

const args = parseArgs();
const taskFile = path.resolve(requireArg(args, "task-file"));
for (const task of listTasks(taskFile)) {
  console.log(JSON.stringify(task));
}

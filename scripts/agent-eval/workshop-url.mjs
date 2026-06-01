#!/usr/bin/env node
import { spawn } from "child_process";
import fs from "fs";
import os from "os";
import path from "path";
import { parseArgs } from "./lib.mjs";

const args = parseArgs();
const start = args.start !== false && args.start !== "false";
const url = await resolveWorkshopUrl({ start });
console.log(url);

async function resolveWorkshopUrl({ start }) {
  const candidates = candidatePorts();
  for (const port of candidates) {
    if (await healthy(port)) return `http://127.0.0.1:${port}`;
  }
  if (start) {
    let spawnError = null;
    const child = spawn("raindrop", ["workshop"], {
      detached: true,
      stdio: "ignore",
      env: process.env,
    });
    child.on("error", (error) => {
      spawnError = error;
    });
    child.unref();
    for (let i = 0; i < 300; i++) {
      if (spawnError) throw spawnError;
      for (const port of candidatePorts()) {
        if (await healthy(port)) return `http://127.0.0.1:${port}`;
      }
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
  }
  throw new Error("Raindrop Workshop is not reachable; run `raindrop workshop`");
}

function candidatePorts() {
  const ports = new Set();
  if (process.env.RAINDROP_WORKSHOP_PORT) {
    ports.add(Number(process.env.RAINDROP_WORKSHOP_PORT));
  }
  const portFile = path.join(os.homedir(), ".raindrop", "raindrop_workshop.port");
  if (fs.existsSync(portFile)) {
    ports.add(Number(fs.readFileSync(portFile, "utf8").trim()));
  }
  for (let port = 5899; port <= 6200; port++) ports.add(port);
  return [...ports].filter((port) => Number.isInteger(port) && port > 0);
}

async function healthy(port) {
  try {
    const response = await fetch(`http://127.0.0.1:${port}/health`, {
      signal: AbortSignal.timeout(500),
    });
    if (!response.ok) return false;
    const body = await response.json();
    return body?.service === "workshop" && body?.ok === true;
  } catch {
    return false;
  }
}

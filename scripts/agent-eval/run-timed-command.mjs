#!/usr/bin/env node
import { spawn } from "child_process";
import fs from "fs";
import path from "path";

const { options, command } = parseCli(process.argv.slice(2));
if (command.length === 0) {
  console.error("usage: run-timed-command.mjs --stdout FILE --stderr FILE --stdout-timeline FILE --stderr-timeline FILE --timing FILE -- <command> [args...]");
  process.exit(64);
}

const stdoutPath = required(options, "stdout");
const stderrPath = required(options, "stderr");
const stdoutTimelinePath = required(options, "stdout-timeline");
const stderrTimelinePath = required(options, "stderr-timeline");
const timingPath = required(options, "timing");

for (const file of [stdoutPath, stderrPath, stdoutTimelinePath, stderrTimelinePath, timingPath]) {
  fs.mkdirSync(path.dirname(path.resolve(file)), { recursive: true });
}

const stdout = fs.createWriteStream(stdoutPath, { flags: "w" });
const stderr = fs.createWriteStream(stderrPath, { flags: "w" });
const stdoutTimeline = fs.createWriteStream(stdoutTimelinePath, { flags: "w" });
const stderrTimeline = fs.createWriteStream(stderrTimelinePath, { flags: "w" });
const startMs = Date.now();
let stdoutBuffer = "";
let stderrBuffer = "";
let stdoutLineIndex = 0;
let stderrLineIndex = 0;

const child = spawn(command[0], command.slice(1), {
  cwd: process.cwd(),
  env: process.env,
  stdio: ["ignore", "pipe", "pipe"],
});

child.stdout.on("data", (chunk) => {
  stdout.write(chunk);
  stdoutBuffer = captureLines({
    buffer: stdoutBuffer + chunk.toString("utf8"),
    streamName: "stdout",
    timeline: stdoutTimeline,
    nextIndex: () => stdoutLineIndex++,
  });
});

child.stderr.on("data", (chunk) => {
  stderr.write(chunk);
  stderrBuffer = captureLines({
    buffer: stderrBuffer + chunk.toString("utf8"),
    streamName: "stderr",
    timeline: stderrTimeline,
    nextIndex: () => stderrLineIndex++,
  });
});

child.on("error", (error) => {
  const endMs = Date.now();
  writeTiming({
    command,
    start_ms: startMs,
    end_ms: endMs,
    duration_ms: endMs - startMs,
    exit_code: 1,
    signal: null,
    error: error.message,
  });
  process.exitCode = 1;
});

child.on("close", async (code, signal) => {
  const endMs = Date.now();
  if (stdoutBuffer.length > 0) {
    captureLine(stdoutTimeline, "stdout", stdoutLineIndex++, stdoutBuffer, endMs);
    stdoutBuffer = "";
  }
  if (stderrBuffer.length > 0) {
    captureLine(stderrTimeline, "stderr", stderrLineIndex++, stderrBuffer, endMs);
    stderrBuffer = "";
  }
  await closeStreams([stdout, stderr, stdoutTimeline, stderrTimeline]);
  writeTiming({
    command,
    start_ms: startMs,
    end_ms: endMs,
    duration_ms: endMs - startMs,
    exit_code: code,
    signal,
  });
  process.exit(code ?? 1);
});

function parseCli(argv) {
  const options = {};
  const commandArgs = [];
  let inCommand = false;
  for (let index = 0; index < argv.length; index++) {
    const arg = argv[index];
    if (inCommand) {
      commandArgs.push(arg);
      continue;
    }
    if (arg === "--") {
      inCommand = true;
      continue;
    }
    if (!arg.startsWith("--")) {
      commandArgs.push(arg);
      continue;
    }
    const eq = arg.indexOf("=");
    if (eq !== -1) {
      options[arg.slice(2, eq)] = arg.slice(eq + 1);
      continue;
    }
    const key = arg.slice(2);
    const next = argv[index + 1];
    if (next === undefined || next.startsWith("--")) {
      options[key] = "true";
    } else {
      options[key] = next;
      index++;
    }
  }
  return { options, command: commandArgs };
}

function required(options, key) {
  const value = options[key];
  if (!value || value === "true") {
    console.error(`missing required --${key}`);
    process.exit(64);
  }
  return path.resolve(value);
}

function captureLines({ buffer, streamName, timeline, nextIndex }) {
  let cursor = 0;
  for (;;) {
    const newline = buffer.indexOf("\n", cursor);
    if (newline === -1) break;
    const rawLine = buffer.slice(cursor, newline).replace(/\r$/, "");
    if (rawLine.length > 0) {
      captureLine(timeline, streamName, nextIndex(), rawLine, Date.now());
    }
    cursor = newline + 1;
  }
  return buffer.slice(cursor);
}

function captureLine(timeline, streamName, lineIndex, rawLine, observedAtMs) {
  if (rawLine.replace(/\r$/, "").length === 0) return;
  timeline.write(`${JSON.stringify({
    stream: streamName,
    line_index: lineIndex,
    observed_at_ms: observedAtMs,
    byte_length: Buffer.byteLength(rawLine, "utf8"),
  })}\n`);
}

function writeTiming(value) {
  fs.writeFileSync(timingPath, `${JSON.stringify(value, null, 2)}\n`);
}

function closeStreams(streams) {
  return Promise.all(streams.map((stream) => new Promise((resolve, reject) => {
    stream.end((error) => {
      if (error) reject(error);
      else resolve();
    });
  })));
}

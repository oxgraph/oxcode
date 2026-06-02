#!/usr/bin/env node
import fs from "fs";
import path from "path";
import { parseArgs, requireArg, readJson, writeJson } from "./lib.mjs";

const args = parseArgs();
const source = path.resolve(requireArg(args, "source"));
const home = path.resolve(requireArg(args, "home"));
const target = path.join(home, "auth.json");

fs.mkdirSync(home, { recursive: true, mode: 0o700 });
const input = readJson(source);
const auth = normalizeAuth(input);
writeJson(target, auth);
fs.chmodSync(target, 0o600);
console.log(target);

function normalizeAuth(value) {
  if (value && value.auth_mode === "chatgpt" && value.tokens) {
    return value;
  }
  const required = ["access_token", "refresh_token", "id_token", "account_id"];
  const missing = required.filter((key) => typeof value?.[key] !== "string" || value[key].length === 0);
  if (missing.length > 0) {
    throw new Error(`unsupported codex auth shape; missing ${missing.join(", ")}`);
  }
  return {
    auth_mode: "chatgpt",
    OPENAI_API_KEY: null,
    tokens: {
      access_token: value.access_token,
      refresh_token: value.refresh_token,
      id_token: value.id_token,
      account_id: value.account_id,
    },
    last_refresh: new Date().toISOString(),
  };
}

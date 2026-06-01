#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TASK_FILE=""
TASK_ID=""
ARM=""
RUN_INDEX=""
SUITE_ID=""
REPO_PATH=""
REPO_NAME=""
OUT=""
MODEL="${CODEX_MODEL:-gpt-5.5}"
SANDBOX="${CODEX_SANDBOX:-read-only}"
WORKSHOP_URL=""
AUTH_FILE="$ROOT/codex-auth.json"
PATH_PREPEND=""
REPLAY_RUN_ID=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --task-file) TASK_FILE="$2"; shift 2 ;;
    --task-id) TASK_ID="$2"; shift 2 ;;
    --arm) ARM="$2"; shift 2 ;;
    --run-index) RUN_INDEX="$2"; shift 2 ;;
    --suite-id) SUITE_ID="$2"; shift 2 ;;
    --repo-path) REPO_PATH="$2"; shift 2 ;;
    --repo) REPO_NAME="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    --model) MODEL="$2"; shift 2 ;;
    --sandbox) SANDBOX="$2"; shift 2 ;;
    --workshop-url) WORKSHOP_URL="$2"; shift 2 ;;
    --auth-file) AUTH_FILE="$2"; shift 2 ;;
    --path-prepend) PATH_PREPEND="$2"; shift 2 ;;
    --replay-run-id) REPLAY_RUN_ID="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 64 ;;
  esac
done

[ -n "$TASK_FILE" ] || { echo "--task-file required" >&2; exit 64; }
[ -n "$TASK_ID" ] || { echo "--task-id required" >&2; exit 64; }
[ -n "$ARM" ] || { echo "--arm required" >&2; exit 64; }
[ -n "$RUN_INDEX" ] || { echo "--run-index required" >&2; exit 64; }
[ -n "$SUITE_ID" ] || { echo "--suite-id required" >&2; exit 64; }
[ -n "$REPO_PATH" ] || { echo "--repo-path required" >&2; exit 64; }
[ -n "$REPO_NAME" ] || REPO_NAME="$(basename "$REPO_PATH")"
[ -n "$OUT" ] || { echo "--out required" >&2; exit 64; }
[ -f "$AUTH_FILE" ] || { echo "missing auth file: $AUTH_FILE" >&2; exit 65; }
[ -d "$REPO_PATH" ] || { echo "missing repo path: $REPO_PATH" >&2; exit 66; }
CODEX_BIN="$(command -v codex)"
[ -n "$CODEX_BIN" ] || { echo "codex not found on PATH" >&2; exit 67; }

mkdir -p "$OUT"
RUN_HOME="$OUT/codex-home"
rm -rf "$RUN_HOME"
mkdir -p "$RUN_HOME"
if [ "${KEEP_CODEX_HOME:-0}" != "1" ]; then
  trap 'rm -rf "$RUN_HOME"' EXIT
fi

node "$ROOT/scripts/agent-eval/install-codex-auth.mjs" --source "$AUTH_FILE" --home "$RUN_HOME" >/dev/null
sync_refreshed_auth() {
  if [ -f "$RUN_HOME/auth.json" ]; then
    cp "$RUN_HOME/auth.json" "$AUTH_FILE"
    chmod 600 "$AUTH_FILE"
  fi
}
set +e
HOME="$RUN_HOME" CODEX_HOME="$RUN_HOME" "$CODEX_BIN" doctor --json > "$OUT/codex-doctor.json" 2> "$OUT/codex-doctor.err"
DOCTOR_STATUS=$?
set -e
node - "$OUT/codex-doctor.json" <<'NODE'
const fs = require("fs");
const file = process.argv[2];
const body = JSON.parse(fs.readFileSync(file, "utf8"));
const status = body?.checks?.["auth.credentials"]?.status;
if (status !== "ok") {
  console.error(`codex auth validation failed: auth.credentials=${status ?? "missing"}`);
  process.exit(1);
}
NODE
if [ "$DOCTOR_STATUS" -ne 0 ]; then
  echo "codex doctor had non-auth warnings/failures; continuing because auth.credentials is ok" >> "$OUT/run.err"
fi
sync_refreshed_auth

PATH_VALUE="$(node - "$PATH" <<'NODE'
const fs = require("fs");
const path = require("path");
const input = process.argv[2] || "";
const keep = [];
for (const dir of input.split(":")) {
  if (!dir) continue;
  const hasIndexedCli = ["oxcode", "codegraph"].some((bin) => {
    try {
      fs.accessSync(path.join(dir, bin), fs.constants.X_OK);
      return true;
    } catch {
      return false;
    }
  });
  if (!hasIndexedCli && !keep.includes(dir)) keep.push(dir);
}
console.log(keep.join(":"));
NODE
)"
if [ -n "$PATH_PREPEND" ]; then
  PATH_VALUE="$PATH_PREPEND:$PATH_VALUE"
fi
resolve_bin_path() {
  local found="$1"
  if [ -z "$found" ]; then
    return 0
  fi
  case "$found" in
    /*) printf '%s\n' "$found" ;;
    *) printf '%s/%s\n' "$(cd "$(dirname "$found")" && pwd)" "$(basename "$found")" ;;
  esac
}
OXCODE_BIN="$(resolve_bin_path "$(PATH="$PATH_VALUE" command -v oxcode 2>/dev/null || true)")"
CODEGRAPH_BIN_PATH="$(resolve_bin_path "$(PATH="$PATH_VALUE" command -v codegraph 2>/dev/null || true)")"

OXCODE_BIN="$OXCODE_BIN" CODEGRAPH_BIN="$CODEGRAPH_BIN_PATH" node "$ROOT/scripts/agent-eval/render-prompt.mjs" \
  --task-file "$TASK_FILE" \
  --task-id "$TASK_ID" \
  --arm "$ARM" \
  --out "$OUT" >/dev/null

PROMPT="$(cat "$OUT/prompt.txt")"

CODEX_VERSION="$("$CODEX_BIN" exec --version 2>/dev/null || "$CODEX_BIN" --version 2>/dev/null || echo unknown)"
OXCODE_VERSION="$([ -n "$OXCODE_BIN" ] && "$OXCODE_BIN" --version 2>/dev/null || echo unavailable)"
CODEGRAPH_VERSION="$([ -n "$CODEGRAPH_BIN_PATH" ] && "$CODEGRAPH_BIN_PATH" --version 2>/dev/null || echo unavailable)"
REPO_COMMIT="$(git -C "$REPO_PATH" rev-parse HEAD 2>/dev/null || echo unknown)"
START_MS="$(node -e 'console.log(Date.now())')"

set +e
PATH="$PATH_VALUE" HOME="$RUN_HOME" CODEX_HOME="$RUN_HOME" node "$ROOT/scripts/agent-eval/run-timed-command.mjs" \
  --stdout "$OUT/run.jsonl" \
  --stderr "$OUT/run.err" \
  --stdout-timeline "$OUT/run.timeline.jsonl" \
  --stderr-timeline "$OUT/run.stderr-timeline.jsonl" \
  --timing "$OUT/run.timing.json" \
  -- "$CODEX_BIN" --ask-for-approval never exec \
  --json --ignore-user-config --ignore-rules --ephemeral \
  -c shell_environment_policy.inherit=all \
  --sandbox "$SANDBOX" \
  --disable plugins --disable apps \
  --disable skill_mcp_dependency_install --disable tool_suggest \
  -C "$REPO_PATH" -m "$MODEL" \
  -o "$OUT/final-answer.txt" \
  "$PROMPT" > "$OUT/run.jsonl" 2> "$OUT/run.err"
CODEX_STATUS=$?
set -e
END_MS="$(node -e 'console.log(Date.now())')"
sync_refreshed_auth

META_ARGS=(
  --out "$OUT"
  --suite-id "$SUITE_ID"
  --task-id "$TASK_ID"
  --task-file "$TASK_FILE"
  --repo "$REPO_NAME"
  --repo-path "$REPO_PATH"
  --repo-commit "$REPO_COMMIT"
  --arm "$ARM"
  --run-index "$RUN_INDEX"
  --model "$MODEL"
  --sandbox "$SANDBOX"
  --codex-exit-code "$CODEX_STATUS"
  --start-ms "$START_MS"
  --end-ms "$END_MS"
  --codex-version "$CODEX_VERSION"
  --oxcode-version "$OXCODE_VERSION"
  --codegraph-version "$CODEGRAPH_VERSION"
  --path-prepend "$PATH_PREPEND"
  --oxcode-bin "$OXCODE_BIN"
  --codegraph-bin "$CODEGRAPH_BIN_PATH"
  --timeline-path "$OUT/run.timeline.jsonl"
  --stderr-timeline-path "$OUT/run.stderr-timeline.jsonl"
  --timing-path "$OUT/run.timing.json"
)
if [ -n "$REPLAY_RUN_ID" ]; then
  META_ARGS+=(--replay-run-id "$REPLAY_RUN_ID")
fi
node "$ROOT/scripts/agent-eval/write-run-metadata.mjs" "${META_ARGS[@]}" >/dev/null

if [ -n "$WORKSHOP_URL" ]; then
  node "$ROOT/scripts/agent-eval/codex-jsonl-to-otlp.mjs" \
    --run-dir "$OUT" \
    --workshop-url "$WORKSHOP_URL" \
    --post true > "$OUT/trace-ingest.json"
  node "$ROOT/scripts/agent-eval/export-metrics.mjs" \
    --workshop-url "$WORKSHOP_URL" \
    --suite-id "$SUITE_ID" \
    --task-file "$TASK_FILE" \
    --task-id "$TASK_ID" \
    --arm "$ARM" \
    --run-index "$RUN_INDEX" \
    --run-dir "$OUT" \
    --out "$OUT/metrics.json" > /dev/null
else
  node "$ROOT/scripts/agent-eval/codex-jsonl-to-otlp.mjs" \
    --run-dir "$OUT" \
    --post false > "$OUT/trace-ingest.json"
  node "$ROOT/scripts/agent-eval/export-metrics.mjs" \
    --suite-id "$SUITE_ID" \
    --task-file "$TASK_FILE" \
    --task-id "$TASK_ID" \
    --arm "$ARM" \
    --run-index "$RUN_INDEX" \
    --run-dir "$OUT" \
    --out "$OUT/metrics.json" > /dev/null
fi

exit "$CODEX_STATUS"

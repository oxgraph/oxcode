#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RUNS=1
MODEL="${CODEX_MODEL:-gpt-5.5}"
OUT="$ROOT/target/agent-eval/smoke-$(date +%Y%m%d-%H%M%S)"
AUTH_FILE="$ROOT/codex-auth.json"

while [ "$#" -gt 0 ]; do
  case "$1" in
    --out) OUT="$2"; shift 2 ;;
    --model) MODEL="$2"; shift 2 ;;
    --auth-file) AUTH_FILE="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 64 ;;
  esac
done

if [ ! -f "$AUTH_FILE" ] && [ "$AUTH_FILE" = "$ROOT/codex-auth.json" ] && [ -f "/Users/snowmead/.harnessing/chatgpt-auth.json" ]; then
  cp "/Users/snowmead/.harnessing/chatgpt-auth.json" "$AUTH_FILE"
  chmod 600 "$AUTH_FILE"
fi
[ -f "$AUTH_FILE" ] || { echo "missing auth file: $AUTH_FILE" >&2; exit 65; }

OUT="$(mkdir -p "$(dirname "$OUT")" && cd "$(dirname "$OUT")" && pwd)/$(basename "$OUT")"
SUITE_ID="$(basename "$OUT")"
TASK_FILE="$ROOT/tasks/smoke.yaml"
TASK_ID="smoke-entry-helper"
mkdir -p "$OUT/bin/oxcode" "$OUT/bin/codegraph" "$OUT/runs" "$OUT/corpus/source"

echo "### smoke suite $SUITE_ID"
echo "### building oxcode"
cargo build -p oxcode
ln -sf "$ROOT/target/debug/oxcode" "$OUT/bin/oxcode/oxcode"

SOURCE_REPO="$OUT/corpus/source/smoke-rust"
rm -rf "$SOURCE_REPO"
cp -R "$ROOT/fixtures/smoke-rust" "$SOURCE_REPO"

echo "### indexing smoke fixture with oxcode"
OXCODE_REPO="$OUT/corpus/oxcode-cli/smoke-rust"
mkdir -p "$(dirname "$OXCODE_REPO")"
cp -R "$SOURCE_REPO" "$OXCODE_REPO"
"$OUT/bin/oxcode/oxcode" index "$OXCODE_REPO" > "$OUT/oxcode-index.out" 2> "$OUT/oxcode-index.err"

ARMS=("empty" "oxcode-cli")
if [ -n "${CODEGRAPH_BIN:-}" ]; then
  ln -sf "$CODEGRAPH_BIN" "$OUT/bin/codegraph/codegraph"
  CODEGRAPH_REPO="$OUT/corpus/codegraph-cli/smoke-rust"
  mkdir -p "$(dirname "$CODEGRAPH_REPO")"
  cp -R "$SOURCE_REPO" "$CODEGRAPH_REPO"
  (cd "$CODEGRAPH_REPO" && "$OUT/bin/codegraph/codegraph" init . --index) > "$OUT/codegraph-index.out" 2> "$OUT/codegraph-index.err" || {
    "$OUT/bin/codegraph/codegraph" init "$CODEGRAPH_REPO" >> "$OUT/codegraph-index.out" 2>> "$OUT/codegraph-index.err" || true
    "$OUT/bin/codegraph/codegraph" index "$CODEGRAPH_REPO" >> "$OUT/codegraph-index.out" 2>> "$OUT/codegraph-index.err"
  }
  ARMS+=("codegraph-cli")
fi

WORKSHOP_URL="$(node "$ROOT/scripts/agent-eval/workshop-url.mjs" --start true)"
echo "$WORKSHOP_URL" > "$OUT/workshop-url.txt"
raindrop replay register --cwd="$ROOT" > "$OUT/replay-register.out" 2> "$OUT/replay-register.err"

for arm in "${ARMS[@]}"; do
  for run in $(seq 1 "$RUNS"); do
    run_dir="$OUT/runs/$TASK_ID/$arm/$run"
    path_prepend=""
    repo="$OUT/corpus/$arm/smoke-rust"
    if [ "$arm" = "empty" ] && [ ! -d "$repo" ]; then
      mkdir -p "$(dirname "$repo")"
      cp -R "$SOURCE_REPO" "$repo"
    fi
    if [ "$arm" = "oxcode-cli" ]; then path_prepend="$OUT/bin/oxcode"; fi
    if [ "$arm" = "codegraph-cli" ]; then path_prepend="$OUT/bin/codegraph"; fi
    echo "### smoke $arm run $run"
    "$ROOT/scripts/agent-eval/run-codex-arm.sh" \
      --task-file "$TASK_FILE" \
      --task-id "$TASK_ID" \
      --arm "$arm" \
      --run-index "$run" \
      --suite-id "$SUITE_ID" \
      --repo smoke-rust \
      --repo-path "$repo" \
      --out "$run_dir" \
      --model "$MODEL" \
      --workshop-url "$WORKSHOP_URL" \
      --auth-file "$AUTH_FILE" \
      --path-prepend "$path_prepend"
  done
done

node "$ROOT/scripts/agent-eval/export-metrics.mjs" \
  --workshop-url "$WORKSHOP_URL" \
  --suite-id "$SUITE_ID" \
  --task-file "$TASK_FILE" \
  --suite-dir "$OUT" \
  --out "$OUT/suite-metrics.json" > /dev/null

node "$ROOT/scripts/agent-eval/validate-smoke.mjs" \
  --suite-dir "$OUT" \
  --arms "$(IFS=,; echo "${ARMS[*]}")"

echo "### smoke passed: $OUT"

#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# Agent runs are high-variance; 6 runs/arm give the Student-t 95% CI that
# export-metrics reports enough degrees of freedom to be meaningful.
RUNS=6
MODEL="${CODEX_MODEL:-gpt-5.5}"
# Model for the blind LLM-as-judge grader. Defaults to the agent model (the only
# auth available); the judge is blind to the arm, so any same-model bias applies
# uniformly across arms and does not skew the cross-arm deltas we care about.
JUDGE_MODEL="${JUDGE_MODEL:-$MODEL}"
OUT="$ROOT/target/agent-eval/rust-$(date +%Y%m%d-%H%M%S)"
AUTH_FILE="$ROOT/codex-auth.json"
SKIP_SMOKE=0
CORPUS=""
RESUME_EXISTING=0
ARMS_ARG=""
TASK_FILE="$ROOT/tasks/rust.yaml"
TASKS_ARG=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --runs) RUNS="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    --model) MODEL="$2"; shift 2 ;;
    --auth-file) AUTH_FILE="$2"; shift 2 ;;
    --skip-smoke) SKIP_SMOKE=1; shift ;;
    --resume-existing) RESUME_EXISTING=1; shift ;;
    --corpus) CORPUS="$2"; shift 2 ;;
    --arms) ARMS_ARG="$2"; shift 2 ;;
    --task-file) TASK_FILE="$2"; shift 2 ;;
    --tasks) TASKS_ARG="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 64 ;;
  esac
done

if [ "$SKIP_SMOKE" -ne 1 ]; then
  "$ROOT/scripts/agent-eval/smoke.sh" \
    --model "$MODEL" \
    --auth-file "$AUTH_FILE" \
    --out "$OUT-smoke"
fi

if [ ! -f "$AUTH_FILE" ] && [ "$AUTH_FILE" = "$ROOT/codex-auth.json" ] && [ -f "/Users/snowmead/.harnessing/chatgpt-auth.json" ]; then
  cp "/Users/snowmead/.harnessing/chatgpt-auth.json" "$AUTH_FILE"
  chmod 600 "$AUTH_FILE"
fi
[ -f "$AUTH_FILE" ] || { echo "missing auth file: $AUTH_FILE" >&2; exit 65; }

OUT="$(mkdir -p "$(dirname "$OUT")" && cd "$(dirname "$OUT")" && pwd)/$(basename "$OUT")"
SUITE_ID="$(basename "$OUT")"
CORPUS="${CORPUS:-$OUT/corpus}"
mkdir -p "$OUT/bin/oxcode" "$OUT/bin/codegraph" "$OUT/runs" "$CORPUS/sources"

echo "### benchmark suite $SUITE_ID RUNS=$RUNS"
echo "### building oxcode"
cargo build -p oxcode
ln -sf "$ROOT/target/debug/oxcode" "$OUT/bin/oxcode/oxcode"

ARMS=("empty" "oxcode-cli")
if [ -n "${CODEGRAPH_BIN:-}" ]; then
  ln -sf "$CODEGRAPH_BIN" "$OUT/bin/codegraph/codegraph"
  ARMS+=("codegraph-cli")
fi
if [ -n "$ARMS_ARG" ]; then
  IFS=',' read -r -a ARMS <<< "$ARMS_ARG"
fi

WORKSHOP_URL="$(node "$ROOT/scripts/agent-eval/workshop-url.mjs" --start true)"
echo "$WORKSHOP_URL" > "$OUT/workshop-url.txt"
raindrop replay register --cwd="$ROOT" > "$OUT/replay-register.out" 2> "$OUT/replay-register.err"

task_enabled() {
  local task_id="$1"
  if [ -z "$TASKS_ARG" ]; then
    return 0
  fi
  IFS=',' read -r -a enabled_tasks <<< "$TASKS_ARG"
  for enabled in "${enabled_tasks[@]}"; do
    if [ "$enabled" = "$task_id" ]; then
      return 0
    fi
  done
  return 1
}

prepare_arm_repo() {
  local source_repo="$1"
  local dest_repo="$2"
  if [ -d "$dest_repo/.git" ]; then
    return 0
  fi
  rm -rf "$dest_repo"
  mkdir -p "$(dirname "$dest_repo")"
  if [ -d "$source_repo/.git" ]; then
    git clone --shared "$source_repo" "$dest_repo" >/dev/null
  else
    cp -R "$source_repo" "$dest_repo"
  fi
}

TASK_JSON_LINES=()
while IFS= read -r task_json_line; do
  TASK_JSON_LINES+=("$task_json_line")
done < <(node "$ROOT/scripts/agent-eval/list-tasks.mjs" --task-file "$TASK_FILE")
for task_json in "${TASK_JSON_LINES[@]}"; do
  task_id="$(node -e 'const t=JSON.parse(process.argv[1]); console.log(t.id)' "$task_json")"
  if ! task_enabled "$task_id"; then
    continue
  fi
  repo_name="$(node -e 'const t=JSON.parse(process.argv[1]); console.log(t.repo)' "$task_json")"
  repo_url="$(node -e 'const t=JSON.parse(process.argv[1]); console.log(t.repo_url || "")' "$task_json")"
  source_repo="$CORPUS/sources/$repo_name"
  if [ ! -d "$source_repo/.git" ]; then
    [ -n "$repo_url" ] || { echo "task $task_id missing repo_url and repo not found at $source_repo" >&2; exit 66; }
    echo "### cloning $repo_name"
    git clone --depth 1 "$repo_url" "$source_repo"
  fi

  echo "### indexing $repo_name with oxcode"
  oxcode_repo="$CORPUS/$repo_name/oxcode-cli"
  prepare_arm_repo "$source_repo" "$oxcode_repo"
  "$OUT/bin/oxcode/oxcode" index --path "$oxcode_repo" > "$OUT/$repo_name-oxcode-index.out" 2> "$OUT/$repo_name-oxcode-index.err"

  if [ -n "${CODEGRAPH_BIN:-}" ]; then
    echo "### indexing $repo_name with codegraph"
    codegraph_repo="$CORPUS/$repo_name/codegraph-cli"
    prepare_arm_repo "$source_repo" "$codegraph_repo"
    (cd "$codegraph_repo" && "$OUT/bin/codegraph/codegraph" init . --index) > "$OUT/$repo_name-codegraph-index.out" 2> "$OUT/$repo_name-codegraph-index.err" || {
      "$OUT/bin/codegraph/codegraph" init "$codegraph_repo" >> "$OUT/$repo_name-codegraph-index.out" 2>> "$OUT/$repo_name-codegraph-index.err" || true
      "$OUT/bin/codegraph/codegraph" index "$codegraph_repo" >> "$OUT/$repo_name-codegraph-index.out" 2>> "$OUT/$repo_name-codegraph-index.err"
    }
  fi

  for arm in "${ARMS[@]}"; do
    repo_path="$CORPUS/$repo_name/$arm"
    prepare_arm_repo "$source_repo" "$repo_path"
    for run in $(seq 1 "$RUNS"); do
      run_dir="$OUT/runs/$task_id/$arm/$run"
      if [ "$RESUME_EXISTING" -eq 1 ] && [ -f "$run_dir/metrics.json" ]; then
        echo "### $task_id $arm run $run already has metrics, skipping"
        continue
      fi
      path_prepend=""
      if [ "$arm" = "oxcode-cli" ]; then path_prepend="$OUT/bin/oxcode"; fi
      if [ "$arm" = "codegraph-cli" ]; then path_prepend="$OUT/bin/codegraph"; fi
      echo "### $task_id $arm run $run"
      "$ROOT/scripts/agent-eval/run-codex-arm.sh" \
        --task-file "$TASK_FILE" \
        --task-id "$task_id" \
        --arm "$arm" \
        --run-index "$run" \
        --suite-id "$SUITE_ID" \
        --repo "$repo_name" \
        --repo-path "$repo_path" \
        --out "$run_dir" \
        --model "$MODEL" \
        --workshop-url "$WORKSHOP_URL" \
        --auth-file "$AUTH_FILE" \
        --path-prepend "$path_prepend" || true
    done
  done
done

# Blind LLM-as-judge grading: scores each run's final answer against the task
# rubric without seeing the arm, writing a cached grade.json per run. Slow but
# cached, so re-running export-metrics is free. Failures here leave runs ungraded
# (quality_score null) rather than aborting the suite.
echo "### grading answers (blind LLM judge, model=$JUDGE_MODEL)"
node "$ROOT/scripts/agent-eval/grade-answer.mjs" \
  --suite-dir "$OUT" \
  --task-file "$TASK_FILE" \
  --auth-file "$AUTH_FILE" \
  --judge-model "$JUDGE_MODEL" \
  --concurrency 4 || echo "### grading had failures; ungraded runs will show null quality"

node "$ROOT/scripts/agent-eval/export-metrics.mjs" \
  --workshop-url "$WORKSHOP_URL" \
  --suite-id "$SUITE_ID" \
  --task-file "$TASK_FILE" \
  --suite-dir "$OUT" \
  --out "$OUT/suite-metrics.json" > /dev/null

echo "### benchmark complete: $OUT"

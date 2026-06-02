#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
STOP_WORKSHOP=0
TARGET=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --stop-workshop) STOP_WORKSHOP=1; shift ;;
    *) TARGET="$1"; shift ;;
  esac
done

[ -n "$TARGET" ] || { echo "usage: clean-suite.sh <suite-id-or-path> [--stop-workshop]" >&2; exit 64; }

AGENT_EVAL_ROOT="$(cd "$ROOT/target/agent-eval" 2>/dev/null || { mkdir -p "$ROOT/target/agent-eval"; cd "$ROOT/target/agent-eval"; } && pwd)"
if [[ "$TARGET" = /* ]]; then
  TARGET_PATH="$(node -e 'console.log(require("path").resolve(process.argv[1]))' "$TARGET")"
else
  case "$TARGET" in
    *..*|*/*|"") echo "refusing unsafe suite id: $TARGET" >&2; exit 65 ;;
  esac
  TARGET_PATH="$AGENT_EVAL_ROOT/$TARGET"
fi

case "$TARGET_PATH" in
  "$AGENT_EVAL_ROOT"/*) ;;
  *) echo "refusing to delete outside $AGENT_EVAL_ROOT: $TARGET_PATH" >&2; exit 65 ;;
esac
[ "$TARGET_PATH" != "$AGENT_EVAL_ROOT" ] || { echo "refusing to delete agent-eval root" >&2; exit 65; }
rm -rf "$TARGET_PATH"

if [ "$STOP_WORKSHOP" -eq 1 ]; then
  raindrop workshop stop || true
fi

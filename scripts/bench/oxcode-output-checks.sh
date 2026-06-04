#!/usr/bin/env bash
#
# oxcode-output-checks.sh — code-aware correctness assertions on oxcode's --json
# output. This evaluates the TOOL directly (no agent), so it is the floor that
# protects oxcode's contract: every check is a bug we refuse to reintroduce.
#
# It builds oxcode (release), indexes a target repo (default: the committed
# fixtures/smoke-rust crate; override with --path <repo> to point at a larger
# real repo), then runs a table of (command, expectation) checks and asserts
# each one against the JSON, exiting non-zero on any failure.
#
# Usage:
#   scripts/bench/oxcode-output-checks.sh [--path <repo>] [--bin <oxcode>] [--no-build]
#
# Notes:
#   - The default smoke fixture names its crate "smoke-rust", so qualified names
#     are prefixed "smoke_rust::" (NOT "crate::"; that prefix is only used by the
#     unnamed temp crates in crates/oxcode-cli/tests/cli.rs).
#   - JSON is parsed with jq when available, else with a python3 fallback. The
#     two paths are kept behaviorally identical.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

REPO_SRC="$ROOT/fixtures/smoke-rust"
BIN=""
BUILD=1
# The crate prefix and selectors below match the default smoke fixture. When you
# point --path at a different repo these expectations no longer hold, so we run a
# reduced, repo-agnostic set of checks (presence/agent-safety) instead.
IS_DEFAULT_FIXTURE=1

while [ "$#" -gt 0 ]; do
  case "$1" in
    --path) REPO_SRC="$2"; IS_DEFAULT_FIXTURE=0; shift 2 ;;
    --bin) BIN="$2"; shift 2 ;;
    --no-build) BUILD=0; shift ;;
    -h|--help)
      sed -n '2,30p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 64 ;;
  esac
done

if [ ! -d "$REPO_SRC" ]; then
  echo "target repo does not exist: $REPO_SRC" >&2
  exit 66
fi

# ---- JSON parsing: prefer jq, fall back to python3 -------------------------
JSON_TOOL=""
if [ -n "${FORCE_PY:-}" ]; then JSON_TOOL=python3; elif command -v jq >/dev/null 2>&1; then
  JSON_TOOL="jq"
elif command -v python3 >/dev/null 2>&1; then
  JSON_TOOL="python3"
else
  echo "need either jq or python3 to parse JSON output" >&2
  exit 69
fi

# Each probe is expressed twice — once as a jq filter, once as a named "op" for
# the python3 fallback. Both engines must return the same scalar so the
# assertions are engine-independent. The python path uses a small, fixed set of
# operations (no eval/exec) so there is no arbitrary-code-execution surface.

jq_get() { # <file> <jq-filter>
  jq -r "$2" "$1"
}

# py_get <file> <op> [args...]
#   ops:
#     scalar <dotted.path>                     -> the value at path
#     len <dotted.path>                        -> length of array at path (or 0)
#     first_field <list.path> <key> <val> <out>-> field <out> of first item where item[key]==val, else MISSING
#     has_nonempty <list.path> <key> <val> <out> -> "yes" if that field is non-empty/non-null else "no"
#     any_field <list.path> <key> <val>        -> "true" if any item has item[key]==val else "false"
py_get() {
  local file="$1" op="$2"; shift 2
  python3 - "$file" "$op" "$@" <<'PY'
import json, sys

with open(sys.argv[1]) as fh:
    d = json.load(fh)
op = sys.argv[2]
rest = sys.argv[3:]

def dig(root, dotted):
    cur = root
    for part in [p for p in dotted.split(".") if p]:
        if isinstance(cur, dict):
            cur = cur.get(part)
        else:
            return None
    return cur

def emit(val):
    if isinstance(val, bool):
        print("true" if val else "false")
    elif val is None:
        print("null")
    else:
        print(val)

if op == "scalar":
    emit(dig(d, rest[0]))
elif op == "len":
    val = dig(d, rest[0])
    emit(len(val) if isinstance(val, list) else 0)
elif op in ("first_field", "has_nonempty"):
    items = dig(d, rest[0]) or []
    key, want, out = rest[1], rest[2], rest[3]
    match = next((it for it in items if isinstance(it, dict) and dig(it, key) == want), None)
    field = dig(match, out) if isinstance(match, dict) else None
    if op == "first_field":
        emit(field if field is not None else "MISSING")
    else:
        emit("yes" if (field not in (None, "")) else "no")
elif op == "any_field":
    items = dig(d, rest[0]) or []
    key, want = rest[1], rest[2]
    emit(any(isinstance(it, dict) and dig(it, key) == want for it in items))
else:
    print("UNKNOWN_OP", file=sys.stderr)
    sys.exit(2)
PY
}

# ---- build / locate the binary ---------------------------------------------
if [ -z "$BIN" ]; then
  if [ "$BUILD" -eq 1 ]; then
    echo "### building oxcode (release)"
    cargo build -p oxcode --release >&2
  fi
  BIN="$ROOT/target/release/oxcode"
fi
if [ ! -x "$BIN" ]; then
  echo "oxcode binary not found or not executable: $BIN" >&2
  echo "(build it with: cargo build -p oxcode --release, or pass --bin)" >&2
  exit 67
fi

# ---- isolated copy + index --------------------------------------------------
WORK="$(mktemp -d "${TMPDIR:-/tmp}/oxcode-output-checks.XXXXXX")"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

REPO="$WORK/repo"
# rsync if present (skip vendored index/build dirs); else cp -R then prune.
if command -v rsync >/dev/null 2>&1; then
  rsync -a --exclude target --exclude .git --exclude .oxcode "$REPO_SRC/" "$REPO/"
else
  cp -R "$REPO_SRC" "$REPO"
  rm -rf "$REPO/target" "$REPO/.git" "$REPO/.oxcode"
fi

echo "### indexing $REPO_SRC"
"$BIN" index --path "$REPO" >/dev/null

# ---- check harness ----------------------------------------------------------
PASS=0
FAIL=0
declare -a FAILURES=()

# run_json <out-file> <args...> : run oxcode <args> --json, capture stdout
run_json() {
  local out="$1"; shift
  "$BIN" "$@" --json --path "$REPO" >"$out" 2>"$out.err" || true
}

# get <out-file> <jq-filter> <py-op> [py-args...] : print scalar via active engine
get() {
  local file="$1" jqf="$2"; shift 2
  if [ "$JSON_TOOL" = "jq" ]; then
    jq_get "$file" "$jqf"
  else
    py_get "$file" "$@"
  fi
}

# check <label> <actual> <expected>
check() {
  local label="$1" actual="$2" expected="$3"
  if [ "$actual" = "$expected" ]; then
    PASS=$((PASS + 1))
    printf '  PASS  %s\n' "$label"
  else
    FAIL=$((FAIL + 1))
    FAILURES+=("$label (expected [$expected], got [$actual])")
    printf '  FAIL  %s\n        expected [%s], got [%s]\n' "$label" "$expected" "$actual"
  fi
}

echo "### running output checks (engine: $JSON_TOOL)"

# === Anti-hallucination: a non-existent selector must NOT be invented ========
# This guarantee is repo-agnostic, so we always run it.
NF="$WORK/not_found.json"
run_json "$NF" symbol "name:Nonexistent_DoesNotExist_ZZZ"
check "non-existent selector -> status=not_found" \
  "$(get "$NF" '.status' scalar status)" "not_found"
check "non-existent selector -> empty matches (invents nothing)" \
  "$(get "$NF" '(.matches | length)' len matches)" "0"

if [ "$IS_DEFAULT_FIXTURE" -ne 1 ]; then
  echo
  echo "### NOTE: --path points at a non-default repo; ran only the"
  echo "###       repo-agnostic anti-hallucination checks above."
  echo "###       The named-symbol expectations below are smoke-fixture specific."
else
  # === symbols "helper" => a match named helper with non-empty preview =======
  SYM="$WORK/symbols_helper.json"
  run_json "$SYM" symbols "helper"
  # first match whose symbol.name == "helper"
  HELPER_NAME="$(get "$SYM" \
    'first(.matches[] | select(.symbol.name == "helper") | .symbol.name) // "MISSING"' \
    first_field matches symbol.name helper symbol.name)"
  check 'symbols "helper" -> a match named helper' "$HELPER_NAME" "helper"
  HELPER_SIG_OK="$(get "$SYM" \
    '([.matches[] | select(.symbol.name=="helper") | .symbol.signature] | first | if (. != null and . != "") then "yes" else "no" end)' \
    has_nonempty matches symbol.name helper symbol.signature)"
  check 'symbols "helper" -> non-empty signature' "$HELPER_SIG_OK" "yes"
  HELPER_PREVIEW_OK="$(get "$SYM" \
    '([.matches[] | select(.symbol.name=="helper") | .symbol.source_preview] | first | if (. != null and . != "") then "yes" else "no" end)' \
    has_nonempty matches symbol.name helper symbol.source_preview)"
  check 'symbols "helper" -> non-empty source_preview' "$HELPER_PREVIEW_OK" "yes"

  # === symbol smoke_rust::entry => matched, defined in src/lib.rs ============
  ENTRY="$WORK/symbol_entry.json"
  run_json "$ENTRY" symbol "smoke_rust::entry"
  check "symbol smoke_rust::entry -> status=matched" \
    "$(get "$ENTRY" '.status' scalar status)" "matched"
  check "symbol smoke_rust::entry -> defined in src/lib.rs" \
    "$(get "$ENTRY" '.report.symbol.definition.file_path' scalar report.symbol.definition.file_path)" \
    "src/lib.rs"

  # === calls smoke_rust::entry => outgoing edge to helper ====================
  CALLS="$WORK/calls_entry.json"
  run_json "$CALLS" calls "smoke_rust::entry"
  check "calls smoke_rust::entry -> direction=outgoing" \
    "$(get "$CALLS" '.report.direction' scalar report.direction)" "outgoing"
  check "calls smoke_rust::entry -> outgoing edge target is helper" \
    "$(get "$CALLS" \
      '([.report.edges[] | select(.target.name=="helper")] | length > 0)' \
      any_field report.edges target.name helper)" \
    "true"

  # === callers smoke_rust::helper => incoming edge from entry ================
  CALLERS="$WORK/callers_helper.json"
  run_json "$CALLERS" callers "smoke_rust::helper"
  check "callers smoke_rust::helper -> direction=incoming" \
    "$(get "$CALLERS" '.report.direction' scalar report.direction)" "incoming"
  check "callers smoke_rust::helper -> incoming edge source is entry" \
    "$(get "$CALLERS" \
      '([.report.edges[] | select(.source.name=="entry")] | length > 0)' \
      any_field report.edges source.name entry)" \
    "true"

  # === agent-safe selector outcomes ==========================================
  # The smoke fixture has no duplicated symbol name, so a true `ambiguous`
  # status can't be produced from it (see crates/oxcode-cli/tests/cli.rs
  # `selector_discovery_outcomes_are_agent_safe` for the ambiguous case on a
  # purpose-built crate). Here we lock the `not_found` half of the guarantee on
  # a qualified selector too, proving oxcode reports not_found instead of
  # snapping to a near-name.
  QNF="$WORK/qualified_not_found.json"
  run_json "$QNF" symbol "smoke_rust::does_not_exist"
  check "qualified non-existent selector -> status=not_found" \
    "$(get "$QNF" '.status' scalar status)" "not_found"
  check "qualified non-existent selector -> empty matches" \
    "$(get "$QNF" '(.matches | length)' len matches)" "0"
fi

# ---- summary ----------------------------------------------------------------
echo
echo "================ oxcode output checks ================"
echo "  target: $REPO_SRC"
echo "  passed: $PASS"
echo "  failed: $FAIL"
if [ "$FAIL" -gt 0 ]; then
  echo "  failures:"
  for f in "${FAILURES[@]}"; do echo "    - $f"; done
  echo "====================================================="
  echo "RESULT: FAIL"
  exit 1
fi
echo "====================================================="
echo "RESULT: PASS"

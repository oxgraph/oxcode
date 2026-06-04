#!/usr/bin/env bash
#
# oxcode-perf.sh — reproducible latency bench for oxcode indexing + query, so the
# oxgraph 0.3.0 gains documented in BENCHMARKS.md stay tracked and protected.
#
# It works on a FRESH COPY of a real Rust repo (rsync, excluding target/.git/
# .oxcode) and times:
#   1. cold index            (index a never-indexed copy)
#   2. reindex-unchanged     (re-index with no source change -> should be O(1)-ish)
#   3. reindex-after-1-edit  (append a unique marker fn to a real */src/*.rs file,
#                             reindex, then VERIFY the marker symbol is now found —
#                             a fast-but-empty reindex is meaningless, so we fail
#                             loudly if the marker did not persist)
#   4. query p50             (median of 5 `oxcode symbols "<keywords>"` runs)
# It also records DB size and delta-log bytes.
#
# Usage:
#   scripts/bench/oxcode-perf.sh --path <repo> \
#       [--bin <oxcode>] [--no-build] \
#       [--keywords "<symbol query>"] [--query-runs N] \
#       [--max-reindex-ms <N>] [--max-query-ms <N>]
#
# With --max-reindex-ms / --max-query-ms set, the script exits non-zero when the
# reindex-after-1-edit time or the query p50 exceeds the threshold (regression
# guard). With no thresholds it just reports.
#
# CRITICAL methodology (these are real gotchas that produced bogus numbers):
#   - macOS has NO `timeout`. We wrap every CLI call in a python3
#     subprocess.run(timeout=) so a hang fails the bench instead of wedging.
#   - Timing is python3 time.time() around the CLI (wall clock), not `time`.
#   - The reindex is persistence-verified by querying the appended marker.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

REPO_SRC=""
BIN=""
BUILD=1
KEYWORDS=""
QUERY_RUNS=5
MAX_REINDEX_MS=""
MAX_QUERY_MS=""
CMD_TIMEOUT_S=900   # per-CLI-call hard cap

while [ "$#" -gt 0 ]; do
  case "$1" in
    --path) REPO_SRC="$2"; shift 2 ;;
    --bin) BIN="$2"; shift 2 ;;
    --no-build) BUILD=0; shift ;;
    --keywords) KEYWORDS="$2"; shift 2 ;;
    --query-runs) QUERY_RUNS="$2"; shift 2 ;;
    --max-reindex-ms) MAX_REINDEX_MS="$2"; shift 2 ;;
    --max-query-ms) MAX_QUERY_MS="$2"; shift 2 ;;
    --cmd-timeout) CMD_TIMEOUT_S="$2"; shift 2 ;;
    -h|--help)
      sed -n '2,34p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 64 ;;
  esac
done

if [ -z "$REPO_SRC" ]; then
  cat >&2 <<'MSG'
oxcode-perf.sh needs a real Rust repo to index.

  scripts/bench/oxcode-perf.sh --path <repo> [--max-reindex-ms N] [--max-query-ms N]

Point --path at a checked-out Rust crate/workspace (e.g. a clone of tokio,
ripgrep, hyper, or cargo). The smoke fixture is too small to produce meaningful
latency numbers, so this script intentionally does not default to it.
MSG
  exit 64
fi
if [ ! -d "$REPO_SRC" ]; then
  echo "target repo does not exist: $REPO_SRC" >&2
  exit 66
fi
if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required (used for safe timeouts + wall-clock timing)" >&2
  exit 69
fi

# ---- build / locate the binary ---------------------------------------------
if [ -z "$BIN" ]; then
  if [ "$BUILD" -eq 1 ]; then
    echo "### building oxcode (release)" >&2
    cargo build -p oxcode --release >&2
  fi
  BIN="$ROOT/target/release/oxcode"
fi
if [ ! -x "$BIN" ]; then
  echo "oxcode binary not found or not executable: $BIN" >&2
  echo "(build it with: cargo build -p oxcode --release, or pass --bin)" >&2
  exit 67
fi

# ---- fresh copy (exclude target/.git/.oxcode) ------------------------------
WORK="$(mktemp -d "${TMPDIR:-/tmp}/oxcode-perf.XXXXXX")"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

REPO="$WORK/repo"
echo "### copying $REPO_SRC -> fresh tree (excluding target/.git/.oxcode)" >&2
if command -v rsync >/dev/null 2>&1; then
  rsync -a --exclude target --exclude .git --exclude .oxcode "$REPO_SRC/" "$REPO/"
else
  cp -R "$REPO_SRC" "$REPO"
  rm -rf "$REPO/target" "$REPO/.git" "$REPO/.oxcode"
fi

# ---- timed runner -----------------------------------------------------------
# timed_run <timeout_s> -- <argv...>
#   Runs argv under a python subprocess.run(timeout=) wrapper, times it with
#   time.time(), prints elapsed milliseconds (integer) to stdout. A non-zero
#   exit or a timeout aborts the bench (set -e via the captured rc).
timed_run() {
  local to="$1"; shift
  [ "$1" = "--" ] && shift
  python3 - "$to" "$@" <<'PY'
import subprocess, sys, time
timeout = float(sys.argv[1])
argv = sys.argv[2:]
start = time.time()
try:
    proc = subprocess.run(
        argv,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        timeout=timeout,
    )
except subprocess.TimeoutExpired:
    elapsed = (time.time() - start) * 1000.0
    sys.stderr.write(f"TIMEOUT after {elapsed:.0f} ms: {' '.join(argv)}\n")
    sys.exit(124)
elapsed = (time.time() - start) * 1000.0
if proc.returncode != 0:
    sys.stderr.write(proc.stderr.decode("utf-8", "replace"))
    sys.stderr.write(f"FAILED rc={proc.returncode}: {' '.join(argv)}\n")
    sys.exit(proc.returncode)
print(f"{elapsed:.0f}")
PY
}

# query_symbol_found <keywords> <symbol-name>
#   Runs `oxcode symbols <keywords> --json` and prints "yes"/"no" depending on
#   whether any returned match has symbol.name == <symbol-name>.
#   NOTE: oxcode's JSON is written to a temp file, NOT piped into `python3 -`.
#   `python3 -` reads its PROGRAM from stdin, so piping JSON in too makes the two
#   collide; reading from a file keeps stdin free for the heredoc program.
query_symbol_found() {
  local kw="$1" want="$2"
  local out="$WORK/verify-query.json"
  "$BIN" symbols "$kw" --json --path "$REPO" >"$out" 2>/dev/null || true
  python3 - "$out" "$want" <<'PY'
import json, sys
path, want = sys.argv[1], sys.argv[2]
try:
    with open(path) as fh:
        data = json.load(fh)
except Exception:
    print("no"); sys.exit(0)
matches = data.get("matches") or []
found = any((m.get("symbol") or {}).get("name") == want for m in matches)
print("yes" if found else "no")
PY
}

# dir_bytes <dir> : total size in bytes of all files under dir (0 if missing)
dir_bytes() {
  local d="$1"
  [ -d "$d" ] || { echo 0; return; }
  find "$d" -type f -print0 2>/dev/null \
    | xargs -0 stat -f '%z' 2>/dev/null \
    | awk '{ s += $1 } END { print s + 0 }'
}

# delta_log_bytes <oxgdb-dir> : total size of delta-*.log files
delta_log_bytes() {
  local d="$1"
  [ -d "$d" ] || { echo 0; return; }
  find "$d" -type f -name 'delta-*.log' -print0 2>/dev/null \
    | xargs -0 stat -f '%z' 2>/dev/null \
    | awk '{ s += $1 } END { print s + 0 }'
}

OXGDB_DIR="$REPO/.oxcode/index.oxgdb"

# ---- pick query keywords ----------------------------------------------------
# Default to a real symbol from the repo so the p50 query exercises the index.
if [ -z "$KEYWORDS" ]; then
  KEYWORDS="$(python3 - "$REPO" <<'PY'
import os, re, sys
root = sys.argv[1]
# Grab the first plausible fn name from a src/*.rs file for a representative query.
fn_re = re.compile(r"\bfn\s+([a-z][a-z0-9_]{3,})\s*[(<]")
for dirpath, dirnames, filenames in os.walk(root):
    if os.sep + "target" in dirpath or os.sep + ".git" in dirpath:
        continue
    if os.sep + "src" not in dirpath + os.sep:
        continue
    for name in filenames:
        if not name.endswith(".rs"):
            continue
        try:
            with open(os.path.join(dirpath, name), encoding="utf-8", errors="ignore") as fh:
                text = fh.read()
        except OSError:
            continue
        m = fn_re.search(text)
        if m:
            print(m.group(1).replace("_", " "))
            sys.exit(0)
print("new")
PY
)"
fi
echo "### query keywords: \"$KEYWORDS\"" >&2

# ============================================================================
# 1. COLD INDEX
# ============================================================================
echo "### [1/4] cold index" >&2
COLD_MS="$(timed_run "$CMD_TIMEOUT_S" -- "$BIN" index --path "$REPO")"
DB_BYTES="$(dir_bytes "$OXGDB_DIR")"
DELTA_BYTES_COLD="$(delta_log_bytes "$OXGDB_DIR")"

# ============================================================================
# 2. REINDEX, NO CHANGE
# ============================================================================
echo "### [2/4] reindex unchanged" >&2
REINDEX_NOCHANGE_MS="$(timed_run "$CMD_TIMEOUT_S" -- "$BIN" index --path "$REPO")"

# ============================================================================
# 3. REINDEX AFTER 1-FILE EDIT (persistence-verified)
# ============================================================================
echo "### [3/4] reindex after 1-file edit" >&2
# Find a real */src/*.rs file to mutate.
TARGET_RS="$(find "$REPO" -type f -name '*.rs' -path '*/src/*' \
  ! -path '*/target/*' 2>/dev/null | head -n 1)"
if [ -z "$TARGET_RS" ]; then
  TARGET_RS="$(find "$REPO" -type f -name '*.rs' ! -path '*/target/*' 2>/dev/null | head -n 1)"
fi
if [ -z "$TARGET_RS" ]; then
  echo "no *.rs file found under $REPO to edit" >&2
  exit 70
fi
MARKER="oxcode_perf_marker_$(date +%s)_$$"
printf '\n\npub fn %s() -> u32 { 0 }\n' "$MARKER" >> "$TARGET_RS"
echo "### appended marker fn $MARKER to ${TARGET_RS#$REPO/}" >&2

REINDEX_EDIT_MS="$(timed_run "$CMD_TIMEOUT_S" -- "$BIN" index --path "$REPO")"
DELTA_BYTES_EDIT="$(delta_log_bytes "$OXGDB_DIR")"
DB_BYTES_AFTER="$(dir_bytes "$OXGDB_DIR")"

# Persistence verification: the marker symbol MUST now be findable. A reindex
# that "succeeded" quickly but dropped the change is a silent corruption.
FOUND="$(query_symbol_found "$MARKER" "$MARKER")"
if [ "$FOUND" != "yes" ]; then
  echo >&2
  echo "FATAL: reindex did not persist the appended marker symbol '$MARKER'." >&2
  echo "       A fast reindex that loses edits is worse than a slow one." >&2
  exit 71
fi
echo "### marker symbol persisted and is queryable (reindex verified)" >&2

# ============================================================================
# 4. QUERY p50
# ============================================================================
echo "### [4/4] query p50 over $QUERY_RUNS runs" >&2
QUERY_SAMPLES=()
for i in $(seq 1 "$QUERY_RUNS"); do
  ms="$(timed_run "$CMD_TIMEOUT_S" -- "$BIN" symbols "$KEYWORDS" --path "$REPO")"
  QUERY_SAMPLES+=("$ms")
done
QUERY_P50_MS="$(python3 - "${QUERY_SAMPLES[@]}" <<'PY'
import sys
vals = sorted(float(x) for x in sys.argv[1:])
n = len(vals)
mid = n // 2
p50 = (vals[mid - 1] + vals[mid]) / 2 if n % 2 == 0 else vals[mid]
print(f"{p50:.0f}")
PY
)"

# ---- pretty bytes -----------------------------------------------------------
human() {
  python3 - "$1" <<'PY'
import sys
n = float(sys.argv[1])
for unit in ("B", "KiB", "MiB", "GiB"):
    if n < 1024 or unit == "GiB":
        print(f"{n:.1f} {unit}" if unit != "B" else f"{int(n)} B")
        break
    n /= 1024
PY
}

# ---- report -----------------------------------------------------------------
echo
echo "==================== oxcode perf ===================="
echo "  repo            : $REPO_SRC"
echo "  binary          : $BIN"
echo "  query keywords  : \"$KEYWORDS\" (p50 over $QUERY_RUNS runs)"
echo "----------------------------------------------------"
printf "  %-26s %10s\n" "metric" "value"
printf "  %-26s %8s ms\n" "cold index"            "$COLD_MS"
printf "  %-26s %8s ms\n" "reindex, no change"     "$REINDEX_NOCHANGE_MS"
printf "  %-26s %8s ms\n" "reindex after 1 edit"   "$REINDEX_EDIT_MS"
printf "  %-26s %8s ms\n" "symbol query (p50)"     "$QUERY_P50_MS"
echo "----------------------------------------------------"
printf "  %-26s %12s\n" "db size"                  "$(human "$DB_BYTES_AFTER")"
printf "  %-26s %12s\n" "delta-log (after cold)"   "$(human "$DELTA_BYTES_COLD")"
printf "  %-26s %12s\n" "delta-log (after edit)"   "$(human "$DELTA_BYTES_EDIT")"
echo "===================================================="

# ---- regression guards ------------------------------------------------------
RC=0
if [ -n "$MAX_REINDEX_MS" ]; then
  if [ "$REINDEX_EDIT_MS" -gt "$MAX_REINDEX_MS" ]; then
    echo "REGRESSION: reindex-after-1-edit ${REINDEX_EDIT_MS} ms > --max-reindex-ms ${MAX_REINDEX_MS} ms" >&2
    RC=1
  else
    echo "ok: reindex-after-1-edit ${REINDEX_EDIT_MS} ms <= ${MAX_REINDEX_MS} ms"
  fi
fi
if [ -n "$MAX_QUERY_MS" ]; then
  if [ "$QUERY_P50_MS" -gt "$MAX_QUERY_MS" ]; then
    echo "REGRESSION: query p50 ${QUERY_P50_MS} ms > --max-query-ms ${MAX_QUERY_MS} ms" >&2
    RC=1
  else
    echo "ok: query p50 ${QUERY_P50_MS} ms <= ${MAX_QUERY_MS} ms"
  fi
fi

if [ "$RC" -ne 0 ]; then
  echo "RESULT: FAIL (threshold exceeded)"
else
  echo "RESULT: PASS"
fi
exit "$RC"

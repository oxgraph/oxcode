# Codex CLI Agent Benchmark Results

This benchmark measures whether telling Codex about `oxcode` improves
codebase-understanding answers compared with normal shell exploration, and how
that compares with a CodeGraph CLI arm when available.

Wall clock is retained as a diagnostic only. The headline metrics are answer
quality, success rate, command reduction, indexed CLI reliability, and observed
tool execution time captured from streamed Codex JSONL events.

## Benchmark Definition

Each run renders one prompt:

```text
<prompts/common.md>

<prompts/arms/<arm>.md>

Question:
<task.question>
```

`prompts/common.md` is byte-identical across arms. Only the arm-specific block
changes, and every run records the common, arm, and final prompt hashes.

Quality is deterministic from `tasks/*.yaml`:

```text
quality_score =
  required_concept_hit_rate * 0.50
  + expected_symbol_hit_rate * 0.30
  + expected_file_hit_rate * 0.20
```

Missing component totals are omitted and weights are renormalized.

Observed tool time is measured from the timestamp of a Codex `item.started`
event to the matching `item.completed` event. This is agent-visible tool
latency, not process CPU time.

## Corpus

The Rust corpus covers four codebase-understanding tasks:

- Tokio: async runtime scheduling.
- ripgrep: file discovery and ignore rules.
- hyper: accepting connections and dispatching requests.
- Cargo: dependency resolution and package build flow.

The full benchmark runs each task four times per arm.

## Current Results

The table combines the latest clean all-arm suite for `empty` and
`codegraph-cli` with the post-context-fix `oxcode-cli` rerun. The oxcode rerun
was used because the context traversal fix removed the stale long-tail tool
latency seen in the earlier all-arm run.

| arm | runs | success | quality | shell cmds | search cmds | read cmds | indexed CLI cmds | indexed p50 | indexed p95 | indexed total | tool share |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| empty | 16 | 100% | 1.00 | 32.0 | 4.5 | 27.0 | 0.0 | n/a | n/a | 0 ms | 0.02% |
| codegraph-cli | 16 | 100% | 1.00 | 30.5 | 3.0 | 19.0 | 5.5 | 165 ms | 310 ms | 1,242 ms | 0.60% |
| oxcode-cli | 16 | 100% | 1.00 | 22.0 | 2.0 | 12.0 | 7.0 | 1,189 ms | 1,797 ms | 6,913 ms | 4.78% |

Lower is better for command counts and duration metrics. Higher is better for
quality.

## Comparisons

| comparison | shell command reduction | search command reduction | read command reduction | quality delta |
| --- | ---: | ---: | ---: | ---: |
| oxcode-cli vs empty | 31.25% fewer | 55.56% fewer | 55.56% fewer | 0.00 |
| codegraph-cli vs empty | 4.69% fewer | 33.33% fewer | 29.63% fewer | 0.00 |
| oxcode-cli vs codegraph-cli | 27.87% fewer | 33.33% fewer | 36.84% fewer | 0.00 |

Compared with the earlier oxcode all-arm run, the context traversal fix reduced
median indexed CLI execution time from 26,832.5 ms to 6,913 ms, reduced indexed
CLI p95 latency from 23,796.5 ms to 1,797 ms, and reduced tool share from
24.30% to 4.78%.

## Artifact Suites

Generated suites are intentionally under ignored `target/agent-eval/` paths.
They are not committed to the repository.

- `target/agent-eval/rust-full-20260601-pgr-tool-time-authsync`
- `target/agent-eval/rust-full-20260601-pgr-tool-time-contextfix-oxcode`
- `target/agent-eval/rust-mini-20260601-queryjsonfix-oxcode`
- `target/agent-eval/smoke-pgr-tool-time-final-20260601`

Workshop/local metric parity was checked for the clean all-arm suite and the
post-fix oxcode suite. Both comparisons produced 0 metric diffs.

## Validation

The implementation was validated with:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
node --check scripts/agent-eval/*.mjs
node scripts/agent-eval/test-timing-metrics.mjs
scripts/agent-eval/smoke.sh --out target/agent-eval/smoke-pgr-tool-time-final-20260601
```

The final smoke suite passed for `empty`, `oxcode-cli`, and `codegraph-cli`.

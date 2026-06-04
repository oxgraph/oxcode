# Codex CLI Agent Benchmark Results

This benchmark measures whether giving Codex `oxcode` improves codebase
understanding versus normal shell exploration, and how that compares with
codegraph. It is an agent-task efficiency benchmark with a blind quality gate.
See [`agent-eval-methodology.md`](agent-eval-methodology.md) for the framing.

Headline metrics: blind-judged answer quality, tokens, cost, command counts,
wall-clock time, and indexed-CLI latency. The comparable unit across tools is
each tool's improvement **vs its own no-tool control**, not absolute numbers.

## Benchmark Definition

Each run renders one prompt:

```text
<prompts/common.md>

<prompts/arms/<arm>.md>

Question:
<task.question>
```

`prompts/common.md` is byte-identical across arms; only the arm-specific block
changes, and every run records the common, arm, and final prompt hashes.

**Quality** is a blind LLM-as-judge score (0-1) over correct / complete /
grounded, graded without the judge seeing which arm produced the answer
(`grade-answer.mjs`). The old substring grader is retained only as a `keyword%`
diagnostic — it scored every arm a flat 1.00 and could not discriminate.

**Tokens** are exact from codex `turn.completed` usage; **cost** is estimated at
configurable per-Mtok prices (token counts are the model-agnostic metric).
**Observed tool time** is measured from a codex `item.started` event to its
`item.completed` event — agent-visible latency, not CPU time. Each headline
metric carries a Student-t 95% CI over runs.

## Corpus

Four comprehension tasks (Tokio, ripgrep, hyper, Cargo) plus two refusal /
anti-hallucination tasks (a non-existent tokio scheduler and ripgrep `--gpu`
flag) that check oxcode helps the agent decline rather than fabricate.

## Headline run: oxcode vs codegraph on Tokio

- **Task:** `tokio-runtime-schedule` — codegraph's published Tokio question is nearly identical, so this is like-for-like.
- **Ours:** codex / gpt-5.5, oxcode **release** build, `empty` vs `oxcode-cli`, RUNS=6, median + 95% CI.
- **codegraph reference:** its README "Benchmark Results" (Claude Opus 4.8, tool as MCP `codegraph_explore`, median of 4), re-validated 2026-06-02.

| metric | oxcode vs empty (ours) | codegraph vs empty (published) |
|---|---:|---:|
| tokens (fewer) | -5% | -38% |
| cost (cheaper) | -19% | even |
| tool calls (fewer) | -23% | -57% |
| wall time (faster) | -10% | -18% |
| answer quality (blind judge) | +0% (tied, 0.97) | not measured |

Absolute medians (ours): tokens 431k → 410k · cost $0.194 → $0.157 · shell
commands 30 → 23 · wall 103.7s → 93.4s · quality 0.97 → 0.97 · oxcode query
p50 931 ms · tool share 2.9%.

### Reading it honestly

- oxcode improves over its control on every efficiency axis while **holding answer quality** (tied at 0.97). The quality row is the guard codegraph's benchmark lacks: the gains are not bought by the agent giving up sooner.
- The gains are **smaller than codegraph's published Tokio gains** (38% tokens, 57% tool calls vs our 5% and 23%). The -5% token delta sits inside a wide CI (±264k at n=6), so it is not a reliable reduction on this task; cost (-19%) and tool calls (-23%) are firmer.
- The likeliest reason for the gap is **tool delivery, not index quality**: codegraph is measured as an MCP `codegraph_explore` that answers in one call, while oxcode here is a CLI the agent composes over ~6 invocations. The clearest lever is exposing oxcode as an MCP with an explore-style one-call tool, then re-comparing MCP-to-MCP.

### Build-profile gotcha (found and fixed during this run)

The first end-to-end run showed oxcode 2x slower with 58% more tokens — a harness
bug, not oxcode. The bench built oxcode in **debug**: `oxcode symbols Runtime` on
the tokio corpus took **26.89 s** debug vs **1.25 s** release (21x), and the run's
`indexed_cli p50` of 26,878 ms matched the debug timing exactly. Fixed:
`bench-rust.sh` now builds `--release`. After the fix, oxcode query p50 fell to
931 ms and tool share from 40% to 2.9%.

## Reproduce

```bash
scripts/agent-eval/bench-rust.sh --skip-smoke \
  --tasks tokio-runtime-schedule --arms empty,oxcode-cli --runs 6
node scripts/agent-eval/compare-codegraph.mjs \
  --metrics target/agent-eval/<suite>/suite-metrics.json --task tokio-runtime-schedule
```

Generated suites live under ignored `target/agent-eval/` and are not committed.

## Validation

```sh
node --check scripts/agent-eval/*.mjs
scripts/bench/oxcode-output-checks.sh           # 13/13 code-aware oxcode output assertions
scripts/agent-eval/grade-answer.mjs --suite-dir <suite> --task-file tasks/rust.yaml
```

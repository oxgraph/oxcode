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

## Headline: the curated `context` overhaul (direct, robust result)

`oxcode context` was overhauled to be bounded and PageRank-curated — oxgraph's
seeded personalized PageRank over a combined `explore` projection, a hard
`--max-bytes` budget, and skeletal source read from disk. Measured directly on
tokio:

- `oxcode context "How does tokio schedule and run async tasks…"` output dropped
  **695,434 → 21,614 bytes (32× smaller)**, with PageRank surfacing the genuinely
  relevant symbols (`Handle::current`, `spawn_blocking`, worker/task scheduler
  methods) and the budget enforced (`15,244 of 20,000 chars`).

This is the win the overhaul targeted, and it is robust (a single deterministic
measurement, not a noisy agent average).

## Agent benchmark: oxcode-cli vs the no-tool control on Tokio

- codex / gpt-5.5, oxcode **release** build, `empty` vs `oxcode-cli`, RUNS=6, median + 95% CI.
- codegraph reference: README "Benchmark Results" (Opus 4.8, MCP `codegraph_explore`, median of 4), re-validated 2026-06-02. Comparable unit = improvement vs each tool's own control.

| metric | oxcode vs empty (ours) | codegraph vs empty (published) |
|---|---:|---:|
| tokens (fewer) | +6% (within noise) | -38% |
| cost (cheaper) | +4% (within noise) | even |
| tool calls (fewer) | -4% | -57% |
| wall time (faster) | +47% (noisy) | -18% |
| answer quality (blind judge) | -1% (tied, 0.96 vs 0.97) | not measured |

Absolute medians (ours): tokens 408k → 430k · shell commands 27 → 26 · wall 92s → 135s · quality 0.97 → 0.96 · oxcode query p50 958 ms.

### Reading it honestly

- **Quality holds** (0.96 vs 0.97, ±0.02) — robust; the curated context does not degrade the answer.
- On agent efficiency, oxcode-cli is **statistically tied with the no-tool baseline**. The token/cost deltas sit inside enormous CIs (oxcode tokens ±227k on a 430k median, ≈±50%) and tool calls are even. The point estimate swings run-to-run — a prior n=6 release run on the *old* 695 KB context read −19% cost / −23% calls; this n=6 on the bounded context reads +6% tokens — and that swing is noise, not signal. n=6 is too few for these high-variance metrics.
- **Shrinking the context output 32× did not, on its own, win the agent benchmark.** The agent uses oxcode as a *supplement* to its own exploration (this run: 7 oxcode calls **and** 4 greps **and** 15 file reads), not a replacement — so the curated context adds tokens and latency on top of, rather than instead of, the agent's normal grep/read work.
- This re-confirms the codegraph gap is **delivery, not index quality or context size**: codegraph's one-call MCP `codegraph_explore` collapses the whole discovery loop, while an oxcode CLI the agent composes does not. The clear next lever is **exposing oxcode as an MCP with an explore-style one-call tool** and re-comparing MCP-to-MCP. The bounded, PageRank-curated `context` is the right *payload* for that tool; delivery is what remains.

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

# oxcode agent-eval methodology

How we evaluate oxcode as an agent tool, why the suite is shaped the way it is,
and the production loop that turns real runs into regression cases.

oxcode is an augmentation tool. It does not act on its own. It makes a coding
agent better at understanding a codebase. That one fact decides almost everything
below.

## Two framings, and which one fits oxcode

Ben Hylak's ["How to Eval AI Agents"](https://howtoeval.com) splits eval work
into two goals.

- **Benchmark-maxxing.** Push a measurable score up on a fixed task set. This is
  the right frame when you control the thing and want to improve it. Tune it,
  watch the number move, ship the version that wins.
- **Floor-raising.** Guarantee the system never drops below a known-good bar on
  cases you have already paid for in production. This is the right frame when a
  failure is expensive and silent.

oxcode makes a measurable contribution to agent answer quality, so
benchmark-maxxing is the primary frame. We run agents with and without oxcode on
real repos, compare answer quality, latency, and tool behavior, and tune oxcode
to win. But a pure benchmark drifts. It tells you the average got better while a
specific, important query quietly broke. So we add floor-raising on top.

> A floor-raising suite is a memory of bugs you refuse to reintroduce.

Every concrete failure we have seen becomes a committed assertion that fails
loudly forever after: a selector that hallucinated a match, a reindex that lost
an edit, a query that fell off a latency cliff. The two framings work together.
Benchmark-maxxing moves the average up. Floor-raising nails the floor down.

### Where each lives in this repo

| Layer | Frame | Where | What it protects |
| --- | --- | --- | --- |
| Tool-output correctness | floor-raising | [`scripts/bench/oxcode-output-checks.sh`](../scripts/bench/oxcode-output-checks.sh) | oxcode's `--json` contract and anti-hallucination guarantee (no agent in the loop) |
| Engine performance | floor-raising | [`scripts/bench/oxcode-perf.sh`](../scripts/bench/oxcode-perf.sh) | the oxgraph 0.3.0 latency gains in [`BENCHMARKS.md`](../BENCHMARKS.md), with optional regression thresholds |
| Agent answer quality | benchmark-maxxing | `scripts/agent-eval/*` plus `tasks/rust.yaml` | with-oxcode vs without-oxcode answer quality on real repos |
| Promoted goldens | floor-raising | `tasks/goldens.yaml` (via [`promote-to-golden.mjs`](../scripts/agent-eval/promote-to-golden.mjs)) | specific production questions oxcode must keep answering well |

## The scaling model: Stumbles, Issues, Signals, Experiments

Error analysis drives the loop. Look at real traces, name what went wrong in
plain language, cluster the failure modes, and only then build an eval for the
clusters that matter. This is the method Hamel Husain lays out in ["Your AI
Product Needs Evals"](https://hamel.dev/blog/posts/evals/) and his ["A Field
Guide to Rapidly Improving AI Products"](https://hamel.dev/blog/posts/field-guide/).

The same observation flows through four stages, and how you handle each one
changes with traffic.

| Stage | What it is | Low traffic (a handful of runs/day) | High traffic (continuous) |
| --- | --- | --- | --- |
| **Stumbles** | A single run where oxcode helped less than it should. A wrong file cited, a `not_found` for a real symbol, a slow query. | Read the trace by hand. Open the run dir, read `final-answer.txt` and `run.jsonl`. | Sampled review. Flag stumbles automatically from metrics (low quality score, high latency, `oxcode` non-zero exits). |
| **Issues** | A stumble you have named and reproduced. A labeled failure mode, not a one-off. | Write it down (commit message, note). Reproduce on the smoke fixture or a real repo. | Cluster stumbles into named buckets via error analysis. See `analyze-oxcode-failures.mjs`, which already buckets `ambiguous_selector`, `selector_not_found`, `raw_query_unsupported`, and the rest. |
| **Signals** | A measurable proxy for an issue. An assertion or metric that goes red when it recurs. | Add a check to `oxcode-output-checks.sh`, or a golden to `tasks/goldens.yaml`. | Track the signal over time, gate CI and releases on it, alert on drift. |
| **Experiments** | A change you make to move a signal, checked against the suite. | Run the affected script before and after. Read the diff. | A/B the change across the corpus. Compare aggregate quality and latency. Ship the winner. |

One rule holds the loop together. A stumble you cannot reproduce is noise, and a
signal you cannot move is decoration. Spend your effort on the path from a real,
repeated stumble to a committed signal you can experiment against.

## Capturing real oxcode-agent runs

The harness already records everything you need to triage a run after the fact.

- **Run artifacts.** The scripts in `scripts/agent-eval/` (`smoke.sh`,
  `bench-rust.sh`) drive a Codex agent over a task and write a per-run dir under
  `target/agent-eval/<suite>/runs/<task>/<arm>/<n>/`. It holds:
  - `prompt.txt`, the exact prompt (also hashed in `prompt-metadata.json`),
  - `final-answer.txt`, the agent's answer,
  - `run.jsonl`, the full event stream of commands, outputs, and exit codes,
  - `metadata.json`, the repo, repo commit, model, timing, and source task,
  - `metrics.json`, the derived quality and latency metrics.
- **Workshop OTLP traces.** Each suite starts a Workshop session
  (`workshop-url.mjs`) and exports OpenTelemetry traces (`trace.otlp.json`, built
  by `codex-jsonl-to-otlp.mjs`) so you can inspect a run span by span.
- **Raindrop replay.** `raindrop replay register` plus `replay-server.mjs` let
  you replay a captured run deterministically instead of paying for a fresh agent
  invocation while you debug.

Capture costs nothing extra. Real runs already leave a complete, replayable
record.

## Triage workflow

1. **Find the stumbles.** Sort runs by metric: low quality score, high latency,
   any `oxcode` non-zero exit. For oxcode CLI failures, run
   `analyze-oxcode-failures.mjs <suite-dir>`. It scans every `run.jsonl`,
   extracts oxcode invocations, and buckets the failures
   (`ambiguous_selector`, `selector_not_found`, `raw_query_unsupported`,
   `other_failure`) with examples.
2. **Read the trace.** Open the run dir or the Workshop trace. Replay it through
   Raindrop if you need to step through it. Name what went wrong in one sentence.
3. **Decide the frame.**
   - A broken tool contract (hallucinated match, wrong file, dropped edit) is
     floor-raising. Add an assertion to `oxcode-output-checks.sh` or
     `oxcode-perf.sh`.
   - A specific high-value question oxcode should keep answering well is
     floor-raising via a golden. Promote it (next section).
   - A quality gap across many tasks is benchmark-maxxing. It is an experiment
     against `tasks/rust.yaml`, not a single golden.
4. **Lock it in.** Commit the new assertion or golden. From now on the signal
   goes red if the bug returns.

## Promoting a run into a golden

`promote-to-golden.mjs` turns a captured run into a committed regression case.

```sh
node scripts/agent-eval/promote-to-golden.mjs \
  --run-dir target/agent-eval/<suite>/runs/<task>/<arm>/<n> \
  --out tasks/goldens.yaml
```

It reads the run's `metadata.json` and `final-answer.txt` and appends a stanza to
`tasks/goldens.yaml`.

- `id`, `repo`, `repo_commit`, and `question` come straight from the run.
- `repo_url`, `required_concepts`, `expected_files`, and `expected_symbols` are
  pre-filled from the run's source task definition when it is still available,
  and left as `# TODO` comments otherwise. So a run promoted from a known task is
  gradeable right away, and a run with no source task is clearly flagged for a
  human to finish.
- The observed answer is embedded as capped comments for reviewer context.
- It is idempotent. Re-promoting the same id is a no-op, so the command is safe
  to run repeatedly, for example from a triage script.

The emitted file is plain templated text with no YAML library, and it round-trips
through `lib.mjs::parseSimpleYaml`, so the grader reads goldens exactly like any
other task suite. After promoting, fill in the `# TODO` fields. That is the
error-analysis step: choose the `required_concepts` and `expected_files` that
actually separate a good answer from a bad one, then commit.

## References

- Ben Hylak, *How to Eval AI Agents*, <https://howtoeval.com>
- Hamel Husain, *Your AI Product Needs Evals*,
  <https://hamel.dev/blog/posts/evals/>
- Hamel Husain, *A Field Guide to Rapidly Improving AI Products* (error
  analysis), <https://hamel.dev/blog/posts/field-guide/>

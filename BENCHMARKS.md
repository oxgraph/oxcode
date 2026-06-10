# oxcode benchmarks — on oxgraph 0.4.0

End-to-end code-indexing performance of oxcode on the **oxgraph 0.4.0** engine,
versus the same oxcode pipeline on the previous published engine (0.3.2).

## Method

- **baseline**: oxcode at its pre-migration commit on published **oxgraph 0.3.2**.
- **current**: oxcode on **oxgraph 0.4.0** (write-through snapshot encode,
  Arc-shared copy-on-write write path, checksummed self-verifying container).
- Corpus: the **oxgraph workspace** itself — 673 files · 10,428 symbols ·
  166,101 edges. Both columns measured against the same fresh corpus copy.
- Harness: `scripts/bench/oxcode-perf.sh` — fresh rsync copy (excluding
  `target/.git/.oxcode`); cold index; reindex with no change; reindex after a
  one-file edit (persistence-verified by querying the appended marker symbol);
  symbol-query p50 over 5 runs. `--release` binaries, sequential runs on an
  idle 16-core machine.
- Measured 2026-06-10.

## Results

### oxgraph workspace — 673 files · 10,428 symbols · 166,101 edges

| metric | oxgraph 0.3.2 | oxgraph 0.4.0 | change |
| --- | ---: | ---: | ---: |
| cold index | 5,124 ms | 4,717 ms | ~8% faster |
| reindex, no change | 69 ms | 66 ms | flat |
| reindex after 1-file edit | 1,425 ms | 1,410 ms | flat |
| symbol query (p50) | 406 ms | 395 ms | flat |
| db size | 282.2 MiB | 282.2 MiB | flat |
| delta-log after edit | 2.8 MiB | 2.8 MiB | flat |

## Reading the numbers

- 0.4.0 is an architecture and format release — one snapshot write path, a
  self-checking container (per-section CRC-32C + table CRC, verified at bind),
  subsystem-typed errors, and structural sharing on the database write path —
  that **holds end-to-end indexing performance** while restructuring the
  engine underneath.
- The cold-index improvement comes from the write-through snapshot encode:
  freeze streams sections to their final offsets instead of owning every
  payload and copying the whole snapshot again (~1x peak memory, was ~2x).
- The engine-level wins are visible in oxgraph's own criterion suites rather
  than these corpus metrics: a single-element write over a committed-but-
  unfolded overlay of 100k entries dropped from ~18 ms to ~9-10 ms
  (Arc-shared copy-on-write seeding), and integrity checking moved from a
  whole-base CRC scan to per-section verification at bind time with no open
  regression.

See the engine-side notes in
[oxgraph `BENCHMARKS.md`](https://github.com/oxgraph/oxgraph/blob/main/BENCHMARKS.md).

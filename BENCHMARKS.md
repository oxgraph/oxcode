# oxcode benchmarks — on oxgraph 0.3.0

End-to-end code-indexing performance after moving oxcode onto the **oxgraph 0.3.0**
engine (identity-reconcile writes + zero-copy index open), versus the previous
engine.

## Method

- **baseline**: oxcode at its pre-overhaul commit on published **oxgraph 0.2.4**
  (`apply_delta` wholesale rewrite + O(total-incidences) tombstone).
- **current**: oxcode on **oxgraph 0.3.0** (`reconcile_database`: `upsert`/`retain`
  identity reconcile; zero-copy index open).
- Identical source corpora; `--release` binaries; sequential runs on an idle
  16-core machine. Incremental reindex = append one function to a source file and
  re-index, verified by querying the new symbol afterward.
- Measured 2026-06-03.

## Results

### storage-hub — 328 files · 40,749 symbols · 527,999 edges

| metric | 0.2.4 | 0.3.0 | change |
| --- | ---: | ---: | ---: |
| **reindex after 1-file edit** | **> 150 s** (~62 min, O(n²)) | **4,842 ms** | **≈ 770× faster** |
| symbol query (p50) | 3,902 ms | 988 ms | 3.9× faster |
| cold index | 15,165 ms | 11,968 ms | 1.3× faster |
| reindex, no change | 1,349 ms | 834 ms | 1.6× faster |

### harnessing — 76 files · 11,091 symbols · 45,901 edges

| metric | 0.2.4 | 0.3.0 | change |
| --- | ---: | ---: | ---: |
| **reindex after 1-file edit** | **27,648 ms** | **444 ms** | **62× faster** |
| symbol query (p50) | 457 ms | 154 ms | 3.0× faster |
| cold index | 1,797 ms | 1,313 ms | 1.4× faster |

## Why

- **Incremental reindex** was inverted on 0.2.4 — a one-file edit reindex was
  *slower than a full rebuild* because the store replaced all edges wholesale and
  the engine's tombstone was O(total incidences), making bulk delete O(n²) (~62 min
  on storage-hub). oxcode now drives oxgraph 0.3.0's identity-reconcile verbs:
  unchanged symbols/edges keep their ids and emit zero mutations, so reindex is
  O(change) — and the per-reindex WAL dropped from ~953 MB to ~5 MB.
- **Query latency** dropped 3–4× because 0.3.0 opens the database without
  rebuilding the index (it is persisted and borrowed from the memory map).

See the engine-side notes in
[oxgraph `BENCHMARKS.md`](https://github.com/oxgraph/oxgraph/blob/main/BENCHMARKS.md).

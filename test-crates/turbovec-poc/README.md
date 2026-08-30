# TurboVec POC — standalone feasibility benchmark

Validates the technical claims in `../../turbovec_integration_design_report.html`
("Replacing DuckDB-VSS with TurboVec") against this repo's own real, indexed
embeddings, before committing engineering time to the full 4-phase production
migration. **This is a throwaway spike — delete this directory once the
go/no-go call is made.** No production code, schema, or PyO3 surface was
touched; everything lives here.

## How to run

```bash
# 1. One-off: dump this repo's real embeddings to a flat fixture (requires the
#    repo's own .venv; only needs to be run once, or again if the corpus changes)
uv run python scripts/dump_embeddings.py

# 2. Benchmark: compression, recall, latency, and the write()/load() roundtrip.
#    Also persists fixtures/index_4bit.tv, which the live demo (below) loads.
cargo run --release --bin bench

# 3. Correctness tests: the write()/load() whole-index roundtrip, plus (as of
#    the git-pinned unreleased turbovec commit, see Correction #2) the
#    from_parts() per-vector rebuild path.
cargo test

# 4. Live demo: same query, both backends, side by side (see the "Live Demo"
#    section at the bottom of this README)
uv run python scripts/demo_compare.py --interactive
```

**Environment note**: `turbovec` unconditionally needs a BLAS library linked
in on Linux (see Correction 3 below). This machine needed
`sudo apt-get install libopenblas-dev` before `cargo build` would link.

**Dependency note**: `Cargo.toml` currently git-pins `turbovec` to an
unreleased upstream commit (`ae31ba3`, not yet on crates.io) specifically to
test the `from_parts()` fix — see Correction #2. This is intentional for this
round of testing, not an accident; revert to `turbovec = "0.9"` if you want
to reproduce the original published-crate-only results.

## Real benchmark results (this repo's corpus: 72,974-vector corpus / 500 held-out queries, 256-dim, `qwen3-embedding-4b`)

| Metric | bit_width=2 | bit_width=4 | Threshold | Verdict |
|---|---|---|---|---|
| Compression (amortized, incl. id+scale overhead) | 13.5x | 7.3x | ≥14x / ≥7.5x | **Both marginally below threshold** |
| Recall@1 (exact top-1 match vs. brute-force cosine) | 0.8100 | 0.8900 | ≥0.99 | **Fail — well below design doc's claimed 0.997** |
| Recall@10 (true top-1 within top-10) | 0.9960 | 1.0000 | ≥0.999 | **Pass** |
| Search latency (median, this ~73K-vector corpus) | — | 2.37ms/query (422 q/s) | not dramatically slower than brute-force | **Pass** — ~12x *faster* than the 28.34ms/query brute-force baseline |
| Brute-force baseline | — | 28.34ms/query (35 q/s) | (ground truth, informational) | — |
| `write()`/`load()` roundtrip | — | bit-identical search results | must match exactly | **Pass** (`cargo test`) |
| Build | — | succeeds, but only after installing `libopenblas-dev` | no extra host access | **Partial** — see Correction 3 |

Run `cargo run --release --bin bench` yourself to reproduce; these numbers
are real, not simulated.

## Cross-repo validation at 13x scale (mono corpus)

The benchmark above uses this repo's own ~73K-vector corpus. To check whether
the findings hold at a much larger, more repetitive scale, the same
`dump_embeddings.py` → `bench` pipeline was re-run against the `mono` repo's
`embeddings_256` table (928,800 vectors after the held-out split, same
`qwen3-embedding-4b`/256-dim pipeline, same held-out methodology):

| Metric | bit_width=2 | bit_width=4 |
|---|---|---|
| Compression (amortized) | 13.5x (76.0 B/vector) | 7.3x (140.0 B/vector) |
| Recall@1 (exact top-1 match) | 0.6920 | 0.8080 |
| Recall@10 (true top-1 within top-10) | 0.9860 | 1.0000 |
| Search latency (median / p95) | 20.09ms / 21.18ms (50 q/s) | 39.06ms / 43.28ms (26 q/s) |
| Cold-start rebuild cost | 7.64s | 5.85s |
| Near-tie gap (mean, hits vs. misses) | 0.0219 vs. 0.0038 | 0.0200 vs. 0.0008 |

Brute-force baseline: median 371.57ms/query (3 q/s) — TurboVec at 4-bit is
still ~9.5x faster despite the ~13x larger corpus.

**Compression is scale-invariant, as expected** (identical 13.5x/7.3x — it's a
fixed per-vector overhead ratio, not corpus-dependent). **Recall@1 drops
further at this scale** (0.89→0.81 at 4-bit, 0.81→0.69 at 2-bit vs. the
73K-vector corpus) — this is exactly the corpus-scale effect predicted in the
"Recall@1 gap diagnosis" section below: a bigger corpus means more chances for
a near-tied competitor to appear per query, independent of TurboVec's
quantization. **Recall@10 held essentially steady** (1.0000 at 4-bit), which
is the metric that matters for ChunkHound's real top-k usage pattern — the
go/no-go read doesn't change at this larger scale, it's reinforced.

## Go/no-go read

**Mixed, leaning no-go on the design doc as specified — but not a dead end.**

- The metric that actually matters for ChunkHound's real usage pattern —
  **recall@10 = 1.0000 at 4-bit** — comfortably passes, and ChunkHound already
  returns top-10+ results by default (the design doc's own risk mitigation).
  If the product only ever needs "is the right chunk somewhere in the
  results," TurboVec clearly works here.
- But **raw recall@1 (0.89 at 4-bit) falls far short of the design doc's
  claimed 0.997** — a real, substantial gap, not a rounding difference.
  Diagnosed below in "Recall@1 gap diagnosis": it's driven mostly by
  near-tied candidates in the exact ranking, plus a smaller, real
  contribution from the 256-dim truncation itself. Whether this matters
  depends on whether any ChunkHound caller relies on the single top-1
  result specifically (reranking, single-hop strategy's top candidate,
  etc.) rather than the full top-k.
- **Compression at both bit-widths lands just under the (already-corrected)
  thresholds** (7.3x vs ≥7.5x at 4-bit, 13.5x vs ≥14x at 2-bit) — driven by a
  real, structural 12 bytes/vector of id-table + per-vector scale overhead
  on top of the raw packed codes (128 bytes/vector at 4-bit, dim=256). This
  overhead doesn't amortize away at larger corpus sizes; it's proportional,
  not fixed. Still a large win over float32 (1024 bytes/vector) — just not
  the "16x" the design doc claims.

**Recommendation**: the compression shortfall is minor and probably
acceptable as-is. The recall@1 gap is now diagnosed (see below) rather than
an open question — most of it is a corpus-scale/near-tie effect that would
likely persist regardless of TurboVec, with truncation contributing a real
but smaller share. If any caller needs a reliable single top-1 answer rather
than "somewhere in top-10," that's the part still worth weighing.

## Recall@1 gap diagnosis

Two candidate explanations were tested for why recall@1 (0.89 at 4-bit) falls
so far short of the design doc's 0.997: (A) quantization noise flipping
near-tied candidates, independent of dimensionality, and (B) the 256-dim
client-side truncation itself (native dim is **2560**, not the 1536 assumed
earlier — `qwen3-embedding-4b`'s real embedding size, confirmed empirically
via `scripts/probe_native_dim.py`; `QWEN3_TUNING.md`'s 1536 figure appears to
be wrong, or refers to something else).

**Stage A — near-tie correlation (free, uses data `bench` already computes).**
For every held-out query, `bench` now also records the exact top1–top2 cosine
score gap and buckets it by whether TurboVec got the top-1 right:

| bit_width | mean gap on hits | mean gap on misses | ratio |
|---|---|---|---|
| 2 | 0.0637 (n=405) | 0.0044 (n=95) | ~14x |
| 4 | 0.0589 (n=445) | 0.0009 (n=55) | ~65x |

Misses cluster overwhelmingly on queries where the true top-1 and top-2 were
already almost indistinguishable in **exact, unquantized** cosine similarity
(mean gap 0.0009 — essentially a coin flip) — not on queries with a clear
winner. Most of the recall@1 shortfall is ranking ambiguity that any
approximate method would struggle with, not a TurboVec-specific defect.
Reproduce: `cargo run --release --bin bench` (prints this alongside the main
report).

**Stage B — native vs. truncated dim, matched subset (new embedding calls).**
`scripts/probe_native_dim.py` picked a fixed 4,000-corpus/300-query sample
from this repo's real chunks, kept their existing 256-dim vectors as-is, and
re-embedded the *same texts* through the *same* provider/model requesting
the native dimension (`output_dims=None, client_side_truncation=False` —
confirmed via `openai_provider.py`/`shared_utils.py` that this returns the
untouched server response, no slicing/renormalization). `probe_dim` then ran
the identical recall@1/@10 pipeline against both, holding everything but
dimensionality constant:

| Fixture | dim | Recall@1 | Recall@10 |
|---|---|---|---|
| `probe_256.bin` | 256 (truncated) | 0.9367 | 1.0000 |
| `probe_native.bin` | 2560 (native) | 0.9700 | 1.0000 |

Recall@1 improves by ~3.3 points at native dimensionality on the exact same
data — truncation is a real, measurable contributor, confirming the user's
hypothesis in part. But it's not the whole story: even at full native
dimensionality, recall@1 is still below the design doc's 0.997 claim, and
these absolute numbers (0.93–0.97) aren't directly comparable to the original
0.89 headline figure — that number came from a ~73K-vector corpus, while this
controlled comparison uses a fixed 4,000-vector corpus (recall@1 degrades as
corpus size grows, independent of dimensionality, since more candidates
means more chances for a near-tied competitor to appear — the same effect
Stage A measured). Reproduce:
```bash
uv run python scripts/probe_native_dim.py   # ~5-10 min, re-embeds ~4,300 texts
cargo run --release --bin probe_dim -- fixtures/probe_256.bin
cargo run --release --bin probe_dim -- fixtures/probe_native.bin
```

**Bottom line**: the 0.89-vs-0.997 gap is mostly a near-tie/corpus-scale
effect (Stage A), with truncation-to-256 contributing a real but smaller
share (Stage B, ~3 points on a matched subset). Neither stage suggests a
fundamental TurboVec defect; both suggest the design doc's 0.997 figure was
measured under different (likely higher-dim, and/or smaller/less
near-duplicate-heavy corpus) conditions than ChunkHound's real data.

## The `from_parts()` problem — two options tested

The design doc's `embeddings_quantized` table has one stated purpose: *"if
the `turbovec_indexes` snapshot is stale or corrupt, rebuild the search index
directly from packed codes without any embedding API calls."* That requires
`TurboQuantIndex::from_parts()` plus accessors, which were `pub(crate)` in the
published turbovec 0.9.0 — unreachable from any downstream crate. Two options
were built and tested against this repo's real corpus; both work.

**Option 1 — pin to the unreleased upstream fix.** The exact fix (making
`from_parts()` public, PR #204) is merged on turbovec's `main` but not yet
released to crates.io. `Cargo.toml` now git-pins the exact commit
(`ae31ba3bed1cbe631a573fad0c420385bb227621`). Result: `cargo test` now
includes `from_parts_reconstructs_index_identical_to_original`
(`tests/roundtrip.rs`), which extracts `packed_codes()`/`scales()`/
`tqplus_shift()`/`tqplus_scale()` from a live index, calls the now-public
`from_parts()`, and gets bit-identical search results — no snapshot blob, no
re-embedding. **Passes.** Existing tests and `bench`'s numbers were confirmed
unchanged against the new commit (regression check). One real gap found:
`IdMapIndex` still has no accessor for its inner `TurboQuantIndex`, so this
only works by going around `IdMapIndex` and hand-rolling the id-reattachment
layer (a parallel `Vec<u64>`, slot → chunk_id) — `from_parts()` being public
unblocks the primitive, not the full ready-made `IdMapIndex` the design doc's
schema implies. See Correction #2 below for the full caveat on relying on an
unreleased commit.

**Option 3(C) — durability engineering, no `from_parts()` needed at all**
(works against the stable, published 0.9.0 crate today; doesn't require the
git pin). Two new tests, both passing:
- `tests/corruption_recovery.rs` (`src/snapshot.rs`: CRC32-checksummed,
  N-generation rotation on top of `IdMapIndex::write()`/`load()`) —
  corrupting one generation's bytes falls through to the next generation
  with search results identical to the pre-corruption baseline; corrupting
  every generation reports failure cleanly (no panic, no silent garbage
  index) rather than ever guessing.
- `tests/rebuild_from_floats.rs` — simulates total snapshot loss and rebuilds
  via `add_with_ids()` on the same stored float32 vectors (standing in for
  ChunkHound's existing `embeddings_{dims}` table), asserting identical
  search results. **Real cold-start cost, measured on this repo's full
  72,974-vector corpus** (`bench`'s new "Cold-start rebuild cost" line):
  **0.45s at 2-bit, 0.62s at 4-bit** — a local re-quantization, not a network
  re-embed, and fast enough that "rebuild from floats" is a genuinely cheap
  fallback even without the rotation layer.
- **Storage cost of the full (C) design** (float32 kept + N=2 checksummed
  snapshot rotation), measured on the real corpus: float32 baseline ≈74.7 MB
  (1024 B/vector × 72,974) + N=2 rotation at 4-bit ≈20.4 MB (280 B/vector ×
  72,974, i.e. 2× the already-measured 140 B/vector single-snapshot cost) =
  **≈95.2 MB total, +27.3% vs. today's float32-only baseline.** (Higher than
  the ~13% a single, non-rotated snapshot would cost — N=2 rotation doubles
  that overhead, which is the real price of the corruption-redundancy
  guarantee.)

**Bottom line**: both options are now proven to work, not just designed on
paper. Option 1 restores the design doc's exact original mechanism but leans
on an unreleased dependency; Option 3(C) ships against the stable crate today
at a measured +27.3% disk cost, with sub-second local rebuild as the true
last resort. Neither requires forking or patching turbovec. Recommendation:
run Option 3(C) as the safety net regardless (cheap, stable-API, and now
proven), and treat Option 1 as the "graduate to `from_parts()`-based design
once turbovec cuts a real release" path — worth revisiting once a version
bump ships, not before.

## Corrections to the design doc (found by reading the real `turbovec` 0.9.0 source, not just its docs)

1. **No `IdMapIndex::from_parts()`.** Only the inner `TurboQuantIndex` has
   anything resembling it — see #2 for its public/private status across
   versions. The whole-index reconstruction path is `IdMapIndex::write(path)` /
   `IdMapIndex::load(path)`, a **file-based** roundtrip (there is no
   `to_bytes()`/`from_bytes()` in-memory equivalent in 0.9.0 either).
   `tests/roundtrip.rs` proves this path works exactly (bit-identical search
   results before/after).

2. **`TurboQuantIndex::from_parts()` and its supporting accessors
   (`packed_codes()`, `scales()`, `tqplus_shift()`, `tqplus_scale()`) were all
   `pub(crate)` in the published 0.9.0** — confirmed by reading
   `turbovec-0.9.0/src/lib.rs` directly. **Update**: the exact fix is already
   merged upstream on `main` (commit `ae31ba3`, PR #204, merged 2026-07-25,
   resolving issue #70 filed by the `pg_turbovec` maintainer for the identical
   use case) but **not yet released to crates.io** — still 0.9.0 there as of
   this writing. This crate is now git-pinned to that exact commit (see
   `Cargo.toml`), and `tests/roundtrip.rs`'s
   `from_parts_reconstructs_index_identical_to_original` test **proves the
   per-vector rebuild path now works**: extract `packed_codes()`/`scales()`/
   `tqplus_shift()`/`tqplus_scale()` from a live index, call the now-public
   `TurboQuantIndex::from_parts()`, and get bit-identical search results —
   no whole-index snapshot blob, no re-embedding. One real gap remains:
   `IdMapIndex` still has no accessor for its inner `TurboQuantIndex` even at
   this commit, so the test (and any real implementation) has to hand-roll
   the id-reattachment layer (a parallel `Vec<u64>`, slot → chunk_id) rather
   than getting a ready-made `IdMapIndex` back — `from_parts()` being public
   unblocks the *primitive*, not the full ergonomic story the design doc
   implies. **Caveat on relying on this today**: it's an unreleased, untagged
   commit — safe by content-addressed SHA pinning, but not a versioned
   release; see the "from_parts() problem" plan discussion for the fuller
   options analysis (git-pin now vs. wait for an official release vs.
   redesign the fallback to not need it at all).

3. **`turbovec` unconditionally requires a system BLAS library on Linux**
   (a target-specific `ndarray` dependency with the `blas` feature enabled,
   visible in `turbovec`'s own `Cargo.toml`), directly contradicting the
   design doc's "no new system libraries" claim. This environment had no
   BLAS installed; fixing it required `sudo apt-get install
   libopenblas-dev` (a from-source build via `openblas-src` failed on this
   network's TLS-intercepting proxy; a from-source reference-BLAS build via
   `netlib-src` failed for lack of a Fortran compiler). Any production
   deployment target for the real migration needs this checked upfront.

   **Root cause and cross-platform breakdown** (from `turbovec`'s own
   `build.rs`): it explicitly picks a BLAS backend per target OS —
   `cargo:rustc-link-lib=openblas` on Linux, `cargo:rustc-link-lib=
   framework=Accelerate` on macOS, and nothing (`_ => {}`) on Windows, which
   falls through to `ndarray`'s pure-Rust `matrixmultiply` fallback. So:
   - **Linux**: real, hard system dependency (`libopenblas`) — the only
     platform where this is a problem.
   - **macOS**: links `Accelerate.framework`, which ships with every macOS
     install — zero extra install, build or runtime.
   - **Windows**: no BLAS at all — zero extra install.

   **Distribution implication for ChunkHound**: the `apt-get` must never
   reach the end user (`pip install chunkhound` can't assume a system
   package manager call). It's only viable as a **CI/build-time** step,
   mirroring how this repo already resolves DuckDB's native-lib dependency
   before publishing (`Cargo.toml`'s `DUCKDB_DOWNLOAD_LIB`/`DUCKDB_STATIC`,
   see `AGENTS.md` RUST_COMMANDS) — resolved once in CI, invisible in the
   shipped wheel. One gap: `.github/workflows/release.yml`'s
   `build-native-wheel` job builds the Linux wheel on plain `ubuntu-latest`
   via `maturin build --release`, with no manylinux container and no
   `auditwheel repair` step — so a naive dynamic link against
   `libopenblas` would ship a wheel that only works on hosts that already
   have it installed. Options, best first: (1) statically link OpenBLAS in
   CI (`openblas-src`'s `static` feature + `gfortran`/`cmake` on the
   runner — no proxy issue on GitHub-hosted runners), giving a
   self-contained `.so` with no runtime dependency, same approach numpy/
   scipy wheels use; (2) dynamic link + `auditwheel repair` to vendor
   `libopenblas.so.0` into the wheel (needs a manylinux-compliant build,
   which this job doesn't currently do); (3) build against a
   PyPI-distributed prebuilt BLAS (e.g. `scipy-openblas64`) to avoid any
   system package manager call in CI. macOS and Windows need no build
   changes at all — already zero-dependency by design.

4. **4-bit compression is ~8x in theory, ~7.3x in practice** (not 16x — the
   design doc's 16x figure is actually the 2-bit number), and the
   in-practice figure includes a real, non-amortizing 12 bytes/vector of
   id-table + per-vector-scale overhead beyond the raw packed codes. See the
   compression row in the results table above.

## What's in here

```
turbovec-poc/
├── Cargo.toml            # standalone crate; turbovec (git-pinned, see "from_parts() problem"),
│                         # blas-src/openblas-src (system), serde, crc32fast
├── src/
│   ├── lib.rs             # shared: normalize()
│   ├── fixture.rs         # flat-file reader for the dumped corpus
│   ├── bruteforce.rs      # exact cosine ground truth
│   ├── metrics.rs         # recall@k, percentile, compression math
│   ├── snapshot.rs        # Option 3(C): checksummed, rotating IdMapIndex snapshots
│   ├── main.rs             # `bench` binary — the benchmark above (Stage A + cold-start rebuild cost)
│   └── bin/
│       ├── serve.rs        # `serve` binary — live-demo query server (see below)
│       └── probe_dim.rs    # Stage B diagnostic: recall@1/@10 against any fixture/dim
├── tests/
│   ├── roundtrip.rs           # write()/load() roundtrip + Option 1's from_parts() test
│   ├── corruption_recovery.rs # Option 3(C): rotation/checksum fallthrough + catastrophic-failure path
│   └── rebuild_from_floats.rs # Option 3(B)/(C): rebuild via add_with_ids(), no re-embedding
├── scripts/
│   ├── dump_embeddings.py    # one-off corpus extraction
│   ├── demo_compare.py       # live-demo orchestrator
│   └── probe_native_dim.py   # Stage B: matched-subset + native-dim re-embedding
└── fixtures/              # gitignored: embeddings_256.bin, index_4bit.tv,
                            # probe_256.bin, probe_native.bin
```

## Live Demo: side-by-side query comparison

Beyond the offline benchmark, `serve` + `demo_compare.py` let you type one
query and see it run against **both** the existing production search path
(DuckDB-VSS/HNSW, exact float32 cosine) and the new TurboVec index, side by
side. See `demo_compare.py --help` for usage; it embeds each query through
this project's own configured embedding provider (same internal server used
for indexing), so results are directly comparable.

Example (run from the repo root):

```
$ uv run python test-crates/turbovec-poc/scripts/demo_compare.py "MCP server HTTP transport" --k 5

Query: 'MCP server HTTP transport'
#   EXISTING (DuckDB-VSS, exact)                                 TURBOVEC (quantized)
1   chunkhound/mcp_server/http_server.py:123-150  (0.8340)       chunkhound/mcp_server/http_server.py:123-150  (0.8335)
2   chunkhound/mcp_server/__init__.py:27-27  (0.8070)            chunkhound/mcp_server/__init__.py:27-27  (0.8043)
3   chunkhound/core/config/mcp_config.py:35-70  (0.7725)         chunkhound/core/config/mcp_config.py:35-70  (0.7702)
4   .okf/components/mcp-server.md:9-10  (0.7663)                 chunkhound/core/config/mcp_config.py:42-89  (0.7652)
5   chunkhound/core/config/mcp_config.py:42-89  (0.7653)         .okf/components/mcp-server.md:9-10  (0.7638)

overlap@5 = 100%   rank-1 match = True
```

Real, natural-language queries against this corpus consistently show 100%
overlap at k=5 with matching rank-1 — noticeably better than the held-out
recall@1 (0.89) measured in the benchmark above, likely because these query
embeddings sit closer to a genuine content cluster than an isolated held-out
corpus vector does. Worth keeping in mind when weighing the recall@1 finding.

**Note on the embedding endpoint**: `.chunkhound.json`'s `base_url` uses a
bare internal hostname (no domain suffix). The corporate proxy can't resolve
bare hostnames — only FQDNs — so without a fix, embedding calls fail with
`dns_unresolved_hostname` even though the host resolves fine locally.
`demo_compare.py` works around this by adding the hostname to `NO_PROXY` for
its own process only (see the comment at the top of the script) — it does
not modify the checked-in project config.

### Live demo against the mono corpus

The same live-demo comparison, re-run against the `mono` repo's much larger
(928,800-vector) corpus (see "Cross-repo validation" above), confirms the
near-tie pattern shows up in real queries too, not just the held-out
benchmark:

```
Query: 'test automation configuration'
#   EXISTING (DuckDB-VSS, exact)                                 TURBOVEC (quantized)
1   .../configuration/TestPropertiesProvider.java:1-1  (0.7403)  .../configuration/CommonProperties.java:1-1  (0.7310)
2   .../configuration/PropertiesFileExtensions.java:1-1  (0.7403) .../configuration/TestPropertiesProvider.java:1-1  (0.7310)
3   .../configuration/CommonProperties.java:1-1  (0.7403)        .../configuration/PropertiesFileExtensions.java:1-1  (0.7310)
4   Q/wf_tests/automation/FlowControl/Material/ScenarioConfiguration.java:101-101  (0.7252)  Q/src_archive/.../InternalStokerMain.java:28-28  (0.7273)
5   Q/src_archive/.../InternalStokerMain.java:28-28  (0.7239)     .../configuration/TestNGConfigTest.java:1-24  (0.7254)

overlap@5 = 80%   rank-1 match = False
```

```
Query: 'wafer alignment calibration algorithm'
#   EXISTING (DuckDB-VSS, exact)                                 TURBOVEC (quantized)
1   .../RemoteInterfaces/RemoteToolImaging.java:204-207  (0.8297) .../RemoteInterfaces/common/RemoteRecipeSetup.java:50-53  (0.8359)
2   .../RemoteInterfaces/common/RemoteRecipeSetup.java:50-53  (0.8285) .../RemoteInterfaces/RemoteToolImaging.java:204-207  (0.8333)
3   .../adc_sequencers/DBImportExportSequencer.java:1734-1734  (0.7924)  .../gmdc/GMDCExportSequencer.java:775-775  (0.7957)
4   .../gmdc/GMDCExportSequencer.java:775-775  (0.7924)           .../adc_sequencers/DBImportExportSequencer.java:1734-1734  (0.7957)
5   .../ToolServices_GrabRegressionTests.java:123-124  (0.7899)   .../ToolServices_GrabRegressionTests.java:123-124  (0.7930)

overlap@5 = 100%   rank-1 match = False
```

Both examples land the same way: the full result *set* matches (100% overlap
in the second query, 80% in the first — same 3 files, just reordered), but
rank-1 flips because the top two candidates are near-tied in score (e.g.
0.8297 vs. 0.8285 exact, 0.8359 vs. 0.8333 quantized — a ~0.001-0.003 gap).
On a large, repetitive monorepo with lots of near-duplicate boilerplate
(similar Java classes across products), this near-tie effect surfaces more
often at rank-1 specifically — consistent with the benchmark's recall@1 drop
at this scale — while the top-k set stays reliable.

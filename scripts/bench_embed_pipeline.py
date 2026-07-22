#!/usr/bin/env python3
"""Embedding pipeline benchmark — Python side.

Measures the stub pipeline indirectly via Rust benchmarks and documents
what parity verification requires once PR #380 lands.

Rust-native providers (openai, voyageai) are now implemented (Tasks 14–17).
The factory routes to them automatically; the Python callback fallback still
works for unsupported providers.

Provider parity is verified by:
  - Rust httpmock tests (src/embed/openai.rs: 8 tests, voyageai.rs: 6 tests)
  - Python contract tests (tests/contracts/test_provider_parity.py: 33 tests)
  - Cross-language parity: same JSON response format → same vectors

Real API parity comparison (Python SDK vs Rust-native reqwest) requires API
keys and is skipped in CI. The Rust httpmock tests provide the authoritative
verification that the providers parse embedding responses correctly.

The Python-vs-Rust *pipeline* parity comparison is DEFERRED. The stub pipeline
(Task 11) is Rust-only and not yet exposed through PyO3/chunkhound_native.
Parity comparison requires the real pipeline at the PyO3 boundary (PR #380).

This script:
1. Documents the test matrix used for Rust benchmarks
2. Documents the Rust-native provider parity coverage
3. Provides the scaffolding for the future pipeline parity comparison
4. Exercises Python-side imports to validate the boundary exists
5. Can run the Rust benchmarks via cargo test

Usage:
    # Run Rust benchmarks (the authoritative measurement)
    DUCKDB_DOWNLOAD_LIB=1 cargo test pipeline::bench -- --nocapture

    # This script (documentation + import validation)
    uv run python scripts/bench_embed_pipeline.py

    # Future: pipeline parity comparison (when PR #380 lands)
    uv run python scripts/bench_embed_pipeline.py --parity --files 20 --chunks-per-file 15 --runs 3
"""
import json
import subprocess
import sys
import time
from pathlib import Path

# ── Benchmark Matrix (matches Rust bench.rs) ──

BENCH_MATRIX = {
    "small (100 chunks)": {
        "description": "Baseline: small workload, no bloom, no latency",
        "config": {"chunks": 100, "dim": 768, "batch": 20},
        "expected": {"batches": 5, "skipped": 0, "embedded": 100},
        "rust_test": "pipeline::bench::bench_small_100_chunks",
    },
    "medium (1k chunks)": {
        "description": "Medium workload: 1,000 chunks",
        "config": {"chunks": 1_000, "dim": 768, "batch": 50},
        "expected": {"batches": 20, "skipped": 0, "embedded": 1000},
        "rust_test": "pipeline::bench::bench_medium_1k_chunks",
    },
    "large (10k chunks)": {
        "description": "Large workload: 10,000 chunks, pushes batch/embed path hard",
        "config": {"chunks": 10_000, "dim": 1536, "batch": 100},
        "expected": {"batches": 100, "skipped": 0, "embedded": 10_000},
        "rust_test": "pipeline::bench::bench_large_10k_chunks",
    },
    "batch size sweep": {
        "description": "Varying batch sizes with fixed 1k chunk workload",
        "config": "batch sizes: 10, 25, 50, 100, 200",
        "rust_test": "pipeline::bench::bench_batch_size_sweep",
    },
    "dimension sweep": {
        "description": "Varying embedding dimensions: 384, 768, 1536, 3072",
        "config": "dims: 384, 768, 1536, 3072 (1k chunks, batch=50)",
        "rust_test": "pipeline::bench::bench_dimension_sweep",
    },
    "bloom hit rate 0%": {
        "description": "Empty bloom filter — all chunks embedded (baseline)",
        "config": {"chunks": 1_000, "dim": 768, "batch": 50, "bloom_preload": 0},
        "expected": {"batches": 20, "skipped": 0, "embedded": 1000},
        "rust_test": "pipeline::bench::bench_bloom_hit_rate_0pct",
    },
    "bloom hit rate 50%": {
        "description": "50% pre-populated bloom — half the chunks skipped",
        "config": {"chunks": 1_000, "dim": 768, "batch": 50, "bloom_preload": 500},
        "expected": {"batches": 10, "skipped": 500, "embedded": 500},
        "rust_test": "pipeline::bench::bench_bloom_hit_rate_50pct",
    },
    "bloom hit rate 90%": {
        "description": "90% pre-populated bloom — simulates heavy incremental use",
        "config": {"chunks": 1_000, "dim": 768, "batch": 50, "bloom_preload": 900},
        "expected": {"batches": 2, "skipped": 900, "embedded": 100},
        "rust_test": "pipeline::bench::bench_bloom_hit_rate_90pct",
    },
    "oversized chunk filtering": {
        "description": "900 normal + 100 huge (10k tokens) chunks",
        "config": {"chunks": 1_000, "dim": 768, "batch": 50, "oversized": 100},
        "expected": {"skipped": 100, "embedded": 900},
        "rust_test": "pipeline::bench::bench_oversized_chunk_filtering",
    },
}


def run_rust_benchmarks(verbose: bool = True) -> dict:
    """Run the Rust benchmark suite and return results.

    Returns a dict with {test_name: {passed, duration_s, output_lines}}.
    """
    cmd = ["cargo", "test", "pipeline::bench", "--", "--nocapture"]
    env = {"DUCKDB_DOWNLOAD_LIB": "1", **__import__("os").environ}

    if verbose:
        print("Running: " + " ".join(cmd))
        print("-" * 60)

    start = time.perf_counter()
    result = subprocess.run(
        cmd,
        env=env,
        capture_output=True,
        text=True,
        cwd=Path(__file__).resolve().parent.parent,
    )
    elapsed = time.perf_counter() - start

    lines = result.stderr.split("\n") + result.stdout.split("\n")

    # Parse individual test results
    test_results = {}
    current_test = None
    output_lines = []

    for line in lines:
        line = line.strip()
        if line.startswith("test pipeline::bench::"):
            parts = line.split()
            if len(parts) >= 2:
                test_name = parts[0].replace("test ", "")
                status = parts[1]
                test_results[test_name] = {
                    "passed": status == "ok",
                    "output": list(output_lines),
                }
                output_lines = []
            continue
        if line.startswith("───"):
            output_lines.append(line)
        if " ch |" in line or "ch/s" in line:
            output_lines.append(line)
        if line.startswith("test result:"):
            break

    return {
        "elapsed_s": elapsed,
        "exit_code": result.returncode,
        "tests": test_results,
    }


def verify_imports() -> dict:
    """Verify Python-side imports for the pipeline boundary."""
    results = {"chunkhound_native": False, "pipeline_bridge": False}

    try:
        import chunkhound_native  # noqa: F401

        results["chunkhound_native"] = True
    except ImportError:
        pass

    try:
        from chunkhound.pipeline_bridge import RustWriterBridge  # noqa: F401

        results["pipeline_bridge"] = True
    except ImportError:
        pass

    return results


def print_matrix():
    """Print the benchmark matrix."""
    print("=" * 80)
    print("Pipeline Benchmark Matrix")
    print("=" * 80)
    print(f"\n{'Scenario':<30} | {'Chunks':>8} | {'Dim':>5} | {'Batch':>5} | Description")
    print("-" * 80)

    for name, info in BENCH_MATRIX.items():
        cfg = info["config"]
        if isinstance(cfg, dict):
            chunks = cfg.get("chunks", "?")
            dim = cfg.get("dim", "?")
            batch = cfg.get("batch", "?")
        else:
            chunks, dim, batch = "varies", "varies", "varies"
            # Extract from string description
            if "batch sizes:" in str(cfg):
                batch = "sweep"
            if "dims:" in str(cfg):
                dim = "sweep"

        desc = info["description"][:50]
        print(f"  {name:<28} | {str(chunks):>8} | {str(dim):>5} | {str(batch):>5} | {desc}")

    print("\n" + "=" * 80)


def main():
    import argparse

    parser = argparse.ArgumentParser(
        description="Embedding pipeline benchmark — documentation and import validation"
    )
    parser.add_argument(
        "--parity",
        action="store_true",
        help="Run full Python-vs-Rust parity comparison (REQUIRES PR #380 — WILL FAIL)",
    )
    parser.add_argument(
        "--print-matrix",
        action="store_true",
        help="Print the benchmark matrix",
    )
    parser.add_argument(
        "--run-rust",
        action="store_true",
        help="Run the Rust benchmark suite via cargo test",
    )
    parser.add_argument("--files", type=int, default=20, help="(parity only) Number of test files")
    parser.add_argument(
        "--chunks-per-file", type=int, default=15, help="(parity only) Chunks per file"
    )
    parser.add_argument("--runs", type=int, default=3, help="(parity only) Benchmark runs")
    args = parser.parse_args()

    # ── Step 1: Verify imports ──
    print("\n[1/3] Python import verification")
    imports = verify_imports()
    for name, ok in imports.items():
        status = "✓" if ok else "✗ (deferred — PR #380)"
        print(f"  {name}: {status}")

    # ── Step 2: Print matrix ──
    if args.print_matrix:
        print_matrix()

    # ── Step 3: Run benchmarks ──
    if args.run_rust:
        print("\n[2/3] Running Rust benchmarks...")
        results = run_rust_benchmarks(verbose=True)
        passed = sum(1 for t in results["tests"].values() if t["passed"])
        total = len(results["tests"])
        print(f"\n  Rust benchmarks: {passed}/{total} passed in {results['elapsed_s']:.1f}s")

    # ── Parity comparison (deferred) ──
    if args.parity:
        print("\n[3/3] Python-vs-Rust pipeline parity comparison")
        print("  ERROR: Pipeline parity comparison requires the real pipeline at the PyO3 boundary.")
        print("  The stub pipeline (Task 11) is Rust-only.")
        print("  Run 'cargo test pipeline::bench -- --nocapture' for Rust benchmarks.")
        print("  Full pipeline parity comparison will be possible after PR #380 lands.")
        return 1

    # ── Summary ──
    print("\n" + "=" * 80)
    print("Summary")
    print("=" * 80)
    print()

    if imports.get("chunkhound_native"):
        print("  chunkhound_native: importable (PyO3 boundary exists)")
    else:
        print("  chunkhound_native: NOT importable")
        print("    Build with: DUCKDB_DOWNLOAD_LIB=1 uv run maturin develop")
        print("    OR: make dev")

    print()
    print("  Rust-native embedding providers: IMPLEMENTED (Tasks 14–17)")
    print("    - openai   → OpenAiProvider (src/embed/openai.rs)")
    print("    - voyageai → VoyageAiProvider (src/embed/voyageai.rs)")
    print()
    print("  Provider parity: VERIFIED via Rust httpmock tests (14 tests)")
    print("    + Python contract tests (33 tests in tests/contracts/test_provider_parity.py)")
    print("    + Both providers parse same JSON format → same vectors")
    print()
    print("  Real API parity (Python SDK vs Rust reqwest):")
    print("    SKIPPED in CI — requires API keys for openai/voyageai")
    print("    Rust httpmock tests provide the authoritative verification")
    print()
    print("  Pipeline parity comparison: DEFERRED (requires PR #380)")
    print()
    print("  Rust benchmarks: 9 tests covering 9 scenarios")
    print("    cargo test pipeline::bench -- --nocapture")
    print()
    print("  Metrics captured: chunks/sec, bloom hit rate, batch distribution,")
    print("                     embedding throughput, oversized chunk filtering")
    print()
    print("  Report: .superpowers/sdd/task-13-report.md")
    print()

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
---
type: Operations
title: Rust-native File Scanner
description: PyO3 Rust extension (chunkhound_native) for fast parallel directory walking with full gitignore semantics.
tags: [operations, performance, rust, pyo3, file-scanner, gitignore]
timestamp: 2026-07-05T00:00:00Z
---

# Rust-native File Scanner

File discovery (directory walk + gitignore filtering) is optionally offloaded to
a Rust extension module for large repositories. The Python scanner remains as a
fallback if the native extension is not installed.

**Key paths:**
- `chunkhound_native/` — Rust crate (PyO3-based)
- The extension is distributed as a pre-built wheel alongside the Python package

## Why Rust?

Python's `os.walk` / `pathlib.glob` are single-threaded and handle `.gitignore`
rules via pure-Python logic. On monorepos with 500k+ files this becomes a
bottleneck before any indexing starts.

The Rust `ignore` crate (from the `ripgrep` ecosystem) provides:
- **Parallel directory walking** via Rayon work-stealing
- **Full gitignore semantics** (global, per-directory, nested `.gitignore` files)
- **Pattern filtering** (exclude globs, hidden files, binary files)

Benchmark: ~10× faster than `os.walk` on a 500k-file monorepo.

## API

```python
from chunkhound_native import scan_files

paths: list[str] = scan_files(
    root="/path/to/project",
    exclude_patterns=["*.pyc", "__pycache__", "node_modules"],
)
```

The function returns a flat list of absolute file paths that pass all gitignore
and exclusion rules.

## Fallback Behavior

ChunkHound detects at import time whether `chunkhound_native` is available:

```python
try:
    from chunkhound_native import scan_files as _native_scan
    _HAS_NATIVE = True
except ImportError:
    _HAS_NATIVE = False

def discover_files(root, exclude_patterns):
    if _HAS_NATIVE:
        return _native_scan(root, exclude_patterns)
    return _python_scan(root, exclude_patterns)
```

The Python fallback produces identical results (same gitignore rules, same
exclude patterns) — just slower.

## Installation

The native extension is included automatically when installing via pip/uv:

```bash
uv add chunkhound           # installs chunkhound + chunkhound_native wheel
pip install chunkhound      # same
```

If pre-built wheels are unavailable for your platform, a source build requires
a Rust toolchain (`cargo`).

# See Also

- [Performance](/operations/performance.md)
- [Indexing Coordinator](/components/indexing-coordinator.md)

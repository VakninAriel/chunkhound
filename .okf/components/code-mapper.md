---
type: Component
title: Code Mapper
description: LLM-driven documentation generation pipeline with coverage tracking and HTML/markdown rendering.
tags: [component, code-mapper, documentation, llm, coverage]
timestamp: 2026-06-30T00:00:00Z
---

# Code Mapper

The code mapper generates human-readable documentation for the codebase by combining
chunk retrieval with LLM synthesis. It is exposed as the `map` and `autodoc` CLI commands.

**Key files:**
- `chunkhound/code_mapper/service.py` — entry point
- `chunkhound/code_mapper/pipeline.py` — orchestration
- `chunkhound/code_mapper/llm.py` — LLM doc generation
- `chunkhound/code_mapper/coverage.py` — coverage tracking
- `chunkhound/code_mapper/render.py` — output rendering
- `chunkhound/code_mapper/metadata.py` — source/module tracking

## Pipeline

```
1. Scope selection
   User specifies a directory or module (e.g., "chunkhound/parsers")
    ↓
2. Chunk retrieval
   db.get_scope_stats(scope_prefix) → file count, chunk count
   db.get_scope_file_paths(scope_prefix) → list of file paths
    ↓
3. Doc generation (per chunk, batched)
   LLM (utility model): "Document this function/class in markdown"
   → docstring, parameter descriptions, return type, examples
    ↓
4. Coverage tracking
   coverage.py records which symbols have generated docs
   → enables incremental runs (skip already-documented chunks)
    ↓
5. Rendering
   render.py assembles docs into HTML report or markdown files
   → one page per module, with navigation sidebar
```

## Dual-Model Usage

Like the [Research Service](/components/research-service.md), the code mapper uses:
- **Utility model** for per-chunk documentation (many calls, cheap)
- **Synthesis model** for module-level summaries (few calls, high quality)

## Output Formats

- **HTML** (`--format html`): self-contained interactive report with navigation
- **Markdown** (`--format md`): flat markdown files, one per module

## Coverage Semantics

Coverage is tracked in `coverage.py` as a set of `(file_path, symbol)` pairs that
have been documented. On subsequent runs, these are skipped. This makes the command
safe to interrupt and resume.

# See Also

- [Research Service](/components/research-service.md)
- [LLM Providers](/providers/llm-providers.md)
- [Database Layer](/components/database-layer.md)

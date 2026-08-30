---
type: Algorithm
title: Search — Regex
description: Pattern matching over raw chunk code using DuckDB's native regexp_matches with optional path filtering.
tags: [algorithm, search, regex, duckdb, pattern-matching]
timestamp: 2026-06-30T00:00:00Z
---

# Regex Search

Regex search matches against the raw `code` field of chunks stored in DuckDB. No
embedding required — results are available even before any embedding has been generated.

**Key file:** `chunkhound/providers/database/duckdb_provider.py` — `_executor_search_regex()`

## SQL Executed

```sql
SELECT c.id, c.symbol, c.code, c.start_line, c.end_line,
       c.chunk_type, c.language, f.path
FROM chunks c
JOIN files f ON c.file_id = f.id
WHERE regexp_matches(c.code, ?)          -- pattern parameter
  AND (? IS NULL OR f.path LIKE '%' || ? || '%')  -- optional path filter
ORDER BY f.path, c.start_line
LIMIT ? OFFSET ?                         -- pagination
```

## Features

- **DuckDB `regexp_matches()`** — PCRE-compatible regex on the full chunk code, not just line-by-line
- **Path filter** — narrow results to a directory subtree (e.g., `path="src/api"`)
- **Sorted output** — results ordered by file path then line number (predictable, diff-friendly)
- **Pagination** — `page_size` and `offset` supported; total count via a separate `COUNT(*)` query
- **No embedding required** — works on a freshly indexed repo with no embedding provider configured

## Limitations

- Regex is case-sensitive by default (DuckDB's `regexp_matches` behavior)
- No ranking by relevance — results are in file-order, not by match quality
- For fuzzy or natural-language queries, use [Single-hop Search](/algorithms/search-single-hop.md)

## Use Cases

- Find all usages of a specific function name: `pattern = r"my_function\s*\("`
- Locate TODO/FIXME comments: `pattern = r"TODO|FIXME"`
- Find imports of a module: `pattern = r"from chunkhound\.parsers import"`
- Combined with path filter: find API endpoints only in `src/api/`

# See Also

- [Hybrid Search](/algorithms/hybrid-search.md)
- [Search — Single-hop](/algorithms/search-single-hop.md)
- [Database Layer](/components/database-layer.md)

---
type: Algorithm
title: Hybrid Search
description: Weighted combination of semantic and regex search results with deduplication by chunk ID.
tags: [algorithm, search, hybrid, scoring, ranking]
timestamp: 2026-06-30T00:00:00Z
---

# Hybrid Search

Combines [semantic search](/algorithms/search-single-hop.md) and [regex search](/algorithms/search-regex.md) results into a single ranked list. Useful when you want both fuzzy semantic matching and exact symbol/pattern matching.

**Key file:** `chunkhound/services/search_service.py`

## Scoring

Each result gets a normalized score in [0, 1] based on its position and similarity:

```
# Semantic result at position i (0-indexed) out of N total:
position_score    = (N - i) / N
similarity_score  = 1.0 - cosine_distance   (from HNSW index)
semantic_score    = 0.3 * position_score + 0.7 * similarity_score

# Regex result at position i out of M total:
regex_score = (M - i) / M
```

## Combination

```
combined_score = semantic_weight * semantic_score
               + (1 - semantic_weight) * regex_score

# Default weights:
semantic_weight = 0.7
regex_weight    = 0.3
```

## Deduplication

A chunk may appear in both the semantic and regex result sets. When a chunk ID is
found in both, the **higher score** is kept and the duplicate is discarded.

```python
seen: dict[ChunkId, float] = {}
for result in all_results:
    if result.chunk_id in seen:
        seen[result.chunk_id] = max(seen[result.chunk_id], result.score)
    else:
        seen[result.chunk_id] = result.score
```

## Result Enhancement

After combining and deduplicating, `ResultEnhancer` post-processes the list:
- Strips `_partN` suffixes from chunk symbols (artifacts from [ChunkSplitter](/algorithms/cast-chunking.md))
- Adds `line_count`, `code_preview` (truncated at 500 chars), file extension
- Formats `similarity` as a percentage (0–100)

# See Also

- [Search — Single-hop](/algorithms/search-single-hop.md)
- [Search — Regex](/algorithms/search-regex.md)
- [Search — Multi-hop](/algorithms/search-multi-hop.md)

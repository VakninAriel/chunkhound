---
type: Algorithm
title: Search — Multi-hop
description: Iterative neighbor expansion with per-round reranking for complex queries requiring contextual depth.
tags: [algorithm, search, multi-hop, reranking, expansion, hnsw]
timestamp: 2026-07-05T00:00:00Z
---

# Multi-hop Search

An advanced semantic search strategy that iteratively expands from high-scoring
results to find contextually related chunks. Used when the embedding provider
supports reranking (VoyageAI rerank-2, Cohere rerank-v3, etc.).

**Key file:** `chunkhound/services/search/multi_hop_strategy.py`

## Flow

```
1. Initial HNSW query → up to 100 candidates (normal mode) or 500 (exhaustive)
   actual cap: min(page_size × 3, INITIAL_LIMIT_CAP_NORMAL=100)
    ↓
2. Rerank initial candidates
   provider.rerank(query, [c.content for c in candidates], top_k=len)
   → scored list sorted by relevance (cross-encoder beats cosine here)
    ↓
3. Select top-5 as expansion seeds (must score > 0)
    ↓
4. For each seed: DB query find_similar_chunks → 20 nearest neighbors (HNSW)
   5 seeds × 20 neighbors = up to 100 new candidates
   Dedup via seen_chunk_ids set — new ones only appended to all_results
    ↓
5. Rerank ALL accumulated candidates (originals + new)
   One rerank API call per expansion round, NOT one per seed
    ↓
6. Termination check — stop if ANY condition met:
   ├─ Time elapsed ≥ time_limit (default 5.0s)
   ├─ result_count ≥ result_limit (default 500)
   ├─ Score degradation: a tracked top-chunk's score dropped ≥ 0.15
   ├─ Minimum top-5 score < 0.3 (quality gate)
   └─ Insufficient seeds (<5 chunks with score > 0)
    ↓
7. Repeat from step 3 with new top-5 seeds
    ↓
8. Apply threshold filter, paginate, return
```

## Why Multi-hop?

Single-hop HNSW finds nearest vectors, but cosine distance measures geometric angle
between independently embedded vectors. A cross-encoder reranker (VoyageAI rerank-2,
Cohere) sees the query and document together — richer cross-attention scores
query-document pairs more accurately.

Expansion further helps when the answer spans multiple related chunks — e.g., a
function definition and its callers live in different positions in embedding space
but share local HNSW graph neighbors.

## Rerank: Once Per Round, Not Once Per Seed

A common misconception: the expand loop calls `find_similar_chunks` 5 times (once
per seed), which looks like 5 rerank calls. It is not. The 5 DB calls collect new
neighbors, then a **single** `provider.rerank()` call scores the entire accumulated
set at the end of the round. One rerank API call = one expansion round.

```python
# expand 5 seeds → collect new_candidates
for candidate in top_candidates:           # 5 DB calls
    neighbors = db.find_similar_chunks(...)
    new_candidates.extend(unseen_neighbors)

# ONE rerank call for everything accumulated
all_results.extend(new_candidates)
rerank_results = await provider.rerank(query, [r["content"] for r in all_results])
```

## Dedup Before Reranking

Deduplication (`seen_chunk_ids`) runs during neighbor collection — before the
rerank call. This is correct: sending duplicate content to a cross-encoder wastes
API tokens and can skew scores. New candidates are guaranteed unique when they
enter `all_results`.

## Configuration

| Parameter | Normal | Exhaustive | Description |
|-----------|--------|------------|-------------|
| `INITIAL_LIMIT_CAP` | 100 | 500 | Max initial HNSW candidates |
| `NEIGHBORS_PER_CANDIDATE` | 20 | 30 | Neighbors per seed per round |
| `time_limit` | 5.0s | (config) | Wall-clock timeout |
| `result_limit` | 500 | (config) | Max chunks to accumulate |
| `seeds_per_round` | 5 | 5 | Top seeds to expand |
| `min_seed_score` | 0.3 | 0.3 | Quality gate for seeds |
| `score_degradation_threshold` | 0.15 | 0.15 | Stop if top score drops this much |

## When Multi-hop Is Selected

`SearchService` auto-selects multi-hop when `provider.supports_reranking()` returns
`True`. Providers with reranking: VoyageAI, Cohere. Single-hop is used for OpenAI
and any provider without a `/rerank` endpoint.

# See Also

- [Search — Single-hop](/algorithms/search-single-hop.md)
- [Hybrid Search](/algorithms/hybrid-search.md)
- [Research Service](/components/research-service.md)

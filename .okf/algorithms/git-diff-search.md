---
type: Algorithm
title: Git-diff Search
description: Semantic search scoped to git diffs — either as a path filter on indexed files or by indexing diff content directly as chunks.
tags: [algorithm, search, git, diff, commits, DiffAwareSearchService]
timestamp: 2026-07-05T00:00:00Z
---

# Git-diff Search

Two distinct modes for searching code changes in git history. The original mode
restricts search to files touched by specific commits. The newer `DiffAwareSearchService`
actually indexes raw diff content as semantic chunks and can search it directly.

## Mode 1: Path-filter Search (original)

Restricts an existing semantic/regex search to files modified in specific commits.
The diff is **not** indexed — only already-indexed chunks from those files are returned.

**Key files:**
- `chunkhound/utils/git_safe.py` — safe git subprocess wrappers
- `chunkhound/utils/git_discovery.py` — git repo detection
- `chunkhound/api/cli/parsers/search_parser.py` — CLI flag parsing

### CLI Flags

```bash
# Search last N commits
chunkhound search "auth logic" --last-n 5

# Search a commit range
chunkhound search "database migration" --commit-range HEAD~10..HEAD

# Search a specific commit
chunkhound search "bug fix" --commit-hash abc1234
```

### Flow

```
1. Parse git flags from CLI
    ↓
2. Resolve modified files
   git_safe.get_files_in_commits(repo, flags) → set of absolute file paths
    ↓
3. Build path filter from those file paths
    ↓
4. Run standard search (semantic or regex) with path filter applied
    ↓
5. Return results in normal format
```

**Gotcha:** Files must already exist in the ChunkHound index. If a modified file was
never indexed, no results are returned — no error is raised.

## Mode 2: DiffAwareSearchService (new)

Indexes the diff itself as chunks and enables searching three targets: the existing
DB index, the live diff, or both merged.

**Key files:**
- `chunkhound/core/git_diff/` — diff parsing and chunk extraction
- `chunkhound/services/search/diff_aware_search_service.py` — DiffAwareSearchService

### New type constants

```python
Language.GIT_DIFF    # language enum for diff content
ChunkType.GIT_DIFF   # chunk type for diff hunks
```

### Three Search Modes

| Mode | What is searched | Use case |
|------|-----------------|----------|
| `db` | Existing ChunkHound index only | Default — unchanged behaviour |
| `diff` | Live `git diff` output only | "What did this PR change related to X?" |
| `both` | DB index + live diff, merged and reranked | Complete picture: existing code + what changed |

### Flow (diff mode)

```
1. Run git diff → raw unified diff text
    ↓
2. Parse diff into hunks → create ChunkType.GIT_DIFF chunks in memory
   (no disk write — in-memory index)
    ↓
3. Embed and search those chunks (single-hop or multi-hop depending on provider)
    ↓
4. For `both` mode: merge DB results + diff results → rerank combined list
    ↓
5. Return unified result set
```

### Key difference from Mode 1

Mode 1 needs the files to already be indexed. Mode 2 works on the raw diff text —
useful for searching changes in files that aren't in the index (e.g., config files,
scripts) or for searching the exact textual change rather than the surrounding code.

# See Also

- [Search — Regex](/algorithms/search-regex.md)
- [Search — Single-hop](/algorithms/search-single-hop.md)
- [Search — Multi-hop](/algorithms/search-multi-hop.md)
- [Config System](/config/config-system.md)

---
type: Algorithm
title: cAST Chunking
description: AST-based semantic code chunking that splits then greedily merges concept nodes to fit size limits.
tags: [algorithm, chunking, parsing, tree-sitter, ast]
timestamp: 2026-06-30T00:00:00Z
---

# cAST Chunking Algorithm

cAST (Code Abstract Syntax Tree) is ChunkHound's core chunking strategy. It produces semantically coherent chunks by working with the code's actual structure rather than splitting on arbitrary line counts.

**Key files:**
- `chunkhound/parsers/universal_parser.py` — main pipeline
- `chunkhound/parsers/universal_engine.py` — tree-sitter wrapper
- `chunkhound/parsers/chunk_splitter.py` — size enforcement
- `chunkhound/parsers/concept_extractor.py` — concept classification
- `chunkhound/parsers/mappings/` — language-specific concept maps

## Pipeline

```
Source file
    ↓
1. TreeSitterEngine.parse(file)
   → AST (tree-sitter grammar, 21 languages)
    ↓
2. ConceptExtractor.extract(ast, language)
   → List[ConceptNode]  (DEFINITION, COMMENT, BLOCK, STRUCTURE, ...)
    ↓
3. MappingAdapter.map(concepts, language)
   → normalized concept list (language-invariant types)
    ↓
4. Greedy merge pass
   → merge adjacent compatible concepts while combined size < 80% of max
    ↓
5. ChunkSplitter.enforce(chunks)
   → split oversized chunks (line-based, or char-based for minified code)
    ↓
Output: List[Chunk]
```

## Concept Types

The `ConceptExtractor` classifies AST nodes into semantic categories:

| Category | Examples |
|----------|---------|
| `DEFINITION` | Function, method, class, struct, interface |
| `COMMENT` | Line comments, block comments, docstrings |
| `BLOCK` | Unnamed code blocks, top-level statements |
| `STRUCTURE` | Module-level groupings, namespaces |

Language-specific mappings in `parsers/mappings/` translate tree-sitter node types
to these universal categories (e.g., `def_statement` in Python → `DEFINITION`).

## Greedy Merge

Adjacent concept nodes are merged if:
1. The pair is a **compatible type combination** (COMMENT+DEFINITION, DEFINITION+STRUCTURE, BLOCK+COMMENT)
2. The combined non-whitespace character count is **< 80% of `max_chunk_size`** (default 960 of 1200)

This keeps a docstring with its function, a decorator with its class, etc.

## ChunkSplitter — Size Enforcement

**Config (`CASTConfig`):**
- `max_chunk_size`: 1200 non-whitespace characters
- `min_chunk_size`: 50 characters
- `safe_token_limit`: 6000 tokens (embedding model safety margin)
- `merge_threshold`: 0.8

**Splitting strategies:**
1. **Line-based** (default): split at line boundaries, respecting indentation
2. **Character-based** (fallback for minified code): split at character positions when lines are too long

Chunks that exceed `safe_token_limit` (estimated via tiktoken or `chars/4` fallback) are always split regardless of the strategy.

## Why Not Fixed-Size Sliding Windows?

Fixed windows cut across function boundaries, leaving incomplete code fragments that
confuse embedding models. cAST preserves the semantic units the model was trained on
(functions, classes, comments) — this measurably improves retrieval quality.

## Language Support

21 languages via tree-sitter grammars. Special-case parsers for:
- **Makefile** — build rules and targets (`parsers/makefile_parser.py`)
- **YAML** — faster than generic (`parsers/rapid_yaml_parser.py`)
- **Vue/Svelte** — component file parsing (`parsers/vue_parser.py`, `parsers/svelte_parser.py`)
- **SQL in strings** — embedded SQL detection (`parsers/embedded_sql_detector.py`)

# See Also

- [Domain Models](/architecture/domain-models.md)
- [Embedding Pipeline](/algorithms/embedding-pipeline.md)
- [Indexing Coordinator](/components/indexing-coordinator.md)

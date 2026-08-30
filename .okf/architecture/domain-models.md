---
type: Architecture
title: Domain Models
description: Core data types — Chunk, File, Embedding — and their supporting enums.
tags: [architecture, models, types, chunk, embedding]
timestamp: 2026-06-30T00:00:00Z
---

# Domain Models

All models live in `chunkhound/core/models/` and `chunkhound/core/types/`.

## Chunk

The central unit of ChunkHound. A `Chunk` is an immutable, semantically bounded
excerpt of source code extracted by the [cAST algorithm](/algorithms/cast-chunking.md).

```python
@dataclass(frozen=True)
class Chunk:
    symbol: str              # Function/class name (or file path for top-level blocks)
    start_line: LineNumber   # 1-indexed
    end_line: LineNumber
    code: str                # Raw source text
    chunk_type: ChunkType    # Semantic classification
    file_id: FileId          # FK to File
    language: Language       # Programming language
    metadata: dict[str, Any] # Provider-specific extras
    start_byte: ByteOffset | None
    end_byte: ByteOffset | None
```

**ChunkType enum** — 40+ values covering:
- Code definitions: `FUNCTION`, `CLASS`, `METHOD`, `INTERFACE`, `STRUCT`, `ENUM`, `TRAIT`
- Documentation: `COMMENT`, `DOCSTRING`
- Structure: `BLOCK`, `MODULE`, `NAMESPACE`, `IMPORT`
- Language-specific: `HEADER_GUARD`, `MACRO`, `DECORATOR`, `ANNOTATION`
- Fallback: `CODE_BLOCK` (unnamed top-level code)

## File

Tracks source file metadata and change detection.

```python
@dataclass
class File:
    path: FilePath           # Absolute path
    language: Language
    size: int                # Bytes
    mtime: float             # Modification time (Unix timestamp)
    hash: str                # SHA-256 of file content (change detection)
    created_at: Timestamp
    updated_at: Timestamp
    id: FileId | None = None # Set after DB insert
```

Hash-based change detection: if `hash` is unchanged, all existing chunks and embeddings are preserved on re-index.

## Embedding

A vector embedding tied to a specific chunk, provider, and model.

```python
@dataclass
class Embedding:
    chunk_id: ChunkId
    provider: str            # e.g. "openai"
    model: str               # e.g. "text-embedding-3-small"
    dims: Dimensions         # e.g. 1536
    vector: list[float]
    created_at: Timestamp
```

Embeddings are keyed on `(chunk_id, provider, model)`. Changing provider or model requires re-embedding all chunks.

## Type Aliases

Defined via `NewType` in `chunkhound/core/types/common.py`:

| Alias | Base | Purpose |
|-------|------|---------|
| `ChunkId` | `int` | PK in chunks table |
| `FileId` | `int` | PK in files table |
| `FilePath` | `str` | Absolute filesystem path |
| `LineNumber` | `int` | 1-indexed source line |
| `ByteOffset` | `int` | Byte position in file |
| `Timestamp` | `float` | Unix timestamp |
| `Distance` | `float` | Cosine distance [0, 1] |
| `Dimensions` | `int` | Embedding vector length |

## Language Enum

21 supported languages: Python, JavaScript, TypeScript, Java, Go, Rust, C, C++, C#, PHP, Ruby, Swift, Kotlin, Scala, Haskell, Lua, R, Julia, SQL, YAML, Makefile.

Language detection is file-extension-based via `chunkhound/core/detection/`.

# See Also

- [cAST Chunking](/algorithms/cast-chunking.md)
- [Database Layer](/components/database-layer.md)
- [Provider Plugin System](/architecture/provider-plugin-system.md)

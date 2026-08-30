"""Stage B diagnostic: does TurboVec's recall@1 gap shrink at native dim?

Picks a matched sample of chunks from this repo's own indexed corpus, dumps
their EXISTING 256-dim (truncated) embeddings as-is, then re-embeds the same
texts through the same provider/model but requesting the native dimension
(no truncation) — holding corpus/query split identical so `probe_dim` can
compare recall@1 at 256-dim vs native dim on literally the same data.
qwen3-embedding-4b's actual native dim is 2560 (not 1536 as QWEN3_TUNING.md
implies) — confirmed empirically by this script; don't hardcode it.

Run from the repo root:
    uv run python test-crates/turbovec-poc/scripts/probe_native_dim.py
"""

import asyncio
import os
import random
import struct
from pathlib import Path

# See scripts/demo_compare.py for why: the embedding server's base_url uses a
# bare internal hostname the corporate proxy's resolver can't handle.
os.environ["NO_PROXY"] = os.environ.get("NO_PROXY", "") + ",pdc-llm-srv1"
os.environ["no_proxy"] = os.environ["NO_PROXY"]

import duckdb

from chunkhound.core.config.config import Config
from chunkhound.core.config.embedding_factory import EmbeddingProviderFactory

REPO_ROOT = Path(__file__).resolve().parents[3]
POC_ROOT = Path(__file__).resolve().parent.parent
DB_PATH = REPO_ROOT / ".chunkhound" / "db" / "chunks.db"

N_CORPUS = 4000
N_QUERY = 300
SEED = 20260803


def write_fixture(path: Path, dim: int, chunk_ids: list[int], vectors: list[list[float]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "wb") as f:
        f.write(b"CHKNVEC1")
        f.write(struct.pack("<II", len(chunk_ids), dim))
        for chunk_id, emb in zip(chunk_ids, vectors):
            f.write(struct.pack("<q", chunk_id))
            f.write(struct.pack(f"<{len(emb)}f", *emb))
    print(f"Wrote {len(chunk_ids)} vectors (dim={dim}) to {path} ({path.stat().st_size / 1e6:.1f} MB)")


async def main() -> None:
    con = duckdb.connect(str(DB_PATH), read_only=True)
    all_rows = con.execute(
        """
        SELECT e.chunk_id, c.code, e.embedding
        FROM embeddings_256 e
        JOIN chunks c ON e.chunk_id = c.id
        WHERE c.code IS NOT NULL AND length(c.code) > 0
        ORDER BY e.id
        """
    ).fetchall()
    con.close()
    print(f"{len(all_rows)} candidate chunks available")

    rng = random.Random(SEED)
    sample = rng.sample(all_rows, N_CORPUS + N_QUERY)
    corpus_rows = sample[:N_CORPUS]
    query_rows = sample[N_CORPUS:]

    # Fixed order shared by both fixtures: corpus first, then held-out
    # queries — probe_dim.rs splits by position (last N_QUERY = queries),
    # same convention as bench's main.rs.
    ordered_rows = corpus_rows + query_rows
    chunk_ids = [r[0] for r in ordered_rows]
    texts = [r[1] for r in ordered_rows]
    existing_256 = [list(r[2]) for r in ordered_rows]

    write_fixture(POC_ROOT / "fixtures" / "probe_256.bin", 256, chunk_ids, existing_256)

    config = Config(target_dir=str(REPO_ROOT))
    native_embedding_config = config.embedding.model_copy(
        update={"output_dims": None, "client_side_truncation": False}
    )
    native_provider = EmbeddingProviderFactory.create_provider(native_embedding_config)

    print(f"Re-embedding {len(texts)} texts at native dim (no truncation)...")
    native_vectors = await native_provider.embed(texts)
    native_dim = len(native_vectors[0])
    print(f"Native dim confirmed: {native_dim}")

    write_fixture(POC_ROOT / "fixtures" / "probe_native.bin", native_dim, chunk_ids, native_vectors)


if __name__ == "__main__":
    asyncio.run(main())

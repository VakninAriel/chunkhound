"""One-off extraction: dump embeddings_256 from the repo's own chunks.db to a flat
little-endian binary fixture the Rust POC can read without any DB dependency.

Run once from the repo's own .venv:
    uv run python test-crates/turbovec-poc/scripts/dump_embeddings.py
"""

import struct
from pathlib import Path

import duckdb

REPO_ROOT = Path(__file__).resolve().parents[3]
DB_PATH = REPO_ROOT / ".chunkhound" / "db" / "chunks.db"
OUT_PATH = Path(__file__).resolve().parent.parent / "fixtures" / "embeddings_256.bin"


def main() -> None:
    con = duckdb.connect(str(DB_PATH), read_only=True)
    rows = con.execute("SELECT chunk_id, embedding FROM embeddings_256 ORDER BY id").fetchall()
    con.close()

    dim = len(rows[0][1])
    OUT_PATH.parent.mkdir(parents=True, exist_ok=True)
    with open(OUT_PATH, "wb") as f:
        f.write(b"CHKNVEC1")
        f.write(struct.pack("<II", len(rows), dim))
        for chunk_id, emb in rows:
            f.write(struct.pack("<q", chunk_id))
            f.write(struct.pack(f"<{len(emb)}f", *emb))

    print(f"Wrote {len(rows)} vectors (dim={dim}) to {OUT_PATH} ({OUT_PATH.stat().st_size / 1e6:.1f} MB)")


if __name__ == "__main__":
    main()

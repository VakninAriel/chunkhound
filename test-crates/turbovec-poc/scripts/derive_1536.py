"""Derive a 1536-dim fixture from the native 2560-dim one (no new API calls):
slice the first 1536 dims off each native vector and L2-renormalize, same
transform ChunkHound's own client-side truncation applies (see
apply_client_side_truncation/l2_normalize in
chunkhound/providers/embeddings/shared_utils.py) just at a different cutoff.
Gives a real (if synthetic) 1536-dim data point for the 256 vs. 1536 vs.
2560 (native) comparison, without re-embedding anything.

Run from the repo root, after scripts/probe_native_dim.py:
    uv run python test-crates/turbovec-poc/scripts/derive_1536.py
"""

import math
import struct
from pathlib import Path

POC_ROOT = Path(__file__).resolve().parent.parent
SRC_PATH = POC_ROOT / "fixtures" / "probe_native.bin"
DST_PATH = POC_ROOT / "fixtures" / "probe_1536.bin"
TARGET_DIM = 1536

MAGIC = b"CHKNVEC1"


def read_fixture(path: Path):
    with open(path, "rb") as f:
        data = f.read()
    assert data[:8] == MAGIC, "bad fixture magic"
    n, dim = struct.unpack("<II", data[8:16])
    record_size = 8 + dim * 4
    assert len(data) == 16 + n * record_size, "fixture size does not match header"

    chunk_ids = []
    vectors = []
    offset = 16
    for _ in range(n):
        (chunk_id,) = struct.unpack("<q", data[offset : offset + 8])
        offset += 8
        vec = struct.unpack(f"<{dim}f", data[offset : offset + dim * 4])
        offset += dim * 4
        chunk_ids.append(chunk_id)
        vectors.append(vec)
    return chunk_ids, dim, vectors


def l2_normalize(v: tuple[float, ...]) -> list[float]:
    norm = math.sqrt(sum(x * x for x in v))
    if norm == 0.0:
        return list(v)
    return [x / norm for x in v]


def main() -> None:
    chunk_ids, dim, vectors = read_fixture(SRC_PATH)
    assert dim >= TARGET_DIM, f"native dim {dim} is smaller than target {TARGET_DIM}"

    truncated = [l2_normalize(v[:TARGET_DIM]) for v in vectors]

    with open(DST_PATH, "wb") as f:
        f.write(MAGIC)
        f.write(struct.pack("<II", len(chunk_ids), TARGET_DIM))
        for chunk_id, emb in zip(chunk_ids, truncated):
            f.write(struct.pack("<q", chunk_id))
            f.write(struct.pack(f"<{TARGET_DIM}f", *emb))

    print(f"Wrote {len(chunk_ids)} vectors (dim={TARGET_DIM}) to {DST_PATH} ({DST_PATH.stat().st_size / 1e6:.1f} MB)")


if __name__ == "__main__":
    main()

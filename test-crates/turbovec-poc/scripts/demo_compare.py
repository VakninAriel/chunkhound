"""Live demo: run the same query against the existing production search path
(DuckDB-VSS/HNSW) and the new TurboVec index, side by side.

Run from the repo root, e.g.:
    uv run python test-crates/turbovec-poc/scripts/demo_compare.py "some query"
    uv run python test-crates/turbovec-poc/scripts/demo_compare.py --interactive

Requires `cargo run --release --bin bench` to have been run at least once
(it produces fixtures/index_4bit.tv, which `serve` loads).
"""

import argparse
import asyncio
import json
import os
import subprocess
import sys
from pathlib import Path

# The embedding server's base_url (.chunkhound.json) uses a bare internal
# hostname (no domain suffix). The corporate proxy's own resolver can't
# resolve bare hostnames (only FQDNs), so httpx routes the request through
# the proxy and it fails with "dns_unresolved_hostname" even though this
# host resolves fine locally. Excluding it from the proxy here is scoped to
# this process only — it doesn't touch the checked-in project config.
os.environ["NO_PROXY"] = os.environ.get("NO_PROXY", "") + ",pdc-llm-srv1"
os.environ["no_proxy"] = os.environ["NO_PROXY"]

import duckdb

from chunkhound.core.config.config import Config
from chunkhound.core.config.embedding_factory import EmbeddingProviderFactory
from chunkhound.database_factory import create_services
from chunkhound.embeddings import EmbeddingManager

REPO_ROOT = Path(__file__).resolve().parents[3]
POC_ROOT = Path(__file__).resolve().parent.parent
DB_PATH = REPO_ROOT / ".chunkhound" / "db" / "chunks.db"
SERVE_BIN = POC_ROOT / "target" / "release" / "serve"


def start_serve() -> subprocess.Popen:
    if not SERVE_BIN.exists():
        sys.exit(f"{SERVE_BIN} not found — run `cargo build --release --bin serve` first")
    proc = subprocess.Popen(
        [str(SERVE_BIN)],
        cwd=str(POC_ROOT),
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        text=True,
        bufsize=1,
    )
    ready_line = proc.stdout.readline().strip()
    if ready_line != "READY":
        sys.exit(f"serve did not report READY, got: {ready_line!r}")
    return proc


def turbovec_search(proc: subprocess.Popen, vector: list[float], k: int) -> tuple[list[int], list[float]]:
    proc.stdin.write(json.dumps({"vector": vector, "k": k}) + "\n")
    proc.stdin.flush()
    resp = json.loads(proc.stdout.readline())
    return resp["chunk_ids"], resp["scores"]


def fetch_metadata(con: duckdb.DuckDBPyConnection, chunk_ids: list[int]) -> dict[int, dict]:
    if not chunk_ids:
        return {}
    placeholders = ",".join("?" for _ in chunk_ids)
    rows = con.execute(
        f"""
        SELECT e.chunk_id, f.path, c.start_line, c.end_line
        FROM embeddings_256 e
        JOIN chunks c ON e.chunk_id = c.id
        JOIN files f ON c.file_id = f.id
        WHERE e.chunk_id IN ({placeholders})
        """,
        chunk_ids,
    ).fetchall()
    return {row[0]: {"file_path": row[1], "start_line": row[2], "end_line": row[3]} for row in rows}


def format_row(file_path, start_line, end_line, score: float) -> str:
    if file_path is None:
        return "(missing metadata)"
    return f"{file_path}:{start_line}-{end_line}  ({score:.4f})"


async def run_query(query: str, k: int, embed_provider, services, serve_proc, meta_con) -> None:
    vector = await embed_provider.embed_single(query)

    existing_results, _ = services.provider.search_semantic(
        query_embedding=vector,
        provider=embed_provider.name,
        model=embed_provider.model,
        page_size=k,
    )
    existing_ids = [r["chunk_id"] for r in existing_results]

    tv_ids, tv_scores = turbovec_search(serve_proc, vector, k)
    tv_meta = fetch_metadata(meta_con, tv_ids)

    print(f"\nQuery: {query!r}")
    print(f"{'#':<3} {'EXISTING (DuckDB-VSS, exact)':<60} {'TURBOVEC (quantized)':<60}")
    for rank in range(k):
        existing_str = "-"
        if rank < len(existing_results):
            r = existing_results[rank]
            existing_str = format_row(r["file_path"], r["start_line"], r["end_line"], r["similarity"])
        tv_str = "-"
        if rank < len(tv_ids):
            m = tv_meta.get(tv_ids[rank])
            tv_str = (
                format_row(m["file_path"], m["start_line"], m["end_line"], tv_scores[rank])
                if m
                else f"(chunk_id {tv_ids[rank]} not found) ({tv_scores[rank]:.4f})"
            )
        print(f"{rank + 1:<3} {existing_str:<60} {tv_str:<60}")

    overlap = len(set(existing_ids) & set(tv_ids)) / k
    top1_match = bool(existing_ids and tv_ids and existing_ids[0] == tv_ids[0])
    print(f"\noverlap@{k} = {overlap:.0%}   rank-1 match = {top1_match}")


async def main_async(args: argparse.Namespace) -> None:
    config = Config(target_dir=str(REPO_ROOT))
    embed_provider = EmbeddingProviderFactory.create_provider(config.embedding)
    services = create_services(db_path=str(DB_PATH), config=config, embedding_manager=EmbeddingManager())
    # DuckDB refuses a second connection to the same file with a different
    # config (e.g. read_only=True) once create_services has already opened
    # one read-write — match that mode here; this connection only issues
    # SELECTs.
    meta_con = duckdb.connect(str(DB_PATH))
    serve_proc = start_serve()

    try:
        if args.interactive:
            print("Interactive live demo. Type a query, or 'exit'/blank line to quit.")
            while True:
                try:
                    query = input("query> ").strip()
                except EOFError:
                    break
                if not query or query.lower() in ("exit", "quit"):
                    break
                await run_query(query, args.k, embed_provider, services, serve_proc, meta_con)
        else:
            await run_query(args.query, args.k, embed_provider, services, serve_proc, meta_con)
    finally:
        serve_proc.stdin.close()
        serve_proc.wait(timeout=5)
        meta_con.close()


def main() -> None:
    parser = argparse.ArgumentParser(description="Live demo: compare existing-DB vs TurboVec search")
    parser.add_argument("query", nargs="?", help="query text (omit with --interactive)")
    parser.add_argument("--interactive", action="store_true", help="REPL mode")
    parser.add_argument("--k", type=int, default=10)
    args = parser.parse_args()
    if not args.interactive and not args.query:
        parser.error("either provide a query or use --interactive")
    asyncio.run(main_async(args))


if __name__ == "__main__":
    main()

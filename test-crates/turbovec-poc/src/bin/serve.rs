#![forbid(unsafe_code)]

//! Live-demo query server: loads the 4-bit index snapshot once, then answers
//! one query per stdin line until EOF. Kept warm across queries so a Python
//! orchestrator (scripts/demo_compare.py) can drive an interactive REPL
//! without paying index-load cost per query.
//!
//! Protocol: prints "READY" once loaded. Each stdin line is one JSON object
//! `{"vector": [f32; dim], "k": usize}`; each response is one JSON line
//! `{"chunk_ids": [u64...], "scores": [f32...]}`.

extern crate blas_src as _;

use std::io::{self, BufRead, Write};

use serde::{Deserialize, Serialize};
use turbovec::IdMapIndex;
use turbovec_poc::normalize;

const INDEX_SNAPSHOT_PATH: &str = "fixtures/index_4bit.tv";

#[derive(Deserialize)]
struct QueryRequest {
    vector: Vec<f32>,
    k: usize,
}

#[derive(Serialize)]
struct QueryResponse {
    chunk_ids: Vec<u64>,
    scores: Vec<f32>,
}

fn main() {
    let index = IdMapIndex::load(INDEX_SNAPSHOT_PATH).unwrap_or_else(|e| {
        panic!(
            "failed to load {INDEX_SNAPSHOT_PATH} (run `cargo run --release --bin bench` first): {e}"
        )
    });

    println!("READY");
    io::stdout().flush().expect("flush stdout");

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = line.expect("read stdin line");
        if line.trim().is_empty() {
            continue;
        }
        let req: QueryRequest = serde_json::from_str(&line).expect("parse query JSON");

        let mut vector = req.vector;
        normalize(&mut vector);
        let (scores, chunk_ids) = index.search(&vector, req.k);

        let resp = QueryResponse { chunk_ids, scores };
        let resp_json = serde_json::to_string(&resp).expect("serialize response JSON");
        writeln!(stdout, "{resp_json}").expect("write response line");
        stdout.flush().expect("flush stdout");
    }
}

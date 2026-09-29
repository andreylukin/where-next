//! Query-path overhead on a 20k-file index, without model weights: the hashing encoder stands in
//! for the model, so this measures ranking, abstain logic and reply assembly. Real-model encode
//! latency is measured by `wn-mcp`'s `latency` example.

use criterion::{criterion_group, criterion_main, Criterion};
use wn_core::encoder::HashEncoder;
use wn_core::index::{Index, IndexedFile};
use wn_core::runtime::{suggest, SuggestOptions};
use wn_sources::Kind;

fn synthetic_index(files: usize, dim: usize) -> (Index, HashEncoder) {
    let dir = tempfile::tempdir().unwrap();
    let encoder = HashEncoder { dim };
    let list: Vec<IndexedFile> = (0..files)
        .map(|i| IndexedFile {
            path: format!("pkg{}/module_{i}.rs", i % 97),
            cid: format!("c{i}"),
            kind: Kind::Source,
        })
        .collect();
    let read = |path: &str, _k: Kind| {
        Some(format!(
            "//! Module {path}.\nfn handle_{0}() {{}}\nfn retry_{0}() {{}}\n",
            path.len()
        ))
    };
    let mut index = Index::open(&dir.keep(), "bench");
    index.refresh(&list, &read, &encoder, false).unwrap();
    (index, encoder)
}

fn bench(c: &mut Criterion) {
    let (index, encoder) = synthetic_index(20_000, 1024);
    let opts = SuggestOptions::default();
    c.bench_function("suggest_20k_files_1024d", |b| {
        b.iter(|| {
            suggest(
                &index,
                None,
                &encoder,
                "retry the request when the upstream times out",
                "",
                opts,
            )
        })
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);

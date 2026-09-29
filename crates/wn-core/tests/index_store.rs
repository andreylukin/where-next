//! Index storage and ranking: ranking reads vectors in place, and an interrupted first build
//! resumes from its last checkpoint instead of starting over.

use std::cell::Cell;

use proptest::prelude::*;
use wn_core::encoder::{EncodeError, Encoder, HashEncoder, QueryInput};
use wn_core::index::{EntryKind, Index, IndexedFile};
use wn_core::index_lifecycle::IndexState;
use wn_core::rank::top_k;
use wn_sources::Kind;

fn files(n: usize) -> Vec<IndexedFile> {
    (0..n)
        .map(|i| IndexedFile {
            path: format!("src/mod{i}.py"),
            cid: format!("v{i}"),
            kind: if i % 7 == 0 {
                Kind::Config
            } else {
                Kind::Source
            },
        })
        .collect()
}

fn read(path: &str, _kind: Kind) -> Option<String> {
    Some(format!("def handler_{}():\n    pass\n", path.len()))
}

/// Embeds like `HashEncoder`, but fails once `budget` documents have been embedded.
struct FlakyEncoder {
    inner: HashEncoder,
    budget: Cell<usize>,
    calls: Cell<usize>,
}

impl FlakyEncoder {
    fn new(budget: usize) -> Self {
        Self {
            inner: HashEncoder { dim: 32 },
            budget: Cell::new(budget),
            calls: Cell::new(0),
        }
    }
}

impl Encoder for FlakyEncoder {
    fn fingerprint(&self) -> String {
        self.inner.fingerprint()
    }
    fn documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EncodeError> {
        self.calls.set(self.calls.get() + 1);
        if texts.len() > self.budget.get() {
            return Err(EncodeError("interrupted".into()));
        }
        self.budget.set(self.budget.get() - texts.len());
        self.inner.documents(texts)
    }
    fn queries(&self, items: &[QueryInput]) -> Result<Vec<Vec<f32>>, EncodeError> {
        self.inner.queries(items)
    }
}

#[test]
fn interrupted_first_build_resumes_from_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let all = files(50);

    let mut index = Index::open(dir.path(), "hash-bow2-32");
    index.set_checkpoint_every(10);
    let flaky = FlakyEncoder::new(25);
    assert!(index.refresh(&all, &read, &flaky, false).is_err());
    assert_eq!(index.state(), IndexState::Error);

    // Reopening keeps the checkpointed vectors but refuses to serve an unfinished build.
    let mut reopened = Index::open(dir.path(), "hash-bow2-32");
    assert_eq!(reopened.state(), IndexState::Uninitialized);
    let saved = reopened.count(EntryKind::File) + reopened.count(EntryKind::Config);
    assert_eq!(
        saved, 20,
        "two checkpoints of 10 were written before the failure"
    );

    let good = HashEncoder { dim: 32 };
    let stats = reopened.refresh(&all, &read, &good, false).unwrap();
    assert_eq!(
        stats.encoded, 30,
        "only the files missing from the checkpoint are embedded"
    );
    assert_eq!(reopened.state(), IndexState::Ready);
    assert_eq!(
        reopened.count(EntryKind::File) + reopened.count(EntryKind::Config),
        50
    );

    // A completed build reopens ready to serve.
    assert_eq!(
        Index::open(dir.path(), "hash-bow2-32").state(),
        IndexState::Ready
    );
}

#[test]
fn encoding_happens_in_checkpoint_sized_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let mut index = Index::open(dir.path(), "hash-bow2-32");
    index.set_checkpoint_every(8);
    let enc = FlakyEncoder::new(usize::MAX);
    index.refresh(&files(20), &read, &enc, false).unwrap();
    assert_eq!(enc.calls.get(), 3, "20 documents in chunks of 8");
}

proptest! {
    /// Ranking in place returns exactly what ranking a copied per-kind matrix returns.
    #[test]
    fn rank_matches_copied_matrix(n in 1usize..40, k in 1usize..8, seed in 0u64..1000) {
        let dir = tempfile::tempdir().unwrap();
        let mut index = Index::open(dir.path(), "hash-bow2-16");
        let enc = HashEncoder { dim: 16 };
        index.refresh(&files(n), &read, &enc, false).unwrap();
        let q: Vec<f32> = (0..16).map(|i| (((seed + i) * 2654435761) % 97) as f32 / 97.0 - 0.5).collect();
        for kind in [EntryKind::File, EntryKind::Config] {
            let (rows, mat) = index.matrix(kind);
            let expected: Vec<(String, f32)> = top_k(&mat, 16, &q, k)
                .into_iter()
                .map(|(i, s)| (rows[i].path.clone(), s))
                .collect();
            let got: Vec<(String, f32)> = index
                .rank(&q, kind, k)
                .into_iter()
                .map(|h| (h.path, h.similarity as f32))
                .collect();
            prop_assert_eq!(got.len(), expected.len());
            for (g, e) in got.iter().zip(&expected) {
                prop_assert_eq!(&g.0, &e.0);
                prop_assert!((g.1 - wn_core::rank::round4(e.1) as f32).abs() < 1e-6);
            }
        }
    }
}

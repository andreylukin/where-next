//! CI quality gate: a small fixed benchmark that runs without model weights.
//!
//! `tests/data/gate/` holds precomputed gemma-xl1 vectors (Matryoshka-truncated to 256 dimensions,
//! f16) for ContextBench tasks from small public repositories: each task's issue-text query, the
//! candidate files at its base commit, the gold files, and adapter history (older commit messages
//! with the file each changed, shared per repository). The test ranks every task with wn-core's ranking, adapter and
//! calibrated abstain, and fails if hit@3 drops more than one point below `baseline.json` or if
//! ranking gets slower than the latency budget.
//!
//! Regenerate the fixture: `modal run calib_modal.py::make_gate` in the research repo (see
//! `tests/data/gate/README.md`). Re-record the baseline after an intended change with
//! `WN_GATE_RECORD=1 cargo test -p wn-core --release --test quality_gate`.

use std::path::PathBuf;
use std::time::Instant;

use serde::Deserialize;
use wn_core::adapter::{apply_adapter, fit_adapter, sample_negatives, AdapterParams};
use wn_core::rank::{abstain_with, top_k, Calibration, Hint, QueryKind};

#[derive(Deserialize)]
struct Fixture {
    dim: usize,
    repos: Vec<Repo>,
}

#[derive(Deserialize)]
struct Repo {
    name: String,
    docs: [usize; 2],
    /// Adapter history shared by the repository's tasks: `[query vector, document]`.
    hist: Vec<[usize; 2]>,
    tasks: Vec<Task>,
}

#[derive(Deserialize)]
struct Task {
    kind: QueryKind,
    q: usize,
    cand: Vec<usize>,
    gold: Vec<usize>,
}

#[derive(Deserialize, serde::Serialize, Default, Debug)]
struct Scores {
    tasks: usize,
    plain_hit3: f64,
    adapter_hit3: f64,
    adapter_tasks: usize,
    answered: usize,
    answered_hit3: f64,
}

/// Slack on hit@3 before the gate fails (one point).
const SLACK: f64 = 0.01;
/// Budget for ranking one query over its candidates, p95, in microseconds (debug builds are slow).
const RANK_P95_US: u128 = 20_000;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/gate")
}

fn load() -> (Fixture, Vec<f32>) {
    let fixture: Fixture =
        serde_json::from_str(&std::fs::read_to_string(data_dir().join("fixture.json")).unwrap())
            .unwrap();
    let bytes = std::fs::read(data_dir().join("fixture.bin")).unwrap();
    let vecs = bytes
        .chunks_exact(2)
        .map(|b| half::f16::from_le_bytes([b[0], b[1]]).to_f32())
        .collect();
    (fixture, vecs)
}

fn rows(vecs: &[f32], d: usize, ids: impl IntoIterator<Item = usize>) -> Vec<f32> {
    ids.into_iter()
        .flat_map(|i| vecs[i * d..(i + 1) * d].iter().copied())
        .collect()
}

fn hits(order: &[(usize, f32)], wanted: &[usize], k: usize) -> bool {
    order.iter().take(k).any(|(i, _)| wanted.contains(i))
}

fn calibration() -> Calibration {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../wn-embed/calibrations/gemma-xl1.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn quality_gate() {
    let (fx, vecs) = load();
    let d = fx.dim;
    let cal = calibration();
    let mut s = Scores::default();
    let (mut plain, mut adapted, mut answered_hits) = (0usize, 0usize, 0usize);
    let mut rank_us = Vec::new();
    for repo in &fx.repos {
        let doc0 = repo.docs[0];
        for t in &repo.tasks {
            let cand: Vec<usize> = t.cand.iter().map(|&c| doc0 + c).collect();
            let mat = rows(&vecs, d, cand.iter().copied());
            let wanted: Vec<usize> = t
                .gold
                .iter()
                .filter_map(|g| t.cand.iter().position(|c| c == g))
                .collect();
            let q = &vecs[t.q * d..(t.q + 1) * d];
            let started = Instant::now();
            let order = top_k(&mat, d, q, 10);
            rank_us.push(started.elapsed().as_micros());
            plain += hits(&order, &wanted, 3) as usize;
            s.tasks += 1;
            // Personal adapter from the repository's earlier commits.
            let usable: Vec<(usize, usize)> = repo
                .hist
                .iter()
                .filter_map(|[v, doc]| t.cand.iter().position(|c| c == doc).map(|p| (*v, p)))
                .collect();
            let (final_order, has_adapter) = if usable.len() >= 20 {
                let qv = rows(&vecs, d, usable.iter().map(|(v, _)| *v));
                let pos: Vec<usize> = usable.iter().map(|(_, p)| *p).collect();
                let params = AdapterParams::default();
                let negs = sample_negatives(&pos, t.cand.len(), params.negatives, 0);
                let w = fit_adapter(&qv, &mat, d, &pos, &negs, &params);
                let aq = apply_adapter(q, &w, d);
                let ao = top_k(&mat, d, &aq, 10);
                adapted += hits(&ao, &wanted, 3) as usize;
                s.adapter_tasks += 1;
                (ao, true)
            } else {
                (order, false)
            };
            let files: Vec<Hint> = final_order
                .iter()
                .take(3)
                .map(|(i, sim)| Hint {
                    path: i.to_string(),
                    similarity: *sim as f64,
                    name: None,
                    line: None,
                })
                .collect();
            let abstain = cal
                .thresholds(t.kind, has_adapter, false)
                .and_then(|th| abstain_with(&files, th));
            if abstain.is_none() {
                s.answered += 1;
                answered_hits += hits(&final_order, &wanted, 3) as usize;
            }
        }
        assert!(!repo.name.is_empty());
    }
    s.plain_hit3 = plain as f64 / s.tasks as f64;
    s.adapter_hit3 = adapted as f64 / s.adapter_tasks.max(1) as f64;
    s.answered_hit3 = answered_hits as f64 / s.answered.max(1) as f64;
    rank_us.sort();
    let p95 = rank_us[(rank_us.len() * 95 / 100).min(rank_us.len() - 1)];
    println!("{s:?} rank p95 {p95} us");
    let baseline_path = data_dir().join("baseline.json");
    if std::env::var("WN_GATE_RECORD").is_ok() {
        std::fs::write(
            &baseline_path,
            serde_json::to_string_pretty(&s).unwrap() + "\n",
        )
        .unwrap();
        return;
    }
    let base: Scores =
        serde_json::from_str(&std::fs::read_to_string(baseline_path).unwrap()).unwrap();
    assert_eq!(
        s.tasks, base.tasks,
        "fixture changed; re-record the baseline"
    );
    assert!(
        s.plain_hit3 >= base.plain_hit3 - SLACK,
        "plain hit@3 {:.3} < baseline {:.3}",
        s.plain_hit3,
        base.plain_hit3
    );
    assert!(
        s.adapter_hit3 >= base.adapter_hit3 - SLACK,
        "adapter hit@3 {:.3} < baseline {:.3}",
        s.adapter_hit3,
        base.adapter_hit3
    );
    assert!(
        s.answered_hit3 >= base.answered_hit3 - SLACK,
        "answered hit@3 {:.3} < baseline {:.3}",
        s.answered_hit3,
        base.answered_hit3
    );
    assert!(
        p95 <= RANK_P95_US,
        "ranking p95 {p95} us > {RANK_P95_US} us"
    );
}

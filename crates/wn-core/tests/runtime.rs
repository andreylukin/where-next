//! Index, adapter and query flow end to end, with a deterministic lexical encoder.

use std::cell::Cell;
use std::collections::HashMap;

use wn_core::adapter::AdapterParams;
use wn_core::encoder::{EncodeError, Encoder, HashEncoder, QueryInput};
use wn_core::index::{EntryKind, Index, IndexedFile};
use wn_core::index_lifecycle::IndexState;
use wn_core::rank::AnswerState;
use wn_core::runtime::{
    fit_from_history, load_adapter, save_adapter, suggest, FitSkipped, HistoryExample,
    SuggestOptions,
};
use wn_sources::Kind;

/// Wraps an encoder, counting documents embedded; optionally fails, optionally "calibrated".
struct Probe {
    inner: HashEncoder,
    docs: Cell<usize>,
    fail: bool,
    calibrated: bool,
    fingerprint: &'static str,
}

impl Probe {
    fn new() -> Self {
        Self {
            inner: HashEncoder::default(),
            docs: Cell::new(0),
            fail: false,
            calibrated: false,
            fingerprint: "probe-a",
        }
    }
}

impl Encoder for Probe {
    fn fingerprint(&self) -> String {
        self.fingerprint.to_string()
    }
    fn calibration(&self) -> Option<wn_core::rank::Calibration> {
        self.calibrated.then(wn_core::rank::Calibration::v2b)
    }
    fn documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EncodeError> {
        if self.fail {
            return Err(EncodeError("boom".into()));
        }
        self.docs.set(self.docs.get() + texts.len());
        self.inner.documents(texts)
    }
    fn queries(&self, items: &[QueryInput]) -> Result<Vec<Vec<f32>>, EncodeError> {
        if self.fail {
            return Err(EncodeError("boom".into()));
        }
        self.inner.queries(items)
    }
}

fn repo() -> HashMap<String, String> {
    HashMap::from([
        ("src/auth.py".to_string(), "\"\"\"Login and token verification.\"\"\"\ndef login(user):\n    pass\ndef verify_token(t):\n    pass\n".to_string()),
        ("src/upload.go".to_string(), "// Package upload retries S3 uploads on timeout.\npackage upload\nfunc Retry() {}\n".to_string()),
        ("src/picker.ts".to_string(), "/** Model picker listbox. */\nexport function renderPicker() {}\n".to_string()),
        ("src/empty.rs".to_string(), "// nothing here but a comment line\n".to_string()),
        ("Dockerfile".to_string(), "FROM rust\nRUN cargo build\n".to_string()),
    ])
}

fn files_of(r: &HashMap<String, String>, cid: &str) -> Vec<IndexedFile> {
    let mut v: Vec<IndexedFile> = r
        .keys()
        .map(|p| IndexedFile {
            path: p.clone(),
            cid: cid.to_string(),
            kind: if p == "Dockerfile" {
                Kind::Config
            } else {
                Kind::Source
            },
        })
        .collect();
    v.sort_by(|a, b| a.path.cmp(&b.path));
    v
}

fn reader(r: &HashMap<String, String>) -> impl Fn(&str, Kind) -> Option<String> + '_ {
    move |p, _| r.get(p).cloned()
}

#[test]
fn builds_persists_and_refreshes_incrementally() {
    let dir = tempfile::tempdir().unwrap();
    let r = repo();
    let enc = Probe::new();
    let mut index = Index::open(dir.path(), "probe-a");
    assert_eq!(index.state(), IndexState::Uninitialized);
    let stats = index
        .refresh(&files_of(&r, "v1"), &reader(&r), &enc, true)
        .unwrap();
    assert_eq!(index.state(), IndexState::Ready);
    assert_eq!(index.count(EntryKind::File), 4);
    assert_eq!(index.count(EntryKind::Config), 1);
    assert_eq!(
        index.count(EntryKind::FunctionNone),
        1,
        "empty.rs has no definitions"
    );
    assert!(index.count(EntryKind::Function) >= 4);
    assert_eq!(stats.encoded, enc.docs.get());

    // Reopening loads the snapshot; an unchanged repository embeds nothing.
    let mut again = Index::open(dir.path(), "probe-a");
    assert_eq!(again.state(), IndexState::Ready);
    let before = enc.docs.get();
    assert_eq!(
        again
            .refresh(&files_of(&r, "v1"), &reader(&r), &enc, true)
            .unwrap()
            .encoded,
        0
    );
    assert_eq!(enc.docs.get(), before);

    // One changed file re-embeds only that file (file doc + its functions); a removed one drops.
    let mut files = files_of(&r, "v1");
    files.retain(|f| f.path != "src/picker.ts");
    files
        .iter_mut()
        .find(|f| f.path == "src/upload.go")
        .unwrap()
        .cid = "v2".into();
    let s = again.refresh(&files, &reader(&r), &enc, true).unwrap();
    assert_eq!(s.encoded, 2, "upload.go file doc + Retry");
    assert!(s.removed >= 4);
    assert_eq!(again.count(EntryKind::File), 3);

    // A different model never reuses these vectors.
    assert_eq!(
        Index::open(dir.path(), "probe-b").state(),
        IndexState::Uninitialized
    );
}

#[test]
fn ranking_and_answers() {
    let dir = tempfile::tempdir().unwrap();
    let r = repo();
    let enc = Probe::new();
    let mut index = Index::open(dir.path(), "probe-a");
    index
        .refresh(&files_of(&r, "v1"), &reader(&r), &enc, true)
        .unwrap();

    let out = suggest(
        &index,
        None,
        &enc,
        "the login token check is broken",
        "",
        SuggestOptions {
            with_functions: true,
            ..Default::default()
        },
    );
    assert_eq!(
        out.state,
        AnswerState::Ok,
        "uncalibrated encoders never abstain"
    );
    assert_eq!(out.hints.files[0].path, "src/auth.py");
    assert!(out.hints.files.len() + out.hints.functions.len() <= 3);
    assert!(!out.adapter.applied);

    // A calibrated encoder with weak similarity abstains.
    let strict = Probe {
        calibrated: true,
        ..Probe::new()
    };
    let out = suggest(
        &index,
        None,
        &strict,
        "zzzz qqqq",
        "",
        SuggestOptions::default(),
    );
    assert_eq!(out.state, AnswerState::Abstain);
    assert!(out.hints.files.is_empty());
    assert!(out.abstain.unwrap().starts_with("request: top similarity"));
}

#[test]
fn failures_fail_open() {
    let dir = tempfile::tempdir().unwrap();
    let r = repo();
    let broken = Probe {
        fail: true,
        ..Probe::new()
    };
    let mut index = Index::open(dir.path(), "probe-a");
    assert!(index
        .refresh(&files_of(&r, "v1"), &reader(&r), &broken, false)
        .is_err());
    assert_eq!(index.state(), IndexState::Error);
    let out = suggest(
        &index,
        None,
        &broken,
        "anything",
        "",
        SuggestOptions::default(),
    );
    assert_eq!(out.state, AnswerState::Error);

    let empty_dir = tempfile::tempdir().unwrap();
    let mut empty = Index::open(empty_dir.path(), "probe-a");
    let enc = Probe::new();
    empty.refresh(&[], &|_, _| None, &enc, false).unwrap();
    assert_eq!(empty.state(), IndexState::Ready);
    let out = suggest(&empty, None, &enc, "q", "", SuggestOptions::default());
    assert_eq!(out.state, AnswerState::EmptyIndex);
    let out = suggest(
        &empty,
        None,
        &enc,
        "q",
        "",
        SuggestOptions {
            unsupported_only: true,
            ..Default::default()
        },
    );
    assert_eq!(out.state, AnswerState::UnsupportedScope);
}

#[test]
fn adapter_fits_from_history_roundtrips_and_applies() {
    let dir = tempfile::tempdir().unwrap();
    let r = repo();
    let enc = Probe::new();
    let mut index = Index::open(dir.path(), "probe-a");
    index
        .refresh(&files_of(&r, "v1"), &reader(&r), &enc, false)
        .unwrap();

    let subjects = [
        ("fix login", "src/auth.py"),
        ("retry uploads", "src/upload.go"),
        ("picker keyboard", "src/picker.ts"),
    ];
    let commits: Vec<HistoryExample> = (0..30)
        .map(|i| {
            let (s, p) = subjects[i % 3];
            HistoryExample {
                sha: format!("{i:040}"),
                date: format!("2026-09-{:02}", 28 - i % 28),
                subject: format!("{s} #{i}"),
                body: String::new(),
                paths: vec!["gone/file.py".into(), p.into()],
            }
        })
        .collect();

    let too_few = fit_from_history(
        &index,
        &commits[..5],
        &[],
        &enc,
        &AdapterParams::default(),
        200,
        0,
    );
    assert_eq!(too_few, Err(FitSkipped::TooFewExamples { found: 5 }));

    let params = AdapterParams {
        steps: 20,
        ..AdapterParams::default()
    };
    let a = fit_from_history(&index, &commits, &[], &enc, &params, 200, 0).unwrap();
    assert_eq!(a.meta.base, "probe-a");
    assert_eq!(a.meta.n_train, 30);
    assert_eq!(
        a.meta.history_cutoff.as_deref(),
        Some(commits[0].sha.as_str())
    );
    assert_eq!(a.w.len(), a.meta.dim * a.meta.dim);

    let adir = dir.path().join("adapter");
    save_adapter(&adir, &a).unwrap();
    let loaded = load_adapter(&adir).unwrap();
    assert_eq!(loaded.meta, a.meta);
    let max_err = loaded
        .w
        .iter()
        .zip(&a.w)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max);
    assert!(max_err < 1e-3, "float16 storage error {max_err}");

    let out = suggest(
        &index,
        Some(&loaded),
        &enc,
        "fix login",
        "",
        SuggestOptions::default(),
    );
    assert!(out.adapter.applied);
    assert_eq!(
        out.adapter.revision.as_deref(),
        Some(a.meta.revision.as_str())
    );

    // Weights fitted for another model are never applied.
    let other = Probe {
        fingerprint: "probe-b",
        ..Probe::new()
    };
    let mut idx_b = Index::open(&dir.path().join("b"), "probe-b");
    idx_b
        .refresh(&files_of(&r, "v1"), &reader(&r), &other, false)
        .unwrap();
    let out = suggest(
        &idx_b,
        Some(&loaded),
        &other,
        "fix login",
        "",
        SuggestOptions::default(),
    );
    assert!(!out.adapter.applied);
    assert!(out.adapter.reason.unwrap().contains("another model"));
}

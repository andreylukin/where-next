//! `wn bench` against repositories built in the test, with known answers.

use std::fs;
use std::path::Path;
use std::process::Command;

use proptest::prelude::*;
use wn_cli::bench::{
    self, BenchEvent, BenchLifecycle, BenchOptions, BenchState, BENCH_TRANSITIONS,
};
use wn_core::encoder::HashEncoder;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write(dir: &Path, path: &str, text: &str) {
    let p = dir.join(path);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn append(dir: &Path, path: &str, line: &str) {
    let p = dir.join(path);
    let mut text = fs::read_to_string(&p).unwrap();
    text.push_str(line);
    text.push('\n');
    fs::write(p, text).unwrap();
}

const TOPICS: [(&str, &str, &str); 6] = [
    (
        "src/auth.py",
        "# Login sessions and token verification.\ndef login():\n    pass\n",
        "login session token",
    ),
    (
        "src/upload.go",
        "// Package upload retries storage uploads.\npackage upload\n\nfunc Retry() {}\n",
        "upload storage retry",
    ),
    (
        "web/picker.ts",
        "/** Model picker listbox. */\nexport function picker() {}\n",
        "picker listbox keyboard",
    ),
    (
        "src/billing.rs",
        "//! Invoice totals and tax rounding.\npub fn invoice() {}\n",
        "invoice tax rounding",
    ),
    (
        "src/search.java",
        "/** Full text search ranking. */\nclass Search {}\n",
        "search ranking query",
    ),
    (
        "src/cache.c",
        "/* LRU cache eviction. */\nvoid evict(void) {}\n",
        "cache eviction lru",
    ),
];

/// 45 commits that each change one topic file (eligible), plus commits that must be skipped:
/// test-only, more than 6 files, docs-only and a brand-new file.
fn project(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    for (path, text, _) in TOPICS {
        write(dir, path, text);
    }
    write(
        dir,
        "pkg/tests/test_auth.py",
        "def test_login():\n    pass\n",
    );
    write(dir, "README.md", "# demo\n");
    for i in 0..7 {
        write(dir, &format!("src/extra{i}.py"), "x = 1\n");
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "initial layout"]);
    for i in 0..45 {
        let (path, _, words) = TOPICS[i % TOPICS.len()];
        append(
            dir,
            path,
            &format!(
                "{} {words} {i}",
                if path.ends_with(".py") { "#" } else { "//" }
            ),
        );
        git(dir, &["commit", "-q", "-am", &format!("fix {words} ({i})")]);
        match i {
            10 => {
                append(dir, "pkg/tests/test_auth.py", "# more");
                git(dir, &["commit", "-q", "-am", "only tests change"]);
            }
            20 => {
                for k in 0..7 {
                    append(dir, &format!("src/extra{k}.py"), "y = 2");
                }
                git(dir, &["commit", "-q", "-am", "mass rename of seven files"]);
            }
            30 => {
                append(dir, "README.md", "more docs");
                git(dir, &["commit", "-q", "-am", "docs only"]);
            }
            40 => {
                write(dir, "src/brand_new.py", "def new():\n    pass\n");
                git(dir, &["add", "-A"]);
                git(dir, &["commit", "-q", "-m", "add a brand new module"]);
            }
            _ => {}
        }
    }
}

#[test]
fn history_replay_finds_the_changed_files_and_skips_ineligible_commits() {
    let t = tempfile::tempdir().unwrap();
    let repo = t.path().join("repo");
    fs::create_dir(&repo).unwrap();
    project(&repo);
    let cache = t.path().join("cache");
    let enc = HashEncoder { dim: 128 };
    let opts = BenchOptions::default();
    let r = bench::history(&repo, &enc, "hash", None, &cache, &opts, &mut |_| {}).unwrap();
    assert_eq!(r.eligible, 45, "only the one-topic commits are eligible");
    // Scanned: every commit with a source change (the docs-only one is filtered earlier).
    assert_eq!(r.scanned, 48);
    assert_eq!(r.evaluated, 45);
    assert_eq!(
        r.adapted, 25,
        "the adapter starts once 20 earlier commits exist"
    );
    assert_eq!(r.files_median, 14);
    let lexical = &r.matched[0];
    let adapter = &r.matched[2];
    assert_eq!(lexical.method, "lexical (BM25)");
    assert_eq!(adapter.method, "model + adapter");
    assert!(lexical.hit1 > 0.9, "{r:?}");
    assert!(adapter.hit3 > 0.9, "{r:?}");
    assert_eq!(r.timing.docs_reused, 0);
    let embedded = r.timing.docs_embedded;
    assert!(embedded > 14);

    // A second run reuses every vector from the bench cache.
    let again = bench::history(&repo, &enc, "hash", None, &cache, &opts, &mut |_| {}).unwrap();
    assert_eq!(again.timing.docs_embedded, 0);
    assert_eq!(again.timing.docs_reused, embedded);
    assert_eq!(again.matched, r.matched);

    // Tests counted as gold make the test-only commit eligible too.
    let with_tests = BenchOptions {
        with_tests: true,
        adapter: false,
        ..BenchOptions::default()
    };
    let wt = bench::history(&repo, &enc, "hash", None, &cache, &with_tests, &mut |_| {}).unwrap();
    assert_eq!(wt.eligible, 46);
    assert!(wt.matched.is_empty() && wt.adapted == 0);

    // A window smaller than the history scores only the newest commits.
    let small = BenchOptions {
        commits: 10,
        train: 20,
        step: 5,
        ..BenchOptions::default()
    };
    let s = bench::history(&repo, &enc, "hash", None, &cache, &small, &mut |_| {}).unwrap();
    assert_eq!((s.eligible, s.evaluated, s.adapted), (30, 10, 10));
}

#[test]
fn history_replay_of_a_repository_without_history_is_an_error() {
    let t = tempfile::tempdir().unwrap();
    git(t.path(), &["init", "-q", "-b", "main"]);
    let enc = HashEncoder { dim: 128 };
    let err = bench::history(
        t.path(),
        &enc,
        "hash",
        None,
        &t.path().join("c"),
        &BenchOptions::default(),
        &mut |_| {},
    )
    .unwrap_err();
    assert!(err.contains("no commits"), "{err}");
}

#[test]
fn blobless_history_fetches_from_configured_promisor_remote() {
    let t = tempfile::tempdir().unwrap();
    let source = t.path().join("source");
    fs::create_dir(&source).unwrap();
    git(&source, &["init", "-q", "-b", "main"]);
    git(&source, &["config", "commit.gpgsign", "false"]);
    git(&source, &["config", "uploadpack.allowFilter", "true"]);
    write(
        &source,
        "src/topic.py",
        "# unique parent blob\ndef topic():\n    pass\n",
    );
    git(&source, &["add", "-A"]);
    git(&source, &["commit", "-q", "-m", "initial topic"]);
    let parent_blob = git(&source, &["rev-parse", "HEAD:src/topic.py"]);
    append(&source, "src/topic.py", "# more topic detail");
    git(&source, &["commit", "-q", "-am", "improve topic detail"]);

    let clone = t.path().join("clone");
    let url = format!("file://{}", source.display());
    git(
        t.path(),
        &[
            "clone",
            "-q",
            "--filter=blob:none",
            &url,
            clone.to_str().unwrap(),
        ],
    );
    git(&clone, &["remote", "rename", "origin", "archive"]);
    assert_eq!(
        git(&clone, &["config", "--get", "remote.archive.promisor"]),
        "true"
    );
    let missing = Command::new("git")
        .args(["cat-file", "-e", &parent_blob])
        .current_dir(&clone)
        .env("GIT_NO_LAZY_FETCH", "1")
        .status()
        .unwrap();
    assert!(!missing.success(), "parent blob should start absent");

    let opts = BenchOptions {
        commits: 1,
        adapter: false,
        ..BenchOptions::default()
    };
    let cache = t.path().join("cache");
    let enc = HashEncoder { dim: 128 };
    let mut messages = Vec::new();
    let first = bench::history(&clone, &enc, "hash", None, &cache, &opts, &mut |s| {
        messages.push(s.to_string())
    })
    .unwrap();
    assert!(
        messages
            .iter()
            .any(|s| s.contains("fetching 1 missing historical blobs")),
        "{messages:?}"
    );
    assert!(
        !messages.iter().any(|s| s.contains("fetch failed")),
        "{messages:?}"
    );
    assert_eq!((first.eligible, first.evaluated), (1, 1));
    assert_eq!(first.all[0].n, 1);
    assert!(git(&clone, &["cat-file", "-e", &parent_blob]).is_empty());

    let again = bench::history(&clone, &enc, "hash", None, &cache, &opts, &mut |_| {}).unwrap();
    assert_eq!(again.all, first.all);
    assert_eq!(again.by_size, first.by_size);
    assert_eq!(again.eras, first.eras);
    assert_eq!(
        (again.eligible, again.evaluated),
        (first.eligible, first.evaluated)
    );
}

fn wn(repo: &Path, home: &Path, args: &[&str]) -> (String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_wn"))
        .arg("--path")
        .arg(repo)
        .args(args)
        .env("WHERE_NEXT_HOME", home)
        .env("WN_MODELS_HOME", home.join("no-models"))
        .env("WN_NO_DAEMON", "1")
        .env_remove("WN_MODEL_DIR")
        .output()
        .unwrap();
    let text = if out.status.success() {
        out.stdout
    } else {
        out.stderr
    };
    (
        String::from_utf8_lossy(&text).trim_end().to_string(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn bench_command_prints_a_report_and_json() {
    let t = tempfile::tempdir().unwrap();
    let repo = t.path().join("repo");
    fs::create_dir(&repo).unwrap();
    project(&repo);
    let home = t.path().join("home");
    let (text, code) = wn(&repo, &home, &["bench"]);
    assert_eq!(code, 0, "{text}");
    insta::with_settings!({filters => vec![(r"time: .*", "time: [time]")]}, {
        insta::assert_snapshot!("bench_history", text);
    });
    let (json, code) = wn(
        &repo,
        &home,
        &[
            "bench",
            "--history",
            "--json",
            "--commits",
            "20",
            "--no-adapter",
        ],
    );
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["kind"], "history");
    assert_eq!(v["evaluated"], 20);
    assert_eq!(v["model"], "lexical fallback (no model installed)");
    let (err, code) = wn(&t.path().join("nowhere"), &home, &["bench"]);
    assert_eq!(code, 2);
    assert!(err.contains("does not exist"), "{err}");
}

#[test]
fn contextbench_runs_prepared_tasks() {
    let t = tempfile::tempdir().unwrap();
    let data = t.path().join("cb");
    let repo = data.join("repos").join("demo");
    fs::create_dir_all(&repo).unwrap();
    project(&repo);
    let base = git(&repo, &["rev-parse", "HEAD"]);
    let tasks = [
        serde_json::json!({
            "instance_id": "demo-1",
            "repo_dir": "repos/demo",
            "base_commit": base,
            "problem_statement": "Invoice totals are off by one cent: tax rounding is wrong",
            "gold_files": ["/workspace/demo__demo__1.0/src/billing.rs"],
        }),
        serde_json::json!({
            "instance_id": "gone-1",
            "repo_dir": "repos/missing",
            "base_commit": base,
            "problem_statement": "x",
            "gold_files": ["a.py"],
        }),
    ];
    let lines: Vec<String> = tasks.iter().map(|v| v.to_string()).collect();
    fs::write(data.join("tasks.jsonl"), lines.join("\n") + "\n").unwrap();
    let enc = HashEncoder { dim: 128 };
    let cache = t.path().join("cache");
    let r = bench::contextbench(
        &data,
        &enc,
        "hash",
        &|_| cache.clone(),
        &BenchOptions::default(),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(
        (r.scanned, r.eligible, r.evaluated, r.adapted),
        (2, 1, 1, 1)
    );
    assert_eq!(r.all[0].hit1, 1.0, "{r:?}");
    assert_eq!(r.matched[2].method, "model + adapter");
    assert_eq!(r.matched[2].hit3, 1.0, "{r:?}");
    assert!(r
        .notes
        .iter()
        .any(|n| n.contains("1 repositories were not found")));
    let err = bench::contextbench(
        &t.path().join("none"),
        &enc,
        "hash",
        &|_| cache.clone(),
        &BenchOptions::default(),
        &mut |_| {},
    )
    .unwrap_err();
    assert!(err.contains("tasks.jsonl"), "{err}");
}

/// The lifecycle, written out independently of the implementation's table.
fn expected(state: BenchState, event: BenchEvent) -> Option<BenchState> {
    use BenchEvent as E;
    use BenchState as S;
    match (state, event) {
        (S::Idle, E::Start) => Some(S::Collecting),
        (S::Collecting, E::Collected) => Some(S::Embedding),
        (S::Embedding, E::Embedded) => Some(S::Scoring),
        (S::Scoring, E::Scored) => Some(S::Done),
        (S::Collecting | S::Embedding | S::Scoring, E::Fail) => Some(S::Failed),
        _ => None,
    }
}

#[test]
fn lifecycle_every_state_and_event() {
    let mut legal = 0;
    for s in BenchState::ALL {
        for e in BenchEvent::ALL {
            let mut life = BenchLifecycle::default();
            // Drive to `s` along the happy path (or fail from Collecting).
            let path: &[BenchEvent] = match s {
                BenchState::Idle => &[],
                BenchState::Collecting => &[BenchEvent::Start],
                BenchState::Embedding => &[BenchEvent::Start, BenchEvent::Collected],
                BenchState::Scoring => &[
                    BenchEvent::Start,
                    BenchEvent::Collected,
                    BenchEvent::Embedded,
                ],
                BenchState::Done => &[
                    BenchEvent::Start,
                    BenchEvent::Collected,
                    BenchEvent::Embedded,
                    BenchEvent::Scored,
                ],
                BenchState::Failed => &[BenchEvent::Start, BenchEvent::Fail],
            };
            for ev in path {
                life.handle(*ev).unwrap();
            }
            assert_eq!(life.state(), s);
            match expected(s, e) {
                Some(next) => {
                    legal += 1;
                    assert_eq!(life.handle(e), Ok(next));
                    assert_eq!(life.state(), next);
                }
                None => {
                    assert!(life.handle(e).is_err(), "{s:?} + {e:?} should be rejected");
                    assert_eq!(
                        life.state(),
                        s,
                        "a rejected event leaves the state unchanged"
                    );
                }
            }
        }
    }
    assert_eq!(legal, BENCH_TRANSITIONS.len());
}

proptest! {
    #[test]
    fn lifecycle_matches_the_reference_model(events in prop::collection::vec(0usize..5, 0..30)) {
        let mut life = BenchLifecycle::default();
        let mut model = BenchState::Idle;
        for i in events {
            let e = BenchEvent::ALL[i];
            let want = expected(model, e);
            let got = life.handle(e).ok();
            prop_assert_eq!(got, want);
            if let Some(next) = want {
                model = next;
            }
            prop_assert_eq!(life.state(), model);
            // Terminal states accept nothing.
            if matches!(model, BenchState::Done | BenchState::Failed) {
                for e2 in BenchEvent::ALL {
                    prop_assert!(expected(model, e2).is_none());
                }
            }
        }
    }
}

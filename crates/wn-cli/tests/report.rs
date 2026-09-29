//! `wn report`: the flow state machine, the exact report format, truncation, and that random
//! repository content never reaches the report.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use proptest::prelude::*;
use proptest_state_machine::{prop_state_machine, ReferenceStateMachine, StateMachineTest};
use wn_cli::report::{
    collect, issue_url, issue_url_within, render_markdown, run_flow, to_json, Arch, KnownModel, Os,
    Precision, ReportEvent, ReportFlow, ReportIo, ReportOptions, ReportState, System, EVENTS,
    MAX_URL, STATES,
};
use wn_daemon::usage::{BenchEvent, IndexEvent, QueryEvent, RepoUsage};

// ---------------------------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------------------------

/// The specification, written independently of `TABLE`.
fn spec(state: ReportState, event: ReportEvent) -> Option<ReportState> {
    use ReportEvent as E;
    use ReportState as S;
    match (state, event) {
        (S::Collecting, E::Collected) => Some(S::Previewing),
        (S::Collecting, E::CollectFailed) => Some(S::Failed),
        (S::Previewing, E::Accept) => Some(S::Confirmed),
        (S::Previewing, E::Decline) => Some(S::Cancelled),
        (S::Confirmed, E::Post) => Some(S::Posting),
        (S::Posting, E::PostOk) => Some(S::Posted),
        (S::Posting, E::PostFailed) => Some(S::Failed),
        _ => None,
    }
}

fn flow_in(state: ReportState) -> ReportFlow {
    // Drive a fresh machine to `state` along a legal path.
    use ReportEvent as E;
    let path: &[ReportEvent] = match state {
        ReportState::Collecting => &[],
        ReportState::Previewing => &[E::Collected],
        ReportState::Confirmed => &[E::Collected, E::Accept],
        ReportState::Cancelled => &[E::Collected, E::Decline],
        ReportState::Posting => &[E::Collected, E::Accept, E::Post],
        ReportState::Posted => &[E::Collected, E::Accept, E::Post, E::PostOk],
        ReportState::Failed => &[E::CollectFailed],
    };
    let mut f = ReportFlow::default();
    for e in path {
        f.handle(*e).unwrap();
    }
    assert_eq!(f.state(), state);
    f
}

#[test]
fn every_state_event_pair_matches_the_spec() {
    let mut legal = 0;
    for s in STATES {
        for e in EVENTS {
            let mut f = flow_in(s);
            match (f.handle(e), spec(s, e)) {
                (Ok(to), Some(want)) => {
                    assert_eq!(to, want, "{s:?} on {e:?}");
                    legal += 1;
                }
                (Err(_), None) => assert_eq!(f.state(), s, "rejected event changed state"),
                (got, want) => panic!("{s:?} on {e:?}: got {got:?}, spec {want:?}"),
            }
        }
    }
    assert_eq!(legal, 7);
}

#[derive(Debug, Clone)]
struct Model;

impl ReferenceStateMachine for Model {
    type State = (ReportState, bool);
    type Transition = ReportEvent;

    fn init_state() -> BoxedStrategy<Self::State> {
        Just((ReportState::Collecting, false)).boxed()
    }

    fn transitions(_: &Self::State) -> BoxedStrategy<Self::Transition> {
        proptest::sample::select(EVENTS.to_vec()).boxed()
    }

    fn apply(state: Self::State, t: &Self::Transition) -> Self::State {
        let accepted = state.1 || (state.0 == ReportState::Previewing && *t == ReportEvent::Accept);
        match spec(state.0, *t) {
            Some(to) => (to, accepted),
            None => state,
        }
    }
}

struct Sut;

impl StateMachineTest for Sut {
    type SystemUnderTest = ReportFlow;
    type Reference = Model;

    fn init_test(_: &(ReportState, bool)) -> ReportFlow {
        ReportFlow::default()
    }

    fn apply(mut sut: ReportFlow, reference: &(ReportState, bool), t: ReportEvent) -> ReportFlow {
        let _ = sut.handle(t);
        assert_eq!(sut.state(), reference.0);
        sut
    }

    fn check_invariants(sut: &ReportFlow, reference: &(ReportState, bool)) {
        // Nothing can be sent, or end up posted, without an explicit Accept from Previewing.
        if sut.may_send() || sut.state() == ReportState::Posted {
            assert!(reference.1, "reached {:?} without Accept", sut.state());
        }
    }
}

prop_state_machine! {
    #![proptest_config(ProptestConfig { cases: 256, .. ProptestConfig::default() })]
    #[test]
    fn flow_matches_reference(sequential 1..30 => Sut);
}

// ---------------------------------------------------------------------------------------------
// Fixed report: exact format
// ---------------------------------------------------------------------------------------------

const DAY: u64 = 86_400;
const NOW: u64 = 1_800_000_000;

fn system() -> System {
    System {
        os: Os::Macos,
        arch: Arch::Aarch64,
        threads: 12,
        ram_gb: Some(36),
        model: KnownModel::GemmaXl1,
        precision: Precision::Fp32,
    }
}

fn q(ts: u64, kind: &str, state: &str, ms: u64, hinted: &[&str]) -> QueryEvent {
    QueryEvent {
        ts,
        kind: kind.into(),
        state: state.into(),
        ms,
        model: "gemma-xl1-30ae960f08a8d9e8".into(),
        adapter: state == "ok",
        files: 1500,
        hinted: hinted.iter().map(|s| s.to_string()).collect(),
    }
}

fn fixed_usage() -> Vec<RepoUsage> {
    let mut ext = BTreeMap::new();
    ext.insert("rs".to_string(), 700);
    ext.insert("py".to_string(), 250);
    ext.insert("ts".to_string(), 50);
    vec![
        RepoUsage {
            root: Some(PathBuf::from("/fixture/one")),
            queries: vec![
                q(NOW - 3 * DAY, "issue", "ok", 18, &["src/a.rs"]),
                q(NOW - 2 * DAY, "error", "ok", 23, &["src/b.rs"]),
                q(NOW - DAY, "request", "abstain", 21, &[]),
                q(NOW - 3600, "issue", "ok", 40, &["src/c.rs"]),
            ],
            index: Some(IndexEvent {
                ts: NOW - 4 * DAY,
                ms: 22_000,
                files: 1500,
                configs: 12,
                extensions: ext,
                history_commits: 200,
                peak_mb: Some(900),
                model: "gemma-xl1-30ae960f08a8d9e8".into(),
            }),
            bench: Some(BenchEvent {
                ts: NOW - 4 * DAY,
                model: "gemma-xl1".into(),
                files_median: 1400,
                lexical: [0.331, 0.539, 0.771],
                model_hits: [0.487, 0.762, 0.931],
                adapter_hits: Some([0.666, 0.852, 0.949]),
            }),
        },
        RepoUsage {
            root: None,
            queries: vec![q(NOW - 5 * DAY, "conversational", "empty_index", 5, &[])],
            index: None,
            bench: None,
        },
    ]
}

fn edited_stub(_: &Path, ts: u64, _: u64) -> HashSet<String> {
    // Pretend the first two answers' files were edited afterwards.
    let mut s = HashSet::new();
    if ts == NOW - 3 * DAY {
        s.insert("src/a.rs".to_string());
    }
    if ts == NOW - 2 * DAY {
        s.insert("src/b.rs".to_string());
    }
    s
}

fn fixed_report() -> wn_cli::report::UsageReport {
    let mut r = collect(&fixed_usage(), &system(), NOW, &edited_stub);
    r.wn.commit = None; // the build commit changes with every build
    r
}

#[test]
fn report_format_snapshot() {
    let r = fixed_report();
    assert_eq!(r.quality.hint_usefulness, Some(0.65)); // 2 of 3 answers, rounded to 0.05
    insta::assert_snapshot!("usage_report_markdown", render_markdown(&r));
}

#[test]
fn json_round_trips_and_rejects_extra_fields() {
    let r = fixed_report();
    let json = to_json(&r);
    let back: wn_cli::report::UsageReport = serde_json::from_str(&json).unwrap();
    assert_eq!(back, r);
    let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
    v["setup"]["hostname"] = serde_json::json!("my-laptop");
    assert!(serde_json::from_value::<wn_cli::report::UsageReport>(v).is_err());
    let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
    v["setup"]["model"] = serde_json::json!("secret-internal-model");
    assert!(serde_json::from_value::<wn_cli::report::UsageReport>(v).is_err());
}

#[test]
fn url_fits_and_truncates_gracefully() {
    let mut r = fixed_report();
    let b = r.quality.bench[0].clone();
    r.quality.bench = vec![b; 10];
    let (url, carried) = issue_url(&r);
    assert!(url.len() <= MAX_URL);
    assert!(!carried.truncated);
    let (small, carried) = issue_url_within(&r, 2_500);
    assert!(carried.truncated, "detail dropped under a tight limit");
    assert!(carried.quality.bench.len() < 10);
    assert!(small.len() <= 2_500 || carried.repos.languages.is_empty());
    assert!(render_markdown(&carried).contains("some detail was dropped"));
}

// ---------------------------------------------------------------------------------------------
// Flow with a fake terminal
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct FakeIo {
    answer: bool,
    fail_post: bool,
    shown: Vec<String>,
    posted: Vec<(String, String, String)>,
}

impl ReportIo for FakeIo {
    fn show(&mut self, text: &str) {
        self.shown.push(text.to_string());
    }
    fn confirm(&mut self, _prompt: &str) -> bool {
        self.answer
    }
    fn post(&mut self, url: &str, title: &str, body: &str) -> Result<String, String> {
        self.posted
            .push((url.to_string(), title.to_string(), body.to_string()));
        if self.fail_post {
            Err("offline".into())
        } else {
            Ok("posted".into())
        }
    }
}

#[test]
fn nothing_is_posted_without_confirmation() {
    let r = fixed_report();
    for opts in [
        ReportOptions::default(),
        ReportOptions {
            json: true,
            dry_run: false,
        },
        ReportOptions {
            json: false,
            dry_run: true,
        },
    ] {
        let mut io = FakeIo::default();
        assert_eq!(run_flow(&r, opts, &mut io), ReportState::Cancelled);
        assert!(io.posted.is_empty());
    }
    let mut io = FakeIo {
        answer: true,
        ..FakeIo::default()
    };
    assert_eq!(
        run_flow(&r, ReportOptions::default(), &mut io),
        ReportState::Posted
    );
    assert_eq!(io.posted.len(), 1);
    // The posted body is exactly the preview the user saw.
    assert_eq!(io.posted[0].2, io.shown[0]);
    let mut io = FakeIo {
        answer: true,
        fail_post: true,
        ..FakeIo::default()
    };
    assert_eq!(
        run_flow(&r, ReportOptions::default(), &mut io),
        ReportState::Failed
    );
}

// ---------------------------------------------------------------------------------------------
// Random repository content never reaches the report
// ---------------------------------------------------------------------------------------------

fn canary() -> impl Strategy<Value = String> {
    "[a-z0-9]{8,14}".prop_map(|s| format!("zqx{s}"))
}

fn usage_strategy() -> impl Strategy<Value = (Vec<RepoUsage>, Vec<String>)> {
    prop::collection::vec(
        (
            canary(),
            prop::collection::vec((canary(), canary(), canary(), 0u64..2000), 0..6),
            prop::collection::vec((canary(), 1usize..500), 0..4),
            canary(),
        ),
        1..4,
    )
    .prop_map(|repos| {
        let mut canaries = Vec::new();
        let usage = repos
            .into_iter()
            .map(|(root, queries, exts, model)| {
                canaries.push(root.clone());
                canaries.push(model.clone());
                let queries = queries
                    .into_iter()
                    .enumerate()
                    .map(|(i, (path, kind, state, ms))| {
                        canaries.extend([path.clone(), kind.clone(), state.clone()]);
                        QueryEvent {
                            ts: NOW - (i as u64 + 1) * 3600,
                            kind,
                            state: if i % 2 == 0 { "ok".into() } else { state },
                            ms,
                            model: model.clone(),
                            adapter: i % 3 == 0,
                            files: 100 * i,
                            hinted: vec![path],
                        }
                    })
                    .collect();
                let extensions = exts
                    .into_iter()
                    .map(|(e, n)| {
                        canaries.push(e.clone());
                        (e, n)
                    })
                    .collect();
                RepoUsage {
                    root: Some(PathBuf::from(format!("/home/{root}/{root}"))),
                    queries,
                    index: Some(IndexEvent {
                        ts: NOW - DAY,
                        ms: 1000,
                        files: 50,
                        configs: 1,
                        extensions,
                        history_commits: 10,
                        peak_mb: None,
                        model: model.clone(),
                    }),
                    bench: Some(BenchEvent {
                        ts: NOW - DAY,
                        model: model.clone(),
                        files_median: 40,
                        lexical: [0.1, 0.2, 0.3],
                        model_hits: [0.2, 0.3, 0.4],
                        adapter_hits: None,
                    }),
                }
            })
            .collect();
        (usage, canaries)
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, .. ProptestConfig::default() })]
    #[test]
    fn random_repo_content_never_appears((usage, canaries) in usage_strategy()) {
        let edited = |_: &Path, _: u64, _: u64| HashSet::new();
        let r = collect(&usage, &system(), NOW, &edited);
        let (url, carried) = issue_url(&r);
        let outputs = [to_json(&r), render_markdown(&carried), url];
        for c in &canaries {
            for o in &outputs {
                prop_assert!(!o.contains(c.as_str()), "canary {c} leaked");
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Maintainer side
// ---------------------------------------------------------------------------------------------

#[test]
fn validate_accepts_real_reports_and_rejects_tampering() {
    use wn_cli::report::{extract_json, validate};
    let r = fixed_report();
    let body = render_markdown(&r);
    let json = extract_json(&body).expect("issue body carries a JSON block");
    assert_eq!(validate(json).unwrap(), r);

    let tamper = |f: &dyn Fn(&mut serde_json::Value)| {
        let mut v: serde_json::Value = serde_json::from_str(json).unwrap();
        f(&mut v);
        validate(&v.to_string())
    };
    assert!(tamper(&|v| v["repo_name"] = "acme/secret".into()).is_err());
    assert!(tamper(&|v| v["setup"]["os"] = "Andrey's MacBook".into()).is_err());
    assert!(tamper(&|v| v["quality"]["abstain_rate"] = 3.5.into()).is_err());
    assert!(tamper(&|v| v["repos"]["languages"]["python"] = 33.into()).is_err());
    assert!(tamper(&|v| v["schema"] = 99.into()).is_err());
    assert!(tamper(&|v| v["wn"]["version"] = "0.1 /home/me".into()).is_err());
}

#[test]
fn summary_snapshot() {
    use wn_cli::report::summarize;
    let a = fixed_report();
    let mut b = fixed_report();
    b.setup.os = Os::Linux;
    b.setup.arch = Arch::X86_64;
    b.performance.query_ms_p50 = Some(30);
    insta::assert_snapshot!("usage_report_summary", summarize(&[a, b]));
}

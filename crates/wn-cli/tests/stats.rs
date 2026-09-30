//! `wn stats`: rendering (snapshots), aggregation invariants (properties), the redaction of the
//! share card (a canary), and the whole chain through the binary with synthetic transcripts.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use proptest::prelude::*;
use wn_cli::agents::{self, Action, Agent, Followed, Observation, Similarity, Source};
use wn_cli::report::Language;
use wn_cli::stats::{self, AgentSummary, Ratio, Replay, ShareCard, Stats, Style, Summary};
use wn_daemon::usage::{BenchEvent, IndexEvent, QueryEvent, RepoUsage};

const DAY: u64 = 86_400;
const NOW: u64 = 1_790_000_000;

fn no_edits(_: &Path, _: u64, _: u64) -> HashSet<String> {
    HashSet::new()
}

fn summary(name: &str) -> Summary {
    let mut per_day = vec![0; 30];
    for (i, n) in [(20, 2), (25, 5), (27, 1), (29, 9)] {
        per_day[i] = n;
    }
    Summary {
        name: name.into(),
        root: Some(format!("/home/dev/{name}")),
        queries: 17,
        abstained: 2,
        kinds: BTreeMap::from([("request".into(), 12), ("error".into(), 5)]),
        per_day,
        p50_ms: Some(14),
        p95_ms: Some(41),
        files: Some(294),
        languages: BTreeMap::from([(Language::Python, 250), (Language::Shell, 44)]),
        model: Some("gemma-xl1".into()),
        adapter: true,
        adapter_commits: Some(200),
        agents: AgentSummary {
            scanned: true,
            calls: 12,
            answered: 10,
            exact: Ratio::new(6, 10),
            near: Ratio::new(2, 10),
            elsewhere: Ratio::new(1, 10),
            no_files: Ratio::new(1, 10),
            exact_read: 3,
            exact_ran: 1,
            exact_edited: 2,
            first_pick: Ratio::new(4, 6),
            sessions: BTreeMap::from([(Agent::ClaudeCode, 3), (Agent::Codex, 1)]),
        },
        you_edited: Ratio::new(7, 15),
        replay: Some(Replay {
            tasks: 300,
            grep_style: 0.514,
            model: 0.743,
            adapter: Some(0.865),
            age_days: 2,
        }),
    }
}

fn repo_stats() -> Stats {
    Stats {
        days: 30,
        scope: "repo".into(),
        summary: summary("harbor-demo"),
        repos: Vec::new(),
        unmatched_calls: 0,
    }
}

const PLAIN: Style = Style { color: false };

#[test]
fn text_for_one_repository() {
    insta::assert_snapshot!("stats_repo", stats::render(&repo_stats(), PLAIN));
}

#[test]
fn text_with_little_data_and_no_agents() {
    let mut s = repo_stats();
    s.summary.agents.answered = 3;
    s.summary.agents.exact = Ratio::new(2, 3);
    s.summary.agents.near = Ratio::new(1, 3);
    s.summary.agents.elsewhere = Ratio::new(0, 3);
    s.summary.agents.no_files = Ratio::new(0, 3);
    s.summary.agents.exact_read = 2;
    s.summary.agents.exact_ran = 0;
    s.summary.agents.exact_edited = 0;
    s.summary.agents.first_pick = Ratio::new(1, 2);
    s.summary.replay = None;
    let few = stats::render(&s, PLAIN);
    assert!(
        !few.contains('%') || few.contains("grep"),
        "no percentages below 5 answers:\n{few}"
    );
    insta::assert_snapshot!("stats_few", few);

    s.summary.agents = AgentSummary {
        scanned: true,
        ..AgentSummary::default()
    };
    s.summary.you_edited = Ratio::default();
    s.unmatched_calls = 2;
    insta::assert_snapshot!("stats_no_agent_calls", stats::render(&s, PLAIN));

    s.summary.agents.scanned = false;
    assert!(stats::render(&s, PLAIN).contains("not checked (--no-agents)"));
}

#[test]
fn text_for_all_repositories() {
    let mut other = summary("tiny-cli");
    other.queries = 3;
    other.agents = AgentSummary {
        scanned: true,
        ..AgentSummary::default()
    };
    other.replay = None;
    other.p50_ms = Some(9);
    let mut total = summary("all repositories (2)");
    total.replay = None;
    total.adapter_commits = None;
    let s = Stats {
        days: 7,
        scope: "all".into(),
        summary: total,
        repos: vec![summary("harbor-demo"), other],
        unmatched_calls: 0,
    };
    insta::assert_snapshot!("stats_all", stats::render(&s, PLAIN));
}

#[test]
fn share_card_text_and_svg() {
    let card = ShareCard::from_stats(&repo_stats());
    assert_eq!(card.describe(), "a ~300-file Python repo");
    insta::assert_snapshot!("share_text", stats::render_share(&card));
    let svg = stats::render_svg(&card);
    assert!(svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""));
    assert!(!svg.contains("href"), "no external resources");
    insta::assert_snapshot!("share_svg", svg);
}

#[test]
fn share_svg_without_agent_data_leads_with_the_replay() {
    let mut s = repo_stats();
    s.summary.agents = AgentSummary {
        scanned: true,
        ..AgentSummary::default()
    };
    let svg = stats::render_svg(&ShareCard::from_stats(&s));
    assert!(svg.contains(">87%<"), "{svg}");
    assert!(svg.contains("right file in the top 3 on past commits"));
}

// ---------------------------------------------------------------------------------------------
// Redaction
// ---------------------------------------------------------------------------------------------

const CANARY: &str = "zqcanary";

fn canary_usage() -> Vec<RepoUsage> {
    let root = PathBuf::from(format!("/home/{CANARY}user/{CANARY}-repo"));
    let hinted = vec![format!("src/{CANARY}_auth.py"), format!("{CANARY}/b.py")];
    vec![RepoUsage {
        root: Some(root),
        queries: (0..8)
            .map(|i| QueryEvent {
                ts: NOW - i * 3600,
                kind: "request".into(),
                state: "ok".into(),
                ms: 20 + i,
                model: format!("gemma-xl1-30ae960f08a8d9e8-model-{CANARY}"),
                adapter: true,
                files: 1234,
                hinted: hinted.clone(),
            })
            .collect(),
        index: Some(IndexEvent {
            ts: NOW - DAY,
            ms: 5000,
            files: 1234,
            configs: 3,
            extensions: BTreeMap::from([("py".into(), 1200), (format!("{CANARY}x"), 34)]),
            history_commits: 200,
            peak_mb: Some(700),
            model: format!("gemma-xl1-{CANARY}"),
        }),
        bench: Some(BenchEvent {
            ts: NOW - DAY,
            model: format!("gemma-xl1-{CANARY}"),
            files_median: 1200,
            tasks: 300,
            lexical: [0.3, 0.5, 0.7],
            model_hits: [0.5, 0.7, 0.9],
            adapter_hits: Some([0.6, 0.8, 0.95]),
        }),
    }]
}

#[test]
fn share_outputs_carry_no_repository_information() {
    let usage = canary_usage();
    let root = usage[0].root.clone().unwrap();
    let observations: Vec<Observation> = (0..8)
        .map(|i| Observation {
            agent: Agent::ClaudeCode,
            session: format!("{CANARY}-session-{i}"),
            ts: NOW - i * 3600,
            source: Source::Cli,
            calls: vec![Followed {
                reads: vec![root
                    .join(format!("src/{CANARY}_auth.py"))
                    .to_string_lossy()
                    .into_owned()],
                ..Followed::default()
            }],
        })
        .collect();
    let score = agents::score(&observations, &usage, &|_| true);
    let s = stats::build(&usage, Some(&root), Some(&score), NOW, 30, &no_edits).unwrap();
    assert_eq!(s.summary.agents.exact, Ratio::new(8, 8));
    // The un-redacted view does name the repository (it is local).
    assert!(stats::render(&s, PLAIN).contains(CANARY));
    let card = ShareCard::from_stats(&s);
    let outputs = [
        stats::render_share(&card),
        stats::render_svg(&card),
        serde_json::to_string(&card).unwrap(),
    ];
    for out in outputs {
        assert!(!out.contains(CANARY), "leaked into:\n{out}");
        assert!(!out.contains("/home"), "path leaked into:\n{out}");
    }
}

// ---------------------------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------------------------

fn arb_followed() -> impl Strategy<Value = Followed> {
    let file = prop::sample::select(vec![
        "src/a.rs",
        "src/b.rs",
        "src/c.rs",
        "tests/test_a.py",
        "a.py",
        "docs/x.md",
        "/other/z.rs",
    ]);
    (
        prop::collection::vec(file.clone(), 0..2),
        prop::collection::vec(file.clone(), 0..2),
        prop::collection::vec(
            (
                prop::sample::select(vec!["cat", "pytest", "sed -i s/a/b/", "ls"]),
                file,
            ),
            0..2,
        ),
    )
        .prop_map(|(reads, edits, shell)| Followed {
            reads: reads
                .into_iter()
                .map(|p| format!("/r/{}", p.trim_start_matches('/')))
                .collect(),
            edits: edits.into_iter().map(|p| format!("/r/{p}")).collect(),
            shell: shell.into_iter().map(|(c, p)| format!("{c} {p}")).collect(),
        })
}

fn arb_case() -> impl Strategy<Value = (Vec<Observation>, Vec<RepoUsage>)> {
    let hints = prop::collection::vec(
        prop::sample::select(vec!["src/a.rs", "src/b.rs", "a.py", "lib/q.rs"]),
        0..3,
    );
    let event = (0u64..2_000, prop::bool::ANY, hints).prop_map(|(ts, ok, h)| QueryEvent {
        ts: NOW - ts * 60,
        kind: "request".into(),
        state: if ok { "ok" } else { "abstain" }.into(),
        ms: 10 + ts,
        model: "m".into(),
        adapter: ok,
        files: 50,
        hinted: h.into_iter().map(str::to_string).collect(),
    });
    let obs = (0u64..2_000, prop::collection::vec(arb_followed(), 0..6)).prop_map(|(ts, calls)| {
        Observation {
            agent: if ts % 2 == 0 {
                Agent::ClaudeCode
            } else {
                Agent::Codex
            },
            session: format!("s{}", ts % 5),
            ts: NOW - ts * 60,
            source: Source::Cli,
            calls,
        }
    });
    (
        prop::collection::vec(obs, 0..20),
        prop::collection::vec(event, 0..20),
    )
        .prop_map(|(o, events)| {
            let usage = vec![RepoUsage {
                root: Some(PathBuf::from("/r")),
                queries: events,
                index: None,
                bench: None,
            }];
            (o, usage)
        })
}

proptest! {
    #[test]
    fn similarities_partition_the_answers((obs, usage) in arb_case()) {
        let score = agents::score(&obs, &usage, &|p: &Path| !p.ends_with("docs/x.md"));
        let c = score.per_root.get(Path::new("/r")).cloned().unwrap_or_default();
        let parts: usize = [Similarity::Exact, Similarity::Near, Similarity::Elsewhere, Similarity::None]
            .iter()
            .map(|s| c.count(*s))
            .sum();
        prop_assert_eq!(parts, c.answered);
        let by_action: usize = [Action::Read, Action::Ran, Action::Edited].iter().map(|a| c.exact_action(*a)).sum();
        prop_assert_eq!(by_action, c.count(Similarity::Exact));
        prop_assert!(c.first_pick <= c.count(Similarity::Exact));
        prop_assert!(c.answered <= c.calls);
        prop_assert_eq!(c.calls + score.unmatched, obs.len());
        prop_assert!(c.sessions.len() <= c.calls);

        let s = stats::build(&usage, Some(Path::new("/r")), Some(&score), NOW, 30, &no_edits).unwrap();
        let a = &s.summary.agents;
        prop_assert_eq!(a.exact.n + a.near.n + a.elsewhere.n + a.no_files.n, a.answered);
        for r in [a.exact, a.near, a.elsewhere, a.no_files, a.first_pick, s.summary.you_edited] {
            prop_assert!(r.n <= r.of);
            if let Some(x) = r.rate() {
                prop_assert!((0.0..=1.0).contains(&x));
                prop_assert!(r.of >= stats::MIN_FOR_PERCENT);
            }
        }
        prop_assert_eq!(s.summary.per_day.iter().sum::<usize>(), s.summary.queries);
        prop_assert_eq!(s.summary.queries, usage[0].queries.len());
        prop_assert!(s.summary.abstained <= s.summary.queries);
        prop_assert!(s.summary.p50_ms <= s.summary.p95_ms);
        // Rendering never panics and the card never names the root.
        let text = stats::render(&s, PLAIN);
        prop_assert!(text.starts_with("wn stats · r · last 30 days"));
        let card = ShareCard::from_stats(&s);
        prop_assert!(!stats::render_svg(&card).contains("/r"));
    }

    #[test]
    fn days_window_only_counts_recent_queries(days in 1u64..40) {
        let usage = canary_usage();
        let root = usage[0].root.clone().unwrap();
        let s = stats::build(&usage, Some(&root), None, NOW, days, &no_edits).unwrap();
        prop_assert_eq!(s.days, days.clamp(1, 30));
        prop_assert_eq!(s.summary.per_day.len() as u64, s.days);
        prop_assert!(!s.summary.agents.scanned);
    }
}

// ---------------------------------------------------------------------------------------------
// The binary, end to end
// ---------------------------------------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) {
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
}

fn write(dir: &Path, path: &str, text: &str) {
    let p = dir.join(path);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

struct Env {
    home: PathBuf,
    claude: PathBuf,
    codex: PathBuf,
}

fn wn(repo: &Path, env: &Env, args: &[&str]) -> (String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_wn"))
        .args(args)
        .current_dir(repo)
        .env("WHERE_NEXT_HOME", &env.home)
        .env("WN_MODELS_HOME", env.home.join("no-models"))
        .env("CLAUDE_CONFIG_DIR", &env.claude)
        .env("CODEX_HOME", &env.codex)
        .env_remove("WN_MODEL_DIR")
        .env_remove("WN_NO_LOG")
        .env_remove("WN_STATS_NO_AGENTS")
        .env("WN_NO_DAEMON", "1")
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// `1970-01-01T00:00:00Z`-style timestamp for Unix seconds.
fn rfc3339(ts: u64) -> String {
    let days = (ts / DAY) as i64;
    let secs = ts % DAY;
    // Civil from days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.000Z",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

#[test]
fn rfc3339_round_trips() {
    for ts in [0, 951_868_800, NOW, NOW + 12_345] {
        assert_eq!(agents::parse_ts(&rfc3339(ts)), Some(ts));
    }
}

#[test]
fn stats_follow_agent_transcripts_end_to_end() {
    let repo = tempfile::tempdir().unwrap();
    let d = repo.path();
    git(d, &["init", "-q", "-b", "main"]);
    git(d, &["config", "commit.gpgsign", "false"]);
    write(
        d,
        "src/auth.py",
        "\"\"\"Login sessions and token verification.\"\"\"\ndef login(user):\n    pass\n",
    );
    write(
        d,
        "src/upload.go",
        "// Package upload retries storage uploads.\npackage upload\n",
    );
    write(d, "tests/test_auth.py", "def test_login():\n    pass\n");
    git(d, &["add", "-A"]);
    git(d, &["commit", "-q", "-m", "initial"]);
    let tmp = tempfile::tempdir().unwrap();
    let env = Env {
        home: tmp.path().join("home"),
        claude: tmp.path().join("claude"),
        codex: tmp.path().join("codex"),
    };

    // Nothing logged yet.
    let (out, code) = wn(d, &env, &["stats"]);
    assert_eq!(code, 0);
    assert!(out.contains("nothing logged"), "{out}");

    wn(d, &env, &["init"]);
    for q in [
        "where is login session handling",
        "where are uploads retried",
    ] {
        let (out, code) = wn(d, &env, &["--json", "ask", "--no-abstain", q]);
        assert_eq!(code, 0, "{out}");
    }

    // The usage log holds the answers; build transcripts that call wn at those times.
    let log = fs::read_dir(&env.home)
        .unwrap()
        .flatten()
        .map(|e| e.path().join("usage.jsonl"))
        .find(|p| p.exists())
        .expect("usage log");
    let events: Vec<QueryEvent> = fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(events.len(), 2);
    let root = d.canonicalize().unwrap();
    let first_hint = root.join(&events[0].hinted[0]);

    let claude = [
        r#"{"type":"user","message":{"content":"fix the login bug"}}"#.to_string(),
        format!(
            r#"{{"type":"assistant","timestamp":"{}","message":{{"content":[{{"type":"tool_use","name":"Bash","input":{{"command":"wn ask \"where is login session handling\""}}}}]}}}}"#,
            rfc3339(events[0].ts)
        ),
        format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"Read","input":{{"file_path":"{}"}}}}]}}}}"#,
            first_hint.display()
        ),
        r#"{"type":"user","message":{"content":"thanks"}}"#.to_string(),
    ];
    write(
        &env.claude,
        "projects/p/session-1.jsonl",
        &claude.join("\n"),
    );
    let codex = [
        format!(
            r#"{{"timestamp":"{}","type":"response_item","payload":{{"type":"function_call","name":"shell","arguments":"{{\"command\":[\"bash\",\"-lc\",\"wn ask \\\"where are uploads retried\\\"\"]}}"}}}}"#,
            rfc3339(events[1].ts)
        ),
        r#"{"type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":[\"bash\",\"-lc\",\"ls\"]}"}}"#.to_string(),
    ];
    write(
        &env.codex,
        "sessions/2026/09/29/rollout-x.jsonl",
        &codex.join("\n"),
    );

    let (json, code) = wn(d, &env, &["--json", "stats"]);
    assert_eq!(code, 0, "{json}");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let a = &v["summary"]["agents"];
    assert_eq!(a["calls"], 2, "{json}");
    assert_eq!(a["answered"], 2);
    assert_eq!(a["exact"]["n"], 1);
    assert_eq!(a["exact_read"], 1);
    assert_eq!(a["no_files"]["n"], 1);
    assert_eq!(a["sessions"]["claude-code"], 1);
    assert_eq!(a["sessions"]["codex"], 1);
    assert_eq!(v["summary"]["queries"], 2);

    let (text, _) = wn(d, &env, &["stats"]);
    assert!(
        text.contains("Agents used the hint  exact 1 of 2"),
        "{text}"
    );
    assert!(
        text.contains("from 1 Claude Code / 1 Codex sessions"),
        "{text}"
    );
    assert!(text.contains("not run yet → `wn bench`"), "{text}");

    let (text, _) = wn(d, &env, &["stats", "--no-agents"]);
    assert!(text.contains("not checked (--no-agents)"), "{text}");

    let svg = tmp.path().join("card.svg");
    let (text, code) = wn(d, &env, &["stats", "--svg", svg.to_str().unwrap()]);
    assert_eq!(code, 0);
    let name = root.file_name().unwrap().to_string_lossy().into_owned();
    let card = fs::read_to_string(&svg).unwrap();
    for out in [&card, text.lines().next().unwrap_or("")] {
        assert!(!out.contains(&name), "repository name leaked: {out}");
        assert!(!out.contains("auth.py") && !out.contains("upload.go"));
    }
    assert!(text.contains("wrote "), "{text}");

    let (all, _) = wn(d, &env, &["stats", "--all"]);
    assert!(all.contains("all repositories (1)"), "{all}");
    assert!(all.contains(&name), "{all}");
}

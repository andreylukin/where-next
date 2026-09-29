//! End to end with the `wn` binary: a repository full of unique canary strings (directory and
//! file names, commit messages, author, remote URL, queries) is indexed, queried and benched;
//! `wn report --json` and `wn report --dry-run` must contain none of them.

use std::fs;
use std::path::Path;
use std::process::Command;

const CANARIES: [&str; 8] = [
    "canarydirzq1",
    "canaryfilezq2",
    "canarymsgzq3",
    "canaryorgzq4",
    "canaryuserzq5",
    "canaryqueryzq6",
    "canarysymbolzq7",
    "canaryrepozq8",
];

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "canaryuserzq5")
        .env("GIT_AUTHOR_EMAIL", "canaryuserzq5@canaryorgzq4.example")
        .env("GIT_COMMITTER_NAME", "canaryuserzq5")
        .env("GIT_COMMITTER_EMAIL", "canaryuserzq5@canaryorgzq4.example")
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

fn wn(repo: &Path, home: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_wn"))
        .arg("--path")
        .arg(repo)
        .args(args)
        .env("WHERE_NEXT_HOME", home)
        .env("WN_MODELS_HOME", home.join("no-models"))
        .env_remove("WN_MODEL_DIR")
        .env_remove("WN_NO_LOG")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn report_contains_no_repository_information() {
    let base = tempfile::tempdir().unwrap();
    let repo = base.path().join("canaryrepozq8").join("canarydirzq1");
    fs::create_dir_all(&repo).unwrap();
    let d = repo.as_path();
    git(d, &["init", "-q", "-b", "main"]);
    git(d, &["config", "commit.gpgsign", "false"]);
    git(
        d,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/canaryorgzq4/canaryrepozq8.git",
        ],
    );
    let files = [
        "src/canaryfilezq2.py",
        "src/canarydirzq1/handler.rs",
        "lib/upload.go",
    ];
    for f in files {
        write(d, f, "def canarysymbolzq7():\n    return 'canarymsgzq3'\n");
    }
    git(d, &["add", "-A"]);
    git(d, &["commit", "-q", "-m", "canarymsgzq3 initial layout"]);
    for i in 0..30 {
        let f = files[i % files.len()];
        let mut text = fs::read_to_string(d.join(f)).unwrap();
        text.push_str(&format!("# canarysymbolzq7 change {i}\n"));
        fs::write(d.join(f), text).unwrap();
        git(
            d,
            &[
                "commit",
                "-q",
                "-am",
                &format!("canarymsgzq3 fix canaryfilezq2 {i}"),
            ],
        );
    }

    let home = base.path().join("home");
    let (_, err, code) = wn(d, &home, &["init"]);
    assert_eq!(code, 0, "{err}");
    for query in [
        "where is canaryqueryzq6 handled in canaryfilezq2",
        "canarysymbolzq7 fails with canarymsgzq3",
    ] {
        let (_, err, code) = wn(d, &home, &["ask", "--no-abstain", query]);
        assert_eq!(code, 0, "{err}");
    }
    // A logged-off query leaves no trace.
    let (_, _, code) = wn(d, &home, &["ask", "--no-log", "canaryqueryzq6 again"]);
    assert_eq!(code, 0);
    let _ = wn(
        d,
        &home,
        &["bench", "--commits", "20", "--train", "10", "--step", "10"],
    );

    let log = fs::read_to_string(
        fs::read_dir(&home)
            .unwrap()
            .flatten()
            .map(|e| e.path().join("usage.jsonl"))
            .find(|p| p.exists())
            .expect("usage log written"),
    )
    .unwrap();
    assert_eq!(
        log.lines().count(),
        2,
        "--no-log query must not be recorded"
    );

    let (json, err, code) = wn(d, &home, &["report", "--json"]);
    assert_eq!(code, 0, "{err}");
    let (preview, err, code) = wn(d, &home, &["report", "--dry-run"]);
    assert_eq!(code, 0, "{err}");
    // Non-interactive runs never post.
    let (plain, _, code) = wn(d, &home, &["report"]);
    assert_eq!(code, 0);
    assert!(plain.contains("not posting"));

    let report: serde_json::Value = serde_json::from_str(&json).expect("valid JSON report");
    assert_eq!(report["schema"], 1);
    assert_eq!(report["setup"]["model"], "lexical");
    assert!(preview.contains("https://github.com/andreylukin/where-next/issues/new"));

    let base_str = base.path().to_string_lossy().into_owned();
    for out in [&json, &preview, &plain] {
        for c in CANARIES {
            assert!(!out.contains(c), "canary {c} leaked into:\n{out}");
        }
        assert!(!out.contains(&base_str), "temp path leaked");
        assert!(
            !out.contains("/Users/") && !out.contains("/home/runner"),
            "a home path leaked"
        );
    }
}

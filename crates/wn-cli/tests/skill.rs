//! `wn skill sync` end to end with a temporary HOME: installs for detected agents, is idempotent,
//! shows a diff for outdated copies, never overwrites files it did not write, needs `--yes`
//! without a terminal, re-syncs from recorded state, uninstalls, and merges the Claude Code hook
//! into settings without touching other content.

use std::fs;
use std::path::Path;
use std::process::Command;

struct Env {
    home: tempfile::TempDir,
    wn_home: tempfile::TempDir,
}

impl Env {
    fn new(agents: &[&str]) -> Self {
        let home = tempfile::tempdir().unwrap();
        for a in agents {
            fs::create_dir_all(home.path().join(a)).unwrap();
        }
        Self {
            home,
            wn_home: tempfile::tempdir().unwrap(),
        }
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn wn(&self, args: &[&str]) -> (String, i32) {
        let out = Command::new(env!("CARGO_BIN_EXE_wn"))
            .args(args)
            .current_dir(self.home())
            .env("HOME", self.home())
            .env("WHERE_NEXT_HOME", self.wn_home.path())
            .env("WN_NO_DAEMON", "1")
            .output()
            .unwrap();
        let mut text = String::from_utf8_lossy(&out.stdout).to_string();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        (text, out.status.code().unwrap_or(-1))
    }

    fn skill(&self, agent_dir: &str) -> std::path::PathBuf {
        self.home()
            .join(agent_dir)
            .join("skills/where-next/SKILL.md")
    }
}

#[test]
fn the_packaged_skill_matches_the_repository_copy() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/where-next/SKILL.md");
    assert_eq!(fs::read_to_string(root).unwrap(), wn_cli::skill::SKILL);
}

#[test]
fn rendered_skill_keeps_frontmatter_first_and_carries_the_marker() {
    let text = wn_cli::skill::rendered();
    assert!(text.starts_with("---\nname: where-next\n"), "{text}");
    let after_front = text.splitn(3, "---\n").nth(2).unwrap();
    assert!(
        after_front.starts_with(wn_cli::skill::MARKER),
        "{after_front}"
    );
}

#[test]
fn installs_for_detected_agents_and_is_idempotent() {
    let env = Env::new(&[".claude", ".codex"]);
    let (out, code) = env.wn(&["skill", "sync", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(env.skill(".claude").exists());
    assert!(
        env.skill(".agents").exists(),
        "codex reads ~/.agents/skills"
    );
    assert!(!env.skill(".cursor").exists(), "cursor not detected");

    let (out, code) = env.wn(&["skill", "sync", "--yes"]);
    assert_eq!(code, 0);
    assert!(out.contains("nothing to change"), "{out}");
}

#[test]
fn dry_run_and_missing_yes_write_nothing() {
    let env = Env::new(&[".claude"]);
    let (out, code) = env.wn(&["skill", "sync", "--dry-run"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("create") && out.contains("dry run"), "{out}");
    assert!(!env.skill(".claude").exists());

    let (out, code) = env.wn(&["skill", "sync"]);
    assert_eq!(code, 1, "no terminal and no --yes must not write: {out}");
    assert!(out.contains("--yes"), "{out}");
    assert!(!env.skill(".claude").exists());
}

#[test]
fn outdated_copies_show_a_diff_and_user_files_are_left_alone() {
    let env = Env::new(&[".claude", ".cursor"]);
    env.wn(&["skill", "sync", "--yes", "--agent", "all"]);
    // An older managed copy gets a diff; a hand-written file is never overwritten.
    let managed = env.skill(".claude");
    let old = fs::read_to_string(&managed)
        .unwrap()
        .replace("## How", "## How (old)");
    fs::write(&managed, &old).unwrap();
    let mine = env.skill(".cursor");
    fs::write(
        &mine,
        "---\nname: where-next\ndescription: my own\n---\nmine\n",
    )
    .unwrap();

    let (out, _) = env.wn(&["skill", "sync", "--dry-run", "--agent", "all"]);
    assert!(
        out.contains("-## How (old)") && out.contains("+## How"),
        "{out}"
    );
    assert!(out.contains("not written by wn"), "{out}");

    let (out, code) = env.wn(&["skill", "sync", "--yes", "--agent", "all"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        fs::read_to_string(&managed).unwrap(),
        wn_cli::skill::rendered()
    );
    assert!(fs::read_to_string(&mine).unwrap().contains("mine"));

    let (out, code) = env.wn(&["skill", "sync", "--uninstall", "--yes", "--agent", "all"]);
    assert_eq!(code, 0, "{out}");
    assert!(!managed.exists());
    assert!(mine.exists(), "uninstall only removes our files");
}

#[test]
fn from_state_resyncs_exactly_the_recorded_targets() {
    let env = Env::new(&[".claude", ".cursor"]);
    env.wn(&["skill", "sync", "--yes", "--agent", "cursor"]);
    let path = env.skill(".cursor");
    fs::write(&path, fs::read_to_string(&path).unwrap() + "\nstale\n").unwrap();

    let (out, code) = env.wn(&["skill", "sync", "--yes", "--from-state"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        wn_cli::skill::rendered()
    );
    assert!(
        !env.skill(".claude").exists(),
        "only recorded targets: {out}"
    );
}

#[test]
fn project_scope_installs_into_the_repository() {
    let env = Env::new(&[".claude"]);
    let repo = tempfile::tempdir().unwrap();
    let r = repo.path().to_str().unwrap();
    let (out, code) = env.wn(&[
        "--path",
        r,
        "skill",
        "sync",
        "--yes",
        "--project",
        "--agent",
        "codex",
    ]);
    assert_eq!(code, 0, "{out}");
    assert!(repo
        .path()
        .join(".agents/skills/where-next/SKILL.md")
        .exists());
    assert!(!env.skill(".agents").exists());
}

#[test]
fn the_claude_hook_merges_into_settings_once_and_uninstalls_cleanly() {
    let env = Env::new(&[".claude"]);
    let settings = env.home().join(".claude/settings.json");
    let original = serde_json::json!({
        "model": "opus",
        "hooks": { "UserPromptSubmit": [ { "hooks": [ { "type": "command", "command": "echo hi" } ] } ] }
    });
    fs::write(&settings, serde_json::to_string_pretty(&original).unwrap()).unwrap();

    for _ in 0..2 {
        let (out, code) = env.wn(&["skill", "sync", "--yes", "--with-hook"]);
        assert_eq!(code, 0, "{out}");
    }
    let v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(v["model"], "opus");
    let entries = v["hooks"]["UserPromptSubmit"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "ours added once next to the user's: {v}");
    assert_eq!(entries[0]["hooks"][0]["command"], "echo hi");
    assert_eq!(
        entries[1]["hooks"][0]["command"],
        wn_cli::skill::HOOK_COMMAND
    );

    let (out, code) = env.wn(&["skill", "sync", "--uninstall", "--yes"]);
    assert_eq!(code, 0, "{out}");
    let v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(v, original, "uninstall restores the user's settings");
}

#[test]
fn invalid_settings_are_left_alone() {
    let env = Env::new(&[".claude"]);
    let settings = env.home().join(".claude/settings.json");
    fs::write(&settings, "{ not json").unwrap();
    let (out, code) = env.wn(&["skill", "sync", "--yes", "--with-hook"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("not valid JSON"), "{out}");
    assert_eq!(fs::read_to_string(&settings).unwrap(), "{ not json");
}

#[test]
fn the_hook_is_silent_outside_indexed_repositories_and_after_the_first_prompt() {
    let env = Env::new(&[]);
    let cwd = env.home().to_str().unwrap().to_string();
    let run = |input: String| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_wn"))
            .args(["hook", "claude-prompt"])
            .env("HOME", env.home())
            .env("WHERE_NEXT_HOME", env.wn_home.path())
            .env("WN_NO_DAEMON", "1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write as _;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        (
            String::from_utf8_lossy(&out.stdout).to_string(),
            out.status.code(),
        )
    };
    let event =
        serde_json::json!({ "session_id": "s1", "prompt": "fix login", "cwd": cwd }).to_string();
    assert_eq!(run(event.clone()), (String::new(), Some(0)));
    assert_eq!(run("garbage".into()), (String::new(), Some(0)));
    assert_eq!(run(event), (String::new(), Some(0)));
}

#[test]
fn edit_hook_is_reversible_on_arbitrary_other_hooks() {
    use wn_cli::skill::edit_hook;
    let mut v = serde_json::json!({ "hooks": { "Stop": [ { "hooks": [] } ] } });
    let before = v.clone();
    edit_hook(&mut v, true);
    edit_hook(&mut v, true);
    assert_eq!(v["hooks"]["UserPromptSubmit"].as_array().unwrap().len(), 1);
    edit_hook(&mut v, false);
    assert_eq!(v, before);
}

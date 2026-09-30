//! `wn setup` / `wn skill sync` end to end with a temporary HOME: installs for detected agents, is
//! idempotent, shows a diff for outdated copies, never overwrites files it did not write, needs
//! `--yes` without a terminal, re-syncs from recorded state, uninstalls, and merges the Claude
//! Code, Codex and Cursor hooks into their settings without touching other content.

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
fn from_state_without_recorded_syncs_writes_nothing() {
    let env = Env::new(&[".claude"]);
    let (out, code) = env.wn(&["skill", "sync", "--yes", "--from-state"]);
    assert_eq!(code, 0, "{out}");
    assert!(!env.skill(".claude").exists());
    assert!(!env.wn_home.path().join("skills.json").exists());
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
    let ours = entries[1]["hooks"][0]["command"].as_str().unwrap();
    assert!(ours.ends_with("wn hook claude-prompt"), "{ours}");
    assert_eq!(v["hooks"]["PostToolUse"][0]["matcher"], "Grep|Glob|Bash");
    assert!(v["hooks"]["PostToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .ends_with("wn hook claude-search"));

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
fn the_hook_is_silent_outside_indexed_repositories() {
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
fn edit_hooks_is_idempotent_and_reversible_on_arbitrary_other_hooks() {
    use wn_cli::skill::{edit_hooks, Agent};
    for agent in Agent::ALL {
        // Other hooks, including an empty entry and an event we also use, stay as they are.
        let mut v = serde_json::json!({
            "hooks": {
                "Stop": [ { "hooks": [] } ],
                "UserPromptSubmit": [ { "hooks": [] } ],
                "postToolUse": [ { "command": "./audit.sh" } ]
            },
            "other": 1
        });
        let before = v.clone();
        edit_hooks(&mut v, agent, "wn", true);
        let once = v.clone();
        edit_hooks(&mut v, agent, "/usr/local/bin/wn", true);
        edit_hooks(&mut v, agent, "wn", true);
        assert_eq!(v, once, "{agent}: adding twice changes nothing");
        edit_hooks(&mut v, agent, "wn", false);
        assert_eq!(v, before, "{agent}");
    }
    // An empty file round-trips to an empty object.
    let mut v = serde_json::json!({});
    edit_hooks(&mut v, Agent::Codex, "wn", true);
    edit_hooks(&mut v, Agent::Codex, "wn", false);
    assert_eq!(v, serde_json::json!({}));
}

#[test]
fn our_commands_are_recognised_by_name_only() {
    use wn_cli::skill::is_our_command;
    for ours in [
        "wn hook claude-prompt",
        "/Users/me/.local/bin/wn hook codex-search",
        "'/Applications/My Tools/wn' hook cursor-search",
    ] {
        assert!(is_our_command(ours), "{ours}");
    }
    for theirs in [
        "echo hi",
        "wn ask foo",
        "mywn hook claude-prompt",
        "wn hook claude-prompt && rm -rf /",
    ] {
        assert!(!is_our_command(theirs), "{theirs}");
    }
}

#[test]
fn setup_connects_codex_and_cursor_hooks_and_uninstall_restores_everything() {
    let env = Env::new(&[".codex", ".cursor"]);
    // The user's own Cursor hook stays; Codex's hooks.json does not exist yet.
    let cursor = env.home().join(".cursor/hooks.json");
    let theirs = serde_json::json!({ "version": 1, "hooks": { "afterFileEdit": [ { "command": "./format.sh" } ] } });
    fs::write(&cursor, serde_json::to_string_pretty(&theirs).unwrap()).unwrap();
    let codex = env.home().join(".codex/hooks.json");

    let (out, code) = env.wn(&["setup", "--dry-run"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("hook codex-prompt") && out.contains("hook cursor-search"),
        "shows what it writes: {out}"
    );
    assert!(!codex.exists());

    let (out, code) = env.wn(&["setup", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("/hooks"),
        "tells how to trust Codex hooks: {out}"
    );
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&codex).unwrap()).unwrap();
    assert!(v["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .ends_with("wn hook codex-prompt"));
    assert_eq!(v["hooks"]["PostToolUse"][0]["matcher"], "Bash");
    let c: serde_json::Value = serde_json::from_str(&fs::read_to_string(&cursor).unwrap()).unwrap();
    assert_eq!(
        c["hooks"]["afterFileEdit"],
        theirs["hooks"]["afterFileEdit"]
    );
    assert_eq!(c["hooks"]["postToolUse"][0]["matcher"], "Shell|Grep");
    assert!(c["hooks"]["postToolUse"][0]["command"]
        .as_str()
        .unwrap()
        .ends_with("wn hook cursor-search"));

    let (out, _) = env.wn(&["skill", "sync", "--yes"]);
    assert!(
        out.contains("nothing to change"),
        "`skill sync` is the same command: {out}"
    );

    let (out, code) = env.wn(&["setup", "--uninstall", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(!codex.exists(), "a file only wn wrote is deleted: {out}");
    let c: serde_json::Value = serde_json::from_str(&fs::read_to_string(&cursor).unwrap()).unwrap();
    assert_eq!(c, theirs);
}

#[test]
fn no_hooks_installs_only_the_skill() {
    let env = Env::new(&[".claude", ".cursor"]);
    let (out, code) = env.wn(&["setup", "--yes", "--no-hooks"]);
    assert_eq!(code, 0, "{out}");
    assert!(env.skill(".claude").exists());
    assert!(!env.home().join(".claude/settings.json").exists());
    assert!(!env.home().join(".cursor/hooks.json").exists());
}

#[test]
fn codex_with_inline_hooks_in_config_toml_is_left_alone() {
    let env = Env::new(&[".codex"]);
    fs::write(
        env.home().join(".codex/config.toml"),
        "[[hooks.PreToolUse]]\nmatcher = \"Bash\"\n",
    )
    .unwrap();
    let (out, code) = env.wn(&["setup", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("defines hooks inline"), "{out}");
    assert!(!env.home().join(".codex/hooks.json").exists());
    let config = env.home().join(".codex/config.toml");
    let exe = env!("CARGO_BIN_EXE_wn");
    assert!(
        out.contains(&format!(
            "Codex: hooks NOT connected (config.toml defines hooks inline). Add these entries to {}:",
            config.display()
        )),
        "{out}"
    );
    for (event, matcher, command) in [
        ("SessionStart", None, "codex-start"),
        ("UserPromptSubmit", None, "codex-prompt"),
        ("PostToolUse", Some("Bash"), "codex-search"),
    ] {
        assert!(out.contains(&format!("[[hooks.{event}]]")), "{out}");
        assert!(out.contains(&format!("[[hooks.{event}.hooks]]")), "{out}");
        assert!(
            out.contains(&format!("command = \"{exe} hook {command}\"")),
            "{out}"
        );
        if let Some(matcher) = matcher {
            assert!(out.contains(&format!("matcher = \"{matcher}\"")), "{out}");
        }
    }
    assert!(out.contains("timeout = 5"), "{out}");
    assert_eq!(
        fs::read_to_string(config).unwrap(),
        "[[hooks.PreToolUse]]\nmatcher = \"Bash\"\n"
    );
}

#[test]
fn from_state_updates_recorded_hooks_including_the_old_claude_hook() {
    let env = Env::new(&[".claude"]);
    // An install from an older release: one first-prompt hook, recorded as `hook_settings`.
    let settings = env.home().join(".claude/settings.json");
    fs::write(
        &settings,
        r#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"wn hook claude-prompt","timeout":15}]}]}}"#,
    )
    .unwrap();
    fs::write(
        env.wn_home.path().join("skills.json"),
        serde_json::json!({ "version": "0", "targets": [], "hook_settings": settings }).to_string(),
    )
    .unwrap();
    let (out, code) = env.wn(&["skill", "sync", "--yes", "--from-state"]);
    assert_eq!(code, 0, "{out}");
    let v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
    let prompt = v["hooks"]["UserPromptSubmit"].as_array().unwrap();
    assert_eq!(
        prompt.len(),
        1,
        "the old entry is replaced, not duplicated: {v}"
    );
    assert_eq!(prompt[0]["hooks"][0]["timeout"], 5);
    assert!(v["hooks"]["PostToolUse"].is_array(), "{v}");
    assert!(!env.skill(".claude").exists(), "only recorded targets");
}

#[test]
fn init_suggests_setup_until_an_agent_is_connected() {
    let env = Env::new(&[".claude"]);
    let repo = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let ok = Command::new("git")
            .args(args)
            .current_dir(repo.path())
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    fs::write(repo.path().join("main.py"), "def main():\n    pass\n").unwrap();
    git(&["add", "-A"]);
    git(&["-c", "commit.gpgsign=false", "commit", "-qm", "first"]);
    let init = || {
        let out = Command::new(env!("CARGO_BIN_EXE_wn"))
            .args(["init"])
            .current_dir(repo.path())
            .env("HOME", env.home())
            .env("WHERE_NEXT_HOME", env.wn_home.path())
            .env("WN_MODELS_HOME", env.home().join("no-models"))
            .env("WN_NO_DAEMON", "1")
            .env_remove("WN_MODEL_DIR")
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stderr).to_string()
    };
    assert!(init().contains(wn_cli::skill::CONNECT_HINT));
    let (out, code) = env.wn(&["setup", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(!init().contains(wn_cli::skill::CONNECT_HINT));
}

#[test]
fn settings_behind_a_symlink_keep_the_link_and_their_permissions() {
    use std::os::unix::fs::PermissionsExt as _;
    let env = Env::new(&[".claude"]);
    let real_dir = env.home().join("dotfiles");
    fs::create_dir_all(&real_dir).unwrap();
    let real = real_dir.join("claude-settings.json");
    let original = "{\n  \"env\": { \"API_KEY\": \"sk-secret-canary\" }\n}\n";
    fs::write(&real, original).unwrap();
    fs::set_permissions(&real, fs::Permissions::from_mode(0o600)).unwrap();
    let link = env.home().join(".claude/settings.json");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let (out, code) = env.wn(&["setup", "--yes", "--agent", "claude"]);
    assert_eq!(code, 0, "{out}");
    assert!(fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(fs::read_to_string(&real)
        .unwrap()
        .contains("wn hook claude-prompt"));
    assert_eq!(
        fs::metadata(&real).unwrap().permissions().mode() & 0o777,
        0o600
    );
    // wn keeps no copy of the settings: only which files it touched, privately.
    let state = env.wn_home.path().join("skills.json");
    assert!(!fs::read_to_string(&state)
        .unwrap()
        .contains("sk-secret-canary"));
    assert_eq!(
        fs::metadata(&state).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let leftovers: Vec<_> = fs::read_dir(&real_dir).unwrap().flatten().collect();
    assert_eq!(leftovers.len(), 1, "no temporary files left behind");

    let (out, code) = env.wn(&["setup", "--uninstall", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read_to_string(&real).unwrap(), original);
}

#[test]
fn a_file_edited_after_the_plan_is_not_overwritten() {
    use wn_cli::skill::{apply, plan, Agent, HookRecord};
    let env = Env::new(&[".claude"]);
    let settings = env.home().join(".claude/settings.json");
    fs::write(&settings, "{\"model\": \"opus\"}\n").unwrap();
    let files: Vec<(Agent, std::path::PathBuf, Option<HookRecord>)> =
        vec![(Agent::Claude, settings.clone(), None)];
    let p = plan(&[], false, &files, "wn");
    assert!(p.has_changes());
    fs::write(&settings, "{\"model\": \"sonnet\"}\n").unwrap();
    let err = apply(&p, env.wn_home.path(), false).unwrap_err();
    assert!(err.contains("changed since the plan"), "{err}");
    assert_eq!(
        fs::read_to_string(&settings).unwrap(),
        "{\"model\": \"sonnet\"}\n"
    );
}

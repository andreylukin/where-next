//! Uninstalling: `wn setup` then `wn setup --uninstall` leaves agent settings byte-identical
//! (with the user's own hooks in them), and `wn uninstall` in a temporary HOME leaves nothing
//! wn-owned behind: agent skill and hooks, caches, models, source checkout, binary and the files
//! beside it.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The user's own settings, in formatting `serde_json` would not produce.
const CLAUDE: &str = "{\"model\":\"opus\",\n  \"hooks\": {\"Stop\": [{\"hooks\": [{\"type\": \"command\", \"command\": \"say done\"}]}],\n  \"UserPromptSubmit\": [{\"hooks\": [{\"type\":\"command\",\"command\":\"~/bin/log-prompt\"}]}]}}\n";
const CODEX: &str =
    "{\n    \"hooks\": {\n        \"Stop\": [ { \"hooks\": [ { \"type\": \"command\", \"command\": \"true\" } ] } ]\n    }\n}\n";
const CURSOR: &str = "{\"version\":1,\"hooks\":{\"postToolUse\":[{\"command\":\"./audit.sh\"}]}}";

struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    fn new() -> Home {
        let dir = tempfile::tempdir().unwrap();
        let h = dir.path();
        for d in [".claude", ".codex", ".cursor"] {
            fs::create_dir_all(h.join(d)).unwrap();
        }
        fs::write(h.join(".claude/settings.json"), CLAUDE).unwrap();
        fs::write(h.join(".codex/hooks.json"), CODEX).unwrap();
        fs::write(h.join(".cursor/hooks.json"), CURSOR).unwrap();
        Home { dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn run(&self, exe: &Path, args: &[&str]) -> (String, i32) {
        let out = Command::new(exe)
            .args(args)
            .current_dir(self.path())
            .env("HOME", self.path())
            .env_remove("WHERE_NEXT_HOME")
            .env_remove("WN_MODELS_HOME")
            .env_remove("WN_MODEL_DIR")
            .env_remove("WN_HOME")
            .env_remove("XDG_DATA_HOME")
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME")
            .env("WN_NO_DAEMON", "1")
            .output()
            .unwrap();
        let mut text = String::from_utf8_lossy(&out.stdout).to_string();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        (text, out.status.code().unwrap_or(-1))
    }

    /// Every file under the home, with its bytes.
    fn files(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for e in fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, root, out);
                } else {
                    out.insert(
                        p.strip_prefix(root).unwrap().to_path_buf(),
                        fs::read(&p).unwrap(),
                    );
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(self.path(), self.path(), &mut out);
        out
    }
}

fn wn() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_wn"))
}

#[test]
fn setup_then_uninstall_leaves_agent_settings_byte_identical() {
    let home = Home::new();
    let before = home.files();
    let (out, code) = home.run(&wn(), &["setup", "--yes"]);
    assert_eq!(code, 0, "{out}");
    let during = home.files();
    assert_ne!(during, before, "setup wrote something");
    assert!(during.keys().any(|p| p.ends_with("where-next/SKILL.md")));
    // Twice is the same as once.
    let (out, _) = home.run(&wn(), &["setup", "--yes"]);
    assert!(out.contains("nothing to change"), "{out}");

    let (out, code) = home.run(&wn(), &["setup", "--uninstall", "--yes"]);
    assert_eq!(code, 0, "{out}");
    for said in ["removed", "hooks: removed from"] {
        assert!(out.contains(said), "says what it removed: {out}");
    }
    let mut after = home.files();
    after.retain(|p, _| !p.starts_with(".cache"));
    assert_eq!(after, before, "{out}");
    for skills in [".claude/skills", ".agents/skills", ".cursor/skills"] {
        assert!(!home.path().join(skills).exists(), "{skills} left behind");
    }
}

#[test]
fn uninstall_removes_everything_wn_owns_and_nothing_else() {
    let home = Home::new();
    let h = home.path();
    // A release install: the binary, its runtime library and the installer marker.
    let bin = h.join(".local/bin");
    fs::create_dir_all(&bin).unwrap();
    let exe = bin.join("wn");
    fs::copy(wn(), &exe).unwrap();
    fs::write(bin.join("libonnxruntime.so"), "lib").unwrap();
    fs::write(bin.join("wn.install-method"), "release\n").unwrap();
    fs::write(bin.join("other-tool"), "not ours").unwrap();
    // Caches, a model and a source checkout.
    fs::create_dir_all(h.join(".cache/where-next/repo-1/index")).unwrap();
    fs::create_dir_all(h.join(".cache/where-next-models/gemma-xl1")).unwrap();
    fs::write(h.join(".cache/where-next-models/gemma-xl1/model.onnx"), "m").unwrap();
    fs::create_dir_all(h.join(".local/share/where-next/src/.git")).unwrap();
    fs::create_dir_all(h.join(".cache/other-app")).unwrap();
    let before_setup = home.files();
    let (out, code) = home.run(&exe, &["setup", "--yes"]);
    assert_eq!(code, 0, "{out}");

    // Without a terminal and without --yes: shows the list, removes nothing.
    let (out, code) = home.run(&exe, &["uninstall"]);
    assert_eq!(code, 1, "{out}");
    for listed in [
        "wn.install-method",
        "libonnxruntime.so",
        ".cache/where-next-models",
        ".local/share/where-next",
        "settings.json",
        "--yes",
    ] {
        assert!(out.contains(listed), "{listed} not listed: {out}");
    }
    assert!(exe.exists());

    let (out, code) = home.run(&exe, &["uninstall", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("PATH"), "notes the PATH line: {out}");
    let mut expected = before_setup.clone();
    expected.retain(|p, _| {
        !(p.starts_with(".cache/where-next")
            || p.starts_with(".cache/where-next-models")
            || p.starts_with(".local/share/where-next")
            || p.starts_with(".local/bin/wn")
            || p.ends_with("libonnxruntime.so")
            || p.ends_with("wn.install-method"))
    });
    assert_eq!(home.files(), expected, "{out}");
    for gone in [
        ".cache/where-next",
        ".cache/where-next-models",
        ".local/share/where-next",
    ] {
        assert!(!h.join(gone).exists(), "{gone} left behind");
    }
    assert!(h.join(".cache/other-app").exists());
    assert_eq!(
        fs::read_to_string(h.join(".claude/settings.json")).unwrap(),
        CLAUDE
    );
}

#[test]
fn keep_models_keeps_them() {
    let home = Home::new();
    let h = home.path();
    let bin = h.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let exe = bin.join("wn");
    fs::copy(wn(), &exe).unwrap();
    fs::create_dir_all(h.join(".cache/where-next-models/gemma-xl1")).unwrap();
    fs::create_dir_all(h.join(".cache/where-next")).unwrap();
    let (out, code) = home.run(&exe, &["uninstall", "--yes", "--keep-models"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("kept"), "{out}");
    assert!(h.join(".cache/where-next-models/gemma-xl1").exists());
    assert!(!h.join(".cache/where-next").exists());
    assert!(!exe.exists());
}

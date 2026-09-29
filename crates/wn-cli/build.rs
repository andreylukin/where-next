//! Embeds the git commit this binary was built from, for `wn --version` and `wn update`.
//!
//! Sets `WN_GIT_COMMIT` (full sha, or empty when not built from a git checkout) and
//! `WN_VERSION_SUFFIX` (` (abc1234 2026-09-29)`, or empty). `WN_GIT_COMMIT` in the build
//! environment overrides detection (reproducible builds, packaging).

use std::path::Path;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn main() {
    println!("cargo:rerun-if-env-changed=WN_GIT_COMMIT");
    let (commit, date) = match std::env::var("WN_GIT_COMMIT") {
        Ok(c) if !c.is_empty() => (Some(c), None),
        _ => (
            git(&["rev-parse", "HEAD"]),
            git(&["log", "-1", "--format=%cs"]),
        ),
    };
    // Rebuild when HEAD moves (checkout, commit, fast-forward). Only existing files are watched:
    // a missing path would make cargo rerun this script on every build.
    let watch = |p: &Path| {
        if p.exists() {
            println!("cargo:rerun-if-changed={}", p.display());
        }
    };
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        let dir = Path::new(&dir);
        watch(&dir.join("HEAD"));
        if let Some(common) = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"]) {
            let common = Path::new(&common);
            watch(&common.join("packed-refs"));
            if let Ok(head) = std::fs::read_to_string(dir.join("HEAD")) {
                if let Some(r) = head.trim().strip_prefix("ref: ") {
                    watch(&common.join(r));
                }
            }
        }
    }
    let commit = commit.unwrap_or_default();
    let suffix = match (&commit, date) {
        (c, _) if c.is_empty() => String::new(),
        (c, Some(d)) => format!(" ({} {d})", &c[..c.len().min(7)]),
        (c, None) => format!(" ({})", &c[..c.len().min(7)]),
    };
    println!("cargo:rustc-env=WN_GIT_COMMIT={commit}");
    println!("cargo:rustc-env=WN_VERSION_SUFFIX={suffix}");
}

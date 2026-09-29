//! `wn model pull | list | remove`: installed models under [`crate::models_home`].
//!
//! Pulling goes through `wn_embed::store::ModelStore`, so a model is only kept after every file
//! matches its SHA-256 manifest (Missing → Downloading → Verifying → Loaded). There is no default
//! source until hosting is decided: `--source` is required.

use std::fs;
use std::path::{Path, PathBuf};

use clap::Subcommand;
use serde::Serialize;

/// `wn model …` actions.
#[derive(Debug, Subcommand)]
pub enum ModelAction {
    /// Download (or copy) a model, verify it against its manifest, and install it.
    Pull {
        /// Name to install under (e.g. gemma-xl1).
        name: String,
        /// Where to get it: a local directory, an https:// base URL, or hf:owner/repo[@revision].
        #[arg(long)]
        source: String,
        /// Replace an installed model with the same name.
        #[arg(long)]
        force: bool,
    },
    /// List installed models.
    List {
        /// Re-hash every file against its manifest (slow for large models).
        #[arg(long)]
        verify: bool,
    },
    /// Delete an installed model.
    Remove {
        /// Installed model name.
        name: String,
        /// Confirm deletion.
        #[arg(long)]
        yes: bool,
    },
}

/// Printed the first time a Gemma-derived model is installed.
pub const GEMMA_NOTICE: &str = "\
This model is fine-tuned from Google's EmbeddingGemma and is provided under and subject to the
Gemma Terms of Use (https://ai.google.dev/gemma/terms), including the Gemma Prohibited Use Policy
(https://ai.google.dev/gemma/prohibited_use_policy). By using it you agree to those terms.
where-next is not affiliated with or endorsed by Google.";

const NOTICE_MARKER: &str = ".license-notice-shown";

/// One installed model, as listed.
#[derive(Debug, Serialize)]
pub struct InstalledModel {
    pub name: String,
    pub path: PathBuf,
    pub family: Option<String>,
    pub license: String,
    pub bytes: u64,
    pub fingerprint: Option<String>,
    /// `Some(true/false)` only with `--verify`.
    pub verified: Option<bool>,
}

/// Result of a model command.
#[derive(Debug, Serialize)]
pub struct ModelReport {
    pub action: &'static str,
    pub ok: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<InstalledModel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
}

fn family(dir: &Path) -> Option<String> {
    let text = fs::read_to_string(dir.join("wn-model.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("family")?.as_str().map(str::to_string)
}

fn license_for(family: Option<&str>) -> String {
    match family {
        Some("gemma") => "Gemma Terms of Use".into(),
        Some(_) => "see the model's source".into(),
        None => "unknown".into(),
    }
}

fn dir_bytes(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| {
            let p = e.path();
            if p.is_dir() {
                dir_bytes(&p)
            } else {
                e.metadata().map(|m| m.len()).unwrap_or(0)
            }
        })
        .sum()
}

fn fail(action: &'static str, message: String) -> (ModelReport, i32) {
    (
        ModelReport {
            action,
            ok: false,
            message,
            models: Vec::new(),
            notice: None,
        },
        1,
    )
}

/// Runs a model action against `models_home`.
pub fn run(action: ModelAction, models_home: &Path) -> (ModelReport, i32) {
    match action {
        ModelAction::Pull {
            name,
            source,
            force,
        } => pull(&name, &source, force, models_home),
        ModelAction::List { verify } => list(models_home, verify),
        ModelAction::Remove { name, yes } => remove(&name, yes, models_home),
    }
}

fn pull(name: &str, source: &str, force: bool, home: &Path) -> (ModelReport, i32) {
    use wn_embed::source::ModelSource;
    use wn_embed::store::ModelStore;

    if !valid_name(name) {
        return fail("pull", format!("invalid model name {name:?}"));
    }
    let source = match ModelSource::parse(source) {
        Ok(s) => s,
        Err(e) => return fail("pull", e.to_string()),
    };
    let dir = home.join(name);
    if dir.exists() {
        if !force {
            return fail(
                "pull",
                format!(
                    "{name} is already installed at {} (use --force)",
                    dir.display()
                ),
            );
        }
        if let Err(e) = fs::remove_dir_all(&dir) {
            return fail("pull", format!("could not remove {}: {e}", dir.display()));
        }
    }
    let mut store = ModelStore::new(&dir, source.clone());
    if let Err(e) = store.ensure() {
        // Never leave an unverified model where `wn` would pick it up.
        let _ = fs::remove_dir_all(&dir);
        return fail("pull", format!("{name}: {e}"));
    }
    if !dir.join("wn-model.json").is_file() {
        let _ = fs::remove_dir_all(&dir);
        return fail("pull", format!("{name}: source has no wn-model.json"));
    }
    let fam = family(&dir);
    let notice =
        (fam.as_deref() == Some("gemma") && !dir.join(NOTICE_MARKER).exists()).then(|| {
            let _ = fs::write(dir.join(NOTICE_MARKER), "");
            GEMMA_NOTICE.to_string()
        });
    let fingerprint = store.manifest().map(|m| m.fingerprint());
    (
        ModelReport {
            action: "pull",
            ok: true,
            message: format!(
                "installed {name} from {} (verified, fingerprint {})",
                source.describe(),
                fingerprint.as_deref().unwrap_or("?")
            ),
            models: Vec::new(),
            notice,
        },
        0,
    )
}

fn list(home: &Path, verify: bool) -> (ModelReport, i32) {
    let mut models = Vec::new();
    let mut entries: Vec<_> = fs::read_dir(home)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("wn-model.json").is_file())
        .collect();
    entries.sort();
    for dir in entries {
        let manifest = fs::read_to_string(dir.join(wn_embed::verify::MANIFEST_FILE))
            .ok()
            .and_then(|t| serde_json::from_str::<wn_embed::verify::Manifest>(&t).ok());
        let fam = family(&dir);
        models.push(InstalledModel {
            name: dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            license: license_for(fam.as_deref()),
            family: fam,
            bytes: dir_bytes(&dir),
            fingerprint: manifest.as_ref().map(|m| m.fingerprint()),
            verified: verify.then(|| manifest.map(|m| m.verify(&dir).is_ok()).unwrap_or(false)),
            path: dir,
        });
    }
    let message = if models.is_empty() {
        format!("no models installed in {}", home.display())
    } else {
        format!("{} model(s) in {}", models.len(), home.display())
    };
    (
        ModelReport {
            action: "list",
            ok: true,
            message,
            models,
            notice: None,
        },
        0,
    )
}

fn remove(name: &str, yes: bool, home: &Path) -> (ModelReport, i32) {
    if !valid_name(name) {
        return fail("remove", format!("invalid model name {name:?}"));
    }
    let dir = home.join(name);
    if !dir.join("wn-model.json").is_file() {
        return fail(
            "remove",
            format!("{name} is not installed in {}", home.display()),
        );
    }
    if !yes {
        return fail(
            "remove",
            format!(
                "this deletes {}; re-run with --yes to confirm",
                dir.display()
            ),
        );
    }
    match fs::remove_dir_all(&dir) {
        Ok(()) => (
            ModelReport {
                action: "remove",
                ok: true,
                message: format!("removed {name}"),
                models: Vec::new(),
                notice: None,
            },
            0,
        ),
        Err(e) => fail("remove", format!("could not remove {}: {e}", dir.display())),
    }
}

/// Text form of a model report.
pub fn render(report: &ModelReport) -> String {
    let mut out = report.message.clone();
    for m in &report.models {
        let verified = match m.verified {
            Some(true) => ", verified",
            Some(false) => ", FAILED verification",
            None => "",
        };
        out.push_str(&format!(
            "\n  {:<12} {:>8.1} MB  {}  license: {}{}",
            m.name,
            m.bytes as f64 / 1e6,
            m.fingerprint.as_deref().unwrap_or("no manifest"),
            m.license,
            verified
        ));
    }
    if let Some(n) = &report.notice {
        out.push_str("\n\n");
        out.push_str(n);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use wn_embed::verify::{Manifest, MANIFEST_FILE};

    fn source_model(family: &str) -> tempfile::TempDir {
        let src = tempfile::tempdir().unwrap();
        let spec = format!(r#"{{"name":"t","family":"{family}"}}"#);
        fs::write(src.path().join("wn-model.json"), spec).unwrap();
        fs::write(src.path().join("model.onnx"), b"weights").unwrap();
        let m = Manifest::compute(src.path(), &["wn-model.json", "model.onnx"]).unwrap();
        fs::write(
            src.path().join(MANIFEST_FILE),
            serde_json::to_string(&m).unwrap(),
        )
        .unwrap();
        src
    }

    fn pull_action(name: &str, source: &Path, force: bool) -> ModelAction {
        ModelAction::Pull {
            name: name.into(),
            source: source.display().to_string(),
            force,
        }
    }

    #[test]
    fn pull_list_remove_round_trip_with_gemma_notice_once() {
        let src = source_model("gemma");
        let home = tempfile::tempdir().unwrap();
        let (r, code) = run(pull_action("gemma-t", src.path(), false), home.path());
        assert_eq!(code, 0, "{}", r.message);
        assert!(r.notice.as_deref().unwrap().contains("Gemma Terms of Use"));

        let (again, code) = run(pull_action("gemma-t", src.path(), false), home.path());
        assert_eq!(code, 1);
        assert!(again.message.contains("--force"));

        let (forced, code) = run(pull_action("gemma-t", src.path(), true), home.path());
        assert_eq!(code, 0);
        // A fresh install shows the notice again (the directory was replaced).
        assert!(forced.notice.is_some());

        let (l, _) = run(ModelAction::List { verify: true }, home.path());
        assert_eq!(l.models.len(), 1);
        assert_eq!(l.models[0].license, "Gemma Terms of Use");
        assert_eq!(l.models[0].verified, Some(true));

        let (no, code) = run(
            ModelAction::Remove {
                name: "gemma-t".into(),
                yes: false,
            },
            home.path(),
        );
        assert_eq!(code, 1);
        assert!(no.message.contains("--yes"));
        assert!(home.path().join("gemma-t").exists());

        let (_, code) = run(
            ModelAction::Remove {
                name: "gemma-t".into(),
                yes: true,
            },
            home.path(),
        );
        assert_eq!(code, 0);
        assert!(!home.path().join("gemma-t").exists());
    }

    #[test]
    fn non_gemma_model_has_no_notice() {
        let src = source_model("qwen");
        let home = tempfile::tempdir().unwrap();
        let (r, code) = run(pull_action("v2b", src.path(), false), home.path());
        assert_eq!(code, 0);
        assert!(r.notice.is_none());
    }

    #[test]
    fn tampered_source_is_not_installed() {
        let src = source_model("gemma");
        fs::write(src.path().join("model.onnx"), b"tampered").unwrap();
        let home = tempfile::tempdir().unwrap();
        let (r, code) = run(pull_action("bad", src.path(), false), home.path());
        assert_eq!(code, 1);
        assert!(r.message.contains("verification failed"), "{}", r.message);
        assert!(!home.path().join("bad").exists());
    }

    #[test]
    fn names_are_validated() {
        let home = tempfile::tempdir().unwrap();
        for bad in ["", "..", "a/b", "x y"] {
            let (_, code) = run(
                ModelAction::Remove {
                    name: bad.into(),
                    yes: true,
                },
                home.path(),
            );
            assert_eq!(code, 1, "{bad:?}");
        }
    }
}

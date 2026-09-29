//! `wn model pull | list | remove`: installed models under [`crate::models_home`].
//!
//! Pulling goes through `wn_embed::store::ModelStore`, so a model is only kept after every file
//! matches its SHA-256 manifest (Missing → Downloading → Verifying → Loaded). Known models (see
//! [`KNOWN_MODELS`]) have a pinned default source, so `wn model pull` with no arguments installs
//! [`DEFAULT_MODEL`]; `--source` overrides it. The source a model was installed from is recorded
//! next to it, so a pull is a no-op when it is current and an upgrade when the pin moved.

use std::fs;
use std::path::{Path, PathBuf};

use clap::Subcommand;
use serde::Serialize;

/// `wn model …` actions.
#[derive(Debug, Subcommand)]
pub enum ModelAction {
    /// Download (or copy) a model, verify it against its manifest, and install it.
    #[command(
        after_help = "Examples:\n  wn model pull                  # install or upgrade the default model (gemma-xl1)\n  wn model pull --check          # exit 0 if current, 10 if a download is needed\n  wn model pull gemma-xl1 --source ./my-model-dir"
    )]
    Pull {
        /// Name to install under (default: the default model, gemma-xl1).
        name: Option<String>,
        /// Where to get it: a local directory, an https:// base URL, or hf:owner/repo[@revision]
        /// (default: the pinned source of a known model).
        #[arg(long)]
        source: Option<String>,
        /// Replace an installed model with the same name.
        #[arg(long)]
        force: bool,
        /// Only report whether a download is needed: exit 0 if installed and current, 10 if not.
        #[arg(long)]
        check: bool,
    },
    /// List installed models and the known models available to pull.
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

/// Records the source a model was installed from (for up-to-date checks).
pub const SOURCE_MARKER: &str = ".wn-source";

/// A model with a published, pinned source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct KnownModel {
    pub name: &'static str,
    /// Pinned source (`hf:owner/repo@revision`); files are still verified against the manifest.
    pub source: &'static str,
    pub family: &'static str,
    /// Approximate download size, for prompts.
    pub download_mb: u32,
}

/// Models `wn model pull NAME` can install without `--source`.
pub const KNOWN_MODELS: &[KnownModel] = &[KnownModel {
    name: "gemma-xl1",
    source: "hf:lukandrey/where-next-gemma-xl1@0ba99c9950a937762f7b908ed6dd05acae185b32",
    family: "gemma",
    download_mb: 1250,
}];

/// What `wn model pull` installs when no name is given.
pub const DEFAULT_MODEL: &str = "gemma-xl1";

/// The known model named `name`.
pub fn known(name: &str) -> Option<&'static KnownModel> {
    KNOWN_MODELS.iter().find(|m| m.name == name)
}

/// Exit code of `wn model pull --check` when a download is needed.
pub const CHECK_NEEDS_DOWNLOAD: i32 = crate::update::EXIT_UPDATE_AVAILABLE;

/// State of the install directory for the requested name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Installed {
    /// No model under that name.
    Missing,
    /// Installed from the requested source.
    SameSource,
    /// Installed from another source (or an unknown one: installed before sources were recorded).
    OtherSource,
}

/// How the source was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requested {
    /// The pinned source of a known model (no `--source`).
    Pinned,
    /// An explicit `--source`.
    Explicit,
}

/// What a pull does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullPlan {
    /// Download and install into an empty slot.
    Install,
    /// Already installed from this source: nothing to do.
    UpToDate,
    /// Replace the installed copy (the pin moved, or `--force`).
    Replace,
    /// Refuse: installed from another source and the caller did not pass `--force`.
    Refuse,
}

/// The pull decision, as a pure function of the install state (tested exhaustively).
pub fn plan(installed: Installed, requested: Requested, force: bool) -> PullPlan {
    match (installed, requested, force) {
        (Installed::Missing, _, _) => PullPlan::Install,
        (_, _, true) => PullPlan::Replace,
        (Installed::SameSource, _, false) => PullPlan::UpToDate,
        // A known model's pin moved: upgrading it is the point of re-running pull/install.
        (Installed::OtherSource, Requested::Pinned, false) => PullPlan::Replace,
        (Installed::OtherSource, Requested::Explicit, false) => PullPlan::Refuse,
    }
}

fn installed_state(dir: &Path, source: &str) -> Installed {
    if !dir.join("wn-model.json").is_file() {
        return Installed::Missing;
    }
    match fs::read_to_string(dir.join(SOURCE_MARKER)) {
        Ok(recorded) if recorded.trim() == source => Installed::SameSource,
        _ => Installed::OtherSource,
    }
}

/// A known model, as listed.
#[derive(Debug, Serialize)]
pub struct AvailableModel {
    pub name: String,
    pub source: String,
    pub download_mb: u32,
    pub installed: bool,
    pub default: bool,
}

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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub available: Vec<AvailableModel>,
}

fn report(action: &'static str, message: String) -> ModelReport {
    ModelReport {
        action,
        ok: true,
        message,
        models: Vec::new(),
        notice: None,
        available: Vec::new(),
    }
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
            ok: false,
            ..report(action, message)
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
            check,
        } => pull(
            name.as_deref().unwrap_or(DEFAULT_MODEL),
            source.as_deref(),
            force,
            check,
            models_home,
        ),
        ModelAction::List { verify } => list(models_home, verify),
        ModelAction::Remove { name, yes } => remove(&name, yes, models_home),
    }
}

fn pull(
    name: &str,
    source: Option<&str>,
    force: bool,
    check: bool,
    home: &Path,
) -> (ModelReport, i32) {
    use wn_embed::source::ModelSource;
    use wn_embed::store::ModelStore;

    if !valid_name(name) {
        return fail("pull", format!("invalid model name {name:?}"));
    }
    let (source_text, requested) = match (source, known(name)) {
        (Some(s), _) => (s.to_string(), Requested::Explicit),
        (None, Some(k)) => (k.source.to_string(), Requested::Pinned),
        (None, None) => {
            let names: Vec<_> = KNOWN_MODELS.iter().map(|m| m.name).collect();
            return fail(
                "pull",
                format!(
                    "{name} is not a known model (known: {}); pass --source",
                    names.join(", ")
                ),
            );
        }
    };
    let source = match ModelSource::parse(&source_text) {
        Ok(s) => s,
        Err(e) => return fail("pull", e.to_string()),
    };
    let dir = home.join(name);
    let state = installed_state(&dir, &source_text);
    let decision = plan(state, requested, force);
    if check {
        let (message, code) = match decision {
            PullPlan::UpToDate => (format!("{name} is installed and current"), 0),
            PullPlan::Install => (
                format!("{name} is not installed ({})", source.describe()),
                CHECK_NEEDS_DOWNLOAD,
            ),
            PullPlan::Replace => (
                format!("{name} has an update ({})", source.describe()),
                CHECK_NEEDS_DOWNLOAD,
            ),
            PullPlan::Refuse => (
                format!("{name} is installed from another source (use --force to replace)"),
                CHECK_NEEDS_DOWNLOAD,
            ),
        };
        return (report("pull", message), code);
    }
    match decision {
        PullPlan::UpToDate => {
            return (
                report(
                    "pull",
                    format!("{name} is up to date ({})", source.describe()),
                ),
                0,
            )
        }
        PullPlan::Refuse => {
            return fail(
                "pull",
                format!(
                    "{name} is already installed at {} from another source (use --force)",
                    dir.display()
                ),
            )
        }
        PullPlan::Install | PullPlan::Replace => {}
    }
    // Download next to the target and swap only after verification, so a failed upgrade keeps
    // the working copy and an unverified model is never where `wn` would pick it up.
    let staging = home.join(format!(".{name}.pulling"));
    let _ = fs::remove_dir_all(&staging);
    let mut store = ModelStore::new(&staging, source.clone());
    if let Err(e) = store.ensure() {
        let _ = fs::remove_dir_all(&staging);
        return fail("pull", format!("{name}: {e}"));
    }
    if !staging.join("wn-model.json").is_file() {
        let _ = fs::remove_dir_all(&staging);
        return fail("pull", format!("{name}: source has no wn-model.json"));
    }
    let _ = fs::write(staging.join(SOURCE_MARKER), &source_text);
    let notice_shown = dir.join(NOTICE_MARKER).exists();
    if dir.exists() {
        if let Err(e) = fs::remove_dir_all(&dir) {
            let _ = fs::remove_dir_all(&staging);
            return fail("pull", format!("could not replace {}: {e}", dir.display()));
        }
    }
    if let Err(e) = fs::rename(&staging, &dir) {
        let _ = fs::remove_dir_all(&staging);
        return fail(
            "pull",
            format!("could not install into {}: {e}", dir.display()),
        );
    }
    let fam = family(&dir);
    let notice = (fam.as_deref() == Some("gemma") && !notice_shown).then(|| {
        let _ = fs::write(dir.join(NOTICE_MARKER), "");
        GEMMA_NOTICE.to_string()
    });
    let fingerprint = store.manifest().map(|m| m.fingerprint());
    let verb = if decision == PullPlan::Replace {
        "updated"
    } else {
        "installed"
    };
    (
        ModelReport {
            notice,
            ..report(
                "pull",
                format!(
                    "{verb} {name} from {} (verified, fingerprint {})",
                    source.describe(),
                    fingerprint.as_deref().unwrap_or("?")
                ),
            )
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
        format!(
            "no models installed in {} (install the default with: wn model pull)",
            home.display()
        )
    } else {
        format!("{} model(s) in {}", models.len(), home.display())
    };
    let available = KNOWN_MODELS
        .iter()
        .map(|k| AvailableModel {
            name: k.name.into(),
            source: k.source.into(),
            download_mb: k.download_mb,
            installed: models.iter().any(|m| m.name == k.name),
            default: k.name == DEFAULT_MODEL,
        })
        .collect();
    (
        ModelReport {
            models,
            available,
            ..report("list", message)
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
        Ok(()) => (report("remove", format!("removed {name}")), 0),
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
    let pullable: Vec<_> = report.available.iter().filter(|a| !a.installed).collect();
    if !pullable.is_empty() {
        out.push_str("\navailable:");
        for a in pullable {
            out.push_str(&format!(
                "\n  {:<12} ~{} MB  wn model pull {}{}",
                a.name,
                a.download_mb,
                a.name,
                if a.default { "  (default)" } else { "" }
            ));
        }
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
            name: Some(name.into()),
            source: Some(source.display().to_string()),
            force,
            check: false,
        }
    }

    fn check_action(name: &str, source: &Path) -> ModelAction {
        ModelAction::Pull {
            name: Some(name.into()),
            source: Some(source.display().to_string()),
            force: false,
            check: true,
        }
    }

    #[test]
    fn pull_list_remove_round_trip_with_gemma_notice_once() {
        let src = source_model("gemma");
        let home = tempfile::tempdir().unwrap();
        let (r, code) = run(pull_action("gemma-t", src.path(), false), home.path());
        assert_eq!(code, 0, "{}", r.message);
        assert!(r.notice.as_deref().unwrap().contains("Gemma Terms of Use"));

        // Same source again: nothing to do (re-running the installer must be cheap).
        let (again, code) = run(pull_action("gemma-t", src.path(), false), home.path());
        assert_eq!(code, 0, "{}", again.message);
        assert!(again.message.contains("up to date"), "{}", again.message);

        let (forced, code) = run(pull_action("gemma-t", src.path(), true), home.path());
        assert_eq!(code, 0);
        assert!(forced.message.starts_with("updated"), "{}", forced.message);
        // The notice is shown once per install location, not again on a replace.
        assert!(forced.notice.is_none());
        assert!(!home.path().join(".gemma-t.pulling").exists());

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
    fn plan_is_total_and_matches_the_spec() {
        use Installed::*;
        use Requested::*;
        for installed in [Missing, SameSource, OtherSource] {
            for requested in [Pinned, Explicit] {
                for force in [false, true] {
                    let expected = match (installed, requested, force) {
                        (Missing, _, _) => PullPlan::Install,
                        (_, _, true) => PullPlan::Replace,
                        (SameSource, _, false) => PullPlan::UpToDate,
                        (OtherSource, Pinned, false) => PullPlan::Replace,
                        (OtherSource, Explicit, false) => PullPlan::Refuse,
                    };
                    assert_eq!(plan(installed, requested, force), expected);
                }
            }
        }
    }

    #[test]
    fn check_reports_without_downloading() {
        let src = source_model("gemma");
        let home = tempfile::tempdir().unwrap();
        let (r, code) = run(check_action("gemma-t", src.path()), home.path());
        assert_eq!(code, CHECK_NEEDS_DOWNLOAD, "{}", r.message);
        assert!(!home.path().join("gemma-t").exists());
        run(pull_action("gemma-t", src.path(), false), home.path());
        let (r, code) = run(check_action("gemma-t", src.path()), home.path());
        assert_eq!(code, 0, "{}", r.message);
        // A different explicit source is not "current".
        let other = source_model("gemma");
        let (_, code) = run(check_action("gemma-t", other.path()), home.path());
        assert_eq!(code, CHECK_NEEDS_DOWNLOAD);
    }

    #[test]
    fn other_explicit_source_is_refused_and_keeps_the_working_copy() {
        let src = source_model("gemma");
        let other = source_model("gemma");
        let home = tempfile::tempdir().unwrap();
        run(pull_action("gemma-t", src.path(), false), home.path());
        let (r, code) = run(pull_action("gemma-t", other.path(), false), home.path());
        assert_eq!(code, 1);
        assert!(r.message.contains("--force"), "{}", r.message);
        assert!(home.path().join("gemma-t/wn-model.json").is_file());
    }

    #[test]
    fn failed_upgrade_keeps_the_installed_model() {
        let src = source_model("gemma");
        let home = tempfile::tempdir().unwrap();
        run(pull_action("gemma-t", src.path(), false), home.path());
        let bad = source_model("gemma");
        fs::write(bad.path().join("model.onnx"), b"tampered").unwrap();
        let (_, code) = run(pull_action("gemma-t", bad.path(), true), home.path());
        assert_eq!(code, 1);
        assert!(home.path().join("gemma-t/wn-model.json").is_file());
        assert!(!home.path().join(".gemma-t.pulling").exists());
    }

    #[test]
    fn default_model_is_known_with_a_pinned_source() {
        let k = known(DEFAULT_MODEL).expect("default model is known");
        assert!(
            k.source.starts_with("hf:") && k.source.contains('@'),
            "{}",
            k.source
        );
        let home = tempfile::tempdir().unwrap();
        let (r, code) = run(
            ModelAction::Pull {
                name: Some("nope".into()),
                source: None,
                force: false,
                check: false,
            },
            home.path(),
        );
        assert_eq!(code, 1);
        assert!(r.message.contains("--source"), "{}", r.message);
        let (l, _) = run(ModelAction::List { verify: false }, home.path());
        assert!(l.available.iter().any(|a| a.default && !a.installed));
        assert!(render(&l).contains("wn model pull gemma-xl1"));
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

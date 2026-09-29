//! `wn update`: the lifecycle state machine (exhaustive + model-based) and the end-to-end flow
//! against a local bare repository with a fake `cargo` (no network).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use proptest::prelude::*;
use proptest::sample::select;
use proptest_state_machine::{prop_state_machine, ReferenceStateMachine, StateMachineTest};
use wn_cli::update::{
    self, UpdateEvent, UpdateLifecycle, UpdateOptions, UpdateState, EXIT_UPDATE_AVAILABLE,
};

use UpdateEvent as E;
use UpdateState as S;

/// The specification, written out independently of the implementation's table.
fn spec() -> HashMap<(UpdateState, UpdateEvent), UpdateState> {
    HashMap::from([
        ((S::Idle, E::Start), S::Fetching),
        ((S::Fetching, E::FetchedSame), S::UpToDate),
        ((S::Fetching, E::FetchedNewer), S::UpdateAvailable),
        ((S::Fetching, E::FetchFailed), S::Failed),
        ((S::UpdateAvailable, E::Build), S::Building),
        ((S::UpToDate, E::Build), S::Building),
        ((S::Building, E::BuildSucceeded), S::Installed),
        ((S::Building, E::BuildFailed), S::Failed),
        ((S::Failed, E::Retry), S::Fetching),
    ])
}

fn machine_in(target: UpdateState) -> UpdateLifecycle {
    let path: &[UpdateEvent] = match target {
        S::Idle => &[],
        S::Fetching => &[E::Start],
        S::UpToDate => &[E::Start, E::FetchedSame],
        S::UpdateAvailable => &[E::Start, E::FetchedNewer],
        S::Building => &[E::Start, E::FetchedNewer, E::Build],
        S::Installed => &[E::Start, E::FetchedNewer, E::Build, E::BuildSucceeded],
        S::Failed => &[E::Start, E::FetchFailed],
    };
    let mut lc = UpdateLifecycle::default();
    for e in path {
        lc.handle(*e).expect("setup path is legal");
    }
    assert_eq!(lc.state(), target);
    lc
}

#[test]
fn every_state_event_pair_matches_the_spec() {
    let spec = spec();
    let mut legal = 0;
    for state in UpdateState::ALL {
        for event in UpdateEvent::ALL {
            let mut lc = machine_in(state);
            match (spec.get(&(state, event)), lc.handle(event)) {
                (Some(expected), Ok(got)) => {
                    assert_eq!(got, *expected, "{state:?} + {event:?}");
                    legal += 1;
                }
                (None, Err(err)) => {
                    assert_eq!((err.state, err.event), (state, event));
                    assert_eq!(lc.state(), state, "illegal event must not change state");
                }
                (expected, got) => {
                    panic!("{state:?} + {event:?}: spec {expected:?}, machine {got:?}")
                }
            }
        }
    }
    assert_eq!(legal, spec.len());
    assert_eq!(UpdateState::ALL.len() * UpdateEvent::ALL.len(), 56);
}

#[derive(Debug, Clone)]
struct Model {
    state: UpdateState,
    last_legal: bool,
    fetched: bool,
    built: bool,
}

struct Reference;

impl ReferenceStateMachine for Reference {
    type State = Model;
    type Transition = UpdateEvent;

    fn init_state() -> BoxedStrategy<Model> {
        Just(Model {
            state: S::Idle,
            last_legal: true,
            fetched: false,
            built: false,
        })
        .boxed()
    }

    fn transitions(_: &Model) -> BoxedStrategy<UpdateEvent> {
        select(UpdateEvent::ALL.to_vec()).boxed()
    }

    fn apply(mut m: Model, e: &UpdateEvent) -> Model {
        match spec().get(&(m.state, *e)) {
            Some(next) => {
                m.last_legal = true;
                m.state = *next;
                match e {
                    E::FetchedSame | E::FetchedNewer => m.fetched = true,
                    E::BuildSucceeded => m.built = true,
                    E::Retry => {
                        m.fetched = false;
                        m.built = false;
                    }
                    _ => {}
                }
            }
            None => m.last_legal = false,
        }
        m
    }
}

struct Sut;

impl StateMachineTest for Sut {
    type SystemUnderTest = UpdateLifecycle;
    type Reference = Reference;

    fn init_test(_: &Model) -> UpdateLifecycle {
        UpdateLifecycle::default()
    }

    fn apply(mut sut: UpdateLifecycle, m: &Model, e: UpdateEvent) -> UpdateLifecycle {
        assert_eq!(sut.handle(e).is_ok(), m.last_legal, "legality of {e:?}");
        assert_eq!(sut.state(), m.state);
        sut
    }

    fn check_invariants(sut: &UpdateLifecycle, m: &Model) {
        // Never report "installed" without having fetched a target and completed a build.
        if sut.state() == S::Installed {
            assert!(m.fetched && m.built, "installed without fetch+build");
        }
        // Building only ever follows a completed fetch.
        if sut.state() == S::Building {
            assert!(m.fetched);
        }
    }
}

prop_state_machine! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn update_lifecycle_matches_reference_model(sequential 1..40 => Sut);
}

// ---- end-to-end flow against a local "upstream" and a fake cargo ----

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

struct Fixture {
    _tmp: tempfile::TempDir,
    upstream: PathBuf,
    src: PathBuf,
    root: PathBuf,
    cargo: PathBuf,
    log: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let upstream = tmp.path().join("upstream");
        fs::create_dir_all(upstream.join("crates/wn-cli")).unwrap();
        git(&upstream, &["init", "-q", "-b", "main"]);
        git(&upstream, &["config", "commit.gpgsign", "false"]);
        fs::write(
            upstream.join("crates/wn-cli/Cargo.toml"),
            "[package]\nname = \"where-next\"\n",
        )
        .unwrap();
        git(&upstream, &["add", "-A"]);
        git(&upstream, &["commit", "-q", "-m", "first"]);
        let log = tmp.path().join("cargo.log");
        let cargo = tmp.path().join("fake-cargo");
        // Records its arguments and "installs" a wn that prints the source commit and logs the
        // arguments it is run with.
        fs::write(
            &cargo,
            format!(
                "#!/bin/sh\necho \"$@\" >> '{log}'\nroot=''\npath=''\nwhile [ $# -gt 0 ]; do case \"$1\" in --root) root=\"$2\"; shift;; --path) path=\"$2\"; shift;; esac; shift; done\nsha=$(git -C \"$path\" rev-parse HEAD)\nmkdir -p \"$root/bin\"\nprintf '#!/bin/sh\\n[ $# -gt 0 ] && echo \"$@\" >> %s/wn.log\\necho wn 0.0.1 %s\\n' \"$root\" \"$sha\" > \"$root/bin/wn\"\nchmod +x \"$root/bin/wn\"\n",
                log = log.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self {
            src: tmp.path().join("home/src"),
            root: tmp.path().join("root"),
            upstream,
            cargo,
            log,
            _tmp: tmp,
        }
    }

    fn head(&self) -> String {
        git(&self.upstream, &["rev-parse", "HEAD"])
    }

    fn commit(&self, msg: &str) -> String {
        fs::write(self.upstream.join(format!("{msg}.txt")), msg).unwrap();
        git(&self.upstream, &["add", "-A"]);
        git(&self.upstream, &["commit", "-q", "-m", msg]);
        self.head()
    }

    fn opts(&self, current: Option<String>) -> UpdateOptions {
        UpdateOptions {
            src: self.src.clone(),
            repo: self.upstream.to_string_lossy().to_string(),
            git_ref: "main".into(),
            check_only: false,
            force: false,
            cargo_root: Some(self.root.clone()),
            cargo: self.cargo.clone(),
            current,
            show_build_output: false,
        }
    }

    fn cargo_calls(&self) -> usize {
        fs::read_to_string(&self.log).map_or(0, |s| s.lines().count())
    }

    /// Commands the installed `wn` was run with (by the post-install steps).
    fn wn_calls(&self) -> Vec<String> {
        fs::read_to_string(self.root.join("wn.log"))
            .map(|s| s.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    fn installed(&self) -> String {
        let out = Command::new(self.root.join("bin/wn")).output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
}

fn yes() -> impl FnMut(&update::UpdateReport) -> bool {
    |_: &update::UpdateReport| true
}

#[cfg(unix)]
#[test]
fn first_run_clones_builds_and_installs() {
    let f = Fixture::new();
    let r = update::run(&f.opts(None), &mut yes());
    assert_eq!(r.state, S::Installed, "{}", r.message);
    assert_eq!(r.exit_code(), 0);
    assert_eq!(r.new.as_deref(), Some(f.head().as_str()));
    assert!(f.src.join(".git").exists());
    assert_eq!(f.installed(), format!("wn 0.0.1 {}", f.head()));
    let log = fs::read_to_string(&f.log).unwrap();
    assert!(log.contains("install --path") && log.contains("--locked") && log.contains("--root"));
    assert_eq!(
        f.wn_calls(),
        ["daemon stop", "skill sync --yes --from-state"],
        "the new binary stops the old daemon and re-syncs skills"
    );
    assert!(r.message.is_empty(), "{}", r.message);
}

#[cfg(unix)]
#[test]
fn up_to_date_does_not_rebuild_and_check_reports_updates() {
    let f = Fixture::new();
    let first = f.head();
    let r = update::run(&f.opts(Some(first.clone())), &mut yes());
    assert_eq!(r.state, S::UpToDate);
    assert_eq!(r.exit_code(), 0);
    assert_eq!(f.cargo_calls(), 0, "no rebuild when up to date");
    assert!(
        f.wn_calls().is_empty(),
        "no post-install steps when up to date"
    );

    let second = f.commit("second");
    let mut check = f.opts(Some(first.clone()));
    check.check_only = true;
    let r = update::run(&check, &mut yes());
    assert_eq!(r.state, S::UpdateAvailable);
    assert_eq!(r.exit_code(), EXIT_UPDATE_AVAILABLE);
    assert_eq!(r.old.as_deref(), Some(first.as_str()));
    assert_eq!(r.new.as_deref(), Some(second.as_str()));
    assert_eq!(f.cargo_calls(), 0, "--check never builds");
    assert!(f.wn_calls().is_empty(), "no post-install steps on --check");

    let r = update::run(&f.opts(Some(first)), &mut yes());
    assert_eq!(r.state, S::Installed, "{}", r.message);
    assert_eq!(f.installed(), format!("wn 0.0.1 {second}"));
    assert!(update::render(&r).contains("->"));
}

#[cfg(unix)]
#[test]
fn force_rebuilds_same_commit_and_ref_selects_a_commit() {
    let f = Fixture::new();
    let first = f.head();
    let _second = f.commit("second");
    let mut forced = f.opts(Some(f.head()));
    forced.force = true;
    assert_eq!(update::run(&forced, &mut yes()).state, S::Installed);
    assert_eq!(f.cargo_calls(), 1);

    let mut pinned = f.opts(None);
    pinned.git_ref = first.clone();
    let r = update::run(&pinned, &mut yes());
    assert_eq!(r.state, S::Installed, "{}", r.message);
    assert_eq!(f.installed(), format!("wn 0.0.1 {first}"));
}

#[cfg(unix)]
#[test]
fn failures_are_reported_not_hidden() {
    let f = Fixture::new();
    let mut bad_ref = f.opts(None);
    bad_ref.git_ref = "no-such-branch".into();
    let r = update::run(&bad_ref, &mut yes());
    assert_eq!(r.state, S::Failed);
    assert_eq!(r.exit_code(), 1);
    assert!(r.message.contains("no-such-branch"), "{}", r.message);

    let mut no_cargo = f.opts(None);
    no_cargo.cargo = PathBuf::from("/nonexistent/cargo");
    let r = update::run(&no_cargo, &mut yes());
    assert_eq!(r.state, S::Failed);
    assert!(r.message.contains("rustup"), "{}", r.message);

    let mut declined = f.opts(None);
    declined.src = f.src.with_file_name("src2");
    let r = update::run(&declined, &mut |_: &update::UpdateReport| false);
    assert_eq!(r.state, S::UpdateAvailable);
    assert_eq!(r.message, "cancelled");
    assert_eq!(f.cargo_calls(), 0);
    assert!(
        f.wn_calls().is_empty(),
        "no post-install steps after a failure"
    );
}

#[test]
fn version_embeds_the_build_commit_when_available() {
    let v = wn_cli::VERSION;
    assert!(v.starts_with(env!("CARGO_PKG_VERSION")));
    if !update::BUILD_COMMIT.is_empty() {
        assert!(v.contains(&update::BUILD_COMMIT[..7]), "{v}");
    }
}

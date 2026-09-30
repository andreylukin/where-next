//! Tests for the per-repository indexer: every (state, event) pair against an independent spec,
//! a model-based test of the lifecycle alone, and a model-based test of several indexers sharing
//! one repository's real lock file. Invariant: at most one indexer per repository is indexing.

use std::collections::HashMap;

use proptest::prelude::*;
use proptest::sample::select;
use proptest_state_machine::{prop_state_machine, ReferenceStateMachine, StateMachineTest};
use wn_daemon::indexer::{Indexer, IndexerEvent, IndexerLifecycle, IndexerState};

use IndexerEvent as E;
use IndexerState as S;

fn spec() -> HashMap<(IndexerState, IndexerEvent), IndexerState> {
    HashMap::from([
        ((S::Idle, E::Acquired), S::Indexing),
        ((S::Idle, E::Busy), S::Waiting),
        ((S::Failed, E::Acquired), S::Indexing),
        ((S::Failed, E::Busy), S::Waiting),
        ((S::Waiting, E::Acquired), S::Indexing),
        ((S::Waiting, E::GaveUp), S::Idle),
        ((S::Indexing, E::Done), S::Idle),
        ((S::Indexing, E::Failed), S::Failed),
    ])
}

fn machine_in(target: IndexerState) -> IndexerLifecycle {
    let path: &[IndexerEvent] = match target {
        S::Idle => &[],
        S::Waiting => &[E::Busy],
        S::Indexing => &[E::Acquired],
        S::Failed => &[E::Acquired, E::Failed],
    };
    let mut lc = IndexerLifecycle::default();
    for event in path {
        lc.handle(*event).expect("setup path is legal");
    }
    assert_eq!(lc.state(), target);
    lc
}

#[test]
fn every_state_event_pair_matches_the_spec() {
    let spec = spec();
    let mut legal = 0;
    for state in IndexerState::ALL {
        for event in IndexerEvent::ALL {
            let mut lc = machine_in(state);
            match (spec.get(&(state, event)), lc.handle(event)) {
                (Some(expected), Ok(got)) => {
                    assert_eq!(got, *expected, "{state:?} + {event:?}");
                    legal += 1;
                }
                (None, Err(err)) => {
                    assert_eq!((err.state, err.event), (state, event));
                    assert_eq!(lc.state(), state);
                }
                (expected, got) => {
                    panic!("{state:?} + {event:?}: spec {expected:?}, machine {got:?}")
                }
            }
        }
    }
    assert_eq!(legal, spec.len());
    assert_eq!(IndexerState::ALL.len() * IndexerEvent::ALL.len(), 20);
}

#[derive(Debug, Clone)]
struct Model {
    state: IndexerState,
    last_legal: bool,
}

struct Reference;

impl ReferenceStateMachine for Reference {
    type State = Model;
    type Transition = IndexerEvent;

    fn init_state() -> BoxedStrategy<Model> {
        Just(Model {
            state: S::Idle,
            last_legal: true,
        })
        .boxed()
    }

    fn transitions(_state: &Model) -> BoxedStrategy<IndexerEvent> {
        select(IndexerEvent::ALL.to_vec()).boxed()
    }

    fn apply(mut model: Model, event: &IndexerEvent) -> Model {
        match spec().get(&(model.state, *event)) {
            Some(next) => {
                model.last_legal = true;
                model.state = *next;
            }
            None => model.last_legal = false,
        }
        model
    }
}

struct Sut;

impl StateMachineTest for Sut {
    type SystemUnderTest = IndexerLifecycle;
    type Reference = Reference;

    fn init_test(_model: &Model) -> IndexerLifecycle {
        IndexerLifecycle::default()
    }

    fn apply(mut sut: IndexerLifecycle, model: &Model, event: IndexerEvent) -> IndexerLifecycle {
        assert_eq!(sut.handle(event).is_ok(), model.last_legal, "{event:?}");
        assert_eq!(sut.state(), model.state);
        sut
    }

    fn check_invariants(sut: &IndexerLifecycle, _model: &Model) {
        assert_eq!(sut.state().holds_lock(), sut.state() == S::Indexing);
    }
}

prop_state_machine! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn indexer_lifecycle_matches_reference_model(sequential 1..48 => Sut);
}

// --- Several indexers, one repository, the real lock file ----------------------------------------

const INDEXERS: usize = 3;

/// What one indexer does next.
#[derive(Debug, Clone, Copy)]
enum Step {
    /// Try to start without waiting.
    TryBegin(usize),
    /// Stop waiting.
    GiveUp(usize),
    /// Finish (ok or failed).
    Finish(usize, bool),
}

#[derive(Debug, Clone)]
struct Shared {
    /// Which indexer holds the lock, per the model.
    holder: Option<usize>,
    states: [IndexerState; INDEXERS],
}

struct SharedReference;

impl ReferenceStateMachine for SharedReference {
    type State = Shared;
    type Transition = Step;

    fn init_state() -> BoxedStrategy<Shared> {
        Just(Shared {
            holder: None,
            states: [S::Idle; INDEXERS],
        })
        .boxed()
    }

    fn transitions(_state: &Shared) -> BoxedStrategy<Step> {
        let id = 0..INDEXERS;
        prop_oneof![
            id.clone().prop_map(Step::TryBegin),
            id.clone().prop_map(Step::GiveUp),
            (id, any::<bool>()).prop_map(|(i, ok)| Step::Finish(i, ok)),
        ]
        .boxed()
    }

    fn apply(mut m: Shared, step: &Step) -> Shared {
        match *step {
            Step::TryBegin(i) => match m.holder {
                None => {
                    m.holder = Some(i);
                    m.states[i] = S::Indexing;
                }
                Some(h) if h == i => {}
                Some(_) => m.states[i] = S::Waiting,
            },
            Step::GiveUp(i) => {
                if m.states[i] == S::Waiting {
                    m.states[i] = S::Idle;
                }
            }
            Step::Finish(i, ok) => {
                if m.states[i] == S::Indexing {
                    m.holder = None;
                    m.states[i] = if ok { S::Idle } else { S::Failed };
                }
            }
        }
        m
    }
}

struct SharedSut {
    _dir: tempfile::TempDir,
    indexers: Vec<Indexer>,
}

struct SharedTest;

impl StateMachineTest for SharedTest {
    type SystemUnderTest = SharedSut;
    type Reference = SharedReference;

    fn init_test(_model: &Shared) -> SharedSut {
        let dir = tempfile::tempdir().unwrap();
        let indexers = (0..INDEXERS).map(|_| Indexer::new(dir.path())).collect();
        SharedSut {
            _dir: dir,
            indexers,
        }
    }

    fn apply(mut sut: SharedSut, model: &Shared, step: Step) -> SharedSut {
        match step {
            Step::TryBegin(i) => {
                let got = sut.indexers[i].try_begin().unwrap();
                assert_eq!(got, model.holder == Some(i), "{step:?}");
            }
            Step::GiveUp(i) => sut.indexers[i].give_up(),
            Step::Finish(i, ok) => sut.indexers[i].finish(ok),
        }
        for (i, ix) in sut.indexers.iter().enumerate() {
            assert_eq!(ix.state(), model.states[i], "indexer {i} after {step:?}");
        }
        sut
    }

    fn check_invariants(sut: &SharedSut, _model: &Shared) {
        let indexing = sut
            .indexers
            .iter()
            .filter(|ix| ix.state() == S::Indexing)
            .count();
        assert!(indexing <= 1, "{indexing} indexers at once");
    }
}

prop_state_machine! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn one_repository_never_has_two_indexers(sequential 1..40 => SharedTest);
}

#[test]
fn a_blocking_indexer_waits_for_the_holder_then_runs() {
    let dir = tempfile::tempdir().unwrap();
    let mut first = Indexer::new(dir.path());
    assert!(first.try_begin().unwrap());
    let path = dir.path().to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let mut second = Indexer::new(&path);
        let waited = second.begin(|| tx.send("waiting").unwrap()).expect("lock");
        assert_eq!(second.state(), S::Indexing);
        second.finish(true);
        waited
    });
    assert_eq!(
        rx.recv_timeout(std::time::Duration::from_secs(5)),
        Ok("waiting")
    );
    std::thread::sleep(std::time::Duration::from_millis(50));
    first.finish(true);
    assert!(waiter.join().unwrap(), "the second indexer had to wait");
}

#[cfg(unix)]
#[test]
fn an_unwritable_cache_directory_is_an_error_not_a_silent_skip() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let mut ix = Indexer::new(&dir.path().join("repo-abc"));
    let result = ix.try_begin();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(result.is_err(), "{result:?}");
    assert_eq!(ix.state(), S::Idle);
}

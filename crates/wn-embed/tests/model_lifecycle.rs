//! Tests for the model lifecycle state machine.
//!
//! 1. Every (state, event) pair checked against a specification written here independently.
//! 2. A model-based property test with the invariant that the model only embeds after a
//!    successful checksum since the last fetch, local discovery or on-disk change.

use std::collections::HashMap;

use proptest::prelude::*;
use proptest::sample::select;
use proptest_state_machine::{prop_state_machine, ReferenceStateMachine, StateMachineTest};
use wn_embed::lifecycle::{ModelEvent, ModelLifecycle, ModelState};

use ModelEvent as E;
use ModelState as S;

fn spec() -> HashMap<(ModelState, ModelEvent), ModelState> {
    HashMap::from([
        ((S::Missing, E::Fetch), S::Downloading),
        ((S::Missing, E::LocalFound), S::Verifying),
        ((S::Downloading, E::DownloadDone), S::Verifying),
        ((S::Downloading, E::DownloadFailed), S::Missing),
        ((S::Verifying, E::ChecksumOk), S::Loaded),
        ((S::Verifying, E::ChecksumMismatch), S::Corrupt),
        ((S::Loaded, E::FilesChanged), S::Verifying),
        ((S::Loaded, E::Evict), S::Missing),
        ((S::Corrupt, E::Fetch), S::Downloading),
        ((S::Corrupt, E::Reset), S::Missing),
    ])
}

fn machine_in(target: ModelState) -> ModelLifecycle {
    let path: &[ModelEvent] = match target {
        S::Missing => &[],
        S::Downloading => &[E::Fetch],
        S::Verifying => &[E::LocalFound],
        S::Loaded => &[E::LocalFound, E::ChecksumOk],
        S::Corrupt => &[E::LocalFound, E::ChecksumMismatch],
    };
    let mut lc = ModelLifecycle::default();
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
    for state in ModelState::ALL {
        for event in ModelEvent::ALL {
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
    assert_eq!(ModelState::ALL.len() * ModelEvent::ALL.len(), 45);
}

#[test]
fn only_loaded_can_embed() {
    let embedding: Vec<_> = ModelState::ALL
        .into_iter()
        .filter(|s| s.can_embed())
        .collect();
    assert_eq!(embedding, vec![S::Loaded]);
}

#[derive(Debug, Clone)]
struct Model {
    state: ModelState,
    last_legal: bool,
    verified_since_change: bool,
}

struct Reference;

impl ReferenceStateMachine for Reference {
    type State = Model;
    type Transition = ModelEvent;

    fn init_state() -> BoxedStrategy<Self::State> {
        Just(Model {
            state: S::Missing,
            last_legal: true,
            verified_since_change: false,
        })
        .boxed()
    }

    fn transitions(_state: &Self::State) -> BoxedStrategy<Self::Transition> {
        select(ModelEvent::ALL.to_vec()).boxed()
    }

    fn apply(mut model: Self::State, event: &Self::Transition) -> Self::State {
        match spec().get(&(model.state, *event)) {
            Some(next) => {
                model.last_legal = true;
                model.state = *next;
                model.verified_since_change = match event {
                    E::ChecksumOk => true,
                    E::Fetch | E::LocalFound | E::DownloadDone | E::FilesChanged => false,
                    _ => model.verified_since_change && model.state == S::Loaded,
                };
            }
            None => model.last_legal = false,
        }
        model
    }
}

struct Sut;

impl StateMachineTest for Sut {
    type SystemUnderTest = ModelLifecycle;
    type Reference = Reference;

    fn init_test(_model: &Model) -> Self::SystemUnderTest {
        ModelLifecycle::default()
    }

    fn apply(mut sut: ModelLifecycle, model: &Model, event: ModelEvent) -> ModelLifecycle {
        assert_eq!(sut.handle(event).is_ok(), model.last_legal, "{event:?}");
        assert_eq!(sut.state(), model.state);
        sut
    }

    fn check_invariants(sut: &ModelLifecycle, model: &Model) {
        if sut.state().can_embed() {
            assert!(
                model.verified_since_change,
                "embedding from unverified files"
            );
        }
    }
}

prop_state_machine! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn model_lifecycle_matches_reference_model(sequential 1..64 => Sut);
}

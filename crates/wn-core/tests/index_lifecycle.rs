//! Tests for the index lifecycle state machine.
//!
//! 1. An exhaustive check of every (state, event) pair against a specification written out
//!    here independently of the implementation's table.
//! 2. A model-based property test: random event sequences run against a reference model and
//!    the real machine, with invariants checked after every step.

use std::collections::HashMap;

use proptest::prelude::*;
use proptest::sample::select;
use proptest_state_machine::{prop_state_machine, ReferenceStateMachine, StateMachineTest};
use wn_core::index_lifecycle::{IndexEvent, IndexLifecycle, IndexState};

use IndexEvent as E;
use IndexState as S;

/// The specification: every legal transition. Anything else must be rejected.
fn spec() -> HashMap<(IndexState, IndexEvent), IndexState> {
    HashMap::from([
        ((S::Uninitialized, E::Init), S::Indexing),
        ((S::Uninitialized, E::ModelChanged), S::Uninitialized),
        ((S::Indexing, E::IndexDone), S::Ready),
        ((S::Indexing, E::IndexFailed), S::Error),
        ((S::Indexing, E::FilesChanged), S::Indexing),
        ((S::Indexing, E::ModelChanged), S::Uninitialized),
        ((S::Ready, E::FilesChanged), S::Stale),
        ((S::Ready, E::ModelChanged), S::Uninitialized),
        ((S::Stale, E::FilesChanged), S::Stale),
        ((S::Stale, E::Refresh), S::Refreshing),
        ((S::Stale, E::ModelChanged), S::Uninitialized),
        ((S::Refreshing, E::RefreshDone), S::Ready),
        ((S::Refreshing, E::RefreshFailed), S::Error),
        ((S::Refreshing, E::FilesChanged), S::Refreshing),
        ((S::Refreshing, E::ModelChanged), S::Uninitialized),
        ((S::Error, E::Init), S::Indexing),
        ((S::Error, E::Reset), S::Uninitialized),
        ((S::Error, E::ModelChanged), S::Uninitialized),
    ])
}

/// Drives a fresh machine into `target` along legal transitions.
fn machine_in(target: IndexState) -> IndexLifecycle {
    let path: &[IndexEvent] = match target {
        S::Uninitialized => &[],
        S::Indexing => &[E::Init],
        S::Ready => &[E::Init, E::IndexDone],
        S::Stale => &[E::Init, E::IndexDone, E::FilesChanged],
        S::Refreshing => &[E::Init, E::IndexDone, E::FilesChanged, E::Refresh],
        S::Error => &[E::Init, E::IndexFailed],
    };
    let mut lc = IndexLifecycle::default();
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
    for state in IndexState::ALL {
        for event in IndexEvent::ALL {
            let mut lc = machine_in(state);
            match (spec.get(&(state, event)), lc.handle(event)) {
                (Some(expected), Ok(got)) => {
                    assert_eq!(got, *expected, "{state:?} + {event:?}");
                    assert_eq!(lc.state(), *expected);
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
    assert_eq!(legal, spec.len(), "every spec transition was exercised");
    assert_eq!(IndexState::ALL.len() * IndexEvent::ALL.len(), 54);
}

#[test]
fn every_state_is_reachable() {
    for state in IndexState::ALL {
        machine_in(state);
    }
}

#[test]
fn serving_states_are_exactly_ready_stale_refreshing() {
    let serving: Vec<_> = IndexState::ALL
        .into_iter()
        .filter(|s| s.can_serve())
        .collect();
    assert_eq!(serving, vec![S::Ready, S::Stale, S::Refreshing]);
}

/// Reference model: the spec plus a flag recording whether a completed build exists.
#[derive(Debug, Clone)]
struct Model {
    state: IndexState,
    last_legal: bool,
    has_built_index: bool,
}

struct Reference;

impl ReferenceStateMachine for Reference {
    type State = Model;
    type Transition = IndexEvent;

    fn init_state() -> BoxedStrategy<Self::State> {
        Just(Model {
            state: S::Uninitialized,
            last_legal: true,
            has_built_index: false,
        })
        .boxed()
    }

    fn transitions(_state: &Self::State) -> BoxedStrategy<Self::Transition> {
        // Illegal events are generated too: rejecting them is part of the contract.
        select(IndexEvent::ALL.to_vec()).boxed()
    }

    fn apply(mut model: Self::State, event: &Self::Transition) -> Self::State {
        match spec().get(&(model.state, *event)) {
            Some(next) => {
                model.last_legal = true;
                model.state = *next;
                if matches!(event, E::IndexDone) {
                    model.has_built_index = true;
                }
                if model.state == S::Uninitialized {
                    model.has_built_index = false;
                }
            }
            None => model.last_legal = false,
        }
        model
    }
}

struct Sut;

impl StateMachineTest for Sut {
    type SystemUnderTest = IndexLifecycle;
    type Reference = Reference;

    fn init_test(_model: &Model) -> Self::SystemUnderTest {
        IndexLifecycle::default()
    }

    fn apply(
        mut sut: Self::SystemUnderTest,
        model: &Model,
        event: IndexEvent,
    ) -> Self::SystemUnderTest {
        let result = sut.handle(event);
        assert_eq!(result.is_ok(), model.last_legal, "legality of {event:?}");
        assert_eq!(sut.state(), model.state);
        sut
    }

    fn check_invariants(sut: &Self::SystemUnderTest, model: &Model) {
        // Never answer a query without a completed build since the last reset or model change.
        if sut.state().can_serve() {
            assert!(
                model.has_built_index,
                "serving from {:?} without a built index",
                sut.state()
            );
        }
    }
}

prop_state_machine! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn index_lifecycle_matches_reference_model(sequential 1..64 => Sut);
}

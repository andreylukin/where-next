//! Tests for the daemon process lifecycle: every (state, event) pair against an independent spec,
//! and a model-based property test. Invariants: requests are accepted only while listening, and
//! nothing leaves a terminal state.

use std::collections::HashMap;

use proptest::prelude::*;
use proptest::sample::select;
use proptest_state_machine::{prop_state_machine, ReferenceStateMachine, StateMachineTest};
use wn_daemon::resident::{ResidentEvent, ResidentLifecycle, ResidentState};

use ResidentEvent as E;
use ResidentState as S;

fn spec() -> HashMap<(ResidentState, ResidentEvent), ResidentState> {
    HashMap::from([
        ((S::Starting, E::Bound), S::Listening),
        ((S::Starting, E::BindFailed), S::Failed),
        ((S::Starting, E::Shutdown), S::Stopped),
        ((S::Listening, E::Request), S::Listening),
        ((S::Listening, E::IdleTimeout), S::Draining),
        ((S::Listening, E::Shutdown), S::Draining),
        ((S::Draining, E::Drained), S::Stopped),
    ])
}

fn machine_in(target: ResidentState) -> ResidentLifecycle {
    let path: &[ResidentEvent] = match target {
        S::Starting => &[],
        S::Listening => &[E::Bound],
        S::Draining => &[E::Bound, E::IdleTimeout],
        S::Stopped => &[E::Bound, E::Shutdown, E::Drained],
        S::Failed => &[E::BindFailed],
    };
    let mut lc = ResidentLifecycle::default();
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
    for state in ResidentState::ALL {
        for event in ResidentEvent::ALL {
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
    assert_eq!(ResidentState::ALL.len() * ResidentEvent::ALL.len(), 30);
}

#[test]
fn draining_rejects_new_requests() {
    let mut lc = machine_in(S::Draining);
    assert!(lc.handle(E::Request).is_err());
    assert!(!lc.state().accepts());
}

#[derive(Debug, Clone)]
struct Model {
    state: ResidentState,
    last_legal: bool,
    terminal_seen: Option<ResidentState>,
}

struct Reference;

impl ReferenceStateMachine for Reference {
    type State = Model;
    type Transition = ResidentEvent;

    fn init_state() -> BoxedStrategy<Model> {
        Just(Model {
            state: S::Starting,
            last_legal: true,
            terminal_seen: None,
        })
        .boxed()
    }

    fn transitions(_state: &Model) -> BoxedStrategy<ResidentEvent> {
        select(ResidentEvent::ALL.to_vec()).boxed()
    }

    fn apply(mut model: Model, event: &ResidentEvent) -> Model {
        match spec().get(&(model.state, *event)) {
            Some(next) => {
                model.last_legal = true;
                model.state = *next;
                if next.is_terminal() && model.terminal_seen.is_none() {
                    model.terminal_seen = Some(*next);
                }
            }
            None => model.last_legal = false,
        }
        model
    }
}

struct Sut;

impl StateMachineTest for Sut {
    type SystemUnderTest = ResidentLifecycle;
    type Reference = Reference;

    fn init_test(_model: &Model) -> ResidentLifecycle {
        ResidentLifecycle::default()
    }

    fn apply(mut sut: ResidentLifecycle, model: &Model, event: ResidentEvent) -> ResidentLifecycle {
        assert_eq!(sut.handle(event).is_ok(), model.last_legal, "{event:?}");
        assert_eq!(sut.state(), model.state);
        sut
    }

    fn check_invariants(sut: &ResidentLifecycle, model: &Model) {
        if sut.state().accepts() {
            assert_eq!(sut.state(), S::Listening);
        }
        if let Some(terminal) = model.terminal_seen {
            assert_eq!(sut.state(), terminal, "left a terminal state");
        }
    }
}

prop_state_machine! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn resident_lifecycle_matches_reference_model(sequential 1..48 => Sut);
}

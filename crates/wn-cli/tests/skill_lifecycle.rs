//! Tests for the `wn skill sync` flow: every (state, event) pair against an independent spec, and
//! a model-based property test. Invariants: files are only written from `Applying`, and nothing
//! leaves a terminal state.

use std::collections::HashMap;

use proptest::prelude::*;
use proptest::sample::select;
use proptest_state_machine::{prop_state_machine, ReferenceStateMachine, StateMachineTest};
use wn_cli::skill::{SyncEvent, SyncLifecycle, SyncState};

use SyncEvent as E;
use SyncState as S;

fn spec() -> HashMap<(SyncState, SyncEvent), SyncState> {
    HashMap::from([
        ((S::Planning, E::Planned), S::Reviewing),
        ((S::Planning, E::NothingToDo), S::Done),
        ((S::Planning, E::Error), S::Failed),
        ((S::Reviewing, E::Preview), S::Previewed),
        ((S::Reviewing, E::Confirm), S::Applying),
        ((S::Reviewing, E::Decline), S::Declined),
        ((S::Applying, E::Applied), S::Done),
        ((S::Applying, E::Error), S::Failed),
    ])
}

fn machine_in(target: SyncState) -> SyncLifecycle {
    let path: &[SyncEvent] = match target {
        S::Planning => &[],
        S::Reviewing => &[E::Planned],
        S::Applying => &[E::Planned, E::Confirm],
        S::Done => &[E::NothingToDo],
        S::Previewed => &[E::Planned, E::Preview],
        S::Declined => &[E::Planned, E::Decline],
        S::Failed => &[E::Error],
    };
    let mut m = SyncLifecycle::default();
    for event in path {
        m.handle(*event).expect("setup path is legal");
    }
    assert_eq!(m.state(), target);
    m
}

#[test]
fn every_state_event_pair_matches_the_spec() {
    let spec = spec();
    let mut legal = 0;
    for state in SyncState::ALL {
        for event in SyncEvent::ALL {
            let mut m = machine_in(state);
            match (spec.get(&(state, event)), m.handle(event)) {
                (Some(expected), Ok(got)) => {
                    assert_eq!(got, *expected, "{state:?} + {event:?}");
                    legal += 1;
                }
                (None, Err(err)) => {
                    assert_eq!((err.state, err.event), (state, event));
                    assert_eq!(m.state(), state);
                }
                (expected, got) => {
                    panic!("{state:?} + {event:?}: spec {expected:?}, machine {got:?}")
                }
            }
        }
    }
    assert_eq!(legal, spec.len());
    assert_eq!(SyncState::ALL.len() * SyncEvent::ALL.len(), 49);
}

#[test]
fn terminal_states_accept_nothing() {
    for state in SyncState::ALL.into_iter().filter(|s| s.is_terminal()) {
        for event in SyncEvent::ALL {
            assert!(
                machine_in(state).handle(event).is_err(),
                "{state:?} + {event:?}"
            );
        }
    }
}

#[derive(Debug, Clone)]
struct Model {
    state: SyncState,
    last_legal: bool,
    applied_from: Option<SyncState>,
    terminal_seen: Option<SyncState>,
}

struct Reference;

impl ReferenceStateMachine for Reference {
    type State = Model;
    type Transition = SyncEvent;

    fn init_state() -> BoxedStrategy<Model> {
        Just(Model {
            state: S::Planning,
            last_legal: true,
            applied_from: None,
            terminal_seen: None,
        })
        .boxed()
    }

    fn transitions(_state: &Model) -> BoxedStrategy<SyncEvent> {
        select(SyncEvent::ALL.to_vec()).boxed()
    }

    fn apply(mut model: Model, event: &SyncEvent) -> Model {
        match spec().get(&(model.state, *event)) {
            Some(next) => {
                if *event == E::Applied {
                    model.applied_from = Some(model.state);
                }
                model.last_legal = true;
                model.state = *next;
                if next.is_terminal() {
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
    type SystemUnderTest = SyncLifecycle;
    type Reference = Reference;

    fn init_test(_model: &Model) -> SyncLifecycle {
        SyncLifecycle::default()
    }

    fn apply(mut sut: SyncLifecycle, model: &Model, event: SyncEvent) -> SyncLifecycle {
        assert_eq!(sut.handle(event).is_ok(), model.last_legal, "{event:?}");
        assert_eq!(sut.state(), model.state);
        sut
    }

    fn check_invariants(sut: &SyncLifecycle, model: &Model) {
        if let Some(from) = model.applied_from {
            assert_eq!(from, S::Applying, "applied outside Applying");
        }
        if let Some(t) = model.terminal_seen {
            assert_eq!(sut.state(), t, "left a terminal state");
        }
    }
}

prop_state_machine! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn sync_matches_reference_model(sequential 1..32 => Sut);
}

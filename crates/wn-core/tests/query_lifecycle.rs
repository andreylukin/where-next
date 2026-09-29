//! Tests for the per-query state machine: every (state × event) pair, plus a model-based test.

use std::collections::HashMap;

use proptest::prelude::*;
use proptest::sample::select;
use proptest_state_machine::{prop_state_machine, ReferenceStateMachine, StateMachineTest};
use wn_core::query_lifecycle::{QueryEvent, QueryLifecycle, QueryState};

use QueryEvent as E;
use QueryState as S;

fn spec() -> HashMap<(QueryState, QueryEvent), QueryState> {
    HashMap::from([
        ((S::Received, E::Ranked), S::Ranked),
        ((S::Received, E::Unavailable), S::FailOpen),
        ((S::Ranked, E::Confident), S::Answer),
        ((S::Ranked, E::NotConfident), S::Abstain),
        ((S::Ranked, E::Unavailable), S::FailOpen),
    ])
}

fn machine_in(target: QueryState) -> QueryLifecycle {
    let path: &[QueryEvent] = match target {
        S::Received => &[],
        S::Ranked => &[E::Ranked],
        S::Answer => &[E::Ranked, E::Confident],
        S::Abstain => &[E::Ranked, E::NotConfident],
        S::FailOpen => &[E::Unavailable],
    };
    let mut m = QueryLifecycle::default();
    for e in path {
        m.handle(*e).expect("setup path is legal");
    }
    assert_eq!(m.state(), target);
    m
}

#[test]
fn every_state_event_pair_matches_the_spec() {
    let spec = spec();
    for state in QueryState::ALL {
        for event in QueryEvent::ALL {
            let mut m = machine_in(state);
            match (spec.get(&(state, event)), m.handle(event)) {
                (Some(want), Ok(got)) => assert_eq!(got, *want, "{state:?} + {event:?}"),
                (None, Err(err)) => {
                    assert_eq!((err.state, err.event), (state, event));
                    assert_eq!(m.state(), state);
                }
                (want, got) => panic!("{state:?} + {event:?}: spec {want:?}, machine {got:?}"),
            }
        }
    }
}

#[test]
fn terminal_states_accept_nothing_and_only_answer_shows_hints() {
    for s in QueryState::ALL {
        let terminal = matches!(s, S::Answer | S::Abstain | S::FailOpen);
        assert_eq!(s.is_terminal(), terminal, "{s:?}");
        assert_eq!(s.shows_hints(), s == S::Answer, "{s:?}");
    }
}

#[derive(Debug, Clone)]
struct Model {
    state: QueryState,
    last_legal: bool,
    ranked: bool,
}

struct Reference;

impl ReferenceStateMachine for Reference {
    type State = Model;
    type Transition = QueryEvent;

    fn init_state() -> BoxedStrategy<Model> {
        Just(Model {
            state: S::Received,
            last_legal: true,
            ranked: false,
        })
        .boxed()
    }

    fn transitions(_: &Model) -> BoxedStrategy<QueryEvent> {
        select(QueryEvent::ALL.to_vec()).boxed()
    }

    fn apply(mut m: Model, e: &QueryEvent) -> Model {
        match spec().get(&(m.state, *e)) {
            Some(next) => {
                m.last_legal = true;
                if *e == E::Ranked {
                    m.ranked = true;
                }
                m.state = *next;
            }
            None => m.last_legal = false,
        }
        m
    }
}

struct Sut;

impl StateMachineTest for Sut {
    type SystemUnderTest = QueryLifecycle;
    type Reference = Reference;

    fn init_test(_: &Model) -> QueryLifecycle {
        QueryLifecycle::default()
    }

    fn apply(mut sut: QueryLifecycle, m: &Model, e: QueryEvent) -> QueryLifecycle {
        assert_eq!(sut.handle(e).is_ok(), m.last_legal);
        assert_eq!(sut.state(), m.state);
        sut
    }

    fn check_invariants(sut: &QueryLifecycle, m: &Model) {
        // Hints are only ever shown for a query that was actually ranked.
        if sut.state().shows_hints() {
            assert!(m.ranked);
        }
    }
}

prop_state_machine! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn query_lifecycle_matches_reference_model(sequential 1..16 => Sut);
}

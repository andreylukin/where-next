//! Tests for the adapter lifecycle state machine: an exhaustive (state × event) check against an
//! independently written spec, and a model-based property test with invariants.

use std::collections::HashMap;

use proptest::prelude::*;
use proptest::sample::select;
use proptest_state_machine::{prop_state_machine, ReferenceStateMachine, StateMachineTest};
use wn_core::adapter_lifecycle::{AdapterEvent, AdapterLifecycle, AdapterState};

use AdapterEvent as E;
use AdapterState as S;

fn spec() -> HashMap<(AdapterState, AdapterEvent), AdapterState> {
    HashMap::from([
        ((S::None, E::Fit), S::Fitting),
        ((S::None, E::ModelChanged), S::None),
        ((S::Fitting, E::FitDone), S::Active),
        ((S::Fitting, E::FitFailed), S::None),
        ((S::Fitting, E::ModelChanged), S::None),
        ((S::Active, E::Fit), S::Refitting),
        ((S::Active, E::ModelChanged), S::Invalidated),
        ((S::Refitting, E::FitDone), S::Active),
        ((S::Refitting, E::FitFailed), S::Active),
        ((S::Refitting, E::ModelChanged), S::Invalidated),
        ((S::Invalidated, E::Fit), S::Fitting),
        ((S::Invalidated, E::ModelChanged), S::Invalidated),
        ((S::Invalidated, E::Discard), S::None),
        ((S::Active, E::Discard), S::None),
    ])
}

fn machine_in(target: AdapterState) -> AdapterLifecycle {
    let path: &[AdapterEvent] = match target {
        S::None => &[],
        S::Fitting => &[E::Fit],
        S::Active => &[E::Fit, E::FitDone],
        S::Refitting => &[E::Fit, E::FitDone, E::Fit],
        S::Invalidated => &[E::Fit, E::FitDone, E::ModelChanged],
    };
    let mut m = AdapterLifecycle::default();
    for e in path {
        m.handle(*e).expect("setup path is legal");
    }
    assert_eq!(m.state(), target);
    m
}

#[test]
fn every_state_event_pair_matches_the_spec() {
    let spec = spec();
    let mut legal = 0;
    for state in AdapterState::ALL {
        for event in AdapterEvent::ALL {
            let mut m = machine_in(state);
            match (spec.get(&(state, event)), m.handle(event)) {
                (Some(want), Ok(got)) => {
                    assert_eq!(got, *want, "{state:?} + {event:?}");
                    legal += 1;
                }
                (None, Err(err)) => {
                    assert_eq!((err.state, err.event), (state, event));
                    assert_eq!(m.state(), state, "illegal event must not change state");
                }
                (want, got) => panic!("{state:?} + {event:?}: spec {want:?}, machine {got:?}"),
            }
        }
    }
    assert_eq!(legal, spec.len());
}

#[test]
fn adapter_applies_exactly_in_active_and_refitting() {
    let applying: Vec<_> = AdapterState::ALL
        .into_iter()
        .filter(|s| s.applies())
        .collect();
    assert_eq!(applying, vec![S::Active, S::Refitting]);
}

/// Reference model: the spec, plus which model the current weights were fitted against.
#[derive(Debug, Clone)]
struct Model {
    state: AdapterState,
    last_legal: bool,
    fitted_for_current_model: bool,
}

struct Reference;

impl ReferenceStateMachine for Reference {
    type State = Model;
    type Transition = AdapterEvent;

    fn init_state() -> BoxedStrategy<Model> {
        Just(Model {
            state: S::None,
            last_legal: true,
            fitted_for_current_model: false,
        })
        .boxed()
    }

    fn transitions(_: &Model) -> BoxedStrategy<AdapterEvent> {
        select(AdapterEvent::ALL.to_vec()).boxed()
    }

    fn apply(mut m: Model, e: &AdapterEvent) -> Model {
        match spec().get(&(m.state, *e)) {
            Some(next) => {
                m.last_legal = true;
                match e {
                    E::FitDone => m.fitted_for_current_model = true,
                    E::ModelChanged | E::Discard => m.fitted_for_current_model = false,
                    _ => {}
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
    type SystemUnderTest = AdapterLifecycle;
    type Reference = Reference;

    fn init_test(_: &Model) -> AdapterLifecycle {
        AdapterLifecycle::default()
    }

    fn apply(mut sut: AdapterLifecycle, m: &Model, e: AdapterEvent) -> AdapterLifecycle {
        assert_eq!(sut.handle(e).is_ok(), m.last_legal, "legality of {e:?}");
        assert_eq!(sut.state(), m.state);
        sut
    }

    fn check_invariants(sut: &AdapterLifecycle, m: &Model) {
        // Never map a query through weights fitted against a different embedding model.
        if sut.state().applies() {
            assert!(
                m.fitted_for_current_model,
                "applying stale weights in {:?}",
                sut.state()
            );
        }
    }
}

prop_state_machine! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn adapter_lifecycle_matches_reference_model(sequential 1..64 => Sut);
}

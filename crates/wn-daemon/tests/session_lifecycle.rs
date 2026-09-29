//! Tests for the session lifecycle: every (state, event) pair against an independent spec, and a
//! model-based property test. Invariants: answers only after a successful warm-up since the last
//! failure, and nothing leaves `Stopped`.

use std::collections::HashMap;

use proptest::prelude::*;
use proptest::sample::select;
use proptest_state_machine::{prop_state_machine, ReferenceStateMachine, StateMachineTest};
use wn_daemon::session::{SessionEvent, SessionLifecycle, SessionState};

use SessionEvent as E;
use SessionState as S;

fn spec() -> HashMap<(SessionState, SessionEvent), SessionState> {
    HashMap::from([
        ((S::Starting, E::Warm), S::Warming),
        ((S::Starting, E::Shutdown), S::ShuttingDown),
        ((S::Warming, E::WarmDone), S::Serving),
        ((S::Warming, E::WarmFailed), S::Degraded),
        ((S::Warming, E::Shutdown), S::ShuttingDown),
        ((S::Serving, E::Failure), S::Degraded),
        ((S::Serving, E::Shutdown), S::ShuttingDown),
        ((S::Degraded, E::Recovered), S::Serving),
        ((S::Degraded, E::Warm), S::Warming),
        ((S::Degraded, E::Shutdown), S::ShuttingDown),
        ((S::ShuttingDown, E::Closed), S::Stopped),
    ])
}

fn machine_in(target: SessionState) -> SessionLifecycle {
    let path: &[SessionEvent] = match target {
        S::Starting => &[],
        S::Warming => &[E::Warm],
        S::Serving => &[E::Warm, E::WarmDone],
        S::Degraded => &[E::Warm, E::WarmFailed],
        S::ShuttingDown => &[E::Shutdown],
        S::Stopped => &[E::Shutdown, E::Closed],
    };
    let mut lc = SessionLifecycle::default();
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
    for state in SessionState::ALL {
        for event in SessionEvent::ALL {
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
    assert_eq!(SessionState::ALL.len() * SessionEvent::ALL.len(), 42);
}

#[test]
fn shutdown_is_reachable_from_every_live_state() {
    for state in [S::Starting, S::Warming, S::Serving, S::Degraded] {
        let mut lc = machine_in(state);
        assert_eq!(lc.handle(E::Shutdown).unwrap(), S::ShuttingDown);
    }
}

#[derive(Debug, Clone)]
struct Model {
    state: SessionState,
    last_legal: bool,
    warmed_since_failure: bool,
    ever_stopped: bool,
}

struct Reference;

impl ReferenceStateMachine for Reference {
    type State = Model;
    type Transition = SessionEvent;

    fn init_state() -> BoxedStrategy<Model> {
        Just(Model {
            state: S::Starting,
            last_legal: true,
            warmed_since_failure: false,
            ever_stopped: false,
        })
        .boxed()
    }

    fn transitions(_state: &Model) -> BoxedStrategy<SessionEvent> {
        select(SessionEvent::ALL.to_vec()).boxed()
    }

    fn apply(mut model: Model, event: &SessionEvent) -> Model {
        match spec().get(&(model.state, *event)) {
            Some(next) => {
                model.last_legal = true;
                model.state = *next;
                match event {
                    E::WarmDone | E::Recovered => model.warmed_since_failure = true,
                    E::Failure | E::WarmFailed | E::Warm => model.warmed_since_failure = false,
                    _ => {}
                }
                model.ever_stopped |= model.state == S::Stopped;
            }
            None => model.last_legal = false,
        }
        model
    }
}

struct Sut;

impl StateMachineTest for Sut {
    type SystemUnderTest = SessionLifecycle;
    type Reference = Reference;

    fn init_test(_model: &Model) -> SessionLifecycle {
        SessionLifecycle::default()
    }

    fn apply(mut sut: SessionLifecycle, model: &Model, event: SessionEvent) -> SessionLifecycle {
        assert_eq!(sut.handle(event).is_ok(), model.last_legal, "{event:?}");
        assert_eq!(sut.state(), model.state);
        sut
    }

    fn check_invariants(sut: &SessionLifecycle, model: &Model) {
        if sut.state().can_answer() {
            assert!(
                model.warmed_since_failure,
                "answering without a good warm-up"
            );
        }
        if model.ever_stopped {
            assert_eq!(sut.state(), S::Stopped, "a stopped session came back");
        }
    }
}

prop_state_machine! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn session_lifecycle_matches_reference_model(sequential 1..64 => Sut);
}

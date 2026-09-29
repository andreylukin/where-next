//! Tests for a command's connection to the daemon: every (state, event) pair against an
//! independent spec, and a model-based property test. Invariants: at most two spawns (one fresh,
//! one replacement), `Ready` only right after an accepted handshake, never `Ready` after two
//! version mismatches, and nothing leaves a terminal state except `Ready --Broken--> InProcess`.

use std::collections::HashMap;

use proptest::prelude::*;
use proptest::sample::select;
use proptest_state_machine::{prop_state_machine, ReferenceStateMachine, StateMachineTest};
use wn_daemon::connect::{ConnectEvent, ConnectState, Connection};

use ConnectEvent as E;
use ConnectState as S;

fn spec() -> HashMap<(ConnectState, ConnectEvent), ConnectState> {
    HashMap::from([
        ((S::Idle, E::Connect), S::Connecting),
        ((S::Idle, E::Disabled), S::InProcess),
        ((S::Connecting, E::Connected), S::Handshaking),
        ((S::Connecting, E::Refused), S::Spawning),
        ((S::Connecting, E::TimedOut), S::InProcess),
        ((S::Spawning, E::Spawned), S::Waiting),
        ((S::Spawning, E::SpawnFailed), S::InProcess),
        ((S::Waiting, E::Connected), S::Handshaking),
        ((S::Waiting, E::TimedOut), S::InProcess),
        ((S::Handshaking, E::Accepted), S::Ready),
        ((S::Handshaking, E::Mismatch), S::Restarting),
        ((S::Handshaking, E::TimedOut), S::InProcess),
        ((S::Restarting, E::Stopped), S::Respawning),
        ((S::Restarting, E::TimedOut), S::InProcess),
        ((S::Respawning, E::Spawned), S::Rewaiting),
        ((S::Respawning, E::SpawnFailed), S::InProcess),
        ((S::Rewaiting, E::Connected), S::Rehandshaking),
        ((S::Rewaiting, E::TimedOut), S::InProcess),
        ((S::Rehandshaking, E::Accepted), S::Ready),
        ((S::Rehandshaking, E::Mismatch), S::InProcess),
        ((S::Rehandshaking, E::TimedOut), S::InProcess),
        ((S::Ready, E::Broken), S::InProcess),
    ])
}

fn machine_in(target: ConnectState) -> Connection {
    let path: &[ConnectEvent] = match target {
        S::Idle => &[],
        S::Connecting => &[E::Connect],
        S::Handshaking => &[E::Connect, E::Connected],
        S::Spawning => &[E::Connect, E::Refused],
        S::Waiting => &[E::Connect, E::Refused, E::Spawned],
        S::Restarting => &[E::Connect, E::Connected, E::Mismatch],
        S::Respawning => &[E::Connect, E::Connected, E::Mismatch, E::Stopped],
        S::Rewaiting => &[
            E::Connect,
            E::Connected,
            E::Mismatch,
            E::Stopped,
            E::Spawned,
        ],
        S::Rehandshaking => &[
            E::Connect,
            E::Connected,
            E::Mismatch,
            E::Stopped,
            E::Spawned,
            E::Connected,
        ],
        S::Ready => &[E::Connect, E::Connected, E::Accepted],
        S::InProcess => &[E::Disabled],
    };
    let mut c = Connection::default();
    for event in path {
        c.handle(*event).expect("setup path is legal");
    }
    assert_eq!(c.state(), target);
    c
}

#[test]
fn every_state_event_pair_matches_the_spec() {
    let spec = spec();
    let mut legal = 0;
    for state in ConnectState::ALL {
        for event in ConnectEvent::ALL {
            let mut c = machine_in(state);
            match (spec.get(&(state, event)), c.handle(event)) {
                (Some(expected), Ok(got)) => {
                    assert_eq!(got, *expected, "{state:?} + {event:?}");
                    legal += 1;
                }
                (None, Err(err)) => {
                    assert_eq!((err.state, err.event), (state, event));
                    assert_eq!(c.state(), state);
                }
                (expected, got) => {
                    panic!("{state:?} + {event:?}: spec {expected:?}, machine {got:?}")
                }
            }
        }
    }
    assert_eq!(legal, spec.len());
    assert_eq!(ConnectState::ALL.len() * ConnectEvent::ALL.len(), 121);
}

#[test]
fn a_second_version_mismatch_falls_back_in_process() {
    let mut c = machine_in(S::Rehandshaking);
    assert_eq!(c.handle(E::Mismatch).unwrap(), S::InProcess);
}

#[test]
fn every_non_terminal_state_can_fail_open() {
    for state in ConnectState::ALL {
        if state.is_terminal() {
            continue;
        }
        let reaches = ConnectEvent::ALL.iter().any(|e| {
            let mut c = machine_in(state);
            c.handle(*e) == Ok(S::InProcess)
        });
        assert!(reaches, "{state:?} has no way to fail open");
    }
}

#[derive(Debug, Clone)]
struct Model {
    state: ConnectState,
    last_legal: bool,
    spawns: usize,
    mismatches: usize,
    last_event: Option<ConnectEvent>,
    in_process_seen: bool,
}

struct Reference;

impl ReferenceStateMachine for Reference {
    type State = Model;
    type Transition = ConnectEvent;

    fn init_state() -> BoxedStrategy<Model> {
        Just(Model {
            state: S::Idle,
            last_legal: true,
            spawns: 0,
            mismatches: 0,
            last_event: None,
            in_process_seen: false,
        })
        .boxed()
    }

    fn transitions(_state: &Model) -> BoxedStrategy<ConnectEvent> {
        select(ConnectEvent::ALL.to_vec()).boxed()
    }

    fn apply(mut model: Model, event: &ConnectEvent) -> Model {
        match spec().get(&(model.state, *event)) {
            Some(next) => {
                model.last_legal = true;
                model.state = *next;
                model.last_event = Some(*event);
                match event {
                    E::Spawned => model.spawns += 1,
                    E::Mismatch => model.mismatches += 1,
                    _ => {}
                }
                model.in_process_seen |= *next == S::InProcess;
            }
            None => model.last_legal = false,
        }
        model
    }
}

struct Sut;

impl StateMachineTest for Sut {
    type SystemUnderTest = Connection;
    type Reference = Reference;

    fn init_test(_model: &Model) -> Connection {
        Connection::default()
    }

    fn apply(mut sut: Connection, model: &Model, event: ConnectEvent) -> Connection {
        assert_eq!(sut.handle(event).is_ok(), model.last_legal, "{event:?}");
        assert_eq!(sut.state(), model.state);
        sut
    }

    fn check_invariants(sut: &Connection, model: &Model) {
        assert!(model.spawns <= 2, "spawned more than twice");
        if sut.state() == S::Ready {
            assert_eq!(model.last_event, Some(E::Accepted));
            assert!(model.mismatches <= 1, "ready after two mismatches");
        }
        if model.in_process_seen {
            assert_eq!(sut.state(), S::InProcess, "left InProcess");
        }
    }
}

prop_state_machine! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn connection_matches_reference_model(sequential 1..64 => Sut);
}

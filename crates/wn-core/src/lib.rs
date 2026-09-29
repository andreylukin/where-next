//! Core of where-next: shared types, ranking, and the explicit state machines.
//!
//! Policy (see CONTRIBUTING.md): anything with a lifecycle or I/O is an explicit state machine
//! with a complete transition table, tested over every (state, event) pair and with model-based
//! property tests. Pure functions get ordinary property tests.
//!
//! Behaviour that the published model depends on (query text, adapter objective, abstain
//! thresholds, hint budget) is pinned to the evaluated reference implementation by golden tests.

pub mod adapter;
pub mod index_lifecycle;
pub mod rank;
pub mod text;

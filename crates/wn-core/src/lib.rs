//! Core of where-next: shared types, ranking, and the explicit state machines.
//!
//! Policy (see CONTRIBUTING.md): anything with a lifecycle or I/O is an explicit state machine
//! with a complete transition table, tested over every (state, event) pair and with model-based
//! property tests. Pure functions get ordinary property tests.

pub mod index_lifecycle;

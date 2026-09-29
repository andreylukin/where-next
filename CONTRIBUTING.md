# Contributing to where-next

Thanks for helping. Two rules shape the codebase.

## 1. Tests first

Write the failing test, then make it pass. Every pull request runs:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Which kind of test to write:

| Code | Test |
|---|---|
| Pure functions (ranking maths, tokenizing, path normalization) | Unit tests plus `proptest` properties |
| Anything with a lifecycle or I/O | An explicit state machine (see below) |
| CLI and MCP output | `insta` snapshot tests |
| Git behavior (history, renames, deletions, untracked files) | Fixture repositories generated inside the test |
| Retrieval quality and latency | The CI benchmark gate (hit@3 and p95 budgets), once it lands |

## 2. Anything with a lifecycle is an explicit state machine

Indexes, model downloads, adapters, daemon and MCP sessions, and query handling all move through
states. Each one is written as:

1. A state enum and an event enum, each with an `ALL` constant.
2. A complete transition table: every legal `(state, event) -> state`. Everything else is rejected
   with an error and leaves the state unchanged.
3. Tests:
   - an **exhaustive** test over every `(state, event)` pair, checked against a specification written
     in the test file independently of the table;
   - a **model-based** property test with `proptest-state-machine`: random event sequences, including
     illegal ones, run against a reference model, with invariants checked after every step.

`crates/wn-core/src/index_lifecycle.rs` and `crates/wn-core/tests/index_lifecycle.rs` are the template.

Don't force pure computation into a state machine; that adds ceremony without safety.

## Data and privacy

Never commit model weights, datasets, private repositories, session logs or secrets. Personal data
only ever feeds the on-device adapter.

## Commits and pull requests

Small, focused pull requests with a clear description of the behavior change and the tests that
cover it.

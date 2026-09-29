# Changelog

All notable changes are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). Breaking changes to the MCP tool schema are called out
explicitly.

## [Unreleased]

### Added

- Rust workspace skeleton with seven crates and a `wn` binary placeholder.
- Index lifecycle state machine with exhaustive transition tests and model-based tests.
- Documentation: quickstart, how it works, privacy and licensing, adding a source, FAQ.
- Benchmark protocols: ContextBench, history replay, agent trial, and metric definitions.
- Launch plan (LAUNCH.md), security policy, and issue and pull request templates.
- `wn-core`: query text, ranking, per-repo adapter, abstain logic and hint budget, with golden parity
  tests against the research prototype; adapter and query lifecycle state machines; repository index
  and query runtime.
- `wn-git`: repository scan (tracked and untracked files), history mining and co-change.
- `wn-sources`: file kinds, multi-language skeletons and symbols, config files, with golden parity tests.
- `wn-embed`: ONNX Runtime inference with manifest verification and a model lifecycle state machine;
  parity with the Python reference, including quantized graphs.
- `wn-daemon`: resident session with background warm-up and refresh, failing open.
- `wn-mcp`: stdio MCP server (`where_next`, `refresh_index`, `status`) and the `wn-mcp-server` binary;
  latency benchmarks.
- Docs: building and testing guide; agent-trial pilot 1 results.

### Changed

- README, LAUNCH.md and FAQ now report the agent-trial pilot honestly: hints got a cheap, capable
  agent to the right file sooner, but did not reduce cost or change success, so we make no
  agent-savings claim. The pitch is fast local navigation for people and agents.

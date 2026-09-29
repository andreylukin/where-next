# Quality-gate fixture

Precomputed vectors for `tests/quality_gate.rs`, so CI can check ranking, the personal adapter and
calibrated abstain end to end without model weights.

- **Source:** 208 ContextBench tasks (issue text → files the fix edited) from 18 small public
  repositories (fd, jq, requests, fmt, dayjs, pytest, jackson-core, xarray, simdjson, flipt, zstd,
  vue core, navidrome, serverless, nlohmann/json, logstash, ponyc, openlibrary).
- **Vectors:** gemma-xl1, Matryoshka-truncated to 128 dimensions, little-endian f16 in
  `fixture.bin`; `fixture.json` indexes them (per repository: document range, shared adapter
  history as `[query vector, document]`; per task: query vector, candidates, gold, query kind).
- **Candidates:** each task keeps its top 30 files by plain score plus its gold files and the history
  positives, so the fixture stays small. It is a regression check, not a leakage-free benchmark: a
  repository's tasks share one history.
- **Baseline:** `baseline.json`, recorded with `WN_GATE_RECORD=1 cargo test -p wn-core --release --test
  quality_gate`. The gate fails when plain, adapter or answered hit@3 drops more than one point.

Regenerate (research repo, Modal): `modal run calib_modal.py::make_gate --max-docs 4000 --hist-cap 80`,
then copy `calib/gate/fixture.{json,bin}` from the volume here and re-record the baseline.

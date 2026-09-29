//! Resident where-next session: keeps the model and index warm and answers queries.
//!
//! - [`session`]: the session lifecycle state machine (Starting → Warming → Serving / Degraded →
//!   ShuttingDown → Stopped). Only `Serving` answers; every other state fails open.
//! - [`index`]: vectors per file, brute-force cosine search, atomic on-disk snapshots.
//! - [`engine`]: scans a repository (tracked + untracked), embeds skeletons, refreshes changed
//!   files, answers with at most three hints. The model sits behind the [`engine::Encoder`] trait.
//! - [`daemon`]: the session machine around an engine; answers are hints or explicit fail-open.
//! - `onnx` (feature `onnx`, default): the production encoder backed by `wn-embed`.
//! - [`fake`]: a deterministic hashing encoder for tests.

pub mod daemon;
pub mod engine;
pub mod fake;
pub mod index;
pub mod session;

#[cfg(feature = "onnx")]
pub mod onnx;

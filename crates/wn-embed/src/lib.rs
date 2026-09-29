//! Embedding backends and the model on disk.
//!
//! - [`lifecycle`]: the model lifecycle state machine (Missing → Downloading → Verifying →
//!   Loaded / Corrupt). Only a verified model may embed.
//! - [`verify`]: SHA-256 manifests and the model fingerprint that keys the vector cache.
//! - [`store`]: fetches model files into the cache and drives the lifecycle.
//! - [`spec`]: `wn-model.json` and the exact query/document text each model family expects.
//! - `embedder` (feature `onnx`, on by default): ONNX Runtime inference.

pub mod lifecycle;
pub mod spec;
pub mod store;
pub mod verify;

#[cfg(feature = "onnx")]
pub mod embedder;

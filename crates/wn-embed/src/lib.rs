//! Embedding backends and the model on disk.
//!
//! - [`lifecycle`]: the model lifecycle state machine (Missing → Downloading → Verifying →
//!   Loaded / Corrupt). Only a verified model may embed.
//! - [`verify`]: SHA-256 manifests and the model fingerprint that keys the vector cache.
//! - [`source`]: where model files come from (local directory, base URL, Hugging Face repo).
//! - [`store`]: fetches model files into the cache and drives the lifecycle.
//! - `remote` (feature `remote`): HTTP downloads for URL and Hugging Face sources.
//! - [`spec`]: `wn-model.json` and the exact query/document text each model family expects.
//! - `embedder` (feature `onnx`, on by default): ONNX Runtime inference.
//! - `core_encoder` (feature `onnx`): the model as a `wn_core::encoder::Encoder`.

pub mod lifecycle;
#[cfg(feature = "remote")]
pub mod remote;
pub mod source;
pub mod spec;
pub mod store;
pub mod verify;

#[cfg(feature = "onnx")]
pub mod core_encoder;
#[cfg(feature = "onnx")]
pub mod embedder;

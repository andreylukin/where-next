//! Resident where-next session: keeps the model and index warm and answers queries.
//!
//! - [`session`]: the session lifecycle state machine (Starting → Warming → Serving / Degraded →
//!   ShuttingDown → Stopped). Only `Serving` answers; every other state fails open.
//! - [`workspace`]: one repository on the `wn-core` runtime (index, adapter, `suggest`) and the
//!   `wn-git` scanner; scans run without holding locks.
//! - [`daemon`]: the session machine around a workspace, the object-safe [`daemon::Service`]
//!   for transports, and the background warm-up / refresh thread.
//! - [`usage`]: the local usage log that `wn report` summarises (never sent anywhere by itself).

//! - [`resident`]: the per-user background process behind `wn ask` (Starting → Listening →
//!   Draining → Stopped / Failed); idle timeout and shutdown both drain.
//! - [`connect`]: how one command reaches that process (connect, spawn at most once, replace a
//!   daemon from another build at most once), failing open to in-process answers.

pub mod connect;
pub mod daemon;
pub mod resident;
pub mod session;
pub mod usage;
pub mod workspace;

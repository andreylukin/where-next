//! Resident where-next session: keeps the model and index warm and answers queries.
//!
//! - [`session`]: the session lifecycle state machine (Starting → Warming → Serving / Degraded →
//!   ShuttingDown → Stopped). Only `Serving` answers; every other state fails open.
//! - [`workspace`]: one repository on the `wn-core` runtime (index, adapter, `suggest`) and the
//!   `wn-git` scanner; scans run without holding locks.
//! - [`daemon`]: the session machine around a workspace, the object-safe [`daemon::Service`]
//!   for transports, and the background warm-up / refresh thread.

pub mod daemon;
pub mod session;
pub mod workspace;

//! Best-effort detection of the REAL listening port of a running supervised
//! process.
//!
//! The OS TCP listening table (`netstat -ano`, which lists the owning PID) is
//! matched against each process's subtree. This is what lets the dashboard show
//! a port even when the child ignored our forced `PORT` and bound its own (e.g. a
//! config hardcoded to 8080), and corrects a forced port the child silently
//! refused. A forced/public port that is genuinely listening is treated as
//! authoritative, which also keeps a flutter live-reload proxy showing its public
//! port (bound by the supervisor itself, not by the child subtree).

// The port-resolution logic that used to live in `fill_ports` now lives in the
// combined background sampler (`supervisor::sampler`), which owns the single
// shared `System` refresh. The netstat reader and its local-address port parser
// live in `crate::ports` (the single OS-probe surface); the process-subtree
// graph walk now lives in `super::proc_tree` and is shared with `mem`. These
// re-exports keep the historical `ports_detect::{children_map, subtree}` call
// sites (in `sampler`) pointing at the single shared implementation.

pub(crate) use super::proc_tree::snapshot_children as children_map;
pub(crate) use super::proc_tree::subtree;

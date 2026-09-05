//! Tauri IPC commands, grouped one file per domain. This file only declares
//! the submodules and re-exports every command fn so `ipc::commands::<fn>`
//! keeps resolving unchanged for `lib.rs`'s `invoke_handler` list and for
//! `tests/export_types.rs`'s direct `SystemStats`/`DiskUsage` imports.

mod app;
mod command_defs;
mod groups;
mod hub;
mod ports;
mod procs;
mod projects;

pub use app::*;
pub use command_defs::*;
pub use groups::*;
pub use hub::*;
pub use ports::*;
pub use procs::*;
pub use projects::*;

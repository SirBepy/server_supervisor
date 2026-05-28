use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Lifecycle state of a supervised process. (Forward-looking: the supervisor
/// registry lands in Phase 2; this type already crosses IPC so the dashboard
/// can be built against it.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum ProcStatus {
    Stopped,
    Starting,
    Running,
    Crashed,
}

/// A single supervised process as shown in the dashboard / returned by the API.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct ProcInfo {
    pub id: String,
    pub project: String,
    pub name: String,
    pub status: ProcStatus,
    pub pid: Option<u32>,
}

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A named upstream target for a project's reverse-proxy hub (see
/// `supervisor::proxy_hub`). The dashboard/AI-agent picks one preset as
/// active; the hub forwards every request to whichever one is active, without
/// ever rebinding its listener.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct UpstreamPreset {
    pub id: String,
    pub name: String,
    /// e.g. `http://localhost:9000` or `https://staging.example.com`. No
    /// trailing slash required; the hub strips one if present.
    pub base_url: String,
    /// The dev sets this explicitly for a prod-like target, so the dashboard
    /// can warn before a swap - purely advisory, never enforced backend-side.
    pub danger: bool,
}

//! Command CRUD (add/update/remove) plus register-and-run handlers for the
//! localhost API.

use super::{ai_forbidden, unit_result, ApiState};
use crate::types::{Command, ProcKind};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;

#[derive(Deserialize)]
pub(super) struct RunBody {
    root: String,
    cmd: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    kind: Option<ProcKind>,
    #[serde(default)]
    use_dynamic_port: Option<bool>,
    /// Manual port override; omitted/`null` = auto-assign from the project's
    /// port block (see `ports::PortRegistry::project_port`).
    #[serde(default)]
    port: Option<u16>,
    /// Per-command env overrides, one `KEY=VALUE` per line (see `Command::env`).
    #[serde(default)]
    env: Option<String>,
}

/// Body for `POST /projects/:project_id/commands` (register a command without
/// starting it). `kind` omitted -> inferred from `cmd`. `use_dynamic_port`
/// defaults to true (matching the dashboard add flow and `/run`).
#[derive(Deserialize)]
pub(super) struct AddCommandBody {
    name: String,
    cmd: String,
    #[serde(default)]
    kind: Option<ProcKind>,
    #[serde(default)]
    autostart: Option<bool>,
    #[serde(default)]
    use_dynamic_port: Option<bool>,
    /// Manual port override; omitted/`null` = auto-assign from the project's
    /// port block.
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    env: Option<String>,
}

/// Body for `PATCH /projects/:project_id/commands/:command_id`. Mirrors the IPC
/// `update_command`: a full field replace (kind is always re-inferred from
/// `cmd`), so a caller must send the complete desired state, not a partial diff.
/// Rejected backend-side (400) while the command is running.
#[derive(Deserialize)]
pub(super) struct UpdateCommandBody {
    name: String,
    cmd: String,
    #[serde(default)]
    autostart: Option<bool>,
    #[serde(default)]
    use_dynamic_port: Option<bool>,
    /// Manual port override; omitted/`null` = auto-assign from the project's
    /// port block.
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    env: Option<String>,
}

pub(super) async fn run(State(s): State<ApiState>, Json(b): Json<RunBody>) -> Response {
    if let Some(flags) = &s.ai_flags {
        let (can_add_projects, can_add_commands) = flags();
        if let Some(r) = ai_forbidden(can_add_projects, "AI project creation") {
            return r;
        }
        if let Some(r) = ai_forbidden(can_add_commands, "AI command creation") {
            return r;
        }
    }
    match s.sup.ensure_and_run(
        &b.root,
        &b.cmd,
        b.name,
        // Omitted kind -> inferred from the command; an explicit kind overrides.
        b.kind,
        b.use_dynamic_port.unwrap_or(true),
        b.port,
        b.env.unwrap_or_default(),
    ) {
        Ok(info) => Json(info).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

/// Map a `Result<Command, String>` to JSON-on-success / 400-on-error, matching
/// `unit_result`'s error convention for the CRUD routes that return a command.
fn command_result(r: Result<Command, String>) -> Response {
    match r {
        Ok(c) => Json(c).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

pub(super) async fn add_command(
    State(s): State<ApiState>,
    Path(project_id): Path<String>,
    Json(b): Json<AddCommandBody>,
) -> Response {
    if let Some(flags) = &s.ai_flags {
        let (_, can_add_commands) = flags();
        if let Some(r) = ai_forbidden(can_add_commands, "AI command creation") {
            return r;
        }
    }
    command_result(s.sup.add_command(
        &project_id,
        b.name,
        b.cmd,
        b.kind,
        b.autostart.unwrap_or(false),
        b.use_dynamic_port.unwrap_or(true),
        b.port,
        b.env.unwrap_or_default(),
        // The localhost API has no `role` field on its request body (FE/BE
        // badging is a dashboard-only concept); commands it creates start unset.
        None,
    ))
}

pub(super) async fn update_command(
    State(s): State<ApiState>,
    Path((project_id, command_id)): Path<(String, String)>,
    Json(b): Json<UpdateCommandBody>,
) -> Response {
    command_result(s.sup.update_command(
        &project_id,
        &command_id,
        b.name,
        b.cmd,
        b.autostart.unwrap_or(false),
        b.use_dynamic_port.unwrap_or(true),
        b.port,
        b.env.unwrap_or_default(),
        // Same rationale as add_command: no `role` on the API body, and this
        // endpoint already fully replaces the mutable fields rather than
        // merging (autostart/use_dynamic_port fall back to a default, not the
        // prior value, when omitted), so unset is consistent, not lossy-new.
        None,
    ))
}

pub(super) async fn remove_command(
    State(s): State<ApiState>,
    Path((project_id, command_id)): Path<(String, String)>,
) -> Response {
    unit_result(s.sup.remove_command(&project_id, &command_id))
}

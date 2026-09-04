//! Interactive callbacks with explicit consent and lossless permission decisions.

mod elicitation;
mod permission;
mod schema;

pub use elicitation::ElicitationResolver;
pub use permission::PermissionResolver;

use agent_client_protocol::schema::v2;
use anyhow::{Result, ensure};
use serde_json::Value;

/// A backend callback that can be represented by the client's standard ACP capabilities.
pub enum Interaction {
    Permission {
        request: v2::RequestPermissionRequest,
        resolver: PermissionResolver,
    },
    Elicitation {
        request: v2::CreateElicitationRequest,
        resolver: ElicitationResolver,
    },
}

/// Return `None` when negotiated Codex extensions are required. Never auto-approve.
pub fn translate(
    session_id: &str,
    method: &str,
    params: &Value,
    capabilities: &v2::ClientCapabilities,
) -> Result<Option<Interaction>> {
    ensure!(
        serde_json::to_vec(params)?.len() <= 1024 * 1024,
        "interactive request exceeds 1 MiB"
    );
    match method {
        "item/commandExecution/requestApproval"
        | "item/fileChange/requestApproval"
        | "item/permissions/requestApproval" => {
            permission::translate(session_id, method, params).map(Some)
        }
        "mcpServer/elicitation/request"
            if params["_meta"]["codex_approval_kind"] == "mcp_tool_call" =>
        {
            permission::mcp_approval(session_id, params)
        }
        "item/tool/requestUserInput" | "mcpServer/elicitation/request" => {
            elicitation::translate(session_id, method, params, capabilities)
        }
        _ => Ok(None),
    }
}

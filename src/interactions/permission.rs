use std::collections::BTreeMap;

use agent_client_protocol::schema::v2;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

use super::Interaction;

/// Resolves only the exact choices offered by one backend approval callback.
pub struct PermissionResolver {
    choices: BTreeMap<String, Value>,
    cancelled: Value,
}

impl PermissionResolver {
    /// An unknown option or future outcome cannot become approval.
    pub fn resolve(self, response: v2::RequestPermissionResponse) -> Result<Value> {
        match response.outcome {
            v2::RequestPermissionOutcome::Cancelled => Ok(self.cancelled),
            v2::RequestPermissionOutcome::Selected(selected) => self
                .choices
                .get(selected.option_id.0.as_ref())
                .cloned()
                .context("permission option was not offered"),
            _ => bail!("unsupported permission outcome"),
        }
    }

    /// Safe response for a cancelled or disconnected client.
    pub fn cancelled(self) -> Value {
        self.cancelled
    }
}

pub(super) fn translate(session_id: &str, method: &str, params: &Value) -> Result<Interaction> {
    let item_id = params["itemId"]
        .as_str()
        .context("approval has no itemId")?;
    let mut choices = BTreeMap::new();
    let mut options = Vec::new();
    let mut add = |name: String, kind, response| {
        let id = format!("choice-{}", options.len());
        choices.insert(id.clone(), response);
        options.push(v2::PermissionOption::new(id, name, kind));
    };
    let permissions_request = method == "item/permissions/requestApproval";
    let title = if permissions_request {
        "Grant additional permissions"
    } else if method == "item/fileChange/requestApproval" {
        "Allow file changes"
    } else {
        "Allow command execution"
    };
    let cancelled = if permissions_request {
        json!({"permissions":{},"scope":"turn"})
    } else {
        json!({"decision":"cancel"})
    };
    if permissions_request {
        let profile = params
            .get("permissions")
            .filter(|profile| profile.is_object())
            .context("missing requested permission profile")?;
        add(
            "Allow requested permissions for this turn".into(),
            v2::PermissionOptionKind::AllowOnce,
            json!({"permissions":profile,"scope":"turn"}),
        );
        add(
            "Allow requested permissions for this session only".into(),
            v2::PermissionOptionKind::AllowAlways,
            json!({"permissions":profile,"scope":"session"}),
        );
        add(
            "Decline permissions".into(),
            v2::PermissionOptionKind::RejectOnce,
            cancelled.clone(),
        );
    } else {
        let decisions = if let Some(decisions) = params
            .get("availableDecisions")
            .filter(|value| !value.is_null())
        {
            decisions
                .as_array()
                .context("invalid availableDecisions")?
                .clone()
        } else {
            let mut decisions = vec![
                json!("accept"),
                json!("acceptForSession"),
                json!("decline"),
                json!("cancel"),
            ];
            if let Some(amendment) = params
                .get("proposedExecpolicyAmendment")
                .filter(|value| !value.is_null())
            {
                decisions.push(
                    json!({"acceptWithExecpolicyAmendment":{"execpolicy_amendment":amendment}}),
                );
            }
            if let Some(amendments) = params["proposedNetworkPolicyAmendments"].as_array() {
                for amendment in amendments {
                    decisions.push(json!({"applyNetworkPolicyAmendment":{"network_policy_amendment":amendment}}));
                }
            }
            decisions
        };
        ensure!(
            !decisions.is_empty() && decisions.len() <= 32,
            "invalid approval choices"
        );
        for decision in decisions {
            let (name, kind) = match decision.as_str() {
                Some("accept") => ("Allow once".into(), v2::PermissionOptionKind::AllowOnce),
                Some("acceptForSession") => (
                    "Allow for this session only".into(),
                    v2::PermissionOptionKind::AllowAlways,
                ),
                Some("decline") => (
                    "Decline and continue".into(),
                    v2::PermissionOptionKind::RejectOnce,
                ),
                Some("cancel") => (
                    "Decline and cancel turn".into(),
                    v2::PermissionOptionKind::RejectOnce,
                ),
                _ if decision.get("acceptWithExecpolicyAmendment").is_some() => (
                    format!(
                        "Allow and persist command rule: {}",
                        decision["acceptWithExecpolicyAmendment"]
                    ),
                    v2::PermissionOptionKind::AllowAlways,
                ),
                _ if decision.get("applyNetworkPolicyAmendment").is_some() => {
                    let amendment = &decision["applyNetworkPolicyAmendment"];
                    let kind = if amendment["network_policy_amendment"]["action"] == "deny"
                        || amendment["networkPolicyAmendment"]["action"] == "deny"
                    {
                        v2::PermissionOptionKind::RejectAlways
                    } else {
                        v2::PermissionOptionKind::AllowAlways
                    };
                    (format!("Persist network rule: {amendment}"), kind)
                }
                _ => bail!("unsupported backend approval decision"),
            };
            add(name, kind, json!({"decision":decision}));
        }
    }
    let tool = v2::ToolCallUpdate::new(item_id)
        .title(title.to_owned())
        .status(v2::ToolCallStatus::Pending)
        .raw_input(params.clone());
    let mut request = v2::RequestPermissionRequest::new(session_id, title, options)
        .subject(v2::RequestPermissionSubject::from(tool));
    let mut description = params["reason"].as_str().unwrap_or_default().to_owned();
    for (key, label) in [
        ("command", "Command or terminal input"),
        ("cwd", "Working directory"),
    ] {
        if let Some(value) = params[key].as_str() {
            description.push_str(&format!("\n{label}: {value}"));
        }
    }
    for (key, label) in [
        ("networkApprovalContext", "Network access"),
        ("additionalPermissions", "Additional permissions"),
    ] {
        if let Some(value) = params.get(key).filter(|value| !value.is_null()) {
            description.push_str(&format!("\n{label}: {value}"));
        }
    }
    if permissions_request {
        description.push_str(&format!(
            "\nRequested permissions: {}",
            params["permissions"]
        ));
    }
    if let Some(root) = params["grantRoot"].as_str() {
        description.push_str(&format!("\nRequested write root: {root}"));
    }
    request = request.description(description);
    Ok(Interaction::Permission {
        request,
        resolver: PermissionResolver { choices, cancelled },
    })
}

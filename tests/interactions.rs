use agent_client_protocol::schema::v2;
use anyhow::{Context, Result, bail};
use codex_acp_v2::interactions::{self, Interaction};
use serde_json::json;

#[test]
fn mcp_approval_keeps_tool_context_and_exact_offered_persistence() -> Result<()> {
    let params = json!({"threadId":"s","turnId":"t","serverName":"mail","mode":"form",
        "message":"Allow sending this message?","requestedSchema":{"type":"object","properties":{}},
        "_meta":{"codex_approval_kind":"mcp_tool_call","tool_params":{"to":"person@example.com","body":"Hello"},"persist":["session","always"]}});
    let Some(Interaction::Permission { request, resolver }) = interactions::translate(
        "s",
        "mcpServer/elicitation/request",
        &params,
        &v2::ClientCapabilities::new(),
    )?
    else {
        bail!("MCP approval must not become an empty form")
    };
    let encoded = serde_json::to_value(&request)?;
    assert_eq!(encoded["_meta"]["codex"]["mcpApproval"], params);
    assert!(
        request
            .description
            .as_deref()
            .is_some_and(|text| text.contains("person@example.com"))
    );
    let session = request
        .options
        .iter()
        .find(|option| option.option_id.to_string() == "session")
        .ok_or_else(|| anyhow::anyhow!("missing offered session permission"))?;
    assert_eq!(
        resolver.resolve(v2::RequestPermissionResponse::new(
            v2::RequestPermissionOutcome::Selected(v2::SelectedPermissionOutcome::new(
                session.option_id.clone()
            ),)
        ))?,
        json!({"action":"accept","content":null,"_meta":{"persist":"session"}})
    );
    let mut once_only = params;
    once_only["_meta"]
        .as_object_mut()
        .context("metadata")?
        .remove("persist");
    let Some(Interaction::Permission { resolver, .. }) = interactions::translate(
        "s",
        "mcpServer/elicitation/request",
        &once_only,
        &v2::ClientCapabilities::new(),
    )?
    else {
        bail!("expected permission")
    };
    assert!(
        resolver
            .resolve(v2::RequestPermissionResponse::new(
                v2::RequestPermissionOutcome::Selected(v2::SelectedPermissionOutcome::new(
                    "always"
                ),)
            ))
            .is_err()
    );
    Ok(())
}

#[test]
fn ordinary_mcp_elicitation_preserves_metadata_and_privileged_unknown_forms_require_raw()
-> Result<()> {
    let caps = v2::ClientCapabilities::new().elicitation(
        v2::ElicitationCapabilities::new().form(v2::ElicitationFormCapabilities::new()),
    );
    let mut params = json!({"mode":"form","message":"Choose","requestedSchema":{"type":"object","properties":{}},"_meta":{"provider":{"correlation":"opaque","hint":"keep"}}});
    let Some(Interaction::Elicitation { request, .. }) =
        interactions::translate("s", "mcpServer/elicitation/request", &params, &caps)?
    else {
        bail!("expected ordinary elicitation")
    };
    assert_eq!(serde_json::to_value(request)?["_meta"], params["_meta"]);
    params["_meta"]["codex_approval_kind"] = json!("future_privileged_approval");
    assert!(
        interactions::translate("s", "mcpServer/elicitation/request", &params, &caps)?.is_none()
    );
    Ok(())
}

#[test]
fn approval_returns_exact_offered_policy_and_rejects_forged_selection() -> Result<()> {
    let decision =
        json!({"acceptWithExecpolicyAmendment":{"execpolicy_amendment":["cargo","test"]}});
    let params =
        json!({"itemId":"item","availableDecisions":["decline",decision],"command":"cargo test"});
    let Some(Interaction::Permission { request, resolver }) = interactions::translate(
        "session",
        "item/commandExecution/requestApproval",
        &params,
        &v2::ClientCapabilities::new(),
    )?
    else {
        bail!("expected permission")
    };
    assert_eq!(request.options.len(), 2);
    let selection = v2::RequestPermissionResponse::new(v2::RequestPermissionOutcome::Selected(
        v2::SelectedPermissionOutcome::new(request.options[1].option_id.clone()),
    ));
    assert_eq!(resolver.resolve(selection)?, json!({"decision":decision}));
    let Some(Interaction::Permission { resolver, .. }) = interactions::translate(
        "session",
        "item/commandExecution/requestApproval",
        &params,
        &v2::ClientCapabilities::new(),
    )?
    else {
        bail!("expected permission")
    };
    assert!(
        resolver
            .resolve(v2::RequestPermissionResponse::new(
                v2::RequestPermissionOutcome::Selected(v2::SelectedPermissionOutcome::new(
                    "acceptForSession"
                ))
            ))
            .is_err()
    );
    Ok(())
}

#[test]
fn question_forms_keep_answer_keys_and_do_not_invent_answers_on_cancellation() -> Result<()> {
    let caps = v2::ClientCapabilities::new().elicitation(
        v2::ElicitationCapabilities::new().form(v2::ElicitationFormCapabilities::new()),
    );
    let params = json!({"questions":[{"id":"language","header":"Language","question":"Which language?","options":[{"label":"Rust","description":"Native"},{"label":"Go","description":"Managed"}]}]});
    let Some(Interaction::Elicitation { request, resolver }) =
        interactions::translate("s", "item/tool/requestUserInput", &params, &caps)?
    else {
        bail!("expected form")
    };
    let serialized = serde_json::to_value(request)?;
    assert_eq!(
        serialized["requestedSchema"]["properties"]["language"]["enum"],
        json!(["Rust", "Go"])
    );
    let answer: v2::CreateElicitationResponse =
        serde_json::from_value(json!({"action":"accept","content":{"language":"Rust"}}))?;
    assert_eq!(
        resolver.resolve(answer)?,
        json!({"answers":{"language":{"answers":["Rust"]}}})
    );
    let Some(Interaction::Elicitation { resolver, .. }) =
        interactions::translate("s", "item/tool/requestUserInput", &params, &caps)?
    else {
        bail!("expected form")
    };
    assert_eq!(resolver.cancelled(), json!({"answers":{}}));
    assert!(
        interactions::translate(
            "s",
            "item/tool/requestUserInput",
            &params,
            &v2::ClientCapabilities::new()
        )?
        .is_none()
    );
    Ok(())
}

#[test]
fn mcp_form_never_drops_constraints_and_legacy_enum_titles_survive() -> Result<()> {
    let caps = v2::ClientCapabilities::new().elicitation(
        v2::ElicitationCapabilities::new().form(v2::ElicitationFormCapabilities::new()),
    );
    let mut params = json!({"mode":"form","message":"Choose","requestedSchema":{"type":"object","properties":{"answer":{"type":"string","enum":["a","b"],"enumNames":["Alpha","Beta"]}},"required":["answer"]}});
    let Some(Interaction::Elicitation { request, resolver }) =
        interactions::translate("s", "mcpServer/elicitation/request", &params, &caps)?
    else {
        bail!("expected form")
    };
    assert_eq!(
        serde_json::to_value(request)?["requestedSchema"]["properties"]["answer"]["oneOf"],
        json!([{"const":"a","title":"Alpha"},{"const":"b","title":"Beta"}])
    );
    let response = serde_json::from_value(json!({"action":"accept","content":{"answer":"b"}}))?;
    assert_eq!(
        resolver.resolve(response)?,
        json!({"action":"accept","content":{"answer":"b"},"_meta":null})
    );
    let Some(Interaction::Elicitation { resolver, .. }) =
        interactions::translate("s", "mcpServer/elicitation/request", &params, &caps)?
    else {
        bail!("expected form")
    };
    let invalid =
        serde_json::from_value(json!({"action":"accept","content":{"answer":"not offered"}}))?;
    assert!(resolver.resolve(invalid).is_err());
    params["requestedSchema"]["properties"]["answer"]["unsupportedConstraint"] = json!(true);
    assert!(
        interactions::translate("s", "mcpServer/elicitation/request", &params, &caps)?.is_none()
    );
    Ok(())
}

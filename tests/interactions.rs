use agent_client_protocol::schema::v2;
use anyhow::{Result, bail};
use codex_acp_v2::interactions::{self, Interaction};
use serde_json::json;

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

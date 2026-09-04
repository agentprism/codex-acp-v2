use std::collections::HashSet;

use codex_acp_v2::extensions::{
    ExtensionError, ExtensionPolicy, Negotiation, RequestEnvelope, RequestScope,
};
use serde_json::json;

#[test]
fn scope_checks_block_cross_session_and_host_escalation_without_rewriting_payloads() {
    let owned = HashSet::from(["owned".to_owned()]);
    let mut request: RequestEnvelope = serde_json::from_value(json!({
        "version": 1, "sessionId": "owned", "method": "turn/settings/update",
        "params": {"threadId": "owned", "turnId": "turn-1", "model": "model-a", "opaque": {"threadId": "tool data"}}
    })).unwrap();
    let original = request.params.clone();
    let policy = ExtensionPolicy::new(false);
    assert_eq!(
        policy.authorize(&request, &owned).unwrap(),
        RequestScope::Thread("owned".into())
    );
    assert_eq!(request.params, original);
    request.params["threadId"] = json!("unowned");
    assert!(matches!(
        policy.authorize(&request, &owned),
        Err(ExtensionError::Ownership)
    ));
    request.params["threadId"] = json!("owned");
    request.method = "thread/shellCommand".into();
    assert!(matches!(
        policy.authorize(&request, &owned),
        Err(ExtensionError::HostAccess)
    ));
    assert_eq!(
        ExtensionPolicy::new(true)
            .authorize(&request, &owned)
            .unwrap(),
        RequestScope::Thread("owned".into())
    );
    request.method = "initialize".into();
    assert!(matches!(
        ExtensionPolicy::new(true).authorize(&request, &owned),
        Err(ExtensionError::Unsupported(_))
    ));
}

#[test]
fn negotiation_requires_explicit_callback_support_and_exact_or_wildcard_subscriptions() {
    let negotiation =
        Negotiation::from_meta(&json!({"codex": {"version": 1, "events": ["turn/completed"]}}))
            .unwrap()
            .unwrap();
    assert!(negotiation.wants_event("turn/completed"));
    assert!(!negotiation.wants_event("turn/started"));
    assert!(!negotiation.server_requests);
    assert!(Negotiation::from_meta(&json!({"codex": {"version": 2}})).is_err());
    assert!(
        Negotiation::from_meta(&json!({"codex": {"version": 1, "serverRequests": "true"}}))
            .is_err()
    );
}

#[test]
fn capability_negotiation_selects_lossless_callbacks_without_conflicting_legacy_opt_in() {
    let capability = json!({"codex":{"version":1,"serverRequests":true,
        "rawServerRequests":["item/permissions/requestApproval"],"sessionReset":true}});
    let negotiation = Negotiation::from_initialize_meta(&json!(null), &capability)
        .unwrap()
        .unwrap();
    assert!(negotiation.wants_raw_callback("item/permissions/requestApproval"));
    assert!(!negotiation.wants_raw_callback("item/fileChange/requestApproval"));
    assert!(negotiation.session_reset);
    assert_eq!(
        Negotiation::from_initialize_meta(&capability, &capability).unwrap(),
        Some(negotiation)
    );
    assert!(
        Negotiation::from_initialize_meta(&json!({"codex":{"version":1}}), &capability).is_err()
    );
    assert!(
        Negotiation::from_meta(&json!({"codex":{"version":1,"rawServerRequests":["*"]}})).is_err()
    );
}

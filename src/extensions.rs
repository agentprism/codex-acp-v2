//! Negotiation and authorization for the versioned Codex extension interface.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// An extension call preserves backend parameters, but cannot bypass session ownership.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestEnvelope {
    pub version: u32,
    #[serde(default)]
    pub session_id: Option<String>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, thiserror::Error)]
pub enum ExtensionError {
    #[error("unsupported Codex extension version; expected 1")]
    Version,
    #[error("invalid Codex extension negotiation: {0}")]
    Negotiation(String),
    #[error("Codex extension method is not supported: {0}")]
    Unsupported(String),
    #[error("use the standard ACP session lifecycle for {0}")]
    Lifecycle(String),
    #[error("Codex host methods require --allow-host-methods")]
    HostAccess,
    #[error("Codex extension request does not target an owned session")]
    Ownership,
    #[error("Codex extension parameters must be an object or null")]
    Parameters,
}

/// Explicit client opt-in. Absence of a namespace never implies callback support.
#[derive(Clone, Debug, Default)]
pub struct Negotiation {
    pub events: HashSet<String>,
    pub server_requests: bool,
}

impl Negotiation {
    /// Read `{ "codex": { "version": 1, "events": [...], "serverRequests": true } }`.
    pub fn from_meta(meta: &Value) -> Result<Option<Self>, ExtensionError> {
        let Some(codex) = meta.get("codex") else {
            return Ok(None);
        };
        if codex.get("version").and_then(Value::as_u64) != Some(1) {
            return Err(ExtensionError::Version);
        }
        let mut negotiation = Self::default();
        if let Some(events) = codex.get("events") {
            let events = events
                .as_array()
                .ok_or_else(|| ExtensionError::Negotiation("events must be an array".into()))?;
            if events.len() > 256 {
                return Err(ExtensionError::Negotiation(
                    "at most 256 event subscriptions are permitted".into(),
                ));
            }
            for event in events {
                let event = event.as_str().ok_or_else(|| {
                    ExtensionError::Negotiation("event subscriptions must be strings".into())
                })?;
                if event.is_empty() || event.len() > 256 {
                    return Err(ExtensionError::Negotiation(
                        "event names must contain 1–256 bytes".into(),
                    ));
                }
                negotiation.events.insert(event.into());
            }
        }
        if let Some(server_requests) = codex.get("serverRequests") {
            negotiation.server_requests = server_requests.as_bool().ok_or_else(|| {
                ExtensionError::Negotiation("serverRequests must be a boolean".into())
            })?;
        }
        Ok(Some(negotiation))
    }

    pub fn wants_event(&self, method: &str) -> bool {
        self.events.contains("*") || self.events.contains(method)
    }
}

/// Scope the frontend must retain when routing a request and its resulting events.
#[derive(Debug, PartialEq, Eq)]
pub enum RequestScope {
    Thread(String),
    Discovery,
    Host,
}

/// The operator, not an ACP peer, grants authority for host-global operations.
#[derive(Clone, Debug)]
pub struct ExtensionPolicy {
    allow_host_methods: bool,
}

impl ExtensionPolicy {
    pub fn new(allow_host_methods: bool) -> Self {
        Self { allow_host_methods }
    }

    pub fn capabilities(&self) -> Value {
        json!({
            "version": 1,
            "methods": ["_codex/request", "_codex/event", "_codex/serverRequest"],
            "hostMethods": self.allow_host_methods,
            "eventSubscriptions": true,
            "threadMethods": THREAD_METHODS,
            "discoveryMethods": DISCOVERY_METHODS,
            "hostMethodsAvailable": HOST_METHODS,
            "optionalThreadMethods": ["mcpServer/resource/read"],
            "lifecycleMethodsUseAcp": ["thread/start", "thread/resume", "thread/fork", "thread/unsubscribe"],
            "unsupportedMethods": ["initialize", "mock/experimentalMethod", "getConversationSummary", "gitDiffToRemote", "getAuthStatus"],
        })
    }

    /// Authorize known backend methods without mutating or filtering opaque payloads.
    pub fn authorize(
        &self,
        request: &RequestEnvelope,
        owned_threads: &HashSet<String>,
    ) -> Result<RequestScope, ExtensionError> {
        if request.version != 1 {
            return Err(ExtensionError::Version);
        }
        if !request.params.is_object() && !request.params.is_null() {
            return Err(ExtensionError::Parameters);
        }
        let method = request.method.as_str();
        if matches!(
            method,
            "thread/start" | "thread/resume" | "thread/fork" | "thread/unsubscribe"
        ) {
            return Err(ExtensionError::Lifecycle(method.into()));
        }
        let optional_thread = method == "mcpServer/resource/read";
        let has_thread = request
            .params
            .get("threadId")
            .is_some_and(|value| !value.is_null());
        let thread_method = THREAD_METHODS.contains(&method)
            || (optional_thread && has_thread)
            || method == "thread/shellCommand";
        let host_method = HOST_METHODS.contains(&method) || (optional_thread && !has_thread);
        let discovery = DISCOVERY_METHODS.contains(&method);
        if !thread_method && !host_method && !discovery {
            return Err(ExtensionError::Unsupported(method.into()));
        }
        let session_id = request.session_id.as_deref();
        if let Some(session_id) = session_id
            && !owned_threads.contains(session_id)
        {
            return Err(ExtensionError::Ownership);
        }
        let thread_id = request
            .params
            .get("threadId")
            .filter(|value| !value.is_null());
        if thread_method || thread_id.is_some() {
            let thread_id = thread_id
                .and_then(Value::as_str)
                .ok_or(ExtensionError::Ownership)?;
            if session_id != Some(thread_id) || !owned_threads.contains(thread_id) {
                return Err(ExtensionError::Ownership);
            }
        }
        // Known top-level references cannot smuggle a second session into a scoped call.
        for key in ["parentThreadId", "ancestorThreadId", "beforeThreadId"] {
            if let Some(id) = request.params.get(key).filter(|value| !value.is_null())
                && !id.as_str().is_some_and(|id| owned_threads.contains(id))
            {
                return Err(ExtensionError::Ownership);
            }
        }
        if let Some(ids) = request
            .params
            .get("threadIds")
            .filter(|value| !value.is_null())
        {
            let ids = ids.as_array().ok_or(ExtensionError::Ownership)?;
            if ids
                .iter()
                .any(|id| !id.as_str().is_some_and(|id| owned_threads.contains(id)))
            {
                return Err(ExtensionError::Ownership);
            }
        }
        if host_method {
            if !self.allow_host_methods {
                return Err(ExtensionError::HostAccess);
            }
            return Ok(RequestScope::Host);
        }
        if thread_method {
            return Ok(RequestScope::Thread(
                session_id.ok_or(ExtensionError::Ownership)?.into(),
            ));
        }
        Ok(RequestScope::Discovery)
    }
}

const THREAD_METHODS: &[&str] = &[
    "thread/archive",
    "thread/delete",
    "thread/name/set",
    "thread/goal/set",
    "thread/goal/get",
    "thread/goal/clear",
    "thread/queue/add",
    "thread/queue/list",
    "thread/queue/update",
    "thread/queue/delete",
    "thread/queue/reorder",
    "thread/queue/start",
    "thread/metadata/update",
    "thread/section/move",
    "thread/settings/update",
    "thread/memoryMode/set",
    "thread/unarchive",
    "thread/compact/start",
    "thread/approveGuardianDeniedAction",
    "thread/backgroundTerminals/clean",
    "thread/backgroundTerminals/list",
    "thread/backgroundTerminals/terminate",
    "thread/rollback",
    "thread/revert",
    "thread/searchOccurrences",
    "thread/read",
    "thread/turns/list",
    "thread/items/list",
    "thread/inject_items",
    "thread/increment_elicitation",
    "thread/decrement_elicitation",
    "turn/start",
    "turn/settings/update",
    "turn/steer",
    "turn/interrupt",
    "thread/realtime/start",
    "thread/realtime/appendAudio",
    "thread/realtime/appendText",
    "thread/realtime/appendSpeech",
    "thread/realtime/stop",
    "thread/timeline/list",
    "review/start",
    "mcpServer/event/stream/start",
    "mcpServer/tool/call",
];

const DISCOVERY_METHODS: &[&str] = &[
    "thread/list",
    "thread/loaded/list",
    "thread/search",
    "model/list",
    "modelProvider/capabilities/read",
    "experimentalFeature/list",
    "permissionProfile/list",
    "collaborationMode/list",
    "app/list",
    "app/installed",
    "app/read",
    "plugin/list",
    "plugin/search",
    "plugin/installed",
    "plugin/read",
    "plugin/skill/read",
    "skills/list",
    "hooks/list",
    "mcpServerStatus/list",
    "thread/realtime/listVoices",
    "environment/info",
    "environment/status",
];

const HOST_METHODS: &[&str] = &[
    "thread/delete",
    "mcpServer/event/stream/stop",
    "thread/shellCommand",
    "server/diagnostics",
    "memory/reset",
    "project/list",
    "project/read",
    "project/create",
    "project/import",
    "project/update",
    "project/move",
    "project/delete",
    "threadSection/list",
    "threadSection/create",
    "threadSection/update",
    "threadSection/delete",
    "skills/extraRoots/set",
    "skills/config/write",
    "marketplace/add",
    "marketplace/remove",
    "marketplace/upgrade",
    "plugin/reconcile",
    "plugin/share/save",
    "plugin/share/updateTargets",
    "plugin/share/list",
    "plugin/share/checkout",
    "plugin/share/delete",
    "plugin/install",
    "plugin/uninstall",
    "fs/readFile",
    "fs/writeFile",
    "fs/createDirectory",
    "fs/getMetadata",
    "fs/readDirectory",
    "fs/remove",
    "fs/copy",
    "fs/watch",
    "fs/unwatch",
    "experimentalFeature/enablement/set",
    "remoteControl/enable",
    "remoteControl/disable",
    "remoteControl/status/read",
    "remoteControl/pairing/start",
    "remoteControl/pairing/status",
    "remoteControl/client/list",
    "remoteControl/client/revoke",
    "environment/add",
    "mcpServer/oauth/login",
    "config/mcpServer/reload",
    "windowsSandbox/setupStart",
    "windowsSandbox/readiness",
    "account/login/start",
    "account/bedrock/discover",
    "account/bedrock/setup",
    "account/login/cancel",
    "account/logout",
    "account/read",
    "account/rateLimits/read",
    "account/rateLimitResetCredit/consume",
    "account/usage/read",
    "account/workspaceMessages/read",
    "account/sendAddCreditsNudgeEmail",
    "feedback/upload",
    "command/exec",
    "command/exec/write",
    "command/exec/terminate",
    "command/exec/resize",
    "process/spawn",
    "process/writeStdin",
    "process/kill",
    "process/resizePty",
    "config/read",
    "config/value/write",
    "config/batchWrite",
    "configRequirements/read",
    "externalAgentConfig/detect",
    "externalAgentConfig/import",
    "externalAgentConfig/import/recordHistory",
    "externalAgentConfig/import/readHistories",
    "fuzzyFileSearch",
    "fuzzyFileSearch/sessionStart",
    "fuzzyFileSearch/sessionUpdate",
    "fuzzyFileSearch/sessionStop",
];

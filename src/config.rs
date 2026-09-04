//! Session-local configuration projection; never writes the global Codex config.

use agent_client_protocol::schema::v2;
use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

pub const META_KEY: &str = "codex";

#[derive(Clone, Debug, Default)]
pub(crate) struct Configuration {
    pub settings: Map<String, Value>,
    pub models: Vec<Value>,
}

impl Configuration {
    /// Whether the patch leaves the currently projected effective settings
    /// unchanged. Codex suppresses duplicate settings notifications, so callers
    /// must not wait for an event for these acknowledged no-op requests.
    pub fn is_settings_noop(&self, patch: &Map<String, Value>) -> bool {
        self.settings_match(patch) && patch.get("collaborationMode").is_none_or(|value| {
            value.is_null() || self.settings.get("collaborationMode") == Some(value)
        })
    }

    /// Match a later authoritative snapshot against a desired effective patch.
    /// Null collaboration instructions select backend-private built-in text;
    /// match the mode/model/effort, not that unavailable text template.
    pub fn settings_match(&self, patch: &Map<String, Value>) -> bool {
        patch.iter().all(|(key, value)| {
            if matches!(key.as_str(), "threadId" | "multiAgentMode") {
                return true;
            }
            // Only serviceTier distinguishes an explicit null from omission.
            if value.is_null() && key != "serviceTier" {
                return true;
            }
            match key.as_str() {
                "permissions" => {
                    self.settings
                        .get("activePermissionProfile")
                        .and_then(|profile| profile.get("id"))
                        == Some(value)
                }
                "cwd" => value
                    .as_str()
                    .zip(self.settings.get(key).and_then(Value::as_str))
                    .is_some_and(|(left, right)| {
                        std::path::Path::new(left) == std::path::Path::new(right)
                    }),
                "sandboxPolicy" => {
                    self.settings
                            .get(key)
                            .is_some_and(|current| same_sandbox(current, value))
                }
                "collaborationMode" => self.settings.get(key).is_some_and(|current| {
                    current["mode"] == value["mode"]
                        && current["settings"]["model"] == value["settings"]["model"]
                        && current["settings"]["reasoning_effort"] == value["settings"]["reasoning_effort"]
                        && (value["settings"]["developer_instructions"].is_null()
                            || current["settings"]["developer_instructions"] == value["settings"]["developer_instructions"])
                }),
                "serviceTier" => self.settings.get(key).unwrap_or(&Value::Null) == value,
                _ => self.settings.get(key) == Some(value),
            }
        })
    }

    pub fn from_response(response: &Value, models: Vec<Value>) -> Self {
        let mut settings = response.as_object().cloned().unwrap_or_default();
        settings.remove("thread");
        if let Some(value) = settings.remove("sandbox") {
            settings.insert("sandboxPolicy".into(), value);
        }
        if let Some(value) = settings.remove("reasoningEffort") {
            settings.insert("effort".into(), value);
        }
        Self { settings, models }
    }

    pub fn options(&self) -> Vec<v2::SessionConfigOption> {
        let current_model = self.string("model", "default");
        let mut models: Vec<_> = self
            .models
            .iter()
            .filter_map(|model| {
                Some(v2::SessionConfigSelectOption::new(
                    model.get("model")?.as_str()?,
                    model
                        .get("displayName")
                        .and_then(Value::as_str)
                        .unwrap_or("Model"),
                ))
            })
            .collect();
        if !models
            .iter()
            .any(|model| model.value.to_string() == current_model)
        {
            models.push(v2::SessionConfigSelectOption::new(
                current_model,
                current_model,
            ));
        }
        let mut options = vec![
            v2::SessionConfigOption::select("model", "Model", current_model, models)
                .category(v2::SessionConfigOptionCategory::Model),
        ];
        let catalog = self
            .models
            .iter()
            .find(|model| model["model"] == current_model);
        if let Some(efforts) =
            catalog.and_then(|model| model["supportedReasoningEfforts"].as_array())
        {
            let efforts: Vec<_> = efforts
                .iter()
                .filter_map(|effort| {
                    let value = effort["reasoningEffort"].as_str()?;
                    Some(
                        v2::SessionConfigSelectOption::new(value, value)
                            .description(effort["description"].as_str().unwrap_or(value)),
                    )
                })
                .collect();
            if !efforts.is_empty() {
                let default = catalog
                    .and_then(|model| model["defaultReasoningEffort"].as_str())
                    .unwrap_or("medium");
                options.push(v2::SessionConfigOption::select(
                    "effort",
                    "Reasoning effort",
                    self.string("effort", default),
                    efforts,
                ));
            }
        }
        let mut tiers = vec![v2::SessionConfigSelectOption::new("default", "Default")];
        if let Some(values) = catalog.and_then(|model| model["serviceTiers"].as_array()) {
            tiers.extend(values.iter().filter_map(|tier| {
                Some(v2::SessionConfigSelectOption::new(
                    tier["id"].as_str()?,
                    tier["name"].as_str()?,
                ))
            }));
        }
        let current_tier = self.string("serviceTier", "default");
        if !tiers
            .iter()
            .any(|tier| tier.value.to_string() == current_tier)
        {
            tiers.push(v2::SessionConfigSelectOption::new(
                current_tier,
                current_tier,
            ));
        }
        options.push(v2::SessionConfigOption::select(
            "serviceTier",
            "Service tier",
            current_tier,
            tiers,
        ));
        if let Some(policy) = self.settings.get("approvalPolicy").and_then(Value::as_str) {
            options.push(select(
                "approvalPolicy",
                "Approval policy",
                policy,
                &["untrusted", "on-failure", "on-request", "never"],
            ));
        }
        let sandbox = self
            .settings
            .get("sandboxPolicy")
            .and_then(|value| value["type"].as_str())
            .unwrap_or("readOnly");
        let mut choices = vec!["readOnly", "workspaceWrite", "dangerFullAccess"];
        if !choices.contains(&sandbox) {
            choices.push(sandbox);
        }
        options.push(select("sandbox", "Sandbox", sandbox, &choices));
        let mode = self
            .settings
            .get("collaborationMode")
            .and_then(|value| value["mode"].as_str())
            .unwrap_or("default");
        options.push(
            select("mode", "Collaboration mode", mode, &["default", "plan"])
                .category(v2::SessionConfigOptionCategory::Mode),
        );
        options
    }

    pub fn patch(
        &self,
        id: &str,
        value: &v2::SessionConfigOptionValue,
    ) -> Result<Map<String, Value>> {
        let v2::SessionConfigOptionValue::Id { value } = value else {
            bail!("this option requires an id value")
        };
        let value = value.to_string();
        let option = self
            .options()
            .into_iter()
            .find(|option| option.config_id.to_string() == id)
            .context("unknown session configuration option")?;
        let encoded = serde_json::to_value(option)?;
        let valid = encoded["options"]
            .as_array()
            .is_some_and(|choices| choices.iter().any(|choice| choice["value"] == value));
        if !valid {
            bail!("unsupported value for session configuration option {id}");
        }
        let mut patch = Map::new();
        match id {
            "model" | "effort" | "approvalPolicy" => {
                patch.insert(id.into(), json!(value));
            }
            "serviceTier" => {
                patch.insert(
                    id.into(),
                    if value == "default" {
                        Value::Null
                    } else {
                        json!(value)
                    },
                );
            }
            "sandbox" => {
                let policy = match value.as_str() {
                    "readOnly" => json!({"type":"readOnly","networkAccess":false}),
                    "workspaceWrite" => {
                        json!({"type":"workspaceWrite","writableRoots":[],"networkAccess":false,"excludeTmpdirEnvVar":false,"excludeSlashTmp":false})
                    }
                    "dangerFullAccess" => json!({"type":"dangerFullAccess"}),
                    _ => bail!("this sandbox is backend-managed; choose a supported preset"),
                };
                patch.insert("sandboxPolicy".into(), policy);
            }
            "mode" => {
                let instructions = self
                    .settings
                    .get("collaborationMode")
                    .and_then(|mode| mode.pointer("/settings/developer_instructions"))
                    .cloned()
                    .unwrap_or(Value::Null);
                patch.insert("collaborationMode".into(), json!({"mode":value,"settings":{
                    "model":self.string("model", "default"),"reasoning_effort":self.settings.get("effort"),"developer_instructions":instructions
                }}));
            }
            _ => bail!("unknown session configuration option"),
        }
        Ok(patch)
    }

    fn string<'a>(&'a self, key: &str, fallback: &'a str) -> &'a str {
        self.settings
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or(fallback)
    }
}

fn same_sandbox(current: &Value, requested: &Value) -> bool {
    if current["type"] != requested["type"] {
        return false;
    }
    let fields: &[&str] = match requested["type"].as_str() {
        Some("dangerFullAccess") => &["type"],
        Some("readOnly" | "externalSandbox") => &["type", "networkAccess"],
        Some("workspaceWrite") => &["type", "networkAccess", "writableRoots", "excludeTmpdirEnvVar", "excludeSlashTmp"],
        _ => return current == requested,
    };
    if [current, requested].iter().any(|value| value.as_object().is_none_or(|object| {
        object.keys().any(|key| !fields.contains(&key.as_str()))
    })) {
        return false;
    }
    match requested["type"].as_str() {
        Some("dangerFullAccess") => true,
        Some("readOnly") => {
            current["networkAccess"].as_bool().unwrap_or_default()
                == requested["networkAccess"].as_bool().unwrap_or_default()
        }
        Some("externalSandbox") => {
            current["networkAccess"].as_str().unwrap_or("restricted")
                == requested["networkAccess"].as_str().unwrap_or("restricted")
        }
        Some("workspaceWrite") => {
            ["networkAccess", "excludeTmpdirEnvVar", "excludeSlashTmp"]
                .iter()
                .all(|key| {
                    current[key].as_bool().unwrap_or_default()
                        == requested[key].as_bool().unwrap_or_default()
                })
                && current.get("writableRoots").unwrap_or(&json!([]))
                    == requested.get("writableRoots").unwrap_or(&json!([]))
        }
        _ => current == requested,
    }
}

fn select(id: &str, label: &str, current: &str, choices: &[&str]) -> v2::SessionConfigOption {
    v2::SessionConfigOption::select(
        id,
        label,
        current,
        choices
            .iter()
            .map(|value| v2::SessionConfigSelectOption::new(*value, *value))
            .collect::<Vec<_>>(),
    )
}

pub(crate) fn metadata(
    meta: Option<&v2::Meta>,
    section: &str,
    negotiated: bool,
) -> Result<Map<String, Value>> {
    let Some(codex) = meta.and_then(|meta| meta.get(META_KEY)) else {
        return Ok(Map::new());
    };
    if !negotiated {
        bail!("Codex metadata requires the codex extension negotiation")
    }
    let object = codex.as_object().context("_meta.codex must be an object")?;
    for key in object.keys() {
        if key != section {
            bail!("unsupported _meta.codex key {key}; expected {section}");
        }
    }
    match object.get(section) {
        None => Ok(Map::new()),
        Some(value) => value
            .as_object()
            .cloned()
            .context("Codex metadata section must be an object"),
    }
}

#[derive(Clone, Copy)]
pub(crate) enum ThreadOperation {
    Start,
    Resume,
    Fork,
}

pub(crate) fn thread_parameters(
    operation: ThreadOperation,
    cwd: &v2::AbsolutePath,
    roots: &[v2::AbsolutePath],
    servers: &[v2::McpServer],
    mut extra: Map<String, Value>,
) -> Result<Value> {
    const COMMON: &[&str] = &[
        "model",
        "modelProvider",
        "serviceTier",
        "approvalPolicy",
        "approvalsReviewer",
        "sandbox",
        "permissions",
        "config",
        "baseInstructions",
        "developerInstructions",
    ];
    let allowed = match operation {
        ThreadOperation::Start => &["ephemeral", "historyMode", "environments", "dynamicTools",
            "selectedCapabilityRoots", "experimentalRawEvents", "allowProviderModelFallback",
            "serviceName", "sessionStartSource", "threadSource", "projectId", "personality"][..],
        ThreadOperation::Resume => &["personality"],
        ThreadOperation::Fork => &["lastTurnId", "beforeTurnId", "threadSource", "deferGoalContinuation", "ephemeral"],
    };
    for key in extra.keys() {
        if !COMMON.contains(&key.as_str()) && !allowed.contains(&key.as_str()) {
            bail!("unsupported field for this thread operation: {key}");
        }
    }
    if extra.get("lastTurnId").is_some_and(|value| !value.is_null())
        && extra.get("beforeTurnId").is_some_and(|value| !value.is_null())
    {
        bail!("lastTurnId and beforeTurnId cannot be combined");
    }
    extra.insert("cwd".into(), serde_json::to_value(cwd)?);
    let all_roots: Vec<_> = std::iter::once(cwd).chain(roots).collect();
    extra.insert(
        "runtimeWorkspaceRoots".into(),
        serde_json::to_value(all_roots)?,
    );
    let config = extra
        .entry("config")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("config must be an object")?;
    // Thread config is downstream-owned but global/identity overrides are not session knobs.
    for key in config.keys() {
        if [
            "codex_home",
            "cli_auth_credentials_store",
            "mcp_oauth_credentials_store",
            "forced_login_method",
            "forced_chatgpt_workspace_id",
        ]
        .iter()
        .any(|forbidden| key == forbidden || key.starts_with(&format!("{forbidden}.")))
        {
            bail!("configuration key {key} is not session-scoped");
        }
    }
    let mut mcp = Map::new();
    for server in servers {
        let value = serde_json::to_value(server)?;
        let name = value["name"]
            .as_str()
            .context("MCP server requires a name")?;
        if name.is_empty() || name.contains('.') || mcp.contains_key(name) {
            bail!("MCP server names must be unique, nonempty, and contain no dots");
        }
        let mapped = match value["type"].as_str() {
            Some("stdio") | None => {
                let env = value["env"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|entry| {
                        Some((entry["name"].as_str()?.to_owned(), entry["value"].clone()))
                    })
                    .collect::<Map<_, _>>();
                json!({"command":value["command"],"args":value.get("args").cloned().unwrap_or_else(||json!([])),"env":env})
            }
            Some("http") => {
                let headers = value["headers"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|entry| {
                        Some((entry["name"].as_str()?.to_owned(), entry["value"].clone()))
                    })
                    .collect::<Map<_, _>>();
                json!({"url":value["url"],"http_headers":headers})
            }
            _ => bail!("unsupported MCP transport; use stdio or streamable HTTP"),
        };
        mcp.insert(name.to_owned(), mapped);
    }
    if !mcp.is_empty() {
        if config.contains_key("mcp_servers") {
            bail!("MCP servers cannot be supplied in both metadata and mcpServers");
        }
        config.insert("mcp_servers".into(), Value::Object(mcp));
    }
    Ok(Value::Object(extra))
}

pub(crate) fn turn_parameters(
    meta: Option<&v2::Meta>,
    negotiated: bool,
) -> Result<Map<String, Value>> {
    let extra = metadata(meta, "turn", negotiated)?;
    for key in extra.keys() {
        if ![
            "outputSchema",
            "serviceTierForTurn",
            "additionalContext",
            "turnTrigger",
            "cyberAccessProgram",
        ]
        .contains(&key.as_str())
        {
            bail!(
                "{key} is not a supported per-turn override; use session configuration or _codex/request for explicit backend semantics"
            );
        }
    }
    Ok(extra)
}

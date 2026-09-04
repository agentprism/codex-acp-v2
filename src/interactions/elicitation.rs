use agent_client_protocol::schema::v2;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value, json};

use super::Interaction;

/// Tracks schema/answer correspondence without elevating client data to instructions.
pub struct ElicitationResolver {
    kind: Kind,
}

enum Kind {
    Questions(Vec<Question>),
    Mcp { schema: Option<Value> },
}

struct Question {
    id: String,
    choices: Option<Vec<String>>,
}

impl ElicitationResolver {
    /// Convert a known response; reject invalid question answers instead of silently coercing them.
    pub fn resolve(self, response: v2::CreateElicitationResponse) -> Result<Value> {
        ensure!(
            serde_json::to_vec(&response)?.len() <= 1024 * 1024,
            "elicitation response exceeds 1 MiB"
        );
        match self.kind {
            Kind::Mcp { schema } => match response.action {
                v2::ElicitationAction::Accept(value) => {
                    if let Some(schema) = schema {
                        super::schema::validate(&schema, &serde_json::to_value(&value.content)?)?;
                    }
                    Ok(json!({"action":"accept", "content":value.content, "_meta":response.meta}))
                }
                v2::ElicitationAction::Decline => {
                    Ok(json!({"action":"decline", "content":null,"_meta":response.meta}))
                }
                v2::ElicitationAction::Cancel => {
                    Ok(json!({"action":"cancel", "content":null,"_meta":response.meta}))
                }
                _ => bail!("unsupported elicitation action"),
            },
            Kind::Questions(questions) => {
                let content = match response.action {
                    v2::ElicitationAction::Accept(value) => value
                        .content
                        .context("accepted questions require answers")?,
                    v2::ElicitationAction::Decline | v2::ElicitationAction::Cancel => {
                        return Ok(json!({"answers":{}}));
                    }
                    _ => bail!("unsupported elicitation action"),
                };
                ensure!(
                    content.len() == questions.len(),
                    "answer keys do not match questions"
                );
                let mut answers = Map::new();
                for question in questions {
                    let value = content
                        .get(&question.id)
                        .context("missing question answer")?;
                    let v2::ElicitationContentValue::String(answer) = value else {
                        bail!("question answer must be a string")
                    };
                    ensure!(
                        question
                            .choices
                            .as_ref()
                            .is_none_or(|choices| choices.contains(answer)),
                        "answer is not an offered choice"
                    );
                    answers.insert(question.id, json!({"answers":[answer]}));
                }
                Ok(json!({"answers":answers}))
            }
        }
    }

    /// Safe cancellation, preserving the backend's distinct response shapes.
    pub fn cancelled(self) -> Value {
        match self.kind {
            Kind::Questions(_) => json!({"answers":{}}),
            Kind::Mcp { .. } => json!({"action":"cancel", "content":null, "_meta":null}),
        }
    }
}

pub(super) fn translate(
    session_id: &str,
    method: &str,
    params: &Value,
    capabilities: &v2::ClientCapabilities,
) -> Result<Option<Interaction>> {
    let Some(capabilities) = &capabilities.elicitation else {
        return Ok(None);
    };
    let scope = v2::ElicitationSessionScope::new(session_id);
    if method == "item/tool/requestUserInput" {
        if !capabilities.supports_form() {
            return Ok(None);
        }
        let questions = params["questions"]
            .as_array()
            .context("missing questions")?;
        ensure!(
            !questions.is_empty() && questions.len() <= 32,
            "invalid question count"
        );
        let mut schema = v2::ElicitationSchema::new();
        let mut tracked = Vec::new();
        for question in questions {
            // Standard ACP forms do not promise masking or handling secrets; use the extension.
            if question["isSecret"] == true {
                return Ok(None);
            }
            let id = question["id"]
                .as_str()
                .context("question missing id")?
                .to_owned();
            ensure!(
                !schema.properties.contains_key(&id),
                "duplicate question id"
            );
            let mut property = v2::StringPropertySchema::new()
                .title(question["header"].as_str().unwrap_or("Question"));
            let mut description = question["question"]
                .as_str()
                .context("question missing text")?
                .to_owned();
            let choices = question["options"]
                .as_array()
                .map(|options| {
                    options
                        .iter()
                        .map(|option| {
                            let label = option["label"].as_str().context("option missing label")?;
                            description.push_str(&format!(
                                "\n{label}: {}",
                                option["description"].as_str().unwrap_or_default()
                            ));
                            Ok(label.to_owned())
                        })
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()?;
            let choices =
                choices.filter(|choices| !choices.is_empty() && question["isOther"] != true);
            property = property.description(description);
            property.enum_values = choices.clone();
            schema
                .properties
                .insert(id.clone(), v2::ElicitationPropertySchema::String(property));
            tracked.push(Question { id, choices });
        }
        schema.required = Some(tracked.iter().map(|question| question.id.clone()).collect());
        return Ok(Some(Interaction::Elicitation {
            request: v2::CreateElicitationRequest::new(
                v2::ElicitationFormMode::new(scope, schema),
                "Codex needs your input",
            ),
            resolver: ElicitationResolver {
                kind: Kind::Questions(tracked),
            },
        }));
    }
    let message = params["message"]
        .as_str()
        .context("elicitation missing message")?;
    let mut response_schema = None;
    let mode: v2::ElicitationMode = match params["mode"].as_str() {
        Some("form") if capabilities.supports_form() => {
            let original = normalize_schema(&params["requestedSchema"])?;
            let schema: v2::ElicitationSchema = serde_json::from_value(original.clone())
                .context("MCP schema is not supported by ACP")?;
            // ACP deliberately tolerates some malformed fields. Ensure projection does not erase a constraint.
            if !preserves(&original, &serde_json::to_value(&schema)?) {
                return Ok(None);
            }
            if !super::schema::supported(&original) {
                return Ok(None);
            }
            if schema
                .properties
                .values()
                .any(|property| matches!(property, v2::ElicitationPropertySchema::Other(_)))
            {
                return Ok(None);
            }
            response_schema = Some(original);
            v2::ElicitationFormMode::new(scope, schema).into()
        }
        Some("url") if capabilities.supports_url() => {
            let url = params["url"]
                .as_str()
                .context("URL elicitation missing URL")?;
            ensure!(
                url.starts_with("https://") || url.starts_with("http://"),
                "unsupported elicitation URL scheme"
            );
            v2::ElicitationUrlMode::new(
                scope,
                params["elicitationId"]
                    .as_str()
                    .context("missing elicitation id")?,
                url,
            )
            .into()
        }
        _ => return Ok(None),
    };
    Ok(Some(Interaction::Elicitation {
        request: v2::CreateElicitationRequest::new(mode, message),
        resolver: ElicitationResolver {
            kind: Kind::Mcp {
                schema: response_schema,
            },
        },
    }))
}

fn normalize_schema(schema: &Value) -> Result<Value> {
    ensure!(
        schema["type"] == "object",
        "elicitation schema must describe an object"
    );
    let mut schema = schema.clone();
    for property in schema["properties"]
        .as_object_mut()
        .context("elicitation schema has no properties")?
        .values_mut()
    {
        // MCP's legacy enumNames is semantically equivalent to ACP's titled oneOf.
        if let (Some(names), Some(values)) = (
            property["enumNames"].as_array(),
            property["enum"].as_array(),
        ) {
            ensure!(
                names.len() == values.len(),
                "enum titles do not match values"
            );
            let one_of = values
                .iter()
                .zip(names)
                .map(|(value, name)| json!({"const":value,"title":name}))
                .collect::<Vec<_>>();
            let object = property.as_object_mut().context("invalid property")?;
            object.remove("enumNames");
            object.remove("enum");
            object.insert("oneOf".into(), Value::Array(one_of));
        }
    }
    Ok(schema)
}

fn preserves(original: &Value, projected: &Value) -> bool {
    match original {
        Value::Object(fields) => fields.iter().all(|(key, value)| {
            value.is_null()
                || projected
                    .get(key)
                    .is_some_and(|other| preserves(value, other))
        }),
        Value::Array(values) => projected.as_array().is_some_and(|other| {
            values.len() == other.len()
                && values
                    .iter()
                    .zip(other)
                    .all(|(left, right)| preserves(left, right))
        }),
        _ => original == projected,
    }
}

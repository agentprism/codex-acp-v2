use anyhow::{Context, Result, ensure};
use serde_json::Value;

/// Constraints requiring an external parser stay on the lossless extension path.
pub(super) fn supported(schema: &Value) -> bool {
    schema["properties"].as_object().is_some_and(|properties| {
        properties.values().all(|property| {
            !property
                .get("pattern")
                .is_some_and(|value| !value.is_null())
                && !property.get("format").is_some_and(|value| !value.is_null())
        })
    })
}

pub(super) fn validate(schema: &Value, content: &Value) -> Result<()> {
    let empty = serde_json::Map::new();
    let content = if content.is_null() {
        &empty
    } else {
        content
            .as_object()
            .context("elicitation content must be an object")?
    };
    let properties = schema["properties"]
        .as_object()
        .context("invalid form properties")?;
    if let Some(required) = schema["required"].as_array() {
        ensure!(
            required
                .iter()
                .all(|key| key.as_str().is_some_and(|key| content.contains_key(key))),
            "missing required form answer"
        );
    }
    for (key, value) in content {
        let property = properties.get(key).context("unexpected form answer")?;
        let valid_type = match property["type"].as_str() {
            Some("string") => value.is_string(),
            Some("integer") => value.as_i64().is_some(),
            Some("number") => value.is_number(),
            Some("boolean") => value.is_boolean(),
            Some("array") => value
                .as_array()
                .is_some_and(|values| values.iter().all(Value::is_string)),
            _ => false,
        };
        ensure!(valid_type, "form answer has incorrect type");
        validate_choice(property, value)?;
        if let Some(value) = value.as_str() {
            validate_size(property, "minLength", "maxLength", value.chars().count())?;
        }
        if let Some(value) = value.as_f64() {
            ensure!(
                property["minimum"]
                    .as_f64()
                    .is_none_or(|minimum| value >= minimum),
                "form number below minimum"
            );
            ensure!(
                property["maximum"]
                    .as_f64()
                    .is_none_or(|maximum| value <= maximum),
                "form number above maximum"
            );
        }
        if let Some(values) = value.as_array() {
            validate_size(property, "minItems", "maxItems", values.len())?;
            for (index, value) in values.iter().enumerate() {
                validate_choice(&property["items"], value)?;
                ensure!(
                    property["uniqueItems"] != true || !values[..index].contains(value),
                    "duplicate multiselect answer"
                );
            }
        }
    }
    Ok(())
}

fn validate_choice(schema: &Value, value: &Value) -> Result<()> {
    if let Some(choices) = schema["enum"].as_array() {
        ensure!(
            choices.contains(value),
            "answer is not an offered form choice"
        );
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(choices) = schema[key].as_array() {
            ensure!(
                choices.iter().any(|choice| choice["const"] == *value),
                "answer is not an offered titled choice"
            );
        }
    }
    Ok(())
}

fn validate_size(schema: &Value, min_key: &str, max_key: &str, count: usize) -> Result<()> {
    ensure!(
        schema[min_key]
            .as_u64()
            .is_none_or(|min| count as u64 >= min),
        "form answer shorter than minimum"
    );
    ensure!(
        schema[max_key]
            .as_u64()
            .is_none_or(|max| count as u64 <= max),
        "form answer longer than maximum"
    );
    Ok(())
}

use anyhow::{bail, Context, Result};
use serde_json::Value;

/// Lower nullable type arrays for Gemma's native declarations. The caller's original schema
/// remains authoritative for execution-time validation and is never mutated.
pub fn normalize_tool_schema(schema: &Value) -> Result<Value> {
    let mut schema = schema.clone();
    lower(&mut schema, "parameters")?;
    let object = schema
        .as_object_mut()
        .context("Tool parameters must use an object schema")?;
    if !object.contains_key("type") && object.contains_key("properties") {
        object.insert("type".into(), Value::String("object".into()));
    }
    if object.get("type").and_then(Value::as_str) != Some("object") {
        bail!("Tool parameter schema must have type object");
    }
    Ok(schema)
}

fn lower(schema: &mut Value, path: &str) -> Result<()> {
    let object = schema
        .as_object_mut()
        .with_context(|| format!("Unsupported non-object schema at {path}"))?;
    // The common MCP nullable union is equivalent to a single concrete type
    // plus null. Preserve the original schema separately for execution.
    for union in ["anyOf", "oneOf"] {
        if let Some(branches) = object.remove(union) {
            let branches = branches
                .as_array()
                .with_context(|| format!("Invalid {union} at {path}"))?;
            let nulls: Vec<_> = branches
                .iter()
                .filter(|v| {
                    v.as_object().is_some_and(|o| {
                        o.len() == 1 && o.get("type").and_then(Value::as_str) == Some("null")
                    })
                })
                .collect();
            let concrete: Vec<_> = branches.iter().filter(|v| !nulls.contains(v)).collect();
            if branches.len() != 2 || nulls.len() != 1 || concrete.len() != 1 {
                bail!("Unsupported Gemma tool schema construct {union} at {path}; only a nullable single-type union can be lowered");
            }
            let branch = concrete[0]
                .as_object()
                .context("Nullable union branch must be an object")?;
            let kind = branch
                .get("type")
                .and_then(Value::as_str)
                .context("Nullable union branch needs a concrete type")?;
            if kind == "null" || object.contains_key("type") || object.contains_key("nullable") {
                bail!("Ambiguous nullable union at {path}");
            }
            for (key, value) in branch {
                if let Some(existing) = object.get(key) {
                    if existing != value {
                        bail!("Conflicting nullable union constraint {key} at {path}");
                    }
                } else {
                    object.insert(key.clone(), value.clone());
                }
            }
            object.insert("nullable".into(), Value::Bool(true));
        }
    }
    for key in [
        "allOf",
        "not",
        "$ref",
        "$dynamicRef",
        "if",
        "then",
        "else",
        "prefixItems",
        "patternProperties",
        "dependentSchemas",
    ] {
        if object.contains_key(key) {
            bail!("Unsupported Gemma tool schema construct {key} at {path}; simplify the schema before registration");
        }
    }
    if let Some(types) = object.get("type").and_then(Value::as_array) {
        let mut concrete = Vec::new();
        let mut nullable = false;
        for value in types {
            let name = value
                .as_str()
                .with_context(|| format!("Schema type names must be strings at {path}"))?;
            if name == "null" {
                nullable = true;
            } else if !concrete.contains(&name) {
                concrete.push(name);
            }
        }
        if concrete.len() > 1 {
            bail!("Unsupported multi-type union at {path}; only a single type with optional null is supported");
        }
        let kind = concrete
            .first()
            .copied()
            .or(if nullable { Some("null") } else { None })
            .with_context(|| format!("Empty type array at {path}"))?
            .to_owned();
        object.insert("type".into(), Value::String(kind));
        if nullable && object.get("type").and_then(Value::as_str) != Some("null") {
            object.insert("nullable".into(), Value::Bool(true));
        }
    }
    if !object.contains_key("type") && object.contains_key("properties") {
        object.insert("type".into(), Value::String("object".into()));
    }
    if let Some(kind) = object.get("type") {
        let kind = kind
            .as_str()
            .with_context(|| format!("Schema type must be a string at {path}"))?;
        if !matches!(
            kind,
            "object" | "array" | "string" | "integer" | "number" | "boolean" | "null"
        ) {
            bail!("Unsupported schema type {kind} at {path}");
        }
    }
    if let Some(properties) = object.get_mut("properties") {
        for (name, property) in properties
            .as_object_mut()
            .with_context(|| format!("Schema properties must be an object at {path}"))?
        {
            lower(property, &format!("{path}.{name}"))?;
        }
    }
    if let Some(items) = object.get_mut("items") {
        lower(items, &format!("{path}.items"))?;
    }
    if let Some(additional) = object
        .get_mut("additionalProperties")
        .filter(|v| v.is_object())
    {
        lower(additional, &format!("{path}.additionalProperties"))?;
    }
    for key in ["$defs", "definitions"] {
        if let Some(defs) = object.get_mut(key) {
            for (name, definition) in defs
                .as_object_mut()
                .with_context(|| format!("Invalid {key} at {path}"))?
            {
                lower(definition, &format!("{path}.{key}.{name}"))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nullable_fields_are_lowered_without_mutating_execution_schema() {
        let original = json!({"type":"object","properties":{"expected_sha256":{"type":["string","null"]},"nested":{"type":"array","items":{"type":"object","properties":{"x":{"type":["integer","null"]}}}}},"required":["expected_sha256"]});
        let lowered = normalize_tool_schema(&original).unwrap();
        assert_eq!(
            lowered["properties"]["expected_sha256"],
            json!({"type":"string","nullable":true})
        );
        assert_eq!(
            lowered["properties"]["nested"]["items"]["properties"]["x"],
            json!({"type":"integer","nullable":true})
        );
        assert!(original["properties"]["expected_sha256"]["type"].is_array());
        assert_eq!(lowered["required"], original["required"]);
    }

    #[test]
    fn genuine_unions_and_references_are_rejected() {
        assert!(normalize_tool_schema(
            &json!({"type":"object","properties":{"x":{"type":["string","integer"]}}})
        )
        .is_err());
        assert!(normalize_tool_schema(&json!({"type":"object","properties":{"x":{"oneOf":[{"type":"string"},{"type":"integer"}]}}})).is_err());
        assert!(normalize_tool_schema(
            &json!({"type":"object","properties":{"x":{"$ref":"#/$defs/x"}}})
        )
        .is_err());
    }

    #[test]
    fn lowers_only_equivalent_nullable_mcp_unions() {
        let original = json!({"type":"object","properties":{"path":{"description":"Optional path","anyOf":[{"type":"string"},{"type":"null"}]}}});
        let result = normalize_tool_schema(&original).unwrap();
        assert_eq!(result["properties"]["path"]["type"], "string");
        assert_eq!(result["properties"]["path"]["nullable"], true);
        assert!(original["properties"]["path"]["anyOf"].is_array());
        assert!(normalize_tool_schema(&json!({"type":"object","properties":{"x":{"anyOf":[{"type":"string"},{"type":"integer"}]}}})).is_err());
    }
}

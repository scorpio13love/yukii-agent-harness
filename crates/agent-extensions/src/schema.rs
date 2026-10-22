//! 插件参数支持有限、可审计的 JSON Schema 子集；不支持的关键词直接拒绝。
use serde_json::Value;

pub fn validate_schema(schema: &Value, depth: usize) -> Result<(), String> {
    if depth > 8 {
        return Err("Schema 嵌套过深".into());
    }
    let object = schema.as_object().ok_or("Schema 必须为对象")?;
    let allowed = [
        "type",
        "properties",
        "required",
        "additionalProperties",
        "items",
        "minItems",
        "maxItems",
        "minLength",
        "maxLength",
        "minimum",
        "maximum",
        "enum",
        "description",
    ];
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("Schema 包含尚不支持的关键词".into());
    }
    match schema["type"].as_str() {
        Some("object") => {
            if schema["additionalProperties"] != false {
                return Err("对象 Schema 必须禁止额外字段".into());
            }
            let properties = schema["properties"].as_object().ok_or("缺少 properties")?;
            if properties.len() > 32 {
                return Err("Schema 字段过多".into());
            }
            for value in properties.values() {
                validate_schema(value, depth + 1)?;
            }
            if let Some(required) = schema.get("required") {
                for name in required.as_array().ok_or("required 必须为数组")? {
                    if !name
                        .as_str()
                        .is_some_and(|key| properties.contains_key(key))
                    {
                        return Err("required 引用了不存在的字段".into());
                    }
                }
            }
        }
        Some("array") => validate_schema(&schema["items"], depth + 1)?,
        Some("string" | "number" | "integer" | "boolean") => {}
        _ => return Err("Schema 类型不支持".into()),
    }
    for key in ["minItems", "maxItems", "minLength", "maxLength"] {
        if schema
            .get(key)
            .is_some_and(|value| value.as_u64().is_none())
        {
            return Err("Schema 长度限制必须为非负整数".into());
        }
    }
    for key in ["minimum", "maximum"] {
        if schema.get(key).is_some_and(|value| !value.is_number()) {
            return Err("Schema 数值限制无效".into());
        }
    }
    if schema.get("enum").is_some_and(|value| {
        value
            .as_array()
            .is_none_or(|values| values.is_empty() || values.len() > 64)
    }) {
        return Err("Schema enum 无效".into());
    }
    Ok(())
}

pub fn validate_value(schema: &Value, value: &Value) -> Result<(), String> {
    let invalid = || "工具参数未通过插件 Schema 校验".to_owned();
    if let Some(values) = schema["enum"].as_array()
        && !values.contains(value)
    {
        return Err(invalid());
    }
    match schema["type"].as_str() {
        Some("object") => {
            let values = value.as_object().ok_or_else(invalid)?;
            let properties = schema["properties"].as_object().ok_or_else(invalid)?;
            if values.keys().any(|key| !properties.contains_key(key)) {
                return Err(invalid());
            }
            if let Some(required) = schema["required"].as_array() {
                for key in required {
                    if !key.as_str().is_some_and(|key| values.contains_key(key)) {
                        return Err(invalid());
                    }
                }
            }
            for (key, value) in values {
                validate_value(&properties[key], value)?;
            }
        }
        Some("array") => {
            let values = value.as_array().ok_or_else(invalid)?;
            check_length(schema, values.len(), "minItems", "maxItems")?;
            for value in values {
                validate_value(&schema["items"], value)?;
            }
        }
        Some("string") => check_length(
            schema,
            value.as_str().ok_or_else(invalid)?.chars().count(),
            "minLength",
            "maxLength",
        )?,
        Some("number" | "integer") => {
            let number = value.as_f64().ok_or_else(invalid)?;
            if schema["type"] == "integer" && !value.is_i64() && !value.is_u64() {
                return Err(invalid());
            }
            if schema["minimum"].as_f64().is_some_and(|min| number < min)
                || schema["maximum"].as_f64().is_some_and(|max| number > max)
            {
                return Err(invalid());
            }
        }
        Some("boolean") => {
            if !value.is_boolean() {
                return Err(invalid());
            }
        }
        _ => return Err(invalid()),
    }
    Ok(())
}
fn check_length(schema: &Value, length: usize, min: &str, max: &str) -> Result<(), String> {
    if schema[min]
        .as_u64()
        .is_some_and(|value| length < (value as usize))
        || schema[max]
            .as_u64()
            .is_some_and(|value| length > (value as usize))
    {
        return Err("工具参数长度超过限制".into());
    }
    Ok(())
}

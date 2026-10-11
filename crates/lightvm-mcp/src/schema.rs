//! JSON Schemas of the tools (generated with `schemars` from the same Rust
//! types that produce the results) and a small validator used on every
//! structured result before it is returned (ROADMAP 9: structured results
//! validated against the declared output schemas).
//!
//! The validator covers the subset `schemars` generates for these types:
//! `type`, `properties`, `required`, `additionalProperties: false`,
//! `items`, `enum`, `const`, `anyOf`/`oneOf`/`allOf`, `minimum`/`maximum`,
//! `minLength`/`maxLength`, local `$ref`.

use schemars::{JsonSchema, generate::SchemaSettings};
use serde_json::{Map, Value};

/// Schema of `T` as a JSON object, subschemas inlined (no `$ref`).
#[must_use]
pub fn schema_for<T: JsonSchema>() -> Map<String, Value> {
    let mut settings = SchemaSettings::draft2020_12();
    settings.inline_subschemas = true;
    let schema = settings.into_generator().into_root_schema_for::<T>();
    let mut v = serde_json::to_value(schema).unwrap_or(Value::Null);
    if let Some(o) = v.as_object_mut() {
        o.remove("$schema");
    }
    match v {
        Value::Object(o) => o,
        _ => Map::new(),
    }
}

fn type_ok(t: &str, v: &Value) -> bool {
    match t {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "integer" => v.is_i64() || v.is_u64(),
        "number" => v.is_number(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        _ => true,
    }
}

/// Validates `v` against `schema` (root for `$ref`). Returns the first
/// violation with its JSON path.
pub fn validate(v: &Value, schema: &Value, root: &Value) -> Result<(), String> {
    validate_at(v, schema, root, "$")
}

fn validate_at(v: &Value, schema: &Value, root: &Value, path: &str) -> Result<(), String> {
    let Some(s) = schema.as_object() else {
        // `true` accepts everything, `false` nothing.
        return match schema {
            Value::Bool(false) => Err(format!("{path}: not allowed")),
            _ => Ok(()),
        };
    };
    if let Some(r) = s.get("$ref").and_then(Value::as_str) {
        let target = r
            .strip_prefix("#/")
            .and_then(|p| p.split('/').try_fold(root, |acc, k| acc.get(k)))
            .ok_or_else(|| format!("{path}: unresolved $ref {r}"))?;
        validate_at(v, target, root, path)?;
    }
    if let Some(t) = s.get("type") {
        let ok = match t {
            Value::String(t) => type_ok(t, v),
            Value::Array(ts) => ts.iter().filter_map(Value::as_str).any(|t| type_ok(t, v)),
            _ => true,
        };
        if !ok {
            return Err(format!("{path}: type {t} expected"));
        }
    }
    if let Some(e) = s.get("enum").and_then(Value::as_array)
        && !e.contains(v)
    {
        return Err(format!("{path}: value not in enum"));
    }
    if let Some(c) = s.get("const")
        && c != v
    {
        return Err(format!("{path}: const mismatch"));
    }
    for key in ["anyOf", "oneOf"] {
        if let Some(alts) = s.get(key).and_then(Value::as_array)
            && !alts.iter().any(|a| validate_at(v, a, root, path).is_ok())
        {
            return Err(format!("{path}: no {key} alternative matches"));
        }
    }
    if let Some(all) = s.get("allOf").and_then(Value::as_array) {
        for a in all {
            validate_at(v, a, root, path)?;
        }
    }
    if let Some(n) = v.as_f64() {
        if let Some(min) = s.get("minimum").and_then(Value::as_f64)
            && n < min
        {
            return Err(format!("{path}: below minimum"));
        }
        if let Some(max) = s.get("maximum").and_then(Value::as_f64)
            && n > max
        {
            return Err(format!("{path}: above maximum"));
        }
    }
    if let Some(st) = v.as_str() {
        let len = st.chars().count() as u64;
        if s.get("minLength")
            .and_then(Value::as_u64)
            .is_some_and(|m| len < m)
        {
            return Err(format!("{path}: too short"));
        }
        if s.get("maxLength")
            .and_then(Value::as_u64)
            .is_some_and(|m| len > m)
        {
            return Err(format!("{path}: too long"));
        }
    }
    if let Some(o) = v.as_object() {
        if let Some(req) = s.get("required").and_then(Value::as_array) {
            for r in req.iter().filter_map(Value::as_str) {
                if !o.contains_key(r) {
                    return Err(format!("{path}: missing {r}"));
                }
            }
        }
        let props = s.get("properties").and_then(Value::as_object);
        for (k, val) in o {
            match props.and_then(|p| p.get(k)) {
                Some(ps) => validate_at(val, ps, root, &format!("{path}.{k}"))?,
                None => {
                    if s.get("additionalProperties") == Some(&Value::Bool(false)) {
                        return Err(format!("{path}: unexpected property {k}"));
                    }
                }
            }
        }
    }
    if let (Some(a), Some(items)) = (v.as_array(), s.get("items")) {
        for (i, x) in a.iter().enumerate() {
            validate_at(x, items, root, &format!("{path}[{i}]"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Serialize, JsonSchema)]
    struct Inner {
        n: u32,
        tag: Option<String>,
    }

    #[derive(serde::Serialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct Outer {
        name: String,
        items: Vec<Inner>,
        kind: Kind,
    }

    #[derive(serde::Serialize, JsonSchema)]
    #[serde(rename_all = "snake_case")]
    enum Kind {
        #[allow(dead_code)]
        A,
        B,
    }

    #[test]
    fn generated_schema_validates_its_own_values() {
        let s = Value::Object(schema_for::<Outer>());
        assert_eq!(s["type"], "object");
        let ok = serde_json::to_value(Outer {
            name: "x".into(),
            items: vec![Inner { n: 1, tag: None }],
            kind: Kind::B,
        })
        .unwrap();
        validate(&ok, &s, &s).unwrap();
        let bad = serde_json::json!({"name": 1, "items": [], "kind": "a"});
        assert!(validate(&bad, &s, &s).is_err());
        let bad = serde_json::json!({"name": "x", "items": [{"n": -1}], "kind": "c"});
        assert!(validate(&bad, &s, &s).is_err());
        let bad = serde_json::json!({"name": "x", "items": [], "kind": "a", "extra": 1});
        assert!(validate(&bad, &s, &s).is_err());
    }
}

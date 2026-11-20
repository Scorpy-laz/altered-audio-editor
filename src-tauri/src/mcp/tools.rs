// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

//! The assistant's tools, read from `src/app/assistant/tools.json` (the
//! interface reads the same file), and the input check every call goes
//! through before it reaches the editor.
//!
//! The check covers the parts of JSON Schema the file uses: type, enum,
//! minimum, maximum, minLength, maxLength, required, properties,
//! additionalProperties, minProperties, items, minItems and maxItems.

use std::sync::OnceLock;

use serde_json::{Map, Value};

const TOOLS_JSON: &str = include_str!("../../../src/app/assistant/tools.json");

/// One tool as the registry holds it.
#[derive(Debug, Clone)]
pub struct Tool {
    pub name: String,
    /// Changes the session or writes files: needs Allow changes.
    pub edits: bool,
    /// How long to wait for the editor, in seconds.
    pub timeout_secs: u64,
    pub input_schema: Value,
    /// The definition as `tools/list` sends it.
    pub listing: Value,
}

/// Every tool, in a fixed order.
pub fn all() -> &'static [Tool] {
    static TOOLS: OnceLock<Vec<Tool>> = OnceLock::new();
    TOOLS.get_or_init(|| parse(TOOLS_JSON).expect("tools.json is valid"))
}

pub fn find(name: &str) -> Option<&'static Tool> {
    all().iter().find(|t| t.name == name)
}

/// Replaces every `"#shared:NAME"` string with `shared.NAME`.
fn expand(v: &Value, shared: &Map<String, Value>) -> Result<Value, String> {
    Ok(match v {
        Value::String(s) => match s.strip_prefix("#shared:") {
            Some(key) => shared
                .get(key)
                .cloned()
                .ok_or_else(|| format!("no shared schema {key}"))?,
            None => v.clone(),
        },
        Value::Array(a) => Value::Array(
            a.iter()
                .map(|x| expand(x, shared))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| Ok((k.clone(), expand(x, shared)?)))
                .collect::<Result<_, String>>()?,
        ),
        _ => v.clone(),
    })
}

/// Reads a registry file.
pub fn parse(text: &str) -> Result<Vec<Tool>, String> {
    let root: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let empty = Map::new();
    let shared = root
        .get("shared")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let list = root
        .get("tools")
        .and_then(Value::as_array)
        .ok_or("no tools list")?;
    let mut out = Vec::with_capacity(list.len());
    for t in list {
        let name = t
            .get("name")
            .and_then(Value::as_str)
            .ok_or("a tool has no name")?
            .to_string();
        let edits = t.get("edits").and_then(Value::as_bool).unwrap_or(true);
        let timeout_secs = t.get("timeoutSec").and_then(Value::as_u64).unwrap_or(30);
        let input_schema = expand(t.get("inputSchema").ok_or("no inputSchema")?, shared)?;
        let description = t.get("description").and_then(Value::as_str).unwrap_or("");
        let mut listing = serde_json::json!({
            "name": name,
            "description": description,
            "inputSchema": input_schema,
            "annotations": {
                "readOnlyHint": !edits,
                "destructiveHint": false,
                "openWorldHint": false,
            },
        });
        if let Some(title) = t.get("title").and_then(Value::as_str) {
            listing["title"] = Value::from(title);
            listing["annotations"]["title"] = Value::from(title);
        }
        out.push(Tool {
            name,
            edits,
            timeout_secs,
            input_schema,
            listing,
        });
    }
    Ok(out)
}

fn type_ok(want: &str, v: &Value) -> bool {
    match want {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "number" => v.as_f64().is_some_and(f64::is_finite),
        "integer" => {
            v.is_i64()
                || v.is_u64()
                || v.as_f64()
                    .is_some_and(|f| f.fract() == 0.0 && f.is_finite())
        }
        "null" => v.is_null(),
        _ => true,
    }
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a true or false value",
        Value::Number(_) => "a number",
        Value::String(_) => "text",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

/// Checks `v` against `schema`. The error names the field (such as
/// `target.kind`) and says what was wrong, in words an assistant can act on.
pub fn validate(schema: &Value, v: &Value) -> Result<(), String> {
    check(schema, v, "")
}

fn at(path: &str) -> String {
    if path.is_empty() {
        "The arguments".to_string()
    } else {
        format!("`{path}`")
    }
}

fn check(schema: &Value, v: &Value, path: &str) -> Result<(), String> {
    let Some(s) = schema.as_object() else {
        return Ok(());
    };
    if let Some(t) = s.get("type") {
        let ok = match t {
            Value::String(w) => type_ok(w, v),
            Value::Array(ws) => ws.iter().filter_map(Value::as_str).any(|w| type_ok(w, v)),
            _ => true,
        };
        if !ok {
            let want = match t {
                Value::String(w) => w.clone(),
                _ => t.to_string(),
            };
            return Err(format!("{} must be {want}, not {}.", at(path), kind_of(v)));
        }
    }
    if let Some(Value::Array(choices)) = s.get("enum") {
        if !choices.iter().any(|c| c == v) {
            let list: Vec<String> = choices.iter().map(|c| c.to_string()).collect();
            return Err(format!("{} must be one of {}.", at(path), list.join(", ")));
        }
    }
    if let Some(x) = v.as_f64() {
        if let Some(min) = s.get("minimum").and_then(Value::as_f64) {
            if x < min {
                return Err(format!("{} must be at least {min}.", at(path)));
            }
        }
        if let Some(max) = s.get("maximum").and_then(Value::as_f64) {
            if x > max {
                return Err(format!("{} must be at most {max}.", at(path)));
            }
        }
    }
    if let Some(text) = v.as_str() {
        let n = text.chars().count() as u64;
        if let Some(min) = s.get("minLength").and_then(Value::as_u64) {
            if n < min {
                return Err(format!("{} must not be empty.", at(path)));
            }
        }
        if let Some(max) = s.get("maxLength").and_then(Value::as_u64) {
            if n > max {
                return Err(format!(
                    "{} is too long (at most {max} characters).",
                    at(path)
                ));
            }
        }
    }
    if let Some(items) = v.as_array() {
        let n = items.len() as u64;
        if let Some(min) = s.get("minItems").and_then(Value::as_u64) {
            if n < min {
                return Err(format!("{} needs at least {min} item(s).", at(path)));
            }
        }
        if let Some(max) = s.get("maxItems").and_then(Value::as_u64) {
            if n > max {
                return Err(format!("{} takes at most {max} items.", at(path)));
            }
        }
        if let Some(each) = s.get("items") {
            for (i, item) in items.iter().enumerate() {
                check(each, item, &format!("{path}[{i}]"))?;
            }
        }
    }
    if let Some(obj) = v.as_object() {
        let join = |k: &str| {
            if path.is_empty() {
                k.to_string()
            } else {
                format!("{path}.{k}")
            }
        };
        if let Some(Value::Array(req)) = s.get("required") {
            for r in req.iter().filter_map(Value::as_str) {
                if !obj.contains_key(r) {
                    return Err(format!("`{}` is required.", join(r)));
                }
            }
        }
        if let Some(min) = s.get("minProperties").and_then(Value::as_u64) {
            if (obj.len() as u64) < min {
                return Err(format!("{} needs at least {min} entry.", at(path)));
            }
        }
        let props = s.get("properties").and_then(Value::as_object);
        for (k, item) in obj {
            match props.and_then(|p| p.get(k)) {
                Some(ps) => check(ps, item, &join(k))?,
                None => match s.get("additionalProperties") {
                    Some(Value::Bool(false)) => {
                        let known: Vec<&str> = props
                            .map(|p| p.keys().map(String::as_str).collect())
                            .unwrap_or_default();
                        return Err(if known.is_empty() {
                            format!("`{}` is not taken here; this takes no arguments.", join(k))
                        } else {
                            format!(
                                "`{}` is not taken here. Known: {}.",
                                join(k),
                                known.join(", ")
                            )
                        });
                    }
                    Some(extra @ Value::Object(_)) => check(extra, item, &join(k))?,
                    _ => {}
                },
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
    fn the_registry_loads_with_unique_names_and_valid_schemas() {
        let tools = all();
        assert!(tools.len() >= 20);
        let mut names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), tools.len());
        for t in tools {
            assert!(t
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_'));
            assert_eq!(t.input_schema["type"], "object", "{}", t.name);
            assert!(!t.listing["description"].as_str().unwrap().is_empty());
            // No "#shared:" placeholder is left behind.
            assert!(!t.listing.to_string().contains("#shared:"), "{}", t.name);
            // Every schema accepts a well formed call: an empty object when nothing is required.
            if t.input_schema.get("required").is_none() {
                validate(&t.input_schema, &json!({})).unwrap();
            }
        }
        assert!(!find("get_session").unwrap().edits);
        assert!(find("add_effect").unwrap().edits);
        assert_eq!(
            find("add_effect").unwrap().listing["annotations"]["readOnlyHint"],
            false
        );
    }

    #[test]
    fn checks_types_ranges_and_unknown_fields() {
        let s = &find("add_effect").unwrap().input_schema;
        validate(
            s,
            &json!({"target": {"kind": "track", "id": "Host"}, "effect": "altaudio.limiter"}),
        )
        .unwrap();
        validate(
            s,
            &json!({"target": {"kind": "master"}, "effect": "x", "params": {"ceiling": -3}}),
        )
        .unwrap();
        let e = validate(s, &json!({"effect": "x"})).unwrap_err();
        assert!(e.contains("`target` is required"), "{e}");
        let e = validate(s, &json!({"target": {"kind": "speaker"}, "effect": "x"})).unwrap_err();
        assert!(e.contains("target.kind") && e.contains("\"track\""), "{e}");
        let e = validate(
            s,
            &json!({"target": {"kind": "track"}, "effect": "x", "index": 40}),
        )
        .unwrap_err();
        assert!(e.contains("at most 15"), "{e}");
        let e = validate(
            s,
            &json!({"target": {"kind": "track"}, "effect": "x", "params": {"a": "loud"}}),
        )
        .unwrap_err();
        assert!(e.contains("params.a") && e.contains("number"), "{e}");
        let e = validate(
            s,
            &json!({"target": {"kind": "track"}, "effect": "x", "colour": 1}),
        )
        .unwrap_err();
        assert!(e.contains("colour") && e.contains("Known"), "{e}");
        let e = validate(s, &json!({"target": {"kind": "track"}, "effect": ""})).unwrap_err();
        assert!(e.contains("empty"), "{e}");
        let e = validate(s, &json!([1])).unwrap_err();
        assert!(e.contains("must be object"), "{e}");
        let s = &find("remove_marker").unwrap().input_schema;
        let e = validate(s, &json!({"marker_ids": []})).unwrap_err();
        assert!(e.contains("at least 1"), "{e}");
        let e = validate(s, &json!({"marker_ids": [3]})).unwrap_err();
        assert!(e.contains("marker_ids[0]"), "{e}");
        let s = &find("set_effect_params").unwrap().input_schema;
        let e = validate(s, &json!({"target": {"kind": "master"}, "params": {}})).unwrap_err();
        assert!(e.contains("at least 1"), "{e}");
        // Integers written as 2.0 still count as integers.
        validate(
            &find("move_effect").unwrap().input_schema,
            &json!({"target": {"kind": "master"}, "to_index": 2.0}),
        )
        .unwrap();
        let e = validate(
            &find("move_effect").unwrap().input_schema,
            &json!({"target": {"kind": "master"}, "to_index": 2.5}),
        )
        .unwrap_err();
        assert!(e.contains("integer"), "{e}");
    }

    #[test]
    fn clip_and_automation_tools_check_their_points() {
        for name in [
            "split_clip",
            "delete_clips",
            "move_clip",
            "trim_clip",
            "set_clip_gain",
            "set_clip_fades",
            "set_clip_color",
            "group_clips",
            "ungroup_clips",
            "stretch_clip",
            "set_automation",
            "clear_automation",
        ] {
            assert!(find(name).unwrap().edits, "{name}");
        }
        assert!(!find("get_automation").unwrap().edits);
        let s = &find("set_automation").unwrap().input_schema;
        validate(
            s,
            &json!({"target": {"kind": "clip", "id": "c"}, "parameter": "volume",
                    "points": [{"time_seconds": 1.5, "value": -6}]}),
        )
        .unwrap();
        let e = validate(
            s,
            &json!({"target": {"kind": "track", "id": "t"}, "parameter": "volume",
                    "points": [{"time_seconds": 1}]}),
        )
        .unwrap_err();
        assert!(e.contains("points[0].value"), "{e}");
        let e = validate(
            s,
            &json!({"target": {"kind": "track", "id": "t"}, "parameter": "gain", "points": []}),
        )
        .unwrap_err();
        assert!(e.contains("parameter"), "{e}");
        let s = &find("split_clip").unwrap().input_schema;
        let e = validate(s, &json!({"clip_id": "c", "times_seconds": [-1]})).unwrap_err();
        assert!(e.contains("times_seconds[0]"), "{e}");
        let s = &find("stretch_clip").unwrap().input_schema;
        let e = validate(s, &json!({"clip_id": "c", "ratio": 10})).unwrap_err();
        assert!(e.contains("at most 4"), "{e}");
    }

    #[test]
    fn a_missing_shared_schema_is_an_error() {
        let bad = r##"{"tools":[{"name":"x","inputSchema":{"type":"object","properties":{"a":"#shared:nope"}}}]}"##;
        assert!(parse(bad).is_err());
    }
}

//! The JSON schema of a review, sent as a structured-output format so the model's
//! reply is always valid JSON of the right shape. It mirrors the shape in prompt::SYSTEM.

use serde_json::{json, Value};

/// Models that accept `output_config.format`. Others get the prompt's instructions only.
pub fn supports_structured(model: &str) -> bool {
    let m = model.trim().to_lowercase();
    [
        "claude-fable-5", "claude-mythos-5", "claude-opus-5", "claude-opus-4-8", "claude-opus-4-5",
        "claude-opus-4-1", "claude-sonnet-5", "claude-haiku-5-5", "claude-haiku-4-5",
    ]
    .iter()
    .any(|p| m.starts_with(p))
}

fn obj(props: Value) -> Value {
    let required: Vec<String> = props.as_object().unwrap().keys().cloned().collect();
    json!({"type": "object", "additionalProperties": false, "required": required, "properties": props})
}

fn nullable(v: Value) -> Value {
    json!({"anyOf": [v, {"type": "null"}]})
}

pub fn review() -> Value {
    let s = json!({"type": "string"});
    let strings = json!({"type": "array", "items": {"type": "string"}});
    let cites = json!({"type": "array", "items": {"$ref": "#/$defs/cite"}});
    let one_of = |v: &[&str]| json!({"type": "string", "enum": v});

    let cite = obj(json!({
        "file": s, "side": one_of(&["new", "old"]), "line": {"type": "integer"}, "quote": s,
    }));
    let aim = obj(json!({
        "id": s, "text": s, "source": one_of(&["intent", "inferred"]),
        "status": one_of(&["achieved", "partial", "not_achieved", "unclear"]),
        "hunks": strings, "evidence": cites,
    }));
    let row = obj(json!({
        "subject": s, "aspect": s, "before": s, "after": s, "finding": nullable(s.clone()), "evidence": cites,
    }));
    let edit = obj(json!({"file": s, "find": s, "replace": s}));
    let fix = obj(json!({"note": s, "edits": {"type": "array", "items": edit}}));
    let finding = obj(json!({
        "id": s,
        "kind": one_of(&["bug", "security", "needs_decision", "test_gap", "scope_creep", "achieved"]),
        "title": s, "detail": s, "suggestion": s, "aim": nullable(s.clone()),
        "basis": one_of(&["cited", "inferred"]), "confidence": one_of(&["high", "medium", "low"]),
        "evidence": cites,
        "why": obj(json!({"who": one_of(&["user", "agent", "commit", "ticket", ""]), "quote": s})),
        "fix": nullable(fix),
    }));
    let attention = obj(json!({
        "must_read": strings, "minutes": {"type": "number"},
        "skip_reasons": {"type": "array", "items": obj(json!({"hunk": s, "reason": s}))},
    }));
    let mut root = obj(json!({
        "summary": s, "verdict": s,
        "aims": {"type": "array", "items": aim},
        "behaviour": {"type": "array", "items": row},
        "findings": {"type": "array", "items": finding},
        "attention": attention,
    }));
    root["$defs"] = json!({"cite": cite});
    root
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_object_is_closed_and_fully_required() {
        fn walk(v: &serde_json::Value) {
            if v.get("type").and_then(|t| t.as_str()) == Some("object") {
                assert_eq!(v["additionalProperties"], false);
                let props: Vec<_> = v["properties"].as_object().unwrap().keys().collect();
                assert_eq!(v["required"].as_array().unwrap().len(), props.len());
            }
            match v {
                serde_json::Value::Object(m) => m.values().for_each(walk),
                serde_json::Value::Array(a) => a.iter().for_each(walk),
                _ => {}
            }
        }
        walk(&super::review());
    }

    /// A reply in the schema's shape deserializes into the review model.
    #[test]
    fn schema_shape_matches_model() {
        let reply = serde_json::json!({
            "summary": "s", "verdict": "v", "aims": [], "behaviour": [],
            "findings": [{"id": "F1", "kind": "bug", "title": "t", "detail": "", "suggestion": "", "aim": null,
                "basis": "cited", "confidence": "high", "evidence": [], "why": {"who": "", "quote": ""},
                "fix": {"note": "n", "edits": [{"file": "a", "find": "x", "replace": "y"}]}}],
            "attention": {"must_read": [], "minutes": 1, "skip_reasons": [{"hunk": "H1", "reason": "r"}]}
        });
        let r: crate::model::Review = serde_json::from_value(reply).unwrap();
        assert_eq!(r.attention.skip_reasons["H1"], "r");
        assert_eq!(r.findings[0].fix.as_ref().unwrap().edits[0].find, "x");
    }
}

//! Tool schemas as plain JSON Schema that any LLM tool API accepts. schemars emits `$ref`s into
//! `$defs`, type unions and nullable `anyOf`s for `Option`, arbitrary-precision decimals as
//! `string | number` with a pattern, and formats (`uint32`, `date-time`) that DeepSeek rejects;
//! `portable` rewrites them into inlined, single-typed schemas. Decimals become `number`, which they deserialize from.

use serde_json::{Map, Value};
use zyris::{CapabilityDescriptor, IncomingCall, Outgoing, ServeCapability};

/// Serves `S` unchanged but announces its tools with portable schemas.
pub struct Portable<S>(pub S);

#[zyris::async_trait]
impl<S: ServeCapability> ServeCapability for Portable<S> {
    fn descriptor(&self) -> CapabilityDescriptor {
        portable(self.0.descriptor())
    }

    async fn dispatch(&self, call: IncomingCall) -> zyris::Result<Outgoing> {
        self.0.dispatch(call).await
    }
}

pub fn portable(mut d: CapabilityDescriptor) -> CapabilityDescriptor {
    for t in &mut d.tools {
        t.request_schema = simplify(&t.request_schema);
        t.response_schema = t.response_schema.as_ref().map(simplify);
        t.item_schema = t.item_schema.as_ref().map(simplify);
    }
    d
}

pub fn simplify(root: &Value) -> Value {
    let defs = root.get("$defs").cloned().unwrap_or(Value::Null);
    schema(root, &defs, 0)
}

/// One schema node: resolve `$ref`, recurse into sub-schemas, then flatten what is left.
fn schema(v: &Value, defs: &Value, depth: usize) -> Value {
    let Value::Object(m) = v else { return v.clone() };
    let mut out = match m.get("$ref").and_then(Value::as_str) {
        // Recursive types would never end; past this depth the reference becomes "anything".
        Some(r) if depth < 16 => match schema(&defs[r.trim_start_matches("#/$defs/")], defs, depth + 1) {
            Value::Object(base) => base,
            _ => Map::new(),
        },
        _ => Map::new(),
    };
    for (k, x) in m {
        let x = match k.as_str() {
            "$ref" | "$defs" | "$schema" | "title" => continue,
            "properties" => Value::Object(x.as_object().map(|p| p.iter().map(|(n, s)| (n.clone(), schema(s, defs, depth))).collect()).unwrap_or_default()),
            "items" | "additionalProperties" if x.is_object() => schema(x, defs, depth),
            "anyOf" | "oneOf" | "allOf" => Value::Array(x.as_array().map(|a| a.iter().map(|s| schema(s, defs, depth)).collect()).unwrap_or_default()),
            _ => x.clone(),
        };
        // Keys written next to a `$ref` (its description) win over the referenced definition.
        out.insert(k.clone(), x);
    }
    flatten(out)
}

fn flatten(mut m: Map<String, Value>) -> Value {
    for key in ["anyOf", "oneOf"] {
        let Some(Value::Array(branches)) = m.remove(key) else { continue };
        let real: Vec<&Value> = branches.iter().filter(|b| b.get("type") != Some(&Value::String("null".into()))).collect();
        if real.iter().all(|b| b.get("const").is_some()) && !real.is_empty() {
            // A documented enum: one `const` per variant.
            m.insert("enum".into(), Value::Array(real.iter().map(|b| b["const"].clone()).collect()));
            m.entry("type").or_insert_with(|| real[0].get("type").cloned().unwrap_or("string".into()));
        } else if let Some(Value::Object(first)) = real.first() {
            // ponytail: a union of several real types keeps the first; none of our tools need more.
            for (k, x) in first {
                m.entry(k.clone()).or_insert_with(|| x.clone());
            }
        }
    }
    if let Some(Value::Array(types)) = m.get("type") {
        let types: Vec<&str> = types.iter().filter_map(Value::as_str).filter(|t| *t != "null").collect();
        let one = if types.contains(&"number") { "number" } else { types.first().copied().unwrap_or("string") };
        m.insert("type".into(), one.into());
    }
    if matches!(m.get("type").and_then(Value::as_str), Some("number" | "integer")) {
        m.remove("pattern");
    }
    if m.get("default") == Some(&Value::Null) {
        m.remove("default");
    }
    // DeepSeek rejects any string format but email/hostname/ipv4/ipv6/uuid, and integer formats
    // (`uint32`) are Rust's, not JSON Schema's: drop them all, keeping what a date-time means.
    if m.remove("format").is_some_and(|f| f == "date-time") {
        let hint = "RFC 3339 time, e.g. 2026-09-25T09:00:00+09:00";
        let text = match m.get("description").and_then(Value::as_str) {
            Some(d) => format!("{d} ({hint})"),
            None => hint.to_string(),
        };
        m.insert("description".into(), text.into());
    }
    if m.get("type").and_then(Value::as_str) == Some("object") && !m.contains_key("properties") {
        m.insert("properties".into(), Value::Object(Map::new()));
    }
    Value::Object(m)
}

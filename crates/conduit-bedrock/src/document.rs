//! Converting between `serde_json::Value` and the SDK's `Document`.
//!
//! The Converse API carries a tool's argument schema and the arguments a model
//! sends back as [`aws_smithy_types::Document`], an untyped JSON tree of the
//! SDK's own. Conduit speaks `serde_json::Value` everywhere, and the SDK offers
//! no bridge: `Document`'s serde derives sit behind `aws_sdk_unstable` plus a
//! `serde-serialize` feature, which is not a foundation to build a provider on.
//! So the mapping is written out, in both directions.

use aws_smithy_types::{Document, Number};
use serde_json::Value;

/// Rewrites `value` as the SDK's document tree.
///
/// Total, because every JSON document is a `Document`: the two trees have the
/// same shape, and the only interesting case is a number, which `Document`
/// splits into three cases where JSON has one.
pub fn from_json(value: &Value) -> Document {
    match value {
        Value::Null => Document::Null,
        Value::Bool(boolean) => Document::Bool(*boolean),
        Value::Number(number) => Document::Number(number_from_json(number)),
        Value::String(string) => Document::String(string.clone()),
        Value::Array(items) => Document::Array(items.iter().map(from_json).collect()),
        Value::Object(fields) => Document::Object(
            fields.iter().map(|(name, field)| (name.clone(), from_json(field))).collect(),
        ),
    }
}

/// Rewrites a document tree as JSON.
///
/// Also total, and for the same reason. A float that is not finite is the one
/// value JSON cannot hold, and it becomes null rather than failing the turn: a
/// model that emitted a NaN argument has produced a tool call the tool should
/// reject, not a response worth discarding.
///
/// Only tests call this, and deliberately: nothing on a response comes back as a
/// document — Converse streams a tool call's arguments as JSON text, which is
/// parsed rather than converted. What this direction exists for is asserting that
/// the other one loses nothing, which is the only claim about [`from_json`] worth
/// making.
#[cfg(test)]
pub fn to_json(document: &Document) -> Value {
    match document {
        Document::Null => Value::Null,
        Document::Bool(boolean) => Value::Bool(*boolean),
        Document::Number(number) => number_to_json(*number),
        Document::String(string) => Value::String(string.clone()),
        Document::Array(items) => Value::Array(items.iter().map(to_json).collect()),
        Document::Object(fields) => Value::Object(
            fields.iter().map(|(name, field)| (name.clone(), to_json(field))).collect(),
        ),
    }
}

/// Which of the SDK's three number cases a JSON number is.
fn number_from_json(number: &serde_json::Number) -> Number {
    if let Some(unsigned) = number.as_u64() {
        Number::PosInt(unsigned)
    } else if let Some(signed) = number.as_i64() {
        Number::NegInt(signed)
    } else {
        // `serde_json::Number` is one of these three, so a value that is
        // neither integer is a float.
        Number::Float(number.as_f64().unwrap_or_default())
    }
}

/// A document number as JSON.
#[cfg(test)]
fn number_to_json(number: Number) -> Value {
    match number {
        Number::PosInt(unsigned) => Value::Number(unsigned.into()),
        Number::NegInt(signed) => Value::Number(signed.into()),
        Number::Float(float) => {
            serde_json::Number::from_f64(float).map_or(Value::Null, Value::Number)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_schema_survives_the_round_trip_unchanged() {
        // This is the whole point: what an operator declared as a tool's
        // parameters is what the model is offered. A conversion that dropped a
        // nested `required` list would silently widen every tool.
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "city": { "type": "string", "description": "Where to look" },
                "days": { "type": "integer", "minimum": 1, "maximum": 10 },
                "units": { "type": "string", "enum": ["metric", "imperial"] },
            },
            "required": ["city"],
            "additionalProperties": false,
        });

        assert_eq!(to_json(&from_json(&schema)), schema);
    }

    #[test]
    fn every_kind_of_scalar_crosses_in_both_directions() {
        for value in [
            serde_json::json!(null),
            serde_json::json!(true),
            serde_json::json!(false),
            serde_json::json!(0),
            serde_json::json!(42),
            serde_json::json!(-7),
            serde_json::json!(1.5),
            serde_json::json!(""),
            serde_json::json!("Denver"),
            serde_json::json!([]),
            serde_json::json!({}),
        ] {
            assert_eq!(to_json(&from_json(&value)), value, "{value} did not survive");
        }
    }

    #[test]
    fn a_negative_integer_stays_an_integer_rather_than_becoming_a_float() {
        // `Document` splits integers by sign, and routing a negative one
        // through `Float` would turn `-7` into `-7.0` in a tool's arguments.
        assert_eq!(from_json(&serde_json::json!(-7)), Document::Number(Number::NegInt(-7)));
        assert_eq!(number_to_json(Number::NegInt(-7)), serde_json::json!(-7));
    }

    #[test]
    fn an_unrepresentable_float_becomes_null_rather_than_failing_the_turn() {
        // JSON has no NaN. A model that sent one has made a tool call the tool
        // should refuse; discarding the whole response would be worse.
        assert_eq!(number_to_json(Number::Float(f64::NAN)), Value::Null);
        assert_eq!(number_to_json(Number::Float(f64::INFINITY)), Value::Null);
    }

    #[test]
    fn nesting_is_carried_all_the_way_down() {
        let deep = serde_json::json!({
            "a": [{ "b": [{ "c": [1, 2, 3] }] }],
        });

        assert_eq!(to_json(&from_json(&deep)), deep);
    }
}

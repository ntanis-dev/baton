//! JSON the way Baton's files have always been written: two-space indentation, whole numbers without a
//! fraction, and readers that take what is there and ignore what is not.
//!
//! The files are small state documents that the hooks, the status line, the wrapper and the menu write
//! for each other (and that the operator may edit), so a reader never fails on a field of the wrong
//! type: it reads as missing.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Number, Value};

/// A JSON object.
pub type Object = Map<String, Value>;

/// The largest whole number a JSON reader on any platform keeps exactly (2^53).
const EXACT: f64 = 9_007_199_254_740_992.0;

/// A number as JSON: whole numbers print without a fraction (`90`, not `90.0`), and what is not a
/// number at all (NaN, infinity) is null.
pub fn num(value: f64) -> Value {
    if !value.is_finite() {
        return Value::Null;
    }
    if value.fract() == 0.0 && value.abs() < EXACT {
        return Value::Number(Number::from(value as i64));
    }
    Number::from_f64(value).map_or(Value::Null, Value::Number)
}

/// A number as text: whole numbers without a fraction (`90`), the others as they round-trip (`42.5`).
pub fn show(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < EXACT {
        return format!("{}", value as i64);
    }
    format!("{value}")
}

/// [`num`] of a number that may be missing.
pub fn opt_num(value: Option<f64>) -> Value {
    value.map_or(Value::Null, num)
}

/// Serializes a number the way [`num`] writes it (for `#[serde(serialize_with)]`).
pub fn ser_num<S: Serializer>(value: &f64, serializer: S) -> Result<S::Ok, S::Error> {
    num(*value).serialize(serializer)
}

/// Serializes a number that may be missing the way [`num`] writes it.
pub fn ser_opt_num<S: Serializer>(value: &Option<f64>, serializer: S) -> Result<S::Ok, S::Error> {
    opt_num(*value).serialize(serializer)
}

/// Reads a field that may be missing, null or of the wrong type: anything but a good value is None
/// (for `#[serde(default, deserialize_with)]`).
pub fn lenient<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}

/// A value as indented JSON text, without a final newline.
pub fn pretty<T: Serialize + ?Sized>(value: &T) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".to_owned())
}

/// A value as one line of JSON.
pub fn compact<T: Serialize + ?Sized>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned())
}

/// Parses JSON text the way the programs that write Baton's input read it back. JavaScript's JSON can
/// hold half of a surrogate pair as an escape (`"\ud83d"`: a text cut in the middle of an emoji), which a
/// strict reader refuses; here such an escape reads as the replacement character. None for anything that
/// is not JSON.
pub fn parse(text: &str) -> Option<Value> {
    if let Ok(value) = serde_json::from_str(text) {
        return Some(value);
    }
    serde_json::from_str(&repair_surrogates(text)?).ok()
}

/// The code unit of a `\uXXXX` escape starting at `at` (the backslash), when there is one.
fn escape_unit(bytes: &[u8], at: usize) -> Option<u32> {
    let escape = bytes.get(at..at + 6)?;
    if escape[0] != b'\\' || escape[1] != b'u' {
        return None;
    }
    let digits = std::str::from_utf8(&escape[2..]).ok()?;
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(digits, 16).ok()
}

/// The text with every lone surrogate escape replaced by `\ufffd`; None when there is none to replace.
fn repair_surrogates(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut repaired = false;
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'\\' {
            out.push(bytes[at]);
            at += 1;
            continue;
        }
        match escape_unit(bytes, at) {
            Some(0xD800..=0xDBFF) => {
                if matches!(escape_unit(bytes, at + 6), Some(0xDC00..=0xDFFF)) {
                    out.extend_from_slice(&bytes[at..at + 12]);
                    at += 12;
                } else {
                    out.extend_from_slice(b"\\ufffd");
                    repaired = true;
                    at += 6;
                }
            }
            Some(0xDC00..=0xDFFF) => {
                out.extend_from_slice(b"\\ufffd");
                repaired = true;
                at += 6;
            }
            _ => {
                // Any other escape: the backslash and what it escapes, so that `\\u` is never read as `\u`.
                let end = (at + 2).min(bytes.len());
                out.extend_from_slice(&bytes[at..end]);
                at = end;
            }
        }
    }
    if !repaired {
        return None;
    }
    String::from_utf8(out).ok()
}

/// Reading fields out of JSON whose shape is not known in advance.
pub trait Get {
    /// The text at `key`.
    fn text(&self, key: &str) -> Option<&str>;
    /// The text at `key` when it is not empty.
    fn word(&self, key: &str) -> Option<&str>;
    /// The number at `key` (a finite one).
    fn number(&self, key: &str) -> Option<f64>;
    /// The whole number at `key`.
    fn int(&self, key: &str) -> Option<i64>;
    /// Whether `key` holds `true`.
    fn flag(&self, key: &str) -> bool;
    /// The value at `key`, null when there is none.
    fn at(&self, key: &str) -> &Value;
    /// Whether `key` holds something other than null, false, 0 or the empty string.
    fn truthy(&self, key: &str) -> bool;
}

/// A null to hand out by reference.
static NULL: Value = Value::Null;

impl Get for Value {
    fn text(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Value::as_str)
    }

    fn word(&self, key: &str) -> Option<&str> {
        self.text(key).filter(|text| !text.is_empty())
    }

    fn number(&self, key: &str) -> Option<f64> {
        self.get(key).and_then(Value::as_f64).filter(|n| n.is_finite())
    }

    fn int(&self, key: &str) -> Option<i64> {
        let value = self.get(key)?;
        value
            .as_i64()
            .or_else(|| value.as_f64().filter(|n| n.fract() == 0.0).map(|n| n as i64))
    }

    fn flag(&self, key: &str) -> bool {
        self.get(key).and_then(Value::as_bool) == Some(true)
    }

    fn at(&self, key: &str) -> &Value {
        self.get(key).unwrap_or(&NULL)
    }

    fn truthy(&self, key: &str) -> bool {
        is_truthy(self.at(key))
    }
}

/// Whether a value counts as set: not null, false, 0 or the empty string.
pub fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// A number from JSON the way a loose reader takes it: a number, or text that is one. Null, the empty
/// string and anything else is None.
pub fn loose_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64().filter(|n| n.is_finite()),
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() {
                None
            } else {
                text.parse::<f64>().ok().filter(|n| n.is_finite())
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn whole_numbers_have_no_fraction() {
        assert_eq!(compact(&num(90.0)), "90");
        assert_eq!(compact(&num(42.5)), "42.5");
        assert_eq!(compact(&num(-3.0)), "-3");
        assert_eq!(compact(&num(f64::NAN)), "null");
        assert_eq!(compact(&num(f64::INFINITY)), "null");
        assert_eq!(compact(&opt_num(None)), "null");
        assert_eq!(show(90.0), "90");
        assert_eq!(show(42.5), "42.5");
        assert_eq!(show(-0.0), "0");
    }

    #[test]
    fn pretty_is_two_spaces_and_keeps_key_order() {
        let value = json!({ "zeta": 1, "alpha": { "list": [1, "two"], "empty": {}, "none": [] } });
        assert_eq!(
            pretty(&value),
            "{\n  \"zeta\": 1,\n  \"alpha\": {\n    \"list\": [\n      1,\n      \"two\"\n    ],\n    \"empty\": {},\n    \"none\": []\n  }\n}"
        );
    }

    #[derive(Deserialize, Debug, Default)]
    #[serde(default)]
    struct Loose {
        #[serde(deserialize_with = "lenient")]
        used: Option<f64>,
        #[serde(deserialize_with = "lenient")]
        name: Option<String>,
    }

    #[test]
    fn a_field_of_the_wrong_type_reads_as_missing() {
        let loose: Loose = serde_json::from_value(json!({ "used": "many", "name": 4 })).unwrap();
        assert_eq!(loose.used, None);
        assert_eq!(loose.name, None);
        let loose: Loose = serde_json::from_value(json!({ "used": 4, "extra": true })).unwrap();
        assert_eq!(loose.used, Some(4.0));
        let loose: Loose = serde_json::from_value(json!({ "used": null })).unwrap();
        assert_eq!(loose.used, None);
    }

    #[test]
    fn half_a_surrogate_pair_reads_as_the_replacement_character() {
        // What Claude Code's own JSON writes and reads back, a strict reader refuses.
        assert!(serde_json::from_str::<Value>(r#"{"a":"x\ud83dy"}"#).is_err());
        assert_eq!(parse(r#"{"a":"x\ud83dy"}"#), Some(json!({ "a": "x\u{fffd}y" })));
        assert_eq!(parse(r#"{"a":"x\ude00"}"#), Some(json!({ "a": "x\u{fffd}" })));
        assert_eq!(
            parse(r#"{"a":"\ud83d\ud83d\ude00"}"#),
            Some(json!({ "a": "\u{fffd}\u{1F600}" }))
        );
        // A whole pair, and a backslash that only looks like an escape, are left as they are.
        assert_eq!(
            parse(r#"{"a":"\ud83d\ude00"}"#),
            Some(json!({ "a": "\u{1F600}" }))
        );
        assert_eq!(
            parse(r#"{"a":"\\ud83d","b":"\ud83d"}"#),
            Some(json!({ "a": "\\ud83d", "b": "\u{fffd}" }))
        );
        assert_eq!(parse("{ nope"), None);
        assert_eq!(parse(r#"{"a":"\ud83d" nope"#), None);
    }

    #[test]
    fn fields_of_unknown_json() {
        let value = json!({ "a": "x", "n": 3, "f": 2.5, "t": true, "e": "", "z": 0 });
        assert_eq!(value.text("a"), Some("x"));
        assert_eq!(value.word("e"), None);
        assert_eq!(value.number("n"), Some(3.0));
        assert_eq!(value.int("n"), Some(3));
        assert_eq!(value.int("f"), None);
        assert!(value.flag("t"));
        assert!(!value.flag("a"));
        assert!(value.at("missing").is_null());
        assert!(value.truthy("a") && !value.truthy("e") && !value.truthy("z") && !value.truthy("nope"));
        assert_eq!(loose_number(&json!("12.5")), Some(12.5));
        assert_eq!(loose_number(&json!("")), None);
        assert_eq!(loose_number(&json!(null)), None);
    }
}

//! Strict JSON tool arguments: duplicate object keys are ambiguous even when
//! a permissive JSON parser would silently select the last value.
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use std::fmt;

struct UniqueValue(Value);

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(UniqueVisitor)
    }
}

struct UniqueVisitor;
impl<'de> Visitor<'de> for UniqueVisitor {
    type Value = UniqueValue;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value without duplicate object keys")
    }
    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Bool(v)))
    }
    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Number(v.into())))
    }
    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Number(v.into())))
    }
    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Number(
            Number::from_f64(v).ok_or_else(|| E::custom("non-finite number"))?,
        )))
    }
    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::String(v.into())))
    }
    fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::String(v)))
    }
    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Null))
    }
    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Null))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = seq.next_element::<UniqueValue>()? {
            values.push(value.0);
        }
        Ok(UniqueValue(Value::Array(values)))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut input: A) -> Result<Self::Value, A::Error> {
        let mut values = Map::new();
        while let Some(key) = input.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom(format!("duplicate key: {key}")));
            }
            let value = input.next_value::<UniqueValue>()?;
            values.insert(key, value.0);
        }
        Ok(UniqueValue(Value::Object(values)))
    }
}

pub fn parse_tool_arguments(input: &str) -> Result<Value, serde_json::Error> {
    let mut decoder = serde_json::Deserializer::from_str(input);
    let value = UniqueValue::deserialize(&mut decoder)?;
    decoder.end()?;
    Ok(value.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn rejects_duplicate_keys_at_any_depth_without_changing_valid_values() {
        assert!(parse_tool_arguments(r#"{"path":"a","path":"b"}"#).is_err());
        assert!(parse_tool_arguments(r#"{"point":{"x":1,"x":2}}"#).is_err());
        assert_eq!(
            parse_tool_arguments(r#"{"point":{"x":1},"tags":["中文",null,true,1.5]}"#).unwrap(),
            json!({"point":{"x":1},"tags":["中文",null,true,1.5]})
        );
    }
}

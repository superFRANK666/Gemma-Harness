//! Parse FunctionGemma's native call syntax only after a complete model stream.
//! No recovered/guessed argument is ever sent to an executor.
use crate::{FunctionCall, ToolCall};
use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub const CALL_START: &str = "<start_function_call>";
pub const CALL_END: &str = "<end_function_call>";
pub const RESPONSE_START: &str = "<start_function_response>";
const ESCAPE: &str = "<escape>";
const MAX_BYTES: usize = 4 * 1024 * 1024;

/// llama.cpp's FunctionGemma template inserts the arguments string inside its
/// own braces. Convert canonical JSON to that template's native field syntax.
pub fn encode_arguments(arguments: &str) -> Result<String> {
    if arguments.len() > MAX_BYTES {
        bail!("FunctionGemma arguments exceed the limit");
    }
    let value: Value = serde_json::from_str(arguments).context("Invalid stored tool-call JSON")?;
    let object = value
        .as_object()
        .context("FunctionGemma arguments must be an object")?;
    encode_object(object, 0)
}
fn encode_object(object: &Map<String, Value>, depth: usize) -> Result<String> {
    if depth > 32 {
        bail!("FunctionGemma argument nesting is too deep");
    }
    object
        .iter()
        .map(|(key, value)| {
            let key = if !key.is_empty()
                && key
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
            {
                key.clone()
            } else {
                encode_string(key)?
            };
            Ok(format!("{key}:{}", encode_value(value, depth + 1)?))
        })
        .collect::<Result<Vec<_>>>()
        .map(|parts| parts.join(","))
}
fn encode_string(value: &str) -> Result<String> {
    if [
        ESCAPE,
        CALL_START,
        CALL_END,
        RESPONSE_START,
        "<end_function_response>",
        "<start_of_turn>",
        "<end_of_turn>",
    ]
    .iter()
    .any(|token| value.contains(token))
    {
        bail!("FunctionGemma cannot safely encode a string containing reserved control tokens");
    }
    Ok(format!("{ESCAPE}{value}{ESCAPE}"))
}
fn encode_value(value: &Value, depth: usize) -> Result<String> {
    if depth > 32 {
        bail!("FunctionGemma argument nesting is too deep");
    }
    match value {
        Value::String(value) => encode_string(value),
        Value::Object(value) => Ok(format!("{{{}}}", encode_object(value, depth + 1)?)),
        Value::Array(values) => Ok(format!(
            "[{}]",
            values
                .iter()
                .map(|v| encode_value(v, depth + 1))
                .collect::<Result<Vec<_>>>()?
                .join(",")
        )),
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(value.to_string()),
    }
}

pub fn parse_calls(input: &str) -> Result<Vec<ToolCall>> {
    if input.len() > MAX_BYTES {
        bail!("FunctionGemma output exceeds the call limit");
    }
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let nonce = format!(
        "{:x}_{:x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let mut parser = Parser { rest: input };
    let mut calls = Vec::new();
    parser.space();
    while !parser.rest.is_empty() {
        if calls.len() >= 64 {
            bail!("FunctionGemma returned too many calls");
        }
        parser.expect(CALL_START)?;
        parser.expect("call:")?;
        let end = parser
            .rest
            .find('{')
            .context("FunctionGemma call is missing arguments")?;
        let name = parser.rest[..end].to_string();
        if name.is_empty()
            || name.len() > 200
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
        {
            bail!("Invalid FunctionGemma tool name");
        }
        parser.rest = &parser.rest[end..];
        let arguments = parser.object(0)?;
        parser.expect(CALL_END)?;
        calls.push(ToolCall {
            id: format!("call_{nonce}_{}", calls.len()),
            kind: "function".into(),
            function: FunctionCall {
                name,
                arguments: serde_json::to_string(&arguments)?,
            },
        });
        parser.space();
    }
    if calls.is_empty() {
        bail!("FunctionGemma returned no calls");
    }
    Ok(calls)
}

struct Parser<'a> {
    rest: &'a str,
}
impl<'a> Parser<'a> {
    fn space(&mut self) {
        self.rest = self.rest.trim_start_matches(char::is_whitespace);
    }
    fn expect(&mut self, expected: &str) -> Result<()> {
        self.rest = self
            .rest
            .strip_prefix(expected)
            .with_context(|| format!("Expected {expected}"))?;
        Ok(())
    }
    fn quoted(&mut self) -> Result<String> {
        self.expect(ESCAPE)?;
        let end = self
            .rest
            .find(ESCAPE)
            .context("Unterminated FunctionGemma string")?;
        let value = self.rest[..end].to_string();
        encode_string(&value)?;
        self.rest = &self.rest[end + ESCAPE.len()..];
        Ok(value)
    }
    fn object(&mut self, depth: usize) -> Result<Value> {
        if depth > 32 {
            bail!("FunctionGemma argument nesting is too deep");
        }
        self.expect("{")?;
        let mut object = Map::new();
        self.space();
        if self.rest.starts_with('}') {
            self.expect("}")?;
            return Ok(Value::Object(object));
        }
        loop {
            self.space();
            let key = if self.rest.starts_with(ESCAPE) {
                self.quoted()?
            } else {
                let end = self
                    .rest
                    .find(':')
                    .context("FunctionGemma argument key has no colon")?;
                let key = self.rest[..end].trim().to_string();
                if key.is_empty()
                    || key
                        .chars()
                        .any(|c| c.is_control() || matches!(c, '{' | '}' | '[' | ']' | ','))
                {
                    bail!("Invalid FunctionGemma argument key");
                }
                self.rest = &self.rest[end..];
                key
            };
            encode_string(&key)?;
            self.space();
            self.expect(":")?;
            self.space();
            let value = self.value(depth + 1)?;
            if object.insert(key, value).is_some() {
                bail!("Duplicate FunctionGemma argument key");
            }
            self.space();
            if self.rest.starts_with('}') {
                self.expect("}")?;
                break;
            }
            self.expect(",")?;
        }
        Ok(Value::Object(object))
    }
    fn value(&mut self, depth: usize) -> Result<Value> {
        if depth > 32 {
            bail!("FunctionGemma argument nesting is too deep");
        }
        self.space();
        if self.rest.starts_with(ESCAPE) {
            return Ok(Value::String(self.quoted()?));
        }
        if self.rest.starts_with('{') {
            return self.object(depth);
        }
        if self.rest.starts_with('[') {
            self.expect("[")?;
            self.space();
            let mut values = Vec::new();
            if self.rest.starts_with(']') {
                self.expect("]")?;
                return Ok(Value::Array(values));
            }
            loop {
                values.push(self.value(depth + 1)?);
                self.space();
                if self.rest.starts_with(']') {
                    self.expect("]")?;
                    break;
                }
                self.expect(",")?;
                self.space();
            }
            return Ok(Value::Array(values));
        }
        let end = self.rest.find([',', '}', ']']).unwrap_or(self.rest.len());
        let raw = self.rest[..end].trim();
        let value: Value = serde_json::from_str(raw).context("Invalid FunctionGemma scalar")?;
        if !matches!(value, Value::Null | Value::Bool(_) | Value::Number(_)) {
            bail!("FunctionGemma strings require escape delimiters");
        }
        self.rest = &self.rest[end..];
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn parses_unicode_nested_values_and_parallel_calls_without_guessing() {
        let raw = "<start_function_call>call:lookup{query:<escape>你好, {x} \\\"q\\\"<escape>,n:17,ok:true,empty:null,items:[<escape>a,b<escape>,{x:2}]}<end_function_call><start_function_call>call:other{}<end_function_call>";
        let calls = parse_calls(raw).unwrap();
        assert_eq!(calls.len(), 2);
        let args: Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(
            args,
            json!({"query":"你好, {x} \\\"q\\\"","n":17,"ok":true,"empty":null,"items":["a,b",{"x":2}]})
        );
        let encoded = encode_arguments(&calls[0].function.arguments).unwrap();
        let roundtrip =
            parse_calls(&format!("{CALL_START}call:lookup{{{encoded}}}{CALL_END}")).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&roundtrip[0].function.arguments).unwrap(),
            args
        );
    }
    #[test]
    fn rejects_partial_duplicate_and_extra_text() {
        for raw in [
            "<start_function_call>call:f{x:<escape>oops",
            "<start_function_call>call:f{x:1,x:2}<end_function_call>",
            "<start_function_call>call:f{}<end_function_call>extra",
        ] {
            assert!(parse_calls(raw).is_err());
        }
        assert!(encode_arguments(r#"{"text":"<escape>"}"#).is_err());
    }
}

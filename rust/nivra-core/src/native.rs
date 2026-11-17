//! Strict tool-free native response parsing. No grant, transport, retry or authority.
use serde::{
    Deserialize, Deserializer,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Number, Value};
use std::{collections::BTreeSet, fmt};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    ResponseInvalid,
    OutputInvalid,
    AuthFailed,
    NotEntitled,
    RateLimited,
    ProviderFailed,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ResponseInvalid => "response_invalid",
            Self::OutputInvalid => "output_invalid",
            Self::AuthFailed => "auth_failed",
            Self::NotEntitled => "not_entitled",
            Self::RateLimited => "rate_limited",
            Self::ProviderFailed => "provider_failed",
        })
    }
}
impl std::error::Error for Error {}
struct Strict(Value);
impl<'de> Deserialize<'de> for Strict {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct StrictVisitor;
        impl<'de> Visitor<'de> for StrictVisitor {
            type Value = Strict;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("bounded JSON without duplicate object keys")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Strict, E> {
                Ok(Strict(Value::Bool(v)))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Strict, E> {
                Ok(Strict(Value::Null))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Strict, E> {
                Ok(Strict(Value::Number(v.into())))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Strict, E> {
                Ok(Strict(Value::Number(v.into())))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Strict, E> {
                if v.abs() > 9_007_199_254_740_992.0 {
                    return Err(E::custom("native_number_range"));
                }
                Number::from_f64(v)
                    .map(|n| Strict(Value::Number(n)))
                    .ok_or_else(|| E::custom("native_number_nonfinite"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Strict, E> {
                Ok(Strict(Value::String(v.into())))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Strict, E> {
                Ok(Strict(Value::String(v)))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Strict, A::Error> {
                let mut values = vec![];
                while let Some(Strict(value)) = seq.next_element()? {
                    values.push(value);
                }
                Ok(Strict(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Strict, A::Error> {
                let mut values = Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(de::Error::custom("native_duplicate_key"));
                    }
                    let Strict(value) = map.next_value()?;
                    values.insert(key, value);
                }
                Ok(Strict(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(StrictVisitor)
    }
}
pub fn decode(raw: &[u8]) -> Result<Value, Error> {
    decode_bounded(raw, 1_000_000)
}
pub fn decode_bounded(raw: &[u8], maximum: usize) -> Result<Value, Error> {
    let value = decode_any_bounded(raw, maximum)?;
    if !value.is_object() {
        return Err(Error::ResponseInvalid);
    }
    Ok(value)
}
pub(crate) fn decode_any_bounded(raw: &[u8], maximum: usize) -> Result<Value, Error> {
    if raw.len() > maximum {
        return Err(Error::ResponseInvalid);
    }
    let Strict(value) = serde_json::from_slice(raw).map_err(|_| Error::ResponseInvalid)?;
    Ok(value)
}
fn exact_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 120
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
}
pub fn catalog(raw: &[u8]) -> Result<Vec<String>, Error> {
    if raw.len() > 262144 {
        return Err(Error::OutputInvalid);
    }
    let body = decode(raw).map_err(|_| Error::OutputInvalid)?;
    let rows = body
        .get("data")
        .and_then(Value::as_array)
        .filter(|r| (1..=500).contains(&r.len()))
        .ok_or(Error::OutputInvalid)?;
    let mut ids = BTreeSet::new();
    for row in rows {
        let id = row
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| exact_id(id))
            .ok_or(Error::OutputInvalid)?;
        if !ids.insert(id.to_owned()) {
            return Err(Error::OutputInvalid);
        }
    }
    Ok(ids.into_iter().collect())
}
/// Receipt is deliberately distinct from a schema-validated selection or veto.
#[derive(PartialEq)]
pub struct ReceivedObject(Value);
impl ReceivedObject {
    pub fn object(&self) -> &Value {
        &self.0
    }
    pub fn validated_decision(&self) -> bool {
        false
    }
}
impl fmt::Debug for ReceivedObject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ReceivedObject(unvalidated, redacted)")
    }
}
pub fn structured(raw: &[u8], model: &str) -> Result<ReceivedObject, Error> {
    if !exact_id(model) {
        return Err(Error::OutputInvalid);
    }
    let body = decode(raw).map_err(|_| Error::OutputInvalid)?;
    if body.get("status").and_then(Value::as_str) != Some("completed")
        || body.get("model").and_then(Value::as_str) != Some(model)
        || body.get("error").is_some_and(|v| !v.is_null())
        || body.get("incomplete_details").is_some_and(|v| !v.is_null())
    {
        return Err(Error::OutputInvalid);
    }
    let output = body
        .get("output")
        .and_then(Value::as_array)
        .filter(|r| (1..=128).contains(&r.len()))
        .ok_or(Error::OutputInvalid)?;
    let mut result = None;
    for item in output {
        if !item.is_object() {
            return Err(Error::OutputInvalid);
        }
        if item.get("type").and_then(Value::as_str) == Some("reasoning") {
            continue;
        }
        if item.get("type").and_then(Value::as_str) != Some("message")
            || item.get("role").and_then(Value::as_str) != Some("assistant")
            || item.get("status").and_then(Value::as_str) != Some("completed")
            || result.is_some()
        {
            return Err(Error::OutputInvalid);
        }
        let content = item
            .get("content")
            .and_then(Value::as_array)
            .filter(|a| a.len() == 1)
            .ok_or(Error::OutputInvalid)?;
        if content[0].get("type").and_then(Value::as_str) != Some("output_text") {
            return Err(Error::OutputInvalid);
        }
        let text = content[0]
            .get("text")
            .and_then(Value::as_str)
            .ok_or(Error::OutputInvalid)?;
        result = Some(decode(text.as_bytes()).map_err(|_| Error::OutputInvalid)?);
    }
    result.map(ReceivedObject).ok_or(Error::OutputInvalid)
}

#[derive(Clone, Copy)]
pub enum ResponseKind<'a> {
    Catalog,
    Trading { exact_model: &'a str },
}
pub enum ParsedResponse {
    Catalog(Vec<String>),
    Received(ReceivedObject),
}
impl fmt::Debug for ParsedResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ParsedResponse(redacted, no decision authority)")
    }
}
/// A transport must separately deny redirects/cookies/env auth and enforce deadlines,
/// original grants/leases/journals and bounded streaming before invoking this parser.
pub fn parse_http(
    status: u16,
    content_type: Option<&str>,
    encoding: Option<&str>,
    raw: &[u8],
    kind: ResponseKind<'_>,
) -> Result<ParsedResponse, Error> {
    validate_http_headers(status, content_type, encoding)?;
    match kind {
        ResponseKind::Catalog => catalog(raw).map(ParsedResponse::Catalog),
        ResponseKind::Trading { exact_model } => {
            structured(raw, exact_model).map(ParsedResponse::Received)
        }
    }
}

/// Header admission before consuming a response stream.
pub fn validate_http_headers(
    status: u16,
    content_type: Option<&str>,
    encoding: Option<&str>,
) -> Result<(), Error> {
    if status != 200 {
        return Err(match status {
            401 => Error::AuthFailed,
            403 => Error::NotEntitled,
            429 => Error::RateLimited,
            _ => Error::ProviderFailed,
        });
    }
    if !encoding
        .unwrap_or("identity")
        .eq_ignore_ascii_case("identity")
        || !content_type
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("application/json")
    {
        return Err(Error::OutputInvalid);
    }
    Ok(())
}

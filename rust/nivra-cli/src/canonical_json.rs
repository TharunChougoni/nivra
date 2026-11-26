//! Canonical JSON for content-addressed reports: object keys sorted
//! recursively, arrays in order, integers only, compact output. Money is an
//! exact decimal string, so a float here is a bug. A reader can recompute a
//! claimed SHA-256 from the embedded JSON without knowing Rust field order.
use nivra_core::replay::digest;
use serde::Serialize;
use serde_json::Value;

pub fn bytes<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, String> {
    let mut value =
        serde_json::to_value(value).map_err(|_| "canonical_json_invalid".to_string())?;
    value.sort_all_objects();
    reject_non_integer_numbers(&value)?;
    serde_json::to_vec(&value).map_err(|_| "canonical_json_invalid".to_string())
}

pub fn sha256<T: Serialize + ?Sized>(value: &T) -> Result<String, String> {
    Ok(digest(&bytes(value)?))
}

fn reject_non_integer_numbers(value: &Value) -> Result<(), String> {
    match value {
        Value::Number(number) if !number.is_i64() && !number.is_u64() => {
            Err("canonical_json_float_unsupported".into())
        }
        Value::Object(object) => object.values().try_for_each(reject_non_integer_numbers),
        Value::Array(array) => array.iter().try_for_each(reject_non_integer_numbers),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn sorts_keys_and_refuses_floats() {
        assert_eq!(
            super::bytes(&json!({"b":1,"a":{"d":[2,1],"c":"x"}})).unwrap(),
            br#"{"a":{"c":"x","d":[2,1]},"b":1}"#
        );
        assert!(super::bytes(&json!({"a":1.5})).is_err());
    }
}

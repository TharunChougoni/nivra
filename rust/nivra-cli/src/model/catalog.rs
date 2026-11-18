use super::*;

pub(crate) fn admit_model(model: &str, listed: &[String]) -> Result<()> {
    if model == FAST_VARIANT || !listed.iter().any(|id| id == model) {
        Err("planner_model_not_in_live_catalog".into())
    } else {
        Ok(())
    }
}
pub(crate) fn subscription_catalog(bytes: &[u8]) -> Result<Vec<String>> {
    listed_ids(bytes, 262144)?.ok_or("model_list_invalid".into())
}
pub(crate) fn listed_ids(bytes: &[u8], limit: usize) -> Result<Option<Vec<String>>> {
    if bytes.len() > limit {
        return Err("model_list_invalid".into());
    }
    // The strict decoder requires an object root; wrap either upstream shape
    // without losing duplicate-key rejection within the original JSON.
    let mut wrapped = b"{\"catalog\":".to_vec();
    wrapped.extend_from_slice(bytes);
    wrapped.push(b'}');
    let root = native::decode(&wrapped).map_err(|_| "model_list_invalid")?;
    let value = &root["catalog"];
    let rows = value
        .as_array()
        .or_else(|| value["data"].as_array())
        .or_else(|| value["models"].as_array())
        .ok_or("model_list_invalid")?;
    if rows.is_empty() {
        return Ok(None);
    }
    let rows: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({"id": row.get("id").or_else(|| row.get("model")).or_else(|| row.get("slug"))})
        })
        .collect();
    native::catalog(&serde_json::to_vec(&json!({"data":rows})).map_err(|_| "model_list_invalid")?)
        .map(Some)
        .map_err(|_| "model_list_invalid".to_owned())
}

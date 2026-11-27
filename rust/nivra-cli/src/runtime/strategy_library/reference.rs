//! The repository's `strategies/` directory, embedded in every build. The
//! workspace seeds it read-only for the lanes; the executor runs a named
//! action strategy from it as a labelled fallback. Paper only.
use super::super::Result;
use serde_json::Value;

/// `(path under strategies/, bytes)`, sorted by path.
pub(crate) const FILES: &[(&str, &[u8])] =
    include!(concat!(env!("OUT_DIR"), "/reference_strategies.rs"));

/// The run id a fallback carries in strategy actions, fills and refusals.
pub(crate) const FALLBACK_PREFIX: &str = "fallback:";

/// A `strategy_actions_v1` reference: `<name>.py` with a
/// `{"version":1,"params":{...}}` parameter file.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ReferenceAction {
    pub name: &'static str,
    pub source: &'static [u8],
    pub params: Value,
}

impl ReferenceAction {
    pub(crate) fn run_id(&self) -> String {
        format!("{FALLBACK_PREFIX}{}", self.name)
    }
}

fn file(path: &str) -> Option<&'static [u8]> {
    FILES
        .iter()
        .find(|(p, _)| *p == path)
        .map(|(_, bytes)| *bytes)
}

/// The named action reference, or `None` when it is missing or a bar strategy.
pub(crate) fn action(name: &str) -> Option<ReferenceAction> {
    let (name, source) = FILES.iter().find_map(|(path, bytes)| {
        let stem = path.strip_suffix(".py")?;
        (stem == name && !stem.contains('/')).then_some((stem, *bytes))
    })?;
    let params: Value = serde_json::from_slice(file(&format!("{name}.params.json"))?).ok()?;
    (params["version"] == 1 && params["params"].is_object()).then(|| ReferenceAction {
        name,
        source,
        params: params["params"].clone(),
    })
}

/// Refuse a fallback list that names anything but an action reference, or a
/// name twice.
pub(crate) fn validate_fallback(names: &[String]) -> Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    if names.len() > 16
        || !names
            .iter()
            .all(|name| action(name).is_some() && seen.insert(name))
    {
        return Err("strategy_fallback_invalid".into());
    }
    Ok(())
}

//! A measured experiment report from a qualified host observation.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostObservation {
    pub network_unshared: bool,
    pub home_unmounted: bool,
    pub inputs_read_only: bool,
    pub cgroup_absent: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExperimentReport {
    pub sandbox_experiment: String,
    pub scientific_acceptance: String,
    pub certification: String,
    pub promotion: String,
    pub execution: String,
    pub live_enabled: bool,
    pub missing: Vec<String>,
}
const SCIENTIFIC_GAPS: &[&str] = &[
    "preregistration",
    "out_of_sample",
    "cost_stress",
    "walk_forward",
    "holdout",
    "shared_core_backtest",
    "historical_bundle",
];
fn metric_value<'a>(result: &'a AnalysisResult, name: &str) -> Option<&'a str> {
    result
        .metrics
        .iter()
        .find(|metric| metric.name == name)
        .map(|metric| metric.value.as_str())
}
/// A measured sandbox run is evidence only. It never certifies or promotes a strategy.
pub fn accept_measured_experiment(
    result: &AnalysisResult,
    script_sha256: &str,
    runtime_sha256: &str,
    input_sha256: &str,
    observed: &HostObservation,
) -> ExperimentReport {
    let mut missing = SCIENTIFIC_GAPS
        .iter()
        .map(|gap| (*gap).to_owned())
        .collect::<Vec<_>>();
    let metrics_hidden = [
        "net_visible",
        "home_visible",
        "host_file_visible",
        "input_writable",
    ]
    .iter()
    .all(|name| metric_value(result, name) == Some("0"));
    let bound = result.script_hash == script_sha256
        && result.runtime_hash == runtime_sha256
        && result.input_hash == input_sha256
        && is_hash(&result.identity.job_id)
        && is_hash(&result.identity.lease)
        && is_hash(&result.identity.registration_sha256)
        && is_hash(script_sha256)
        && is_hash(runtime_sha256)
        && is_hash(input_sha256);
    let contained = observed.network_unshared
        && observed.home_unmounted
        && observed.inputs_read_only
        && observed.cgroup_absent
        && metrics_hidden;
    if !bound {
        missing.push("result_binding".into());
    }
    if !contained {
        missing.push("containment_observation".into());
    }
    ExperimentReport {
        sandbox_experiment: if bound && contained {
            "MEASURED".into()
        } else {
            "REJECTED".into()
        },
        scientific_acceptance: "NOT_ACCEPTED".into(),
        certification: "NOT_CERTIFIED".into(),
        promotion: "NOT_AUTHORIZED".into(),
        execution: "NOT_AUTHORIZED".into(),
        live_enabled: false,
        missing,
    }
}

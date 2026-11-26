//! Opt-in planning. Models produce intents; the financial engine owns execution.
use super::*;
use nivra_core::execution::{Instrument, Portfolio};
use nivra_core::finance::{Amount, Price};
use serde_json::{Value, json};
use std::collections::BTreeMap;
mod turn;
pub(super) use turn::call_time_input;
mod plan;
pub(crate) use plan::{PlanSide, Verdict, validate, validate_actions};
mod guard;
pub(crate) use guard::CallGuard;
/// How much evidence the model sees in its user message. `ToolsOnly` is the
/// default: the host keeps the full bundle for tools, validation and audits,
/// while the model receives only a trigger envelope and fetches facts itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ContextMode {
    #[default]
    ToolsOnly,
    Projection,
}
impl ContextMode {
    fn is_tools_only(&self) -> bool {
        *self == Self::ToolsOnly
    }
}
/// Bounds what each model turn re-sends. The host still holds the full evidence:
/// tools answer for every allowlisted key and validation uses the full allowlist.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContextSettings {
    /// Feature rows inlined: held and scanned names first, then by traded value.
    /// Ignored in `tools_only` mode.
    pub universe_rows: usize,
    /// Inline the full validated allowlist (thousands of identities) in prompts.
    /// Ignored in `tools_only` mode.
    #[serde(default)]
    pub include_allowlist: bool,
    /// Decimal places kept for inlined floating-point features and scanner changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decimals: Option<u32>,
    /// `tools_only` (default) or `projection` for exact v2 rollback. The
    /// default is omitted so an existing config's serialized policy bytes do
    /// not change on upgrade.
    #[serde(default, skip_serializing_if = "ContextMode::is_tools_only")]
    pub mode: ContextMode,
}
impl ContextSettings {
    fn validate(&self) -> Result<()> {
        if !(1..=10_000).contains(&self.universe_rows) || self.decimals.is_some_and(|d| d > 12) {
            return Err("planner_context_settings_invalid".into());
        }
        Ok(())
    }
}
/// Call-time tool bounds. Defaults preserve the previous hard caps and are
/// omitted from serialized configuration so sealed policy bytes stay stable.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ToolsSettings {
    pub scanner_limit: usize,
    pub ranking_limit: usize,
    pub history_limit: usize,
    pub max_instrument_keys: usize,
    pub fill_limit: usize,
}
impl Default for ToolsSettings {
    fn default() -> Self {
        Self {
            scanner_limit: 40,
            ranking_limit: 50,
            history_limit: 5,
            max_instrument_keys: super::harness::MAX_INSTRUMENT_KEYS,
            fill_limit: 20,
        }
    }
}
impl ToolsSettings {
    pub(crate) fn is_default(&self) -> bool {
        *self == Self::default()
    }
    pub(crate) fn validate(&self) -> Result<()> {
        if !(1..=256).contains(&self.scanner_limit)
            || !(1..=256).contains(&self.ranking_limit)
            || !(1..=32).contains(&self.history_limit)
            || !(1..=super::harness::MAX_INSTRUMENT_KEYS).contains(&self.max_instrument_keys)
            || !(1..=200).contains(&self.fill_limit)
        {
            return Err("planner_tools_settings_invalid".into());
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct PlannerConfig {
    pub enabled: bool,
    /// Omission retains deployed selector semantics; explicit false disables planning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_led: Option<bool>,
    #[serde(skip_serializing_if = "PlannerConfig::default_max_handoff_instruments")]
    pub max_handoff_instruments: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery: Option<super::discovery::Settings>,
    /// Lane brief size controls. Omission means `tools_only`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextSettings>,
    /// Call-time retrieval bounds. Defaults are omitted from serialized policy.
    #[serde(default, skip_serializing_if = "ToolsSettings::is_default")]
    pub tools: ToolsSettings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_from: Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_config: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research_tools: Option<super::harness::ResearchSettings>,
    /// Lane workspace under the paper book. Omission exposes no workspace tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<super::workspace::WorkspaceSettings>,
    /// Lane web tools (#222). Omission exposes no web tool and makes no
    /// outbound call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_tools: Option<super::web_tools::WebToolsSettings>,
    /// pi agent service lanes (decision, research, review).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<super::agent::AgentSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_evidence: Option<super::decision_evidence::Settings>,
    /// Frozen exit behaviour. Absent keeps the historical execute/liquidate path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_policy: Option<nivra_core::execution::ExitPolicy>,
    /// Research strategy acceptance thresholds. Absent uses the defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_acceptance: Option<super::strategy_library::acceptance::Thresholds>,
    /// Run the library's active strategy on each scan. Absent or disabled
    /// leaves the paper controller exactly as it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_execution: Option<StrategyExecution>,
}
/// The owner's switch for the strategy executor. Paper only.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct StrategyExecution {
    pub enabled: bool,
}
/// Operational allowances do not change the frozen trading policy. Shadow-only
/// selector diagnostics remain excluded; live ranking participates in the policy.
pub(super) fn policy_bytes(cfg: &PlannerConfig) -> Result<Vec<u8>> {
    let mut cfg = cfg.clone();
    if let Some(discovery) = &mut cfg.discovery
        && discovery
            .strategy_selection
            .as_ref()
            .is_none_or(|s| !s.mode.is_live())
    {
        discovery.strategy_selection = None;
    }
    Ok(serde_json::to_vec(&cfg)?)
}
impl Default for PlannerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model_led: None,
            max_handoff_instruments: 20,
            discovery: None,
            context: None,
            tools: ToolsSettings::default(),
            effective_from: None,
            previous_config: None,
            research_tools: None,
            workspace: None,
            web_tools: None,
            agent: None,
            decision_evidence: None,
            exit_policy: None,
            strategy_acceptance: None,
            strategy_execution: None,
        }
    }
}
impl PlannerConfig {
    pub(crate) fn runs_strategies(&self) -> bool {
        self.strategy_execution.as_ref().is_some_and(|s| s.enabled)
    }
    pub(crate) fn uses_pi_paper(&self) -> bool {
        self.agent
            .as_ref()
            .is_some_and(|agent| agent.mode == super::agent::AgentMode::Paper)
    }

    fn default_max_handoff_instruments(v: &usize) -> bool {
        *v == 20
    }
    /// Preserve the deployed handoff-bound schemas when `planner.tools` is
    /// absent. An explicit tools limit wins; otherwise the old
    /// `max_handoff_instruments` bound still sizes instrument-key tools.
    pub(crate) fn effective_tools(&self) -> ToolsSettings {
        let mut tools = self.tools.clone();
        if tools.max_instrument_keys == super::harness::MAX_INSTRUMENT_KEYS
            && self.max_handoff_instruments != super::harness::MAX_INSTRUMENT_KEYS
        {
            tools.max_instrument_keys = self.max_handoff_instruments;
        }
        tools
    }
    fn validate_handoff_limit(&self) -> Result<()> {
        if !(1..=super::harness::MAX_INSTRUMENT_KEYS).contains(&self.max_handoff_instruments) {
            return Err("planner_handoff_limit_invalid".into());
        }
        Ok(())
    }
    /// Prerequisites for the pi paper decision lane. A disabled planner or an
    /// explicit `model_led: false` policy makes no model call and needs none.
    pub(crate) fn decision_lane_requirements(&self) -> Vec<&'static str> {
        if !self.enabled || self.model_led == Some(false) {
            return Vec::new();
        }
        let mut missing = Vec::new();
        if !self.uses_pi_paper() {
            missing.push("pi_paper_planner_required");
        }
        if !self
            .agent
            .as_ref()
            .is_some_and(|agent| agent.lanes.contains_key(&super::agent::Lane::Decision))
        {
            missing.push("pi_paper_decision_lane_required");
        }
        missing
    }
    pub(crate) fn validate_decision_lane(&self) -> Result<()> {
        if let Some(reason) = self.decision_lane_requirements().first() {
            return Err((*reason).into());
        }
        Ok(())
    }
    /// Resolve dated configuration without changing a predecessor book's seal.
    pub fn load_for_session(path: &Path, session: NaiveDate) -> Result<Self> {
        let mut path = path.to_path_buf();
        let mut visited = std::collections::BTreeSet::new();
        for _ in 0..32 {
            if !visited.insert(path.clone()) {
                return Err("planner_config_schedule_cycle".into());
            }
            let cfg = Self::load(&path)?;
            if cfg.effective_from.is_none_or(|date| session >= date) {
                return Ok(cfg);
            }
            path = cfg
                .previous_config
                .ok_or("planner_config_previous_required")?;
        }
        Err("planner_config_schedule_too_deep".into())
    }
    pub fn load(path: &Path) -> Result<Self> {
        let raw = crate::read_bounded(path, 65536).map_err(|_| "planner_config_unreadable")?;
        Self::parse(&raw)
    }
    pub(crate) fn parse(raw: &[u8]) -> Result<Self> {
        let value = nivra_core::native::decode(raw).map_err(|_| "planner_config_invalid")?;
        let c: Self = serde_json::from_value(value).map_err(|_| "planner_config_invalid")?;
        c.validate_handoff_limit()?;
        if let Some(settings) = &c.decision_evidence {
            settings.validate()?;
        }
        if let Some(settings) = &c.agent {
            settings.validate()?;
        }
        if c.effective_from.is_some() != c.previous_config.is_some()
            || c.previous_config.as_ref().is_some_and(|p| !p.is_absolute())
        {
            return Err("planner_config_schedule_invalid".into());
        }
        if let Some(discovery) = &c.discovery {
            discovery.validate()?;
        }
        if let Some(context) = &c.context {
            context.validate()?;
        }
        c.tools.validate()?;
        c.validate_decision_lane()?;
        if let Some(policy) = &c.exit_policy {
            policy.validate().map_err(|error| error.to_string())?;
        }
        if let Some(research) = &c.research_tools {
            research.validate()?;
        }
        if let Some(workspace) = &c.workspace {
            workspace.validate()?;
        }
        if let Some(web_tools) = &c.web_tools {
            web_tools.validate()?;
        }
        if let Some(thresholds) = &c.strategy_acceptance {
            thresholds.validate()?;
        }
        Ok(c)
    }
}
pub(crate) trait Backend {
    /// Optional immutable controller context; absence is an explicit unavailable tool result.
    fn intraday_context(&mut self, _keys: &[String], _at: i64) -> Option<Result<Value>> {
        None
    }
    /// Only an explicitly constructed offline replay backend overrides quote reads.
    fn snapshot(&mut self, _bundle: &Value, _keys: &[String], _at: i64) -> Option<Result<Value>> {
        None
    }
}

/// No prospect and no managed position: the planner has nothing to decide. This
/// is a normal no-op, not a failed attempt, a block or an alert (#221).
pub(crate) const NO_DECISION_WORK: &str = "model_plan_no_prospects_no_positions";
fn load_daily(c: &Config) -> Result<BTreeMap<String, BTreeMap<NaiveDate, Value>>> {
    let mut daily = BTreeMap::<String, BTreeMap<NaiveDate, Value>>::new();
    let Some(dir) = &c.history_directory else {
        return Ok(daily);
    };
    for item in fs::read_dir(dir)? {
        let item = item?;
        if !item
            .file_name()
            .to_string_lossy()
            .ends_with(".receipt.json")
        {
            continue;
        }
        let r = history_download::read_receipt(&item.path()).map_err(|error| match error {
            "history_receipt_unreadable" => "planner_history_unreadable",
            _ => "planner_history_invalid",
        })?;
        if r.unit != "days" {
            continue;
        }
        if item.file_name().to_string_lossy()
            != format!(
                "{}.receipt.json",
                history_download::range_id(&r.key, &r.unit, r.start, r.end)
            )
        {
            return Err("planner_history_binding".into());
        }
        for (day, row) in history_download::candles(&r)? {
            if day >= c.session {
                continue;
            }
            let retained = daily.entry(r.key.clone()).or_default();
            if let Some(old) = retained.insert(day, row.clone())
                && old != row
            {
                return Err("planner_history_conflict".into());
            }
            // Retain the configured warmup window; feature lookbacks keep their schema. Bound resident memory
            // even when the research archive contains an entire broad-universe year.
            while retained.len() > c.daily_history_sessions {
                retained.pop_first();
            }
        }
    }
    Ok(daily)
}
fn reference_document(c: &Config, closes: &BTreeMap<String, (NaiveDate, Price)>) -> Value {
    let prices = closes
        .iter()
        .map(|(key, (_, price))| (key.clone(), *price))
        .collect::<BTreeMap<_, _>>();
    let dates = closes
        .iter()
        .map(|(key, (day, _))| (key.clone(), day.to_string()))
        .collect::<BTreeMap<_, _>>();
    json!({
        "session": c.session,
        "as_of": c.previous_session,
        "close_basis": "latest_daily_before_session",
        "closes": prices,
        "close_dates": dates,
    })
}
fn bundle_with_references(
    c: &Config,
    p: &Portfolio,
    instruments: &[Instrument],
    n: usize,
) -> Result<(Value, Value)> {
    let daily = load_daily(c)?;
    let mut features = Vec::new();
    let mut historical_features = Vec::new();
    let mut reference_closes = BTreeMap::<String, (NaiveDate, Price)>::new();
    for i in instruments {
        let Some(rows) = daily.get(&i.key) else {
            continue;
        };
        if let Some((day, row)) = rows.iter().next_back() {
            reference_closes.insert(i.key.clone(), (*day, Price::parse(&row[4].to_string())?));
        }
        let bars = rows.iter().collect::<Vec<_>>();
        if bars.len() < c.daily_history_sessions.max(21) {
            continue;
        }
        let as_of = *bars.last().unwrap().0;

        let bars = &bars[bars.len() - 21..];
        let num = |v: &Value| -> Result<f64> {
            let n = v.as_f64().ok_or("planner_history_number")?;
            if !n.is_finite() || n < 0.0 {
                return Err("planner_history_number".into());
            }
            Ok(n)
        };
        let closes = bars
            .iter()
            .map(|(_, v)| num(&v[4]))
            .collect::<Result<Vec<_>>>()?;
        if closes.iter().any(|v| *v <= 0.0) {
            return Err("planner_history_close".into());
        }
        let volumes = bars
            .iter()
            .map(|(_, v)| num(&v[5]))
            .collect::<Result<Vec<_>>>()?;
        let returns = closes
            .windows(2)
            .map(|w| w[1] / w[0] - 1.0)
            .collect::<Vec<_>>();
        let mean = returns.iter().sum::<f64>() / 20.0;
        let volatility = (returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / 20.0).sqrt();
        let vm = volumes[..20].iter().sum::<f64>() / 20.0;
        if vm <= 0.0 || !volatility.is_finite() {
            continue;
        }
        let vs = (volumes[..20].iter().map(|v| (v - vm).powi(2)).sum::<f64>() / 20.0).sqrt();
        let prior_close = Price::parse(&bars[20].1[4].to_string())?;
        let row = json!({"instrument":i,"last_close":closes[20],"prior_close":prior_close,"as_of":as_of,"return_1d":closes[20]/closes[19]-1.0,"return_5d":closes[20]/closes[15]-1.0,"return_20d":closes[20]/closes[0]-1.0,"volatility_20d":volatility,"volume_z_score":if vs>0.0 {Some((volumes[20]-vm)/vs)}else{None},"gap":num(&bars[20].1[1])?/closes[19]-1.0,"gap_as_of":bars[20].0,"liquidity":vm*closes[20],"history_as_of":as_of,"history_current":as_of==c.previous_session});
        historical_features.push(row.clone());
        if as_of == c.previous_session {
            features.push(row);
        }
    }
    features.sort_by(|a, b| {
        b["liquidity"]
            .as_f64()
            .partial_cmp(&a["liquidity"].as_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a["volatility_20d"]
                    .as_f64()
                    .partial_cmp(&b["volatility_20d"].as_f64())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                a["instrument"]["key"]
                    .as_str()
                    .cmp(&b["instrument"]["key"].as_str())
            })
    });
    let eligible_feature_count = features.len();
    features.truncate(n);
    Ok((
        json!({"session":c.session,"account":c.account,"cash":p.cash,"positions":p.positions,"allowlist":instruments,"shortlist":features,"historical_features":historical_features,"fees":p.costs.clone().ok_or("paper_costs_required")?,"data_selection":"descending trailing traded value, then ascending volatility; not a strategy","history_missing":daily.is_empty(),"eligible_feature_count":eligible_feature_count,"presented_feature_count":features.len(),"feature_coverage_partial":features.len()<eligible_feature_count}),
        reference_document(c, &reference_closes),
    ))
}

fn is_saved_hash(value: &Value) -> bool {
    value.as_str().is_some_and(|text| {
        text.len() == 64
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}
pub(crate) fn validate_saved(c: &Config, plan: &Value) -> Result<()> {
    let id = plan["decision_id"]
        .as_str()
        .ok_or("planner_saved_plan_invalid")?;
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        || plan["session_binding"] != book_binding(c)?
        || plan["input_hash"] != digest(&serde_json::to_vec(&plan["input"])?)
        || plan["fake"] != false
        || plan["dry_run"] != false
        || plan["verdicts"].as_array().is_none_or(|v| v.len() > 200)
    {
        return Err("planner_saved_binding_invalid".into());
    }
    // A1 intent binding. A present revision must be a well-formed digest; a
    // missing value keeps old archived plans readable and executable.
    for key in [
        "portfolio_revision",
        "policy_sha256",
        "profile_sha256",
        "digest_id",
    ] {
        if let Some(value) = plan.get(key)
            && !value.is_null()
            && !is_saved_hash(value)
        {
            return Err("planner_saved_binding_invalid".into());
        }
    }
    if let Some(references) = plan.get("market_references")
        && (plan["market_references_hash"] != digest(&serde_json::to_vec(references)?)
            || references["session"] != c.session.to_string()
            || references["as_of"] != c.previous_session.to_string()
            || references["closes"]
                .as_object()
                .is_none_or(|m| m.len() > 20000))
    {
        return Err("planner_saved_references_invalid".into());
    }
    Ok(())
}
/// Admission for initial planning does not require an existing model plan.
/// Empty feature bundles must not burn the one-shot legacy attempt.
pub(super) fn has_eligible_input(
    c: &Config,
    p: &Portfolio,
    instruments: &[Instrument],
) -> Result<bool> {
    let cfg = c
        .planner_config
        .as_deref()
        .map(|path| PlannerConfig::load_for_session(path, c.session))
        .transpose()?;
    if cfg.is_some() && !p.positions.is_empty() {
        // A held book is evidence enough to compile a fresh factual digest,
        // even when the scanner has no new prospect.
        return Ok(true);
    }
    if let Some(settings) = cfg.as_ref().and_then(|cfg| cfg.discovery.as_ref()) {
        let snapshot = super::discovery::snapshot(c, instruments, settings, millis())?;
        let fresh = snapshot["fresh_count"].as_u64().unwrap_or(0) > 0
            || snapshot["eligible_count"].as_u64().unwrap_or(0) > 0
            || snapshot["prospects"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty());
        return Ok(fresh);
    }
    let (input, _) = bundle_with_references(c, p, instruments, 1)?;
    Ok(input["shortlist"].as_array().is_some_and(|a| !a.is_empty())
        || p.positions
            .values()
            .any(|p| instruments.contains(&p.instrument)))
}

pub(crate) fn status(plan: &Value) -> Value {
    json!({"status":"validated_plan","decision_id":plan["decision_id"],"model_id":plan["model_id"],"provider":plan["provider"],"actions":plan["verdicts"],"budget_used":plan["budget_used"],"budgets":plan["budgets"]})
}
#[derive(clap::Subcommand)]
pub enum Command {
    /// Validate and describe a dated planner config without credentials or model calls.
    ConfigCheck {
        #[arg(long)]
        planner: PathBuf,
        #[arg(long)]
        session: NaiveDate,
    },
    /// Copy a reviewed, hash-pinned research script into the configured strategy library.
    RegisterRanker {
        #[arg(long)]
        planner: PathBuf,
        #[arg(long)]
        session: NaiveDate,
        #[arg(long)]
        book: PathBuf,
        #[arg(long)]
        script: String,
        #[arg(long)]
        expected_sha256: String,
        #[arg(long)]
        author_model: String,
    },
}
pub fn entry(cmd: Command) -> ExitCode {
    match cmd {
        Command::ConfigCheck { planner, session } => {
            match PlannerConfig::load_for_session(&planner, session) {
                Ok(cfg) => {
                    println!(
                        "{}",
                        json!({
                            "status":"planner_config_valid","session":session,
                            "policy_sha256":digest(&policy_bytes(&cfg).expect("serializable")),
                            "enabled":cfg.enabled,"model_led":cfg.model_led,"discovery":cfg.discovery,
                            "agent_mode":cfg.agent.as_ref().map(|agent| agent.mode),
                            "agent_lanes":cfg.agent.as_ref().map(|agent| &agent.lanes),
                            "effective_from":cfg.effective_from,"paper_only":true,"model_calls":0
                        })
                    );
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("planner_config_refused: {}", failure_code(error.as_ref()));
                    ExitCode::FAILURE
                }
            }
        }
        Command::RegisterRanker {
            planner,
            session,
            book,
            script,
            expected_sha256,
            author_model,
        } => {
            let register = || -> Result<_> {
                let cfg = PlannerConfig::load_for_session(&planner, session)?;
                let workspace = cfg.workspace.as_ref().ok_or("workspace_disabled")?;
                let selection = cfg
                    .discovery
                    .as_ref()
                    .and_then(|discovery| discovery.strategy_selection.as_ref())
                    .filter(|selection| selection.enabled)
                    .ok_or("strategy_selection_library_required")?;
                super::workspace::register_ranker(
                    &book,
                    workspace,
                    &script,
                    &expected_sha256,
                    &author_model,
                    Path::new(&selection.library_path),
                )
            };
            match register() {
                Ok(version) => {
                    println!(
                        "{}",
                        json!({
                            "status":"ranker_registered","name":version.name,"sha256":version.sha256,
                            "version":version.version,"paper_only":true
                        })
                    );
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!(
                        "ranker_registration_refused: {}",
                        failure_code(error.as_ref())
                    );
                    ExitCode::FAILURE
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod remaining_settings_contract;

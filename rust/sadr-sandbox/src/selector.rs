//! Pure offline evidence selection; no runtime activation or model calls.
use crate::library::RunResult;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const DEFAULT_ID: &str = "default";

pub struct CandidateEvidence<'a> {
    pub sha256: &'a str,
    pub name: &'a str,
    pub results: &'a [RunResult],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SelectorConfig {
    pub min_replay_sessions: usize,
    pub min_shadow_sessions: usize,
    pub min_trades: u64,
    pub min_expectancy: f64,
    pub paper_floor_sessions: usize,
    pub paper_floor: f64,
    pub cooldown_ms: i64,
    pub switch_margin: f64,
}
impl Default for SelectorConfig {
    fn default() -> Self {
        Self {
            min_replay_sessions: 5,
            min_shadow_sessions: 5,
            min_trades: 30,
            min_expectancy: 0.0,
            paper_floor_sessions: 3,
            paper_floor: 0.0,
            cooldown_ms: 1_800_000,
            switch_margin: 0.0005,
        }
    }
}
impl SelectorConfig {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.min_replay_sessions == 0
            || self.min_shadow_sessions == 0
            || self.min_trades == 0
            || self.paper_floor_sessions == 0
            || self.cooldown_ms < 0
            || !self.min_expectancy.is_finite()
            || !self.paper_floor.is_finite()
            || !self.switch_margin.is_finite()
            || self.switch_margin < 0.0
        {
            return Err("invalid selector config");
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectorState {
    pub active: String,
    pub last_switch_ms: Option<i64>,
}
impl Default for SelectorState {
    fn default() -> Self {
        Self {
            active: DEFAULT_ID.into(),
            last_switch_ms: None,
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct ScorerHealth {
    pub stale_data: bool,
    /// Includes scorer errors, timeouts and resource limit breaches.
    pub scorer_failure: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionReason {
    UnknownRegime,
    NoEligibleCandidates,
    StaleData,
    ScorerFailure,
    PaperFloorBreach,
    InvalidConfig,
    BestEvidence,
    Cooldown,
    Hysteresis,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub chosen_id: String,
    pub reason: SelectionReason,
    pub switched: bool,
    pub state: SelectorState,
}
struct Evidence<'a> {
    id: &'a str,
    shadow: f64,
    sessions: usize,
    eligible: bool,
    floor_breach: bool,
}
// Sorting before summation makes results independent of input order. Divide first
// to avoid overflowing a sum of otherwise finite metrics.
fn mean(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    let n = values.len() as f64;
    values.into_iter().map(|v| v / n).sum()
}
fn evidence<'a>(c: &CandidateEvidence<'a>, regime: &str, cfg: &SelectorConfig) -> Evidence<'a> {
    let mut sessions: BTreeMap<(&str, &str), Vec<f64>> = BTreeMap::new();
    let mut trades = 0_u64;
    for r in c.results {
        if r.sha256 != c.sha256
            || r.regime.as_deref() != Some(regime)
            || !matches!(r.run_kind.as_str(), "replay" | "shadow" | "paper")
            || chrono::NaiveDate::parse_from_str(&r.session_date, "%Y-%m-%d").is_err()
        {
            continue;
        }
        let Some(count) = r.metrics.get("trades").and_then(serde_json::Value::as_u64) else {
            continue;
        };
        if ["expectancy", "net_pnl_after_costs", "max_drawdown"]
            .iter()
            .any(|k| {
                r.metrics
                    .get(k)
                    .and_then(serde_json::Value::as_f64)
                    .is_none_or(|v| !v.is_finite())
            })
        {
            continue;
        }
        let expectancy = r.metrics["expectancy"].as_f64().unwrap_or_default();
        trades = trades.saturating_add(count);
        sessions
            .entry((&r.run_kind, &r.session_date))
            .or_default()
            .push(expectancy);
    }
    let by_kind = |kind| {
        sessions
            .iter()
            .filter(|((k, _), _)| *k == kind)
            .map(|(_, v)| mean(v.clone()))
            .collect::<Vec<_>>()
    };
    let replay = by_kind("replay");
    let shadow = by_kind("shadow");
    let paper = by_kind("paper");
    let floor_breach = paper
        .iter()
        .rev()
        .take(cfg.paper_floor_sessions)
        .any(|v| *v < cfg.paper_floor);
    let overall = mean(sessions.values().map(|v| mean(v.clone())).collect());
    Evidence {
        id: c.sha256,
        shadow: if shadow.is_empty() {
            f64::NEG_INFINITY
        } else {
            mean(shadow.clone())
        },
        sessions: sessions.len(),
        eligible: c.sha256 != DEFAULT_ID
            && replay.len() >= cfg.min_replay_sessions
            && shadow.len() >= cfg.min_shadow_sessions
            && trades >= cfg.min_trades
            && overall.is_finite()
            && overall > cfg.min_expectancy
            && !floor_breach,
        floor_breach,
    }
}

/// Safety precedence: stale data, scorer failure, active paper floor, config,
/// unknown regime, then evidence. Default fallback always bypasses switch guards.
pub fn select(
    candidates: &[CandidateEvidence<'_>],
    regime_label: Option<&str>,
    state: &SelectorState,
    health: &ScorerHealth,
    now_ms: i64,
    cfg: &SelectorConfig,
) -> Selection {
    use SelectionReason::*;
    let evidence: Vec<_> = regime_label
        .map(|r| candidates.iter().map(|c| evidence(c, r, cfg)).collect())
        .unwrap_or_default();
    let active = evidence.iter().find(|e| e.id == state.active);
    let fallback = if health.stale_data {
        Some(StaleData)
    } else if health.scorer_failure {
        Some(ScorerFailure)
    } else if active.is_some_and(|e| e.floor_breach) {
        Some(PaperFloorBreach)
    } else if cfg.validate().is_err() {
        Some(InvalidConfig)
    } else if regime_label.is_none() {
        Some(UnknownRegime)
    } else if state.active != DEFAULT_ID && !active.is_some_and(|e| e.eligible) {
        Some(NoEligibleCandidates)
    } else {
        None
    };
    let best = evidence.iter().filter(|e| e.eligible).max_by(|a, b| {
        a.shadow
            .total_cmp(&b.shadow)
            .then(a.sessions.cmp(&b.sessions))
            .then_with(|| b.id.cmp(a.id))
    });
    let (mut chosen, mut reason) = if let Some(reason) = fallback {
        (DEFAULT_ID, reason)
    } else {
        best.map_or((DEFAULT_ID, NoEligibleCandidates), |e| (e.id, BestEvidence))
    };
    if chosen != state.active && chosen != DEFAULT_ID {
        if state
            .last_switch_ms
            .is_some_and(|t| now_ms.saturating_sub(t) < cfg.cooldown_ms)
        {
            chosen = &state.active;
            reason = Cooldown;
        } else if let (Some(new), Some(old)) = (best, active)
            && chosen != DEFAULT_ID
            && new.shadow - old.shadow <= cfg.switch_margin
        {
            chosen = &state.active;
            reason = Hysteresis;
        }
    }
    let switched = chosen != state.active;
    Selection {
        chosen_id: chosen.into(),
        reason,
        switched,
        state: SelectorState {
            active: chosen.into(),
            last_switch_ms: if switched {
                Some(now_ms)
            } else {
                state.last_switch_ms
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn runs(id: &str, expectancy: f64) -> Vec<RunResult> {
        ["replay", "shadow"].into_iter().flat_map(|kind| (1..=5).map(move |day| RunResult {
            sha256: id.into(), run_kind: kind.into(), session_date: format!("2026-10-{day:02}"),
            regime: Some("trend".into()), recorded_at_ms: day,
            metrics: json!({"trades":3,"expectancy":expectancy,"net_pnl_after_costs":1,"max_drawdown":0}),
        })).collect()
    }
    fn candidate<'a>(id: &'a str, results: &'a [RunResult]) -> CandidateEvidence<'a> {
        CandidateEvidence {
            sha256: id,
            name: id,
            results,
        }
    }
    #[test]
    fn fallbacks_and_safety_override_cooldown() {
        let cfg = SelectorConfig::default();
        let state = SelectorState {
            active: "a".into(),
            last_switch_ms: Some(100),
        };
        let mut results = runs("a", 0.1);
        let mut paper = results[0].clone();
        paper.run_kind = "paper".into();
        paper.metrics["expectancy"] = json!(-0.1);
        results.push(paper);
        for (regime, health, candidates, expected) in [
            (
                None,
                ScorerHealth::default(),
                vec![],
                SelectionReason::UnknownRegime,
            ),
            (
                Some("trend"),
                ScorerHealth::default(),
                vec![],
                SelectionReason::NoEligibleCandidates,
            ),
            (
                None,
                ScorerHealth {
                    stale_data: true,
                    scorer_failure: false,
                },
                vec![],
                SelectionReason::StaleData,
            ),
            (
                None,
                ScorerHealth {
                    stale_data: false,
                    scorer_failure: true,
                },
                vec![],
                SelectionReason::ScorerFailure,
            ),
            (
                Some("trend"),
                ScorerHealth::default(),
                vec![candidate("a", &results)],
                SelectionReason::PaperFloorBreach,
            ),
        ] {
            let selected = select(&candidates, regime, &state, &health, 101, &cfg);
            assert_eq!(selected.reason, expected);
            assert_eq!(selected.chosen_id, DEFAULT_ID);
            assert!(selected.switched);
            assert_eq!(selected.state.last_switch_ms, Some(101));
            let selected = select(&candidates, regime, &state, &health, 2_000_000, &cfg);
            assert_eq!(selected.reason, expected);
            assert_eq!(selected.chosen_id, DEFAULT_ID);
            assert!(selected.switched);
        }
    }
    #[test]
    fn thin_active_evidence_falls_back_despite_switch_guards() {
        let cfg = SelectorConfig::default();
        let mut thin = runs("a", 0.2);
        thin.pop(); // Too few shadow sessions, despite a better shadow score.
        let eligible = runs("b", 0.1);
        for last_switch_ms in [Some(100), None] {
            let selected = select(
                &[candidate("a", &thin), candidate("b", &eligible)],
                Some("trend"),
                &SelectorState {
                    active: "a".into(),
                    last_switch_ms,
                },
                &ScorerHealth::default(),
                101,
                &cfg,
            );
            assert_eq!(selected.chosen_id, DEFAULT_ID);
            assert_eq!(selected.reason, SelectionReason::NoEligibleCandidates);
            assert!(selected.switched);
            assert_eq!(selected.state.last_switch_ms, Some(101));
        }
    }

    #[test]
    fn switching_to_non_default_still_obeys_cooldown() {
        let results = runs("a", 0.1);
        let selected = select(
            &[candidate("a", &results)],
            Some("trend"),
            &SelectorState {
                active: DEFAULT_ID.into(),
                last_switch_ms: Some(100),
            },
            &ScorerHealth::default(),
            101,
            &SelectorConfig::default(),
        );
        assert_eq!(selected.chosen_id, DEFAULT_ID);
        assert_eq!(selected.reason, SelectionReason::Cooldown);
        assert!(!selected.switched);
    }

    #[test]
    fn ranking_hysteresis_regimes_and_malformed_evidence() {
        let cfg: SelectorConfig = serde_json::from_str("{}").unwrap();
        assert!(cfg.validate().is_ok());
        assert!(
            SelectorConfig {
                switch_margin: f64::NAN,
                ..cfg.clone()
            }
            .validate()
            .is_err()
        );
        let a = runs("a", 0.1);
        let mut b = runs("b", 0.1001);
        let health = ScorerHealth::default();
        let state = SelectorState {
            active: "a".into(),
            last_switch_ms: None,
        };
        assert_eq!(
            select(
                &[candidate("a", &a), candidate("b", &b)],
                Some("trend"),
                &state,
                &health,
                0,
                &cfg
            )
            .reason,
            SelectionReason::Hysteresis
        );
        b = runs("b", 0.2);
        let selected = select(
            &[candidate("a", &a), candidate("b", &b)],
            Some("trend"),
            &state,
            &health,
            10,
            &cfg,
        );
        assert_eq!(selected.chosen_id, "b");
        assert_eq!(selected.state.last_switch_ms, Some(10));
        b.reverse();
        assert_eq!(
            selected,
            select(
                &[candidate("b", &b), candidate("a", &a)],
                Some("trend"),
                &state,
                &health,
                10,
                &cfg
            )
        );
        assert_eq!(
            select(
                &[candidate("a", &a)],
                Some("range"),
                &SelectorState::default(),
                &health,
                0,
                &cfg
            )
            .reason,
            SelectionReason::NoEligibleCandidates
        );
        let mut bad = runs("b", 0.2);
        bad[0].metrics = json!({"expectancy":"invalid"});
        assert_eq!(
            select(
                &[candidate("b", &bad)],
                Some("trend"),
                &SelectorState::default(),
                &health,
                0,
                &cfg
            )
            .reason,
            SelectionReason::NoEligibleCandidates
        );
        let zero = runs("zero", 0.0);
        assert_eq!(
            select(
                &[candidate("zero", &zero)],
                Some("trend"),
                &SelectorState::default(),
                &health,
                0,
                &cfg
            )
            .reason,
            SelectionReason::NoEligibleCandidates
        );
        let same = runs("b", 0.1);
        assert_eq!(
            select(
                &[candidate("b", &same), candidate("a", &a)],
                Some("trend"),
                &SelectorState::default(),
                &health,
                0,
                &cfg
            )
            .chosen_id,
            "a"
        );
    }
}

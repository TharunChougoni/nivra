//! A5 Activity Measurement and Candidate Attribution.
//!
//! Tracks candidate-to-decision/action/fill conversion rates, categorized abstention
//! reasons, after-cost financial performance, turnover and cost drag, peak-to-trough
//! drawdown, decision latency distributions, and experiment outcome attribution
//! linking financial outcomes to selection provenance (`source_hash`) and the trial ledger.

use sadr_core::finance::{Amount, FinancialError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Conversion metrics along the pipeline from evaluated candidates to fills.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateConversion {
    pub candidates_evaluated: u64,
    pub decisions_considered: u64,
    pub actions_proposed: u64,
    pub actions_validated: u64,
    pub fills_executed: u64,
    pub candidate_to_decision_rate: f64,
    pub decision_to_action_rate: f64,
    pub action_to_fill_rate: f64,
    pub overall_conversion_rate: f64,
}

impl CandidateConversion {
    pub fn new(
        candidates_evaluated: u64,
        decisions_considered: u64,
        actions_proposed: u64,
        actions_validated: u64,
        fills_executed: u64,
    ) -> Self {
        let candidate_to_decision_rate = if candidates_evaluated > 0 {
            decisions_considered as f64 / candidates_evaluated as f64
        } else {
            0.0
        };
        let decision_to_action_rate = if decisions_considered > 0 {
            actions_proposed as f64 / decisions_considered as f64
        } else {
            0.0
        };
        let action_to_fill_rate = if actions_proposed > 0 {
            fills_executed as f64 / actions_proposed as f64
        } else {
            0.0
        };
        let overall_conversion_rate = if candidates_evaluated > 0 {
            fills_executed as f64 / candidates_evaluated as f64
        } else {
            0.0
        };
        Self {
            candidates_evaluated,
            decisions_considered,
            actions_proposed,
            actions_validated,
            fills_executed,
            candidate_to_decision_rate,
            decision_to_action_rate,
            action_to_fill_rate,
            overall_conversion_rate,
        }
    }
}

/// Categorized taxonomy of abstention and refusal reasons.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AbstentionTaxonomy {
    pub model_abstained: u64,
    pub risk_preflight_blocked: u64,
    pub freshness_expired: u64,
    pub spread_cost_excessive: u64,
    pub market_closed_or_halted: u64,
    pub universe_filter_excluded: u64,
    pub candidate_confidence_low: u64,
    pub duplicate_or_in_flight: u64,
    pub other: u64,
    pub reasons: BTreeMap<String, u64>,
    pub total_abstentions: u64,
}

impl AbstentionTaxonomy {
    pub fn classify(reason: &str) -> &'static str {
        let r = reason.to_ascii_lowercase();
        if r.contains("wait")
            || r.contains("hold")
            || r.contains("abstain")
            || r.contains("no_trade")
            || r.contains("no_opportunity")
        {
            "model_abstained"
        } else if r.contains("paper_cash_scope")
            || r.contains("insufficient_cash")
            || r.contains("risk")
            || r.contains("drawdown")
            || r.contains("capital")
        {
            "risk_preflight_blocked"
        } else if r.contains("stale")
            || r.contains("freshness")
            || r.contains("age")
            || r.contains("expired")
            || r.contains("deadline")
        {
            "freshness_expired"
        } else if r.contains("spread") || r.contains("cost") || r.contains("slippage") {
            "spread_cost_excessive"
        } else if r.contains("halt") || r.contains("closed") || r.contains("session") {
            "market_closed_or_halted"
        } else if r.contains("universe") || r.contains("exclusion") || r.contains("filter") {
            "universe_filter_excluded"
        } else if r.contains("confidence") || r.contains("score") || r.contains("rank") {
            "candidate_confidence_low"
        } else if r.contains("duplicate")
            || r.contains("pending")
            || r.contains("in_flight")
            || r.contains("superseded")
        {
            "duplicate_or_in_flight"
        } else {
            "other"
        }
    }

    pub fn record(&mut self, reason: &str) {
        self.total_abstentions += 1;
        *self.reasons.entry(reason.to_string()).or_insert(0) += 1;
        match Self::classify(reason) {
            "model_abstained" => self.model_abstained += 1,
            "risk_preflight_blocked" => self.risk_preflight_blocked += 1,
            "freshness_expired" => self.freshness_expired += 1,
            "spread_cost_excessive" => self.spread_cost_excessive += 1,
            "market_closed_or_halted" => self.market_closed_or_halted += 1,
            "universe_filter_excluded" => self.universe_filter_excluded += 1,
            "candidate_confidence_low" => self.candidate_confidence_low += 1,
            "duplicate_or_in_flight" => self.duplicate_or_in_flight += 1,
            _ => self.other += 1,
        }
    }
}

/// Realized and marked P&L after explicit fees, with benchmark slippage reported
/// separately as a diagnostic cost drag. Fill prices already include adverse
/// slippage, so subtracting it from P&L again would count that impact twice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinancialAfterCost {
    pub realized_pnl: Amount,
    pub marked_pnl: Amount,
    pub gross_profit: Amount,
    pub total_fees: Amount,
    pub total_slippage: Amount,
    pub total_cost_drag: Amount,
    pub net_after_cost_pnl: Amount,
}

impl FinancialAfterCost {
    pub fn calculate(
        realized_pnl: Amount,
        marked_pnl: Amount,
        total_fees: Amount,
        total_slippage: Amount,
    ) -> Result<Self, FinancialError> {
        let gross_profit = realized_pnl.checked_add(marked_pnl)?;
        let total_cost_drag = total_fees.checked_add(total_slippage)?;
        let net_after_cost_pnl = gross_profit.checked_sub(total_fees)?;
        Ok(Self {
            realized_pnl,
            marked_pnl,
            gross_profit,
            total_fees,
            total_slippage,
            total_cost_drag,
            net_after_cost_pnl,
        })
    }
}

/// Turnover ratio and fee/slippage cost drag metrics in basis points.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnoverCostDrag {
    pub notional_traded: Amount,
    pub capital: Amount,
    pub turnover_ratio: f64,
    pub fee_drag_bps: f64,
    pub slippage_drag_bps: f64,
    pub total_cost_drag_bps: f64,
}

impl TurnoverCostDrag {
    pub fn calculate(
        notional_traded: Amount,
        capital: Amount,
        total_fees: Amount,
        total_slippage: Amount,
    ) -> Result<Self, FinancialError> {
        let notional_f64 = notional_traded.to_string().parse::<f64>().unwrap_or(0.0);
        let capital_f64 = capital.to_string().parse::<f64>().unwrap_or(1.0);
        let turnover_ratio = if capital_f64 > 0.0 {
            notional_f64 / capital_f64
        } else {
            0.0
        };

        let fees_f64 = total_fees.to_string().parse::<f64>().unwrap_or(0.0);
        let slippage_f64 = total_slippage.to_string().parse::<f64>().unwrap_or(0.0);

        let fee_drag_bps = if notional_f64 > 0.0 {
            (fees_f64 / notional_f64) * 10_000.0
        } else {
            0.0
        };
        let slippage_drag_bps = if notional_f64 > 0.0 {
            (slippage_f64 / notional_f64) * 10_000.0
        } else {
            0.0
        };
        let total_cost_drag_bps = fee_drag_bps + slippage_drag_bps;

        Ok(Self {
            notional_traded,
            capital,
            turnover_ratio,
            fee_drag_bps,
            slippage_drag_bps,
            total_cost_drag_bps,
        })
    }
}

/// Peak-to-trough drawdown calculation from equity time-series.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DrawdownTracker {
    pub high_water_mark: Amount,
    pub current_equity: Amount,
    pub current_drawdown: Amount,
    pub max_drawdown: Amount,
    pub max_drawdown_pct: f64,
    pub max_drawdown_duration_ms: u64,
}

impl DrawdownTracker {
    pub fn from_series(points: &[(u64, Amount)]) -> Result<Self, FinancialError> {
        if points.is_empty() {
            return Ok(Self {
                high_water_mark: Amount::ZERO,
                current_equity: Amount::ZERO,
                current_drawdown: Amount::ZERO,
                max_drawdown: Amount::ZERO,
                max_drawdown_pct: 0.0,
                max_drawdown_duration_ms: 0,
            });
        }

        let mut peak = points[0].1;
        let mut peak_time = points[0].0;
        let mut max_dd = Amount::ZERO;
        let mut max_dd_pct = 0.0;
        let mut max_dd_duration = 0u64;

        for &(time, equity) in points {
            if equity > peak {
                peak = equity;
                peak_time = time;
            } else {
                let dd = peak.checked_sub(equity)?;
                if dd > max_dd {
                    max_dd = dd;
                    let peak_f64 = peak.to_string().parse::<f64>().unwrap_or(1.0);
                    let dd_f64 = dd.to_string().parse::<f64>().unwrap_or(0.0);
                    max_dd_pct = if peak_f64 > 0.0 {
                        (dd_f64 / peak_f64) * 100.0
                    } else {
                        0.0
                    };
                    max_dd_duration = time.saturating_sub(peak_time);
                }
            }
        }

        let current_equity = points.last().map(|p| p.1).unwrap_or(Amount::ZERO);
        let current_drawdown = if peak > current_equity {
            peak.checked_sub(current_equity)?
        } else {
            Amount::ZERO
        };

        Ok(Self {
            high_water_mark: peak,
            current_equity,
            current_drawdown,
            max_drawdown: max_dd,
            max_drawdown_pct: max_dd_pct,
            max_drawdown_duration_ms: max_dd_duration,
        })
    }
}

/// Latency distribution metrics across decision loop turns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionLatencyStats {
    pub count: usize,
    pub min_ms: u64,
    pub max_ms: u64,
    pub mean_ms: f64,
    pub p50_ms: u64,
    pub p90_ms: u64,
    pub p95_ms: u64,
    pub p99_ms: u64,
    pub buckets: BTreeMap<String, u64>,
}

impl DecisionLatencyStats {
    pub fn from_latencies(latencies: &[u64]) -> Self {
        if latencies.is_empty() {
            let mut buckets = BTreeMap::new();
            buckets.insert("<500ms".to_string(), 0);
            buckets.insert("500ms-1s".to_string(), 0);
            buckets.insert("1s-2s".to_string(), 0);
            buckets.insert("2s-5s".to_string(), 0);
            buckets.insert("5s-10s".to_string(), 0);
            buckets.insert(">10s".to_string(), 0);
            return Self {
                count: 0,
                min_ms: 0,
                max_ms: 0,
                mean_ms: 0.0,
                p50_ms: 0,
                p90_ms: 0,
                p95_ms: 0,
                p99_ms: 0,
                buckets,
            };
        }

        let mut sorted = latencies.to_vec();
        sorted.sort_unstable();

        let count = sorted.len();
        let min_ms = sorted[0];
        let max_ms = sorted[count - 1];
        let sum: u64 = sorted.iter().sum();
        let mean_ms = sum as f64 / count as f64;

        let percentile = |p: f64| -> u64 {
            let idx = ((p / 100.0) * (count as f64 - 1.0)).round() as usize;
            sorted[idx.min(count - 1)]
        };

        let p50_ms = percentile(50.0);
        let p90_ms = percentile(90.0);
        let p95_ms = percentile(95.0);
        let p99_ms = percentile(99.0);

        let mut buckets = BTreeMap::new();
        let mut b_500 = 0u64;
        let mut b_1s = 0u64;
        let mut b_2s = 0u64;
        let mut b_5s = 0u64;
        let mut b_10s = 0u64;
        let mut b_inf = 0u64;

        for &lat in latencies {
            match lat {
                0..=499 => b_500 += 1,
                500..=999 => b_1s += 1,
                1000..=1999 => b_2s += 1,
                2000..=4999 => b_5s += 1,
                5000..=9999 => b_10s += 1,
                _ => b_inf += 1,
            }
        }
        buckets.insert("<500ms".to_string(), b_500);
        buckets.insert("500ms-1s".to_string(), b_1s);
        buckets.insert("1s-2s".to_string(), b_2s);
        buckets.insert("2s-5s".to_string(), b_5s);
        buckets.insert("5s-10s".to_string(), b_10s);
        buckets.insert(">10s".to_string(), b_inf);

        Self {
            count,
            min_ms,
            max_ms,
            mean_ms,
            p50_ms,
            p90_ms,
            p95_ms,
            p99_ms,
            buckets,
        }
    }
}

/// Financial outcome attribution to a specific candidate version and its provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentAttribution {
    pub source_hash: String,
    pub spec_id: Option<String>,
    pub selection_reason: Option<String>,
    pub decisions_count: u64,
    pub fills_count: u64,
    pub notional_volume: Amount,
    pub realized_pnl: Amount,
    pub marked_pnl: Amount,
    pub fees_incurred: Amount,
    pub net_after_cost_pnl: Amount,
    pub trial_status: Option<String>,
}

impl ExperimentAttribution {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source_hash: String,
        spec_id: Option<String>,
        selection_reason: Option<String>,
        decisions_count: u64,
        fills_count: u64,
        notional_volume: Amount,
        realized_pnl: Amount,
        marked_pnl: Amount,
        fees_incurred: Amount,
        trial_status: Option<String>,
    ) -> Result<Self, FinancialError> {
        let gross_profit = realized_pnl.checked_add(marked_pnl)?;
        let net_after_cost_pnl = gross_profit.checked_sub(fees_incurred)?;
        Ok(Self {
            source_hash,
            spec_id,
            selection_reason,
            decisions_count,
            fills_count,
            notional_volume,
            realized_pnl,
            marked_pnl,
            fees_incurred,
            net_after_cost_pnl,
            trial_status,
        })
    }
}

/// Comprehensive Activity and Attribution Report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityReport {
    pub session: String,
    pub conversion: CandidateConversion,
    pub abstentions: AbstentionTaxonomy,
    pub financials: FinancialAfterCost,
    pub turnover: TurnoverCostDrag,
    pub drawdown: DrawdownTracker,
    pub latency: DecisionLatencyStats,
    pub experiment_attributions: Vec<ExperimentAttribution>,
}

impl ActivityReport {
    pub fn to_json(&self) -> Value {
        json!({
            "session": self.session,
            "conversion": self.conversion,
            "abstentions": self.abstentions,
            "financials": self.financials,
            "turnover": self.turnover,
            "drawdown": self.drawdown,
            "drawdown_coverage": {
                "basis": "realized_fill_checkpoints_plus_current_marks",
                "complete": false,
                "reason": "historical position marks are not durably retained with this report",
            },
            "latency": self.latency,
            "experiment_attributions": self.experiment_attributions,
        })
    }
}

//! The autonomous research protocol: experiment identity, the keep rule, the
//! IST research-day budget and the loop's working window. Pure functions only;
//! the record lives in `nivra-store::research_memory`.
use crate::finance::Amount;
use crate::replay::digest;
use chrono::{DateTime, NaiveDate};
use serde::{Deserialize, Serialize};

/// Bumped when the experiment key encoding or the keep rule changes. Every
/// experiment key includes it, so a new protocol never reuses an old score.
pub const PROTOCOL_VERSION: u32 = 1;
/// The keep rule's identity, part of every experiment key.
pub const EVALUATOR: &str = "keep_v1:median_paired_after_cost";
/// Research capital per sleeve, in rupees. Every scored book starts here.
pub const RESEARCH_CAPITAL_RUPEES: i64 = 5_000;
const IST_OFFSET_MS: i64 = 19_800_000;

/// Which harness scores an experiment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Archived bars, run as one bounded job.
    Bars,
    /// A recorded quote tape, replayed inside the tool call.
    QuoteReplay,
}

impl Stage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bars => "bars",
            Self::QuoteReplay => "quote_replay",
        }
    }
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "bars" => Some(Self::Bars),
            "quote_replay" => Some(Self::QuoteReplay),
            _ => None,
        }
    }
}

/// The regime an idea declares, classified only from information available
/// at decision time. It organises retrieval; it never splits the score.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Regime {
    Trend,
    Range,
    Unknown,
}

impl Regime {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Trend => "trend",
            Self::Range => "range",
            Self::Unknown => "unknown",
        }
    }
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "trend" => Some(Self::Trend),
            "range" => Some(Self::Range),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}

/// A lowercase slug: a family, a skill name, a mechanism key.
pub fn slug_ok(text: &str, max: usize) -> bool {
    !text.is_empty()
        && text.len() <= max
        && text.as_bytes()[0].is_ascii_lowercase()
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}

/// A lowercase hex SHA-256.
pub fn sha256_ok(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Normalise free text into a mechanism key so two wordings of the same idea
/// collide: lowercase words, sorted and deduplicated, stop words removed.
pub fn mechanism_key(text: &str) -> String {
    const STOP: &[&str] = &[
        "a", "an", "and", "at", "by", "for", "in", "of", "on", "or", "the", "to", "when", "with",
    ];
    let mut words: Vec<String> = text
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .filter(|word| !word.is_empty() && !STOP.contains(&word.as_str()))
        .collect();
    words.sort_unstable();
    words.dedup();
    words.join("-").chars().take(160).collect()
}

/// Everything that determines what an evaluator receives. The request id and
/// the parent stay out, so equivalent requests reuse the result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExperimentIdentity<'a> {
    pub stage: Stage,
    pub abi: &'a str,
    /// SHA-256 of the exact executed source bytes.
    pub source_sha256: &'a str,
    /// SHA-256 of the canonical JSON parameters the evaluator receives.
    pub params_sha256: &'a str,
    /// SHA-256 of the canonical frozen manifest: dataset, partition, universe,
    /// capital, costs and comparator.
    pub manifest_sha256: &'a str,
    /// SHA-256 of the runner image that executes the strategy and its adapter.
    pub runner_sha256: &'a str,
}

impl ExperimentIdentity<'_> {
    /// The versioned experiment key.
    pub fn key(&self) -> String {
        let encoded = format!(
            "nivra-experiment\nprotocol={PROTOCOL_VERSION}\nevaluator={EVALUATOR}\nstage={}\nabi={}\nsource={}\nparams={}\nmanifest={}\nrunner={}\n",
            self.stage.as_str(),
            self.abi,
            self.source_sha256,
            self.params_sha256,
            self.manifest_sha256,
            self.runner_sha256,
        );
        digest(encoded.as_bytes())
    }
}

/// The after-cost evidence one experiment produced on its frozen manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Score {
    /// Net P&L on the scored (selection) split, after fees and slippage.
    pub net_pnl: Amount,
    /// Largest peak-to-trough fall of marked equity, in bps of capital.
    pub max_drawdown_bps: i64,
    pub closed_trades: u64,
    pub open_positions: u64,
}

/// What a scored experiment is compared against.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Comparator {
    /// Uninvested cash: zero P&L.
    Cash,
    /// The pinned parent or the cell incumbent, scored on the same manifest.
    Experiment { id: i64, score: Score },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Keep,
    Reject,
}

impl Verdict {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::Reject => "reject",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Decision {
    pub verdict: Verdict,
    pub reason: &'static str,
    /// Candidate net minus comparator net, in rupees.
    pub delta: Amount,
}

/// The limits a keep must stay inside.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Rule {
    pub max_drawdown_bps: i64,
    /// A quote replay must end flat. A bar run marks what it still holds at
    /// the last close, so its P&L already prices the open position.
    pub require_flat: bool,
}

/// The keep rule. Keep only a strictly positive improvement over every
/// comparator, a positive net P&L and a drawdown inside the limit. Ties
/// retain the incumbent.
pub fn keep(candidate: &Score, comparators: &[Comparator], rule: Rule) -> Decision {
    let best = comparators
        .iter()
        .map(|comparator| match comparator {
            Comparator::Cash => Amount::ZERO,
            Comparator::Experiment { score, .. } => score.net_pnl,
        })
        .max()
        .unwrap_or(Amount::ZERO);
    let delta = candidate.net_pnl.checked_sub(best).unwrap_or(Amount::ZERO);
    let reject = |reason| Decision {
        verdict: Verdict::Reject,
        reason,
        delta,
    };
    if candidate.net_pnl <= Amount::ZERO {
        reject("net_pnl_not_positive")
    } else if delta <= Amount::ZERO {
        reject("not_better_than_comparator")
    } else if candidate.max_drawdown_bps > rule.max_drawdown_bps {
        reject("drawdown_over_limit")
    } else if rule.require_flat && candidate.open_positions > 0 {
        reject("positions_open_at_end")
    } else {
        Decision {
            verdict: Verdict::Keep,
            reason: "beats_comparator_after_costs",
            delta,
        }
    }
}

/// The IST research day an instant belongs to. Budgets reset on it; trial
/// counts never do.
pub fn ist_day(at_ms: i64) -> NaiveDate {
    DateTime::from_timestamp_millis(at_ms.saturating_add(IST_OFFSET_MS))
        .unwrap_or_default()
        .date_naive()
}

/// The IST minute of the day.
pub fn ist_minute(at_ms: i64) -> u32 {
    (at_ms.saturating_add(IST_OFFSET_MS).rem_euclid(86_400_000) / 60_000) as u32
}

/// Ceilings for one IST research day. Never targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayLimits {
    pub variants: u32,
    pub turns: u32,
    pub calls: u64,
    pub usd_micros: i64,
}

/// What one IST research day has used, counting open reservations at their
/// worst case.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaySpend {
    pub variants: u32,
    pub turns: u32,
    pub calls: u64,
    pub usd_micros: i64,
}

impl DayLimits {
    /// Why one more variant cannot start today, if it cannot.
    pub fn variant_refusal(&self, spent: &DaySpend) -> Option<&'static str> {
        (spent.variants >= self.variants).then_some("day_variants_exhausted")
    }
    /// Why one more turn reserving `calls` and `usd_micros` cannot start.
    pub fn turn_refusal(
        &self,
        spent: &DaySpend,
        calls: u64,
        usd_micros: i64,
    ) -> Option<&'static str> {
        if spent.turns >= self.turns {
            Some("day_turns_exhausted")
        } else if spent.calls.saturating_add(calls) > self.calls {
            Some("day_calls_exhausted")
        } else if spent.usd_micros.saturating_add(usd_micros) > self.usd_micros {
            Some("day_usd_exhausted")
        } else {
            None
        }
    }
}

/// Convert a configured USD amount to whole micro-dollars, rounding up so a
/// reservation never understates cost.
pub fn usd_micros(usd: f64) -> i64 {
    if usd.is_finite() && usd > 0.0 {
        (usd * 1_000_000.0).ceil().min(i64::MAX as f64) as i64
    } else {
        0
    }
}

/// Whether the loop may work now. It runs from the seal until the pre-open
/// stop minute, and through the whole of a day without a session.
pub fn window(
    trading_today: bool,
    sealed_today: bool,
    minute: u32,
    stop_minute: u32,
) -> Result<(), &'static str> {
    if !trading_today || sealed_today || minute < stop_minute {
        Ok(())
    } else {
        Err("waiting_for_seal")
    }
}

/// The three proposal slots the loop cycles through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Slot {
    /// Change one thing in a cell incumbent.
    MutateIncumbent,
    /// Revisit a documented failure with a distinct causal fix.
    RevisitFailure,
    /// A new mechanism from an empty family.
    NewFamily,
}

/// The slot for proposal `ordinal`, skipping slots with nothing to work on.
pub fn slot(ordinal: u64, has_incumbent: bool, has_failure: bool) -> Slot {
    const CYCLE: [Slot; 3] = [Slot::MutateIncumbent, Slot::RevisitFailure, Slot::NewFamily];
    (0..3)
        .map(|offset| CYCLE[((ordinal + offset) % 3) as usize])
        .find(|slot| match slot {
            Slot::MutateIncumbent => has_incumbent,
            Slot::RevisitFailure => has_failure,
            Slot::NewFamily => true,
        })
        .unwrap_or(Slot::NewFamily)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULE: Rule = Rule {
        max_drawdown_bps: 300,
        require_flat: true,
    };

    fn score(net: i64, drawdown: i64) -> Score {
        Score {
            net_pnl: Amount::from_integer(net),
            max_drawdown_bps: drawdown,
            closed_trades: 12,
            open_positions: 0,
        }
    }

    #[test]
    fn keep_needs_a_strict_after_cost_improvement_over_every_comparator() {
        let parent = Comparator::Experiment {
            id: 1,
            score: score(40, 100),
        };
        let cases = [
            (score(50, 100), vec![Comparator::Cash], Verdict::Keep),
            (score(0, 100), vec![Comparator::Cash], Verdict::Reject),
            (
                score(40, 100),
                vec![Comparator::Cash, parent.clone()],
                Verdict::Reject,
            ),
            (
                score(41, 100),
                vec![Comparator::Cash, parent],
                Verdict::Keep,
            ),
            (score(50, 301), vec![Comparator::Cash], Verdict::Reject),
        ];
        for (candidate, comparators, expected) in cases {
            assert_eq!(keep(&candidate, &comparators, RULE).verdict, expected);
        }
        let mut open = score(50, 10);
        open.open_positions = 1;
        assert_eq!(keep(&open, &[], RULE).reason, "positions_open_at_end");
    }

    #[test]
    fn experiment_keys_change_with_any_input_and_ignore_nothing_else() {
        let hash = |c: char| c.to_string().repeat(64);
        let (a, b, c, d) = (hash('a'), hash('b'), hash('c'), hash('d'));
        let base = ExperimentIdentity {
            stage: Stage::Bars,
            abi: "archive_bars_v1",
            source_sha256: &a,
            params_sha256: &b,
            manifest_sha256: &c,
            runner_sha256: &d,
        };
        assert_eq!(base.key(), base.clone().key());
        assert!(sha256_ok(&base.key()));
        let mut other = base.clone();
        other.params_sha256 = &a;
        assert_ne!(base.key(), other.key());
        other = base.clone();
        other.stage = Stage::QuoteReplay;
        assert_ne!(base.key(), other.key());
        assert_eq!(
            mechanism_key("Buy the breakout when volume expands"),
            mechanism_key("when volume expands, buy breakout")
        );
    }

    #[test]
    fn budgets_reset_on_the_ist_day_and_the_window_follows_the_seal() {
        // 23:59 and 00:01 IST on either side of midnight.
        let midnight = NaiveDate::from_ymd_opt(2026, 11, 28)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis()
            - IST_OFFSET_MS;
        let before = midnight - 60_000;
        let after = before + 120_000;
        assert_ne!(ist_day(before), ist_day(after));
        assert_eq!(ist_minute(before), 1439);
        let limits = DayLimits {
            variants: 1,
            turns: 2,
            calls: 10,
            usd_micros: usd_micros(0.5),
        };
        let mut spent = DaySpend::default();
        assert_eq!(limits.turn_refusal(&spent, 8, 400_000), None);
        spent.calls = 3;
        assert_eq!(
            limits.turn_refusal(&spent, 8, 400_000),
            Some("day_calls_exhausted")
        );
        spent.variants = 1;
        assert_eq!(
            limits.variant_refusal(&spent),
            Some("day_variants_exhausted")
        );
        assert_eq!(window(true, false, 600, 510), Err("waiting_for_seal"));
        assert_eq!(window(true, true, 1200, 510), Ok(()));
        assert_eq!(window(true, false, 300, 510), Ok(()));
        assert_eq!(window(false, false, 700, 510), Ok(()));
        assert_eq!(slot(0, false, false), Slot::NewFamily);
        assert_eq!(slot(0, true, true), Slot::MutateIncumbent);
        assert_eq!(slot(1, true, false), Slot::NewFamily);
    }
}

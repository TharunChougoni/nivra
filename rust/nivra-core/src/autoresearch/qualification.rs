use super::{Decision, Score, Verdict};
use crate::finance::{Amount, FinancialError};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize)]
pub struct Fold {
    pub qualification_net_pnl: Option<Amount>,
    pub max_drawdown_bps: i64,
    pub closed_trades: u64,
    pub open_positions: u64,
    pub daily_net_pnl: Vec<(NaiveDate, Amount)>,
}

pub fn pooled(folds: &[Fold]) -> Result<Score, FinancialError> {
    let mut score = Score {
        net_pnl: Amount::ZERO,
        max_drawdown_bps: 0,
        closed_trades: 0,
        open_positions: 0,
    };
    for fold in folds {
        score.net_pnl = score
            .net_pnl
            .checked_add(fold.qualification_net_pnl.unwrap_or(Amount::ZERO))?;
        score.max_drawdown_bps = score.max_drawdown_bps.max(fold.max_drawdown_bps);
        score.closed_trades += fold.closed_trades;
        score.open_positions += fold.open_positions;
    }
    Ok(score)
}

#[derive(Serialize)]
pub struct Comparison {
    pub fold_deltas: Vec<Amount>,
    pub median_delta: Amount,
    pub median_delta_bps: f64,
    pub wins: usize,
    pub pooled_net: Amount,
    pub worst_date: Option<NaiveDate>,
    pub weekly_block_interval_rupees: Option<[f64; 2]>,
}

pub fn paired(
    candidate: &[Fold],
    parent: Option<&[Fold]>,
    capital: Amount,
) -> Result<Option<Comparison>, FinancialError> {
    if !matches!(candidate.len(), 1 | 3) || parent.is_some_and(|p| p.len() != candidate.len()) {
        return Ok(None);
    }
    let mut fold_deltas = Vec::new();
    let mut daily = Vec::new();
    for (index, fold) in candidate.iter().enumerate() {
        let other = parent.map(|p| &p[index]);
        let Some(net) = fold.qualification_net_pnl else {
            return Ok(None);
        };
        let baseline = match other {
            None => Amount::ZERO,
            Some(other) => {
                let Some(net) = other.qualification_net_pnl else {
                    return Ok(None);
                };
                if !fold
                    .daily_net_pnl
                    .iter()
                    .map(|(date, _)| date)
                    .eq(other.daily_net_pnl.iter().map(|(date, _)| date))
                {
                    return Ok(None);
                }
                net
            }
        };
        fold_deltas.push(net.checked_sub(baseline)?);
        for (row, &(date, pnl)) in fold.daily_net_pnl.iter().enumerate() {
            let comparator = other.map_or(Amount::ZERO, |o| o.daily_net_pnl[row].1);
            daily.push((date, pnl.checked_sub(comparator)?));
        }
    }
    let mut sorted = fold_deltas.clone();
    sorted.sort_unstable();
    let median_delta = sorted[sorted.len() / 2];
    Ok(Some(Comparison {
        wins: sorted.iter().filter(|&&delta| delta > Amount::ZERO).count(),
        median_delta,
        median_delta_bps: as_float(median_delta) * 10_000.0 / as_float(capital),
        pooled_net: pooled(candidate)?.net_pnl,
        fold_deltas,
        worst_date: daily
            .iter()
            .min_by_key(|(_, pnl)| *pnl)
            .map(|(day, _)| *day),
        weekly_block_interval_rupees: weekly_interval(&daily),
    }))
}

pub fn verdict(score: &Score, comparison: Option<&Comparison>, max_drawdown_bps: i64) -> Decision {
    let delta = comparison.map_or(Amount::ZERO, |c| c.median_delta);
    let reason = if score.open_positions != 0 {
        "bar_positions_open"
    } else if score.max_drawdown_bps > max_drawdown_bps {
        "drawdown_exceeded"
    } else if score.net_pnl <= Amount::ZERO {
        "net_not_positive"
    } else if let Some(c) = comparison {
        if c.median_delta <= Amount::ZERO || c.wins <= c.fold_deltas.len() / 2 {
            "paired_folds_not_improved"
        } else {
            return Decision {
                verdict: Verdict::Keep,
                reason: "paired_folds_improved",
                delta,
            };
        }
    } else {
        "paired_evidence_unavailable"
    };
    Decision {
        verdict: Verdict::Reject,
        reason,
        delta,
    }
}

fn as_float(amount: Amount) -> f64 {
    // Amount's finite decimal representation is always representable as f64.
    amount.to_string().parse().expect("finite amount")
}

fn weekly_interval(daily: &[(NaiveDate, Amount)]) -> Option<[f64; 2]> {
    use chrono::Datelike;
    let mut weeks = Vec::new();
    let mut previous = None;
    for &(date, amount) in daily {
        let week = date.iso_week();
        if previous != Some(week) {
            weeks.push(0.0);
            previous = Some(week);
        }
        *weeks.last_mut()? += as_float(amount);
    }
    if weeks.len() < 8 {
        return None;
    }
    let mut seed = 42_u64;
    let mut draws = [0.0; 1_000];
    for draw in &mut draws {
        for _ in &weeks {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            *draw += weeks[((seed >> 32) as usize) % weeks.len()];
        }
    }
    draws.sort_unstable_by(f64::total_cmp);
    Some([draws[25], draws[974]])
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct DailyMoments {
    pub dates: usize,
    pub sharpe: f64,
    pub skewness: f64,
    pub kurtosis: f64,
}

pub fn daily_moments(folds: &[Fold], capital: Amount) -> Option<DailyMoments> {
    let mut returns = Vec::new();
    for fold in folds {
        fold.qualification_net_pnl?;
        let mut equity = as_float(capital);
        for (_, delta) in &fold.daily_net_pnl {
            if equity <= 0.0 {
                return None;
            }
            let delta = as_float(*delta);
            returns.push(delta / equity);
            equity += delta;
        }
    }
    let n = returns.len() as f64;
    if n < 2.0 {
        return None;
    }
    let mean = returns.iter().sum::<f64>() / n;
    let central = |power| returns.iter().map(|x| (x - mean).powi(power)).sum::<f64>() / n;
    let var = central(2);
    (var > 0.0).then(|| DailyMoments {
        dates: returns.len(),
        sharpe: mean / (var * n / (n - 1.0)).sqrt(),
        skewness: central(3) / var.powf(1.5),
        kurtosis: central(4) / var.powi(2),
    })
}

#[derive(Serialize)]
pub struct DeflatedSharpe {
    pub raw_trials: u32,
    pub comparable_trials: usize,
    pub moments: Option<DailyMoments>,
    pub sharpe_dispersion: Option<f64>,
    pub probability: Option<f64>,
    pub unavailable_reason: Option<&'static str>,
    pub assumptions: &'static str,
}

pub fn deflated_sharpe(
    moments: Option<DailyMoments>,
    variants: &[f64],
    raw_trials: u32,
    missing_trials: bool,
) -> DeflatedSharpe {
    let mut report = DeflatedSharpe {
        raw_trials,
        comparable_trials: variants.len(),
        moments,
        sharpe_dispersion: None,
        probability: None,
        unavailable_reason: None,
        assumptions: "raw trial count treated as independent; serial dependence can distort the estimate",
    };
    let dispersion = (variants.len() >= 2).then(|| {
        let n = variants.len() as f64;
        let mean = variants.iter().sum::<f64>() / n;
        (variants.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0)).sqrt()
    });
    report.sharpe_dispersion = dispersion;
    report.unavailable_reason = if let Some(m) = moments {
        if m.dates < 60 {
            Some("fewer_than_60_scored_dates")
        } else if missing_trials {
            Some("missing_trials")
        } else if let Some(dispersion) = dispersion {
            let denominator =
                1.0 - m.skewness * m.sharpe + (m.kurtosis - 1.0) * m.sharpe.powi(2) / 4.0;
            if dispersion <= 0.0 || denominator <= 0.0 || raw_trials < 2 {
                Some("zero_variance")
            } else {
                let trials = f64::from(raw_trials);
                let gamma = 0.577_215_664_901_532_9;
                let null = dispersion
                    * ((1.0 - gamma) * quantile(1.0 - 1.0 / trials)
                        + gamma * quantile(1.0 - 1.0 / (trials * std::f64::consts::E)));
                report.probability = Some(cdf(
                    (m.sharpe - null) * ((m.dates - 1) as f64 / denominator).sqrt()
                ));
                None
            }
        } else {
            Some("fewer_than_two_comparable_variants")
        }
    } else {
        Some("zero_variance_or_incomplete_returns")
    };
    report
}

fn cdf(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.2316419 * x.abs());
    let tail = (-x * x / 2.0).exp() / (2.0 * std::f64::consts::PI).sqrt()
        * t
        * (0.319381530
            + t * (-0.356563782 + t * (1.781477937 + t * (-1.821255978 + t * 1.330274429))));
    if x < 0.0 { tail } else { 1.0 - tail }
}

fn quantile(p: f64) -> f64 {
    let (mut lo, mut hi) = (-9.0, 9.0);
    for _ in 0..48 {
        let mid = (lo + hi) / 2.0;
        if cdf(mid) < p {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (lo + hi) / 2.0
}

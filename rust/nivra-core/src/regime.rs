//! Offline market regime classification; returns and volatility are decimal fractions.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RegimeConfig {
    /// Symmetric range band: (-trend_return, trend_return).
    pub trend_return: f64,
    pub vol_low: f64,
    pub vol_high: f64,
    pub breadth_weak: f64,
    pub breadth_strong: f64,
    pub gap: f64,
    pub min_instruments: usize,
    pub min_minutes: usize,
}

impl Default for RegimeConfig {
    fn default() -> Self {
        Self {
            trend_return: 0.004,
            vol_low: 0.0005,
            vol_high: 0.002,
            breadth_weak: 0.4,
            breadth_strong: 0.6,
            gap: 0.005,
            min_instruments: 200,
            min_minutes: 15,
        }
    }
}

impl RegimeConfig {
    pub fn validate(&self) -> Result<(), String> {
        let thresholds = [
            self.trend_return,
            self.vol_low,
            self.vol_high,
            self.breadth_weak,
            self.breadth_strong,
            self.gap,
        ];
        if thresholds.iter().any(|v| !v.is_finite())
            || self.trend_return <= 0.0
            || self.gap <= 0.0
            || self.vol_low < 0.0
            || self.vol_low >= self.vol_high
            || self.breadth_weak < 0.0
            || self.breadth_strong > 1.0
            || self.breadth_weak >= self.breadth_strong
            || self.min_instruments == 0
            || self.min_minutes < 2
        {
            return Err("invalid regime thresholds or minimum sample counts".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RegimeInputs {
    pub session_return: f64,
    pub opening_gap: f64,
    pub realised_volatility: f64,
    pub breadth: f64,
    pub instrument_count: usize,
    pub minute_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trend {
    Up,
    Down,
    Range,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Volatility {
    Low,
    Normal,
    High,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Breadth {
    Weak,
    Neutral,
    Strong,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Regime {
    Unknown,
    Known {
        trend: Trend,
        volatility: Volatility,
        breadth: Breadth,
        gap_day: bool,
    },
}

impl Regime {
    pub fn label(&self) -> String {
        let Self::Known {
            trend,
            volatility,
            breadth,
            gap_day,
        } = self
        else {
            return "unknown".into();
        };
        format!(
            "{}/{}/{}/{}",
            match trend {
                Trend::Up => "up_trend",
                Trend::Down => "down_trend",
                Trend::Range => "range",
            },
            match volatility {
                Volatility::Low => "low_vol",
                Volatility::Normal => "normal_vol",
                Volatility::High => "high_vol",
            },
            match breadth {
                Breadth::Weak => "weak",
                Breadth::Neutral => "neutral",
                Breadth::Strong => "strong",
            },
            if *gap_day { "gap" } else { "no_gap" }
        )
    }
}

pub fn classify(inputs: &RegimeInputs, cfg: &RegimeConfig) -> Regime {
    if cfg.validate().is_err()
        || inputs.instrument_count < cfg.min_instruments
        || inputs.minute_count < cfg.min_minutes
        || !inputs.session_return.is_finite()
        || inputs.session_return <= -1.0
        || !inputs.opening_gap.is_finite()
        || inputs.opening_gap <= -1.0
        || !inputs.realised_volatility.is_finite()
        || inputs.realised_volatility < 0.0
        || !inputs.breadth.is_finite()
        || !(0.0..=1.0).contains(&inputs.breadth)
    {
        return Regime::Unknown;
    }
    Regime::Known {
        trend: if inputs.session_return >= cfg.trend_return {
            Trend::Up
        } else if inputs.session_return <= -cfg.trend_return {
            Trend::Down
        } else {
            Trend::Range
        },
        volatility: if inputs.realised_volatility < cfg.vol_low {
            Volatility::Low
        } else if inputs.realised_volatility >= cfg.vol_high {
            Volatility::High
        } else {
            Volatility::Normal
        },
        breadth: if inputs.breadth <= cfg.breadth_weak {
            Breadth::Weak
        } else if inputs.breadth >= cfg.breadth_strong {
            Breadth::Strong
        } else {
            Breadth::Neutral
        },
        gap_day: inputs.opening_gap.abs() >= cfg.gap,
    }
}

/// Aligned price slices must describe the same eligible instruments. Invalid
/// triples are excluded together; mismatched lengths are rejected. Minute values
/// are already-computed one-minute cross-sectional median returns. Volatility
/// uses population standard deviation; invalid minute values are excluded.
pub fn compute_inputs(
    prior_close: &[f64],
    open: &[f64],
    current: &[f64],
    minute_returns: &[f64],
) -> Result<RegimeInputs, String> {
    if prior_close.len() != open.len() || open.len() != current.len() {
        return Err("regime price slices must have equal lengths".into());
    }
    let mut returns = Vec::new();
    let mut gaps = Vec::new();
    let mut up = 0_usize;
    for ((prior, opening), now) in prior_close.iter().zip(open).zip(current) {
        if [prior, opening, now]
            .iter()
            .any(|p| !p.is_finite() || **p <= 0.0)
        {
            continue;
        }
        let session = now / opening - 1.0;
        let gap = opening / prior - 1.0;
        if !session.is_finite() || !gap.is_finite() || session <= -1.0 || gap <= -1.0 {
            continue;
        }
        returns.push(session);
        gaps.push(gap);
        up += usize::from(now > prior);
    }
    let minutes: Vec<_> = minute_returns
        .iter()
        .copied()
        .filter(|r| r.is_finite() && *r > -1.0)
        .collect();
    let mean = minutes.iter().sum::<f64>() / minutes.len() as f64;
    let variance = minutes.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / minutes.len() as f64;
    Ok(RegimeInputs {
        instrument_count: returns.len(),
        minute_count: minutes.len(),
        breadth: up as f64 / returns.len() as f64,
        session_return: median(&mut returns),
        opening_gap: median(&mut gaps),
        realised_volatility: variance.sqrt(),
    })
}

fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    if values.len().is_multiple_of(2) {
        values[mid - 1] / 2.0 + values[mid] / 2.0
    } else {
        values[mid]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_boundaries_and_fail_closed() {
        let cfg = RegimeConfig::default();
        let mut inputs = RegimeInputs {
            session_return: 0.0,
            opening_gap: 0.0,
            realised_volatility: 0.001,
            breadth: 0.5,
            instrument_count: 200,
            minute_count: 15,
        };
        assert_eq!(
            classify(&inputs, &cfg).label(),
            "range/normal_vol/neutral/no_gap"
        );
        for (ret, trend) in [
            (-0.004, "down_trend"),
            (-0.003999, "range"),
            (0.003999, "range"),
            (0.004, "up_trend"),
        ] {
            for (vol, volatility) in [
                (0.000499, "low_vol"),
                (0.0005, "normal_vol"),
                (0.001999, "normal_vol"),
                (0.002, "high_vol"),
            ] {
                for (breadth, label) in [
                    (0.4, "weak"),
                    (0.400001, "neutral"),
                    (0.599999, "neutral"),
                    (0.6, "strong"),
                ] {
                    for (gap, suffix) in [
                        (-0.005, "gap"),
                        (-0.004999, "no_gap"),
                        (0.004999, "no_gap"),
                        (0.005, "gap"),
                    ] {
                        inputs.session_return = ret;
                        inputs.realised_volatility = vol;
                        inputs.breadth = breadth;
                        inputs.opening_gap = gap;
                        let regime = classify(&inputs, &cfg);
                        assert_eq!(
                            regime.label(),
                            format!("{trend}/{volatility}/{label}/{suffix}")
                        );
                        assert_eq!(classify(&inputs, &cfg), regime);
                    }
                }
            }
        }
        for (instruments, minutes) in [(199, 15), (200, 14)] {
            let thin = RegimeInputs {
                instrument_count: instruments,
                minute_count: minutes,
                ..inputs.clone()
            };
            assert_eq!(classify(&thin, &cfg), Regime::Unknown);
        }
        inputs.breadth = f64::NAN;
        assert_eq!(classify(&inputs, &cfg).label(), "unknown");
        assert_eq!(serde_json::from_str::<RegimeConfig>("{}").unwrap(), cfg);
        for field in [
            "trend_return",
            "gap",
            "vol_high",
            "breadth_strong",
            "min_instruments",
            "min_minutes",
        ] {
            let mut value = serde_json::to_value(&cfg).unwrap();
            value[field] = 0.into();
            assert!(
                serde_json::from_value::<RegimeConfig>(value)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
        for invalid in [
            RegimeConfig {
                vol_low: -1.0,
                ..cfg.clone()
            },
            RegimeConfig {
                breadth_weak: -0.1,
                ..cfg.clone()
            },
            RegimeConfig {
                breadth_strong: 1.1,
                ..cfg.clone()
            },
            RegimeConfig {
                gap: f64::NAN,
                ..cfg.clone()
            },
        ] {
            assert!(invalid.validate().is_err());
            assert_eq!(classify(&inputs, &invalid), Regime::Unknown);
        }
    }

    #[test]
    fn computes_proxy_from_aligned_valid_samples() {
        let inputs = compute_inputs(
            &[100.0, 100.0, 0.0, 100.0],
            &[100.0, 102.0, 100.0, f64::NAN],
            &[102.0, 102.0, 100.0, 100.0],
            &[0.001, -0.001, f64::NAN],
        )
        .unwrap();
        assert_eq!((inputs.instrument_count, inputs.minute_count), (2, 2));
        assert!((inputs.session_return - 0.01).abs() < 1e-12);
        assert!((inputs.opening_gap - 0.01).abs() < 1e-12);
        assert_eq!(inputs.breadth, 1.0);
        assert!((inputs.realised_volatility - 0.001).abs() < 1e-12);
        assert_eq!(
            inputs,
            compute_inputs(
                &[100.0, 100.0],
                &[102.0, 100.0],
                &[102.0, 102.0],
                &[-0.001, 0.001]
            )
            .unwrap()
        );
        assert!(compute_inputs(&[100.0], &[], &[], &[]).is_err());
        assert_eq!(
            classify(
                &compute_inputs(&[], &[], &[], &[]).unwrap(),
                &RegimeConfig::default()
            ),
            Regime::Unknown
        );
    }
}

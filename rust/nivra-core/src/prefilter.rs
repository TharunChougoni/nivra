//! Pure shadow shortlist. Scores never authorize candidates or orders.
use crate::strategy::{Feature, FeatureRow};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PrefilterSettings {
    pub enabled: bool,
    pub enter_rank: usize,
    pub exit_rank: usize,
    pub shortlist_size: usize,
    pub market_relative_weight: f64,
    pub rel_volume_tod_weight: f64,
    pub vwap_distance_weight: f64,
    pub spread_weight: f64,
}
impl PrefilterSettings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}
impl Default for PrefilterSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            enter_rank: 100,
            exit_rank: 150,
            shortlist_size: 100,
            market_relative_weight: 1.0,
            rel_volume_tod_weight: 1.0,
            vwap_distance_weight: 1.0,
            spread_weight: 1.0,
        }
    }
}
impl PrefilterSettings {
    pub fn validate(&self) -> Result<(), String> {
        if self.enter_rank == 0
            || self.exit_rank < self.enter_rank
            || self.shortlist_size == 0
            || [
                self.market_relative_weight,
                self.rel_volume_tod_weight,
                self.vwap_distance_weight,
                self.spread_weight,
            ]
            .iter()
            .any(|w| !w.is_finite() || *w < 0.0)
        {
            return Err("invalid prefilter ranks, size or weights".into());
        }
        Ok(())
    }
}
#[derive(Debug, Default, PartialEq, Serialize)]
pub struct Shortlist {
    /// Descending score, then ascending instrument key.
    pub keys: Vec<String>,
    pub entered: BTreeSet<String>,
    pub exited: BTreeSet<String>,
    /// All eligible scores, including rows outside the shortlist.
    pub scores: BTreeMap<String, f64>,
}

/// All inputs remain required even with a zero weight. Missing/nonfinite values
/// and nonpositive price/VWAP are excluded. Returns use bps; VWAP distance and
/// spread use fractions, without implicit normalization.
pub fn prefilter(
    rows: &[FeatureRow],
    previous: &BTreeSet<String>,
    cfg: &PrefilterSettings,
) -> Shortlist {
    let mut scores = BTreeMap::new();
    if cfg.validate().is_ok() {
        for row in rows {
            let value = |f| row.features.get(&f).copied().filter(|v| v.is_finite());
            let (Some(relative), Some(volume), Some(price), Some(vwap), Some(spread)) = (
                value(Feature::MarketRelativeBps),
                value(Feature::RelVolumeTod),
                value(Feature::Price),
                value(Feature::SessionVwap),
                value(Feature::SpreadFraction),
            ) else {
                continue;
            };
            if price <= 0.0 || vwap <= 0.0 {
                continue;
            }
            let score = cfg.market_relative_weight * relative
                + cfg.rel_volume_tod_weight * volume
                + cfg.vwap_distance_weight * (price / vwap - 1.0)
                - cfg.spread_weight * spread;
            if score.is_finite() {
                // Extraction produces unique keys; duplicate input is order independent.
                scores
                    .entry(row.key.clone())
                    .and_modify(|s: &mut f64| *s = s.max(score))
                    .or_insert(score);
            }
        }
    }
    let mut ranked: Vec<_> = scores.keys().cloned().collect();
    ranked.sort_by(|a, b| scores[b].total_cmp(&scores[a]).then_with(|| a.cmp(b)));
    let mut selected = BTreeSet::new();
    for (i, key) in ranked.iter().enumerate() {
        if i < cfg.exit_rank && previous.contains(key) && selected.len() < cfg.shortlist_size {
            selected.insert(key.clone());
        }
    }
    for key in ranked.iter().take(cfg.enter_rank) {
        if selected.len() >= cfg.shortlist_size {
            break;
        }
        selected.insert(key.clone());
    }
    Shortlist {
        keys: ranked
            .into_iter()
            .filter(|k| selected.contains(k))
            .collect(),
        entered: selected.difference(previous).cloned().collect(),
        exited: previous.difference(&selected).cloned().collect(),
        scores,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(key: &str, score: f64) -> FeatureRow {
        FeatureRow {
            key: key.into(),
            features: BTreeMap::from([
                (Feature::MarketRelativeBps, score),
                (Feature::RelVolumeTod, 1.0),
                (Feature::Price, 100.0),
                (Feature::SessionVwap, 100.0),
                (Feature::SpreadFraction, 0.01),
            ]),
        }
    }
    #[test]
    fn hysteresis_missing_cap_and_determinism() {
        let cfg = PrefilterSettings {
            enter_rank: 2,
            exit_rank: 3,
            shortlist_size: 2,
            ..Default::default()
        };
        let mut rows = vec![row("a", 4.0), row("b", 3.0), row("c", 2.0), row("d", 1.0)];
        let previous = BTreeSet::from(["c".into(), "d".into()]);
        let result = prefilter(&rows, &previous, &cfg);
        assert_eq!(result.keys, ["a", "c"]);
        assert_eq!(result.entered, BTreeSet::from(["a".into()]));
        assert_eq!(result.exited, BTreeSet::from(["d".into()]));
        assert_eq!(prefilter(&rows, &BTreeSet::new(), &cfg).keys, ["a", "b"]);
        rows.reverse();
        assert_eq!(prefilter(&rows, &previous, &cfg), result);
        rows[0].features.remove(&Feature::RelVolumeTod);
        rows[1].features.insert(Feature::SessionVwap, f64::NAN);
        assert_eq!(prefilter(&rows, &previous, &cfg).keys, ["a", "b"]);
        for feature in [
            Feature::MarketRelativeBps,
            Feature::RelVolumeTod,
            Feature::Price,
            Feature::SessionVwap,
            Feature::SpreadFraction,
        ] {
            let mut missing = row("missing", 10.0);
            missing.features.remove(&feature);
            assert!(
                prefilter(&[missing], &BTreeSet::new(), &cfg)
                    .keys
                    .is_empty()
            );
        }
        let all_previous = BTreeSet::from(["a".into(), "b".into(), "c".into()]);
        let full = vec![row("a", 3.0), row("b", 2.0), row("c", 1.0)];
        assert_eq!(prefilter(&full, &all_previous, &cfg).keys, ["a", "b"]);
        let tied = vec![row("z", 1.0), row("a", 1.0)];
        assert_eq!(prefilter(&tied, &BTreeSet::new(), &cfg).keys, ["a", "z"]);
        let defaults: PrefilterSettings = serde_json::from_str("{}").unwrap();
        assert!(!defaults.enabled);
        defaults.validate().unwrap();
        assert!(
            PrefilterSettings {
                exit_rank: 1,
                ..defaults
            }
            .validate()
            .is_err()
        );
    }
}

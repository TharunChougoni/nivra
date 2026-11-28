use super::*;
use nivra_research::archive_bars::Probe;

/// Compare the first complete feature match in each minute after a bar closes.
/// Every nonempty bar decision needs a matching observation.
pub(crate) struct TranslationParity {
    probes: BTreeMap<i64, Probe>,
    matched: usize,
    signals: usize,
    matched_signals: usize,
    mismatches: usize,
    dates: BTreeSet<NaiveDate>,
}

impl TranslationParity {
    pub(crate) fn new(probes: Vec<Probe>) -> Self {
        let signals = probes.iter().filter(|p| !p.decisions.is_empty()).count();
        Self {
            probes: probes
                .into_iter()
                .map(|p| (p.bar_closed_at_ms, p))
                .collect(),
            matched: 0,
            signals,
            matched_signals: 0,
            mismatches: 0,
            dates: BTreeSet::new(),
        }
    }

    pub(super) fn observe(
        &mut self,
        at: i64,
        prospects: &[super::super::Prospect],
        actions: Option<&[Value]>,
    ) {
        let minute = at.div_euclid(60_000) * 60_000;
        let Some(probe) = self.probes.get(&minute) else {
            return;
        };
        if prospects.len() != probe.prices_paise.len()
            || prospects.iter().any(|p| {
                let price = p.features.get(&Feature::Price).map(|price| price * 100.0);
                !probe
                    .prices_paise
                    .get(&p.key)
                    .zip(price)
                    .is_some_and(|(&paise, price)| (price - paise as f64).abs() < 0.000_001)
            })
        {
            return;
        }
        self.matched += 1;
        self.dates.insert(nivra_core::autoresearch::ist_day(at));
        self.matched_signals += usize::from(!probe.decisions.is_empty());
        let aligned = actions.and_then(|actions| {
            let mut decisions = BTreeMap::new();
            for action in actions {
                let buy = match action["side"].as_str()? {
                    "buy" => true,
                    "sell" => false,
                    _ => return None,
                };
                if decisions
                    .insert(action["instrument_key"].as_str()?.to_owned(), buy)
                    .is_some()
                {
                    return None;
                }
            }
            Some(decisions)
        });
        if aligned.as_ref() != Some(&probe.decisions) {
            self.mismatches += 1;
        }
        self.probes.remove(&minute);
    }

    pub(crate) fn verdict(&self) -> (Option<&'static str>, Value) {
        let reason = if self.mismatches > 0 {
            Some("translation_decision_parity_failed")
        } else if self.matched < 10
            || self.dates.len() < 3
            || self.signals == 0
            || self.matched_signals != self.signals
        {
            Some("translation_parity_unavailable")
        } else {
            None
        };
        (
            reason,
            json!({"matched_observations":self.matched,"matched_dates":self.dates.len(),
            "bar_signal_batches":self.signals,"matched_signal_batches":self.matched_signals,
            "mismatches":self.mismatches,"reason":reason}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::strategy::Prospect;
    use nivra_core::strategy::Feature;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn at(day: &str, minute: u32) -> i64 {
        let date: NaiveDate = day.parse().expect("date");
        (date
            .and_hms_opt(10, minute, 0)
            .expect("time")
            .and_utc()
            .timestamp_millis())
            - 19_800_000
    }

    fn probe(at: i64, close: i64, decisions: &[(&str, bool)]) -> Probe {
        Probe {
            bar_closed_at_ms: at,
            prices_paise: [("NSE_EQ|A".to_owned(), close)].into(),
            decisions: decisions
                .iter()
                .map(|(key, buy)| ((*key).to_owned(), *buy))
                .collect(),
        }
    }

    fn prospects(close: i64) -> Vec<Prospect> {
        vec![Prospect {
            key: "NSE_EQ|A".into(),
            rank: 1,
            score: 1.0,
            features: [(
                Feature::Price,
                // The action adapter carries rupees; the bar probe is paise.
                close as f64 / 100.0,
            )]
            .into_iter()
            .collect::<BTreeMap<_, _>>(),
        }]
    }

    fn actions(side: &str) -> Vec<Value> {
        vec![json!({"side":side,"instrument_key":"NSE_EQ|A"})]
    }

    fn twelve() -> Vec<Probe> {
        let mut probes = Vec::new();
        for (day, minute) in [
            ("2026-01-05", 1),
            ("2026-01-05", 2),
            ("2026-01-05", 3),
            ("2026-01-05", 4),
            ("2026-01-06", 1),
            ("2026-01-06", 2),
            ("2026-01-06", 3),
            ("2026-01-06", 4),
            ("2026-01-07", 1),
            ("2026-01-07", 2),
            ("2026-01-07", 3),
            ("2026-01-07", 4),
        ] {
            probes.push(probe(at(day, minute), 10_000, &[("NSE_EQ|A", true)]));
        }
        probes
    }

    #[test]
    fn matching_feature_and_decision_parity_passes_on_three_dates() {
        let probes = twelve();
        let mut parity = TranslationParity::new(probes.clone());
        for probe in &probes {
            parity.observe(
                probe.bar_closed_at_ms,
                &prospects(10_000),
                Some(&actions("buy")),
            );
        }
        let (reason, evidence) = parity.verdict();
        assert_eq!(reason, None, "{evidence}");
        assert_eq!(evidence["matched_observations"], 12);
        assert_eq!(evidence["matched_dates"], 3);
        assert_eq!(evidence["bar_signal_batches"], 12);
    }

    #[test]
    fn a_wrong_side_or_price_breaks_parity() {
        let probes = twelve();
        let mut parity = TranslationParity::new(probes.clone());
        for (index, probe) in probes.iter().enumerate() {
            let side = if index == 0 { "sell" } else { "buy" };
            parity.observe(
                probe.bar_closed_at_ms,
                &prospects(10_000),
                Some(&actions(side)),
            );
        }
        assert_eq!(
            parity.verdict().0,
            Some("translation_decision_parity_failed")
        );

        let mut parity = TranslationParity::new(probes.clone());
        for probe in &probes {
            parity.observe(
                probe.bar_closed_at_ms,
                &prospects(9_999),
                Some(&actions("buy")),
            );
        }
        let (reason, evidence) = parity.verdict();
        assert_eq!(reason, Some("translation_parity_unavailable"), "{evidence}");
        assert_eq!(evidence["matched_observations"], 0);
    }
}

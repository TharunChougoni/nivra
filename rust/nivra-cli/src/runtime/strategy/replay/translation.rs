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

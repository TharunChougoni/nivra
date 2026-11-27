use super::super::strategy_run::Frozen;
use super::*;
use crate::runtime::strategy::selection::{REFRESH_MS, Selection, executable};
use nivra_core::market::MAX_FINANCIAL_AGE_MS;

/// Full-depth authority is tied to the receipt that passed the tick grid.
/// REST depth has its own index and can drive calls while the feed is down.
#[derive(Default)]
pub(super) struct Inputs {
    selection: Selection,
    selected_for: Option<String>,
    refreshed_at: Option<i64>,
    pinned: BTreeSet<String>,
    pushed: BTreeMap<String, (i64, i64)>,
    rest: BTreeMap<String, (i64, i64)>,
}

fn clock(quote: &Quote) -> (i64, i64) {
    (quote.source, quote.receipt)
}

impl Inputs {
    fn matches(index: &BTreeMap<String, (i64, i64)>, scan: &Scanner, key: &str, now: i64) -> bool {
        executable(scan, key, now).is_some_and(|quote| index.get(key) == Some(&clock(&quote)))
    }

    fn covered(&self, scan: &Scanner, key: &str, now: i64, full: bool) -> bool {
        Self::matches(if full { &self.pushed } else { &self.rest }, scan, key, now)
    }

    pub(super) fn next_refresh(&self, now: i64) -> i64 {
        self.refreshed_at
            .map_or(now, |at| at.saturating_add(REFRESH_MS))
    }

    pub(super) fn selected_for(&self, strategy: Option<&str>) -> bool {
        self.selected_for.as_deref() == strategy
    }

    pub(super) fn selection(&self) -> &[String] {
        self.selection.keys()
    }

    pub(super) fn coverage(&self, scan: &Scanner, now: i64, full: bool) -> serde_json::Value {
        let nominated = self.selection.nominees().count();
        let covered = self
            .selection
            .nominees()
            .filter(|key| full && Self::matches(&self.pushed, scan, key, now))
            .count();
        let fresh_prospects = self
            .selection
            .keys()
            .iter()
            .filter(|key| self.covered(scan, key, now, full))
            .count();
        serde_json::json!({
            "nominated": nominated,
            "covered": covered,
            "fresh_prospects": fresh_prospects,
            "source": if full { "full" } else { "rest" }
        })
    }

    fn observe_prospect(
        &mut self,
        scan: &mut Scanner,
        instrument: &Instrument,
        point: &market_feed::Point,
        settings: &market_feed::Settings,
        now: i64,
    ) -> Result<bool> {
        if !market_feed::observe_point(scan, instrument, point, settings, now, Some(point.receipt))?
        {
            return Ok(false);
        }
        let key = &instrument.key;
        if let Some(quote) = executable(scan, key, now) {
            self.pushed.insert(key.clone(), clock(&quote));
            Ok(true)
        } else {
            self.pushed.remove(key);
            Ok(false)
        }
    }

    pub(super) fn rest_read(
        &mut self,
        scan: &Scanner,
        instruments: &[Instrument],
        receipt: i64,
        now: i64,
    ) -> bool {
        let mut relevant = false;
        for instrument in instruments {
            let key = &instrument.key;
            if let Some(quote) = executable(scan, key, now).filter(|quote| quote.receipt == receipt)
                && self.rest.get(key) != Some(&clock(&quote))
            {
                self.rest.insert(key.clone(), clock(&quote));
                relevant |= self.pinned.contains(key) || self.selection.keys().contains(key);
            }
        }
        relevant
    }
}

impl Controller {
    pub(super) fn full_strategy_depth(&self) -> bool {
        self.feed_settings()
            .is_some_and(market_feed::Settings::full_depth)
            && self.feed_health["fallback"] == false
    }

    pub(super) fn reconcile_strategy_inputs(&mut self, now: i64, rank_due: bool) -> Result<()> {
        let inputs = &mut self.strategy_inputs;
        let run = self.strategy.as_mut();
        let id = run.as_ref().and_then(|run| run.desired_id());
        let switched = inputs.selected_for.as_deref() != id;
        let held_changed = inputs.pinned.len() != self.p.positions.len()
            || self
                .p
                .positions
                .keys()
                .any(|key| !inputs.pinned.contains(key));
        if !switched
            && !held_changed
            && !rank_due
            && (run.is_none() || now < inputs.next_refresh(now))
            && inputs.refreshed_at.is_some()
        {
            return Ok(());
        }
        if switched {
            inputs.selected_for = id.map(str::to_owned);
            inputs.selection.clear();
        }
        if held_changed {
            inputs.pinned = self.p.positions.keys().cloned().collect();
        }
        let slots = self
            .feed_worker
            .as_ref()
            .map_or(20, |worker| worker.settings.shortlist_size);
        let previous = inputs.selection.keys().to_vec();
        inputs.selection.refresh(
            &self.scan,
            now,
            run.as_ref().and_then(|run| run.screen()),
            &inputs.pinned,
            self.depth_candidates.iter().map(|(key, _)| key.as_str()),
            slots.saturating_sub(inputs.pinned.len()),
        );
        inputs.refreshed_at = Some(now);

        let selected = inputs.selection.keys();
        inputs
            .pushed
            .retain(|key, _| inputs.pinned.contains(key) || selected.contains(key));
        let streaming = self
            .feed_worker
            .as_ref()
            .is_some_and(|worker| worker.settings.full_depth())
            && self.feed_health["fallback"] == false;
        if let Some(worker) = &self.feed_worker {
            let held: Vec<_> = self
                .p
                .positions
                .values()
                .map(|p| p.instrument.clone())
                .collect();
            worker.set_promoted_keys(&held, selected.iter().cloned(), id.is_some())?;
        }
        if let Some(run) = run
            && (switched
                || selected.iter().any(|key| {
                    !previous.contains(key) && inputs.covered(&self.scan, key, now, streaming)
                }))
        {
            run.request_observation();
        }
        Ok(())
    }

    pub(super) fn commit_pushed_depth(&mut self) -> Result<()> {
        let Some(worker) = &self.feed_worker else {
            return Ok(());
        };
        let updates = worker.drain_depth()?;
        let settings = worker.settings.clone();
        if updates.reconnected {
            self.strategy_inputs.pushed.clear();
        }
        if updates.held_gap {
            self.last_protection_read = 0;
            self.tape_gap(millis(), "held_depth_queue_full", 0)?;
        }
        let mut changed = false;
        if !updates.held.is_empty() {
            let tx = self
                .db
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let mut next = self.p.clone();
            let mut newest_held = None;
            for (key, point) in updates.held {
                let Some(instrument) = next
                    .positions
                    .get(&key)
                    .map(|position| &position.instrument)
                else {
                    continue;
                };
                let now = millis();
                let sample = self.scan.samples.entry(key.clone()).or_default();
                if !market_feed::observe_depth(sample, instrument, &point)? {
                    continue;
                }
                changed |= executable(&self.scan, &key, now).is_some();
                if self.scan.samples[&key].executable.is_none() {
                    continue;
                }
                let quotes: BTreeMap<_, _> = next
                    .positions
                    .keys()
                    .filter_map(|key| {
                        let quote = self.scan.samples.get(key)?.executable.as_ref()?;
                        quote
                            .fresh(now, MAX_FINANCIAL_AGE_MS)
                            .then(|| (key.clone(), quote.clone()))
                    })
                    .collect();
                if !quotes.contains_key(&key) {
                    continue;
                }
                let before = next.fills.len();
                next.apply_at(self.config.session, now, &quotes, &[])?;
                for fill in &next.fills[before..] {
                    if let Some(quote) = self
                        .scan
                        .samples
                        .get_mut(&fill.instrument.key)
                        .and_then(|sample| sample.executable.as_mut())
                    {
                        quote.bid_units = quote.bid_units.saturating_sub(fill.units);
                    }
                }
                newest_held = Some(point.receipt);
            }
            if let Some(receipt) = newest_held {
                persist_state(
                    &tx,
                    &self.config,
                    &next,
                    millis(),
                    Some(receipt),
                    None,
                    "pushed_protection",
                )?;
                tx.commit()?;
                self.p = next;
            }
        }

        for (key, point) in updates.prospects {
            if !self.strategy_inputs.selection().contains(&key) {
                continue;
            }
            let Some(&index) = self.tradable_by_key.get(&key) else {
                continue;
            };
            let now = millis();
            changed |= self.strategy_inputs.observe_prospect(
                &mut self.scan,
                &self.tradable[index],
                &point,
                &settings,
                now,
            )?;
        }
        if changed && let Some(run) = &mut self.strategy {
            run.request_observation();
        }
        Ok(())
    }

    pub(super) fn observe_strategy(&mut self) -> Result<()> {
        let full = self.full_strategy_depth();
        let Some(run) = self.strategy.as_mut() else {
            return Ok(());
        };
        if self.p.halted || self.p.stopped || self.p.closed || self.universe.is_none() {
            return Ok(());
        }
        let now = millis();
        if let Some(top_n) = run.take_observation(&self.db, &self.config, now)? {
            let frozen = Frozen::capture(
                &self.scan,
                now,
                top_n,
                &self.p,
                &self.strategy_inputs.selection,
                |key| self.tradable_by_key.get(key).map(|&i| &self.tradable[i]),
                |key| self.strategy_inputs.covered(&self.scan, key, now, full),
            );
            if !frozen.quotes.is_empty() {
                run.start(frozen, &self.p, &millis);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

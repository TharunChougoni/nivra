//! The research quote tape. Every name that reaches the live top K rides the
//! shortlist depth read from that scan until `LINGER_MS` after it last ranked
//! there, so after-close replays have depth for every name the decision lane
//! could have traded. It adds keys to an existing read, never a read.
use super::super::agent::Ranked;
use std::collections::{BTreeMap, BTreeSet};

/// How long a name stays on the tape after it leaves the top K.
pub(super) const LINGER_MS: i64 = 30 * 60_000;

#[derive(Default)]
pub(super) struct TapeWatch {
    /// Watched instrument key and the end of its watch, epoch ms.
    until: BTreeMap<String, i64>,
}

impl TapeWatch {
    pub(super) fn observe(&mut self, now: i64, ranking: &[Ranked], top_k: usize) {
        for row in ranking.iter().take(top_k) {
            self.until
                .insert(row.instrument_key.clone(), now.saturating_add(LINGER_MS));
        }
        self.until.retain(|_, until| *until >= now);
    }

    pub(super) fn len(&self) -> usize {
        self.until.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.until.is_empty()
    }

    /// The keys of one depth read: the shortlist first, then watched names,
    /// most recently ranked first, up to `cap` keys. Held names are skipped
    /// because the protection read covers them. Returns the keys and the
    /// number of watched names left out.
    pub(super) fn batch<'a>(
        &'a self,
        shortlist: &[&'a str],
        held: &BTreeSet<&str>,
        cap: usize,
    ) -> (Vec<&'a str>, usize) {
        let mut keys = shortlist.to_vec();
        let mut watched: Vec<(&String, &i64)> = self
            .until
            .iter()
            .filter(|(key, _)| !held.contains(key.as_str()) && !shortlist.contains(&key.as_str()))
            .collect();
        watched.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        let room = cap.saturating_sub(keys.len());
        let dropped = watched.len().saturating_sub(room);
        keys.extend(watched.into_iter().take(room).map(|(key, _)| key.as_str()));
        (keys, dropped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_k_names_stay_on_the_depth_read_until_the_linger_ends() {
        let ranked = |keys: &[&str]| -> Vec<Ranked> {
            keys.iter()
                .map(|key| Ranked {
                    instrument_key: (*key).into(),
                    score: 20.0,
                })
                .collect()
        };
        let mut tape = TapeWatch::default();
        tape.observe(0, &ranked(&["A", "B", "C"]), 2);
        tape.observe(60_000, &ranked(&["C", "A", "B"]), 2);
        assert_eq!(tape.len(), 3, "B stays watched after it leaves the top K");

        let held = BTreeSet::from(["A"]);
        let (keys, dropped) = tape.batch(&["S", "C"], &held, 10);
        assert_eq!(
            keys,
            ["S", "C", "B"],
            "shortlist first, held and duplicates skipped"
        );
        assert_eq!(dropped, 0);
        let (keys, dropped) = tape.batch(&["S", "T"], &BTreeSet::new(), 3);
        assert_eq!(
            keys,
            ["S", "T", "A"],
            "a most recently ranked name fills the room"
        );
        assert_eq!(dropped, 2, "the batch had no room for C and B");

        tape.observe(LINGER_MS + 1, &[], 2);
        assert_eq!(tape.len(), 2, "B's watch ended; A and C ranked later");
        tape.observe(60_000 + LINGER_MS + 1, &[], 2);
        assert_eq!(tape.len(), 0);
    }
}

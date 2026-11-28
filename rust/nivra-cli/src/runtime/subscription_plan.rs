//! Upstox V3 limits are per user, including when modes share a socket.
use std::collections::BTreeSet;

pub(super) const SINGLE_LTPC: usize = 5_000;
pub(super) const MIXED_LTPC: usize = 2_000;
pub(super) const MIXED_FULL: usize = 1_500;
const BATCH_SIZE: usize = 100;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Plan {
    pub ltpc: BTreeSet<String>,
    pub full: BTreeSet<String>,
}

impl Plan {
    // The stable LTPC core excludes promoted names. Index keys already have
    // priority in the supplied core. Remaining names are covered by REST.
    pub fn mixed(core: &BTreeSet<String>, full: BTreeSet<String>) -> Self {
        Self {
            ltpc: core.difference(&full).cloned().collect(),
            full,
        }
    }

    pub fn keys(&self) -> BTreeSet<String> {
        self.ltpc.union(&self.full).cloned().collect()
    }

    pub fn changes<'a>(
        &'a self,
        next: &'a Self,
    ) -> impl Iterator<Item = (&'static str, &'static str, Vec<String>)> + 'a {
        // Release both pools before adding to either pool. A swap can occur
        // when both pools are already at the combined subscription ceiling.
        [
            ("unsub", "ltpc", self.ltpc.difference(&next.ltpc)),
            ("unsub", "full", self.full.difference(&next.full)),
            ("sub", "ltpc", next.ltpc.difference(&self.ltpc)),
            ("sub", "full", next.full.difference(&self.full)),
        ]
        .into_iter()
        .flat_map(|(method, mode, keys)| {
            let keys: Vec<_> = keys.cloned().collect();
            keys.chunks(BATCH_SIZE)
                .map(|part| (method, mode, part.to_vec()))
                .collect::<Vec<_>>()
        })
    }
}

pub(super) fn promoted(
    held: impl IntoIterator<Item = String>,
    ranked: impl IntoIterator<Item = String>,
    desired: usize,
) -> Result<BTreeSet<String>, &'static str> {
    let mut full: BTreeSet<_> = held.into_iter().collect();
    if full.len() > MIXED_FULL {
        return Err("feed_held_subscription_limit");
    }
    for key in ranked {
        if full.len() >= desired.min(MIXED_FULL) {
            break;
        }
        full.insert(key);
    }
    Ok(full)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_plan_pins_holdings_and_releases_old_modes_before_subscribing() {
        let core = (0..MIXED_LTPC).map(|key| format!("k{key}")).collect();
        let desired = promoted(
            ["held".into(), "k0".into(), "k0".into()],
            ["k1".into(), "k2".into(), "k3".into()],
            3,
        )
        .unwrap();
        assert_eq!(
            desired,
            BTreeSet::from(["held".into(), "k0".into(), "k1".into()])
        );
        let current = Plan::mixed(&core, desired);
        assert_eq!(current.ltpc.len(), MIXED_LTPC - 2);
        let next = Plan::mixed(
            &core,
            BTreeSet::from(["held".into(), "k0".into(), "k2".into()]),
        );
        assert_eq!(
            current.changes(&next).collect::<Vec<_>>(),
            [
                ("unsub", "ltpc", vec!["k2".into()]),
                ("unsub", "full", vec!["k1".into()]),
                ("sub", "ltpc", vec!["k1".into()]),
                ("sub", "full", vec!["k2".into()]),
            ]
        );
        assert_eq!(
            promoted(["held".into()], ["k0".into()], 0).unwrap(),
            BTreeSet::from(["held".into()])
        );
        assert!(promoted((0..=MIXED_FULL).map(|n| n.to_string()), [], 0).is_err());
    }
}

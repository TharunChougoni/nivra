//! Pure, offline subscription planning. No transport or runtime integration.
#![allow(dead_code)] // S3a is deliberately not consumed by the feed yet.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub single_ltpc: usize,
    pub single_full: usize,
    pub combined_ltpc: usize,
    pub combined_full: usize,
    pub connections: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            single_ltpc: 5000,
            single_full: 2000,
            combined_ltpc: 2000,
            combined_full: 1500,
            connections: 2,
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct SubscriptionSettings {
    pub ltpc_core_keys: usize,
    pub full_keys: usize,
    pub sub_batch_size: usize,
    pub limits: Limits,
}
impl Default for SubscriptionSettings {
    fn default() -> Self {
        Self {
            ltpc_core_keys: 1900,
            full_keys: 100,
            sub_batch_size: 100,
            limits: Limits::default(),
        }
    }
}
impl SubscriptionSettings {
    pub fn validate(&self) -> Result<(), PlanError> {
        let l = &self.limits;
        let ceiling = Limits::default();
        if self.sub_batch_size == 0
            || l.connections == 0
            || l.connections > ceiling.connections
            || l.single_ltpc == 0
            || l.single_ltpc > ceiling.single_ltpc
            || l.single_full == 0
            || l.single_full > ceiling.single_full
            || l.combined_ltpc == 0
            || l.combined_ltpc > l.single_ltpc.min(ceiling.combined_ltpc)
            || l.combined_full == 0
            || l.combined_full > l.single_full.min(ceiling.combined_full)
        {
            return Err(PlanError::InvalidSettings);
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mode {
    Ltpc,
    Full,
}
/// Disjoint tiers. Full replaces LTPC for shortlisted core members.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub ltpc: BTreeSet<String>,
    pub full: BTreeSet<String>,
    pub rest: BTreeSet<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subscription {
    pub key: String,
    pub mode: Mode,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeChange {
    pub key: String,
    pub from: Mode,
    pub to: Mode,
}
/// Each inner vector is one bounded batch, sorted by key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diff {
    pub sub: Vec<Vec<Subscription>>,
    pub unsub: Vec<Vec<Subscription>>,
    pub change_mode: Vec<Vec<ModeChange>>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanError {
    InvalidSettings,
    InvalidUniverse,
    UnknownKey,
    InvalidCurrent,
    PromotionCapacity,
    LimitExceeded {
        mode: Mode,
        count: usize,
        limit: usize,
        combined: bool,
    },
}
impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "subscription_plan_{self:?}")
    }
}
impl std::error::Error for PlanError {}

fn modes(p: &Plan) -> Result<BTreeMap<String, Mode>, PlanError> {
    if !p.ltpc.is_disjoint(&p.full) || !p.ltpc.is_disjoint(&p.rest) || !p.full.is_disjoint(&p.rest)
    {
        return Err(PlanError::InvalidCurrent);
    }
    Ok(p.ltpc
        .iter()
        .map(|k| (k.clone(), Mode::Ltpc))
        .chain(p.full.iter().map(|k| (k.clone(), Mode::Full)))
        .collect())
}
fn batches<T: Clone>(items: Vec<T>, size: usize) -> Vec<Vec<T>> {
    items.chunks(size).map(<[T]>::to_vec).collect()
}
/// Rank by descending 20-day turnover, breaking ties by key. Promotions reserve
/// core slots first; excess promotions fail rather than silently dropping keys.
/// Shortlist order supplies priority, with duplicate entries consuming no slots.
pub fn plan(
    universe: &[(String, f64)],
    shortlist: &[String],
    promoted: &BTreeSet<String>,
    current: &Plan,
    cfg: &SubscriptionSettings,
) -> Result<(Plan, Diff), PlanError> {
    cfg.validate()?;
    let old = modes(current)?;
    let mut keys = BTreeSet::new();
    for (key, turnover) in universe {
        if key.trim().is_empty()
            || !turnover.is_finite()
            || *turnover < 0.0
            || !keys.insert(key.clone())
        {
            return Err(PlanError::InvalidUniverse);
        }
    }
    if promoted.iter().chain(shortlist).any(|k| !keys.contains(k)) {
        return Err(PlanError::UnknownKey);
    }
    if promoted.len() > cfg.ltpc_core_keys {
        return Err(PlanError::PromotionCapacity);
    }
    let mut ranked: Vec<_> = universe.iter().collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut core = promoted.clone();
    for (key, _) in ranked {
        if core.len() >= cfg.ltpc_core_keys {
            break;
        }
        core.insert(key.clone());
    }
    let mut full = BTreeSet::new();
    for key in shortlist {
        if full.len() >= cfg.full_keys {
            break;
        }
        full.insert(key.clone());
    }
    let next = Plan {
        ltpc: core.difference(&full).cloned().collect(),
        rest: keys
            .difference(&core)
            .filter(|k| !full.contains(*k))
            .cloned()
            .collect(),
        full,
    };
    let combined = !next.ltpc.is_empty() && !next.full.is_empty();
    let l = &cfg.limits;
    for (mode, count, limit) in [
        (
            Mode::Ltpc,
            next.ltpc.len(),
            if combined {
                l.combined_ltpc
            } else {
                l.single_ltpc
            },
        ),
        (
            Mode::Full,
            next.full.len(),
            if combined {
                l.combined_full
            } else {
                l.single_full
            },
        ),
    ] {
        if count > limit {
            return Err(PlanError::LimitExceeded {
                mode,
                count,
                limit,
                combined,
            });
        }
    }
    // Limits are user-wide; opening a second connection never multiplies them.
    let new = modes(&next)?;
    let mut sub = Vec::new();
    let mut unsub = Vec::new();
    let mut changes = Vec::new();
    for (key, mode) in &old {
        match new.get(key) {
            None => unsub.push(Subscription {
                key: key.clone(),
                mode: *mode,
            }),
            Some(to) if to != mode => changes.push(ModeChange {
                key: key.clone(),
                from: *mode,
                to: *to,
            }),
            _ => {}
        }
    }
    for (key, mode) in &new {
        if !old.contains_key(key) {
            sub.push(Subscription {
                key: key.clone(),
                mode: *mode,
            });
        }
    }
    Ok((
        next,
        Diff {
            sub: batches(sub, cfg.sub_batch_size),
            unsub: batches(unsub, cfg.sub_batch_size),
            change_mode: batches(changes, cfg.sub_batch_size),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn universe(n: usize) -> Vec<(String, f64)> {
        (0..n)
            .map(|i| (format!("k{i:04}"), (n - i) as f64))
            .collect()
    }
    #[test]
    fn limits_switch_and_refuse_without_mutation() {
        let u = universe(5001);
        let mut cfg = SubscriptionSettings {
            ltpc_core_keys: 5000,
            ..Default::default()
        };
        let (current, _) = plan(&u, &[], &BTreeSet::new(), &Plan::default(), &cfg).unwrap();
        assert_eq!(current.ltpc.len(), 5000);
        assert!(matches!(
            plan(&u, &[u[5000].0.clone()], &BTreeSet::new(), &current, &cfg),
            Err(PlanError::LimitExceeded {
                mode: Mode::Ltpc,
                limit: 2000,
                combined: true,
                ..
            })
        ));
        cfg.ltpc_core_keys = 5001;
        assert!(matches!(
            plan(&u, &[], &BTreeSet::new(), &current, &cfg),
            Err(PlanError::LimitExceeded {
                limit: 5000,
                combined: false,
                ..
            })
        ));
        cfg.ltpc_core_keys = 0;
        cfg.full_keys = 2001;
        let shortlist: Vec<_> = u.iter().take(2001).map(|v| v.0.clone()).collect();
        assert!(matches!(
            plan(&u, &shortlist, &BTreeSet::new(), &current, &cfg),
            Err(PlanError::LimitExceeded {
                mode: Mode::Full,
                limit: 2000,
                ..
            })
        ));
        cfg.ltpc_core_keys = 1;
        let shortlist: Vec<_> = u.iter().skip(1).take(1501).map(|v| v.0.clone()).collect();
        assert!(matches!(
            plan(&u, &shortlist, &BTreeSet::new(), &current, &cfg),
            Err(PlanError::LimitExceeded {
                mode: Mode::Full,
                limit: 1500,
                combined: true,
                ..
            })
        ));
        assert_eq!(current.ltpc.len(), 5000);
    }
    #[test]
    fn promotion_minimal_diffs_batches_and_determinism() {
        let mut u = universe(6);
        let cfg = SubscriptionSettings {
            ltpc_core_keys: 3,
            full_keys: 1,
            sub_batch_size: 2,
            ..Default::default()
        };
        let (initial, diff) = plan(&u, &[], &BTreeSet::new(), &Plan::default(), &cfg).unwrap();
        assert_eq!(
            diff.sub.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![2, 1]
        );
        let promoted = BTreeSet::from([u[5].0.clone()]);
        let (next, diff) = plan(&u, &[u[0].0.clone()], &promoted, &initial, &cfg).unwrap();
        assert!(next.ltpc.contains(&u[5].0));
        assert!(next.rest.contains(&u[2].0));
        assert_eq!(diff.sub[0][0].key, u[5].0);
        assert_eq!(diff.unsub[0][0].key, u[2].0);
        assert_eq!(
            diff.change_mode[0][0],
            ModeChange {
                key: u[0].0.clone(),
                from: Mode::Ltpc,
                to: Mode::Full
            }
        );
        assert_eq!(
            plan(&u, &[u[0].0.clone()], &promoted, &next, &cfg)
                .unwrap()
                .1,
            Diff::default()
        );
        u.reverse();
        assert_eq!(
            plan(&u, &["k0000".into()], &promoted, &initial, &cfg).unwrap(),
            (next, diff)
        );
    }
    #[test]
    fn defaults_validation_ties_and_bad_inputs() {
        let cfg: SubscriptionSettings = serde_json::from_str(r#"{"limits":{}}"#).unwrap();
        assert_eq!(cfg, SubscriptionSettings::default());
        for invalid in [
            SubscriptionSettings {
                sub_batch_size: 0,
                ..cfg.clone()
            },
            SubscriptionSettings {
                limits: Limits {
                    connections: 3,
                    ..Limits::default()
                },
                ..cfg.clone()
            },
        ] {
            assert_eq!(invalid.validate(), Err(PlanError::InvalidSettings));
        }
        let cfg = SubscriptionSettings {
            ltpc_core_keys: 1,
            ..cfg
        };
        let u = vec![("b".into(), 1.0), ("a".into(), 1.0)];
        assert_eq!(
            plan(&u, &[], &BTreeSet::new(), &Plan::default(), &cfg)
                .unwrap()
                .0
                .ltpc,
            BTreeSet::from(["a".into()])
        );
        assert_eq!(
            plan(
                &u,
                &["missing".into()],
                &BTreeSet::new(),
                &Plan::default(),
                &cfg
            ),
            Err(PlanError::UnknownKey)
        );
        assert_eq!(
            plan(
                &[("a".into(), f64::NAN)],
                &[],
                &BTreeSet::new(),
                &Plan::default(),
                &cfg
            ),
            Err(PlanError::InvalidUniverse)
        );
        assert_eq!(
            plan(
                &u,
                &[],
                &BTreeSet::from(["a".into(), "b".into()]),
                &Plan::default(),
                &cfg
            ),
            Err(PlanError::PromotionCapacity)
        );
    }
}

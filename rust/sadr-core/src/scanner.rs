//! Quota assignment. Protection, then entry, then a fair scan. Research waits.

use crate::market::MAX_FINANCIAL_AGE_MICROS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Demand {
    Protection,
    Entry,
    Scan,
    Research,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub demand: Demand,
    pub batch_ready: bool,
    pub age_micros: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    pub name: String,
    pub demand: Demand,
    pub served: bool,
    pub stale: bool,
    pub partial: bool,
    pub age_micros: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coverage {
    pub served: u32,
    pub pending: u32,
    pub partial: u32,
}

pub fn schedule(symbols: &[Symbol], quota: u32) -> Vec<Assignment> {
    let mut order: Vec<usize> = (0..symbols.len()).collect();
    order.sort_by(|&left, &right| {
        rank(symbols[left].demand)
            .cmp(&rank(symbols[right].demand))
            .then(left.cmp(&right))
    });
    let mut served = vec![false; symbols.len()];
    let mut remaining = quota;
    for index in order {
        let symbol = &symbols[index];
        let higher_open = symbols.iter().enumerate().any(|(other, candidate)| {
            rank(candidate.demand) < rank(symbol.demand) && !served[other]
        });
        if symbol.demand == Demand::Research && higher_open {
            continue;
        }
        if symbol.demand != Demand::Research && !symbol.batch_ready {
            continue;
        }
        if remaining == 0 {
            continue;
        }
        served[index] = true;
        remaining -= 1;
    }
    symbols
        .iter()
        .enumerate()
        .map(|(index, symbol)| {
            let partial = symbol.demand != Demand::Research && !symbol.batch_ready;
            let age_stale = symbol
                .age_micros
                .is_none_or(|age| !(0..=MAX_FINANCIAL_AGE_MICROS).contains(&age));
            Assignment {
                name: symbol.name.clone(),
                demand: symbol.demand,
                served: served[index],
                stale: partial || age_stale,
                partial,
                age_micros: symbol.age_micros,
            }
        })
        .collect()
}

pub fn coverage(assignments: &[Assignment]) -> Coverage {
    Coverage {
        served: count(assignments, |row| row.served),
        pending: count(assignments, |row| !row.served),
        partial: count(assignments, |row| row.partial),
    }
}

pub fn trading_authorized() -> bool {
    false
}

fn count(assignments: &[Assignment], predicate: impl Fn(&Assignment) -> bool) -> u32 {
    assignments.iter().filter(|row| predicate(row)).count() as u32
}

fn rank(demand: Demand) -> u8 {
    match demand {
        Demand::Protection => 0,
        Demand::Entry => 1,
        Demand::Scan => 2,
        Demand::Research => 3,
    }
}

/// Persist this cursor with the caller's controller checkpoint, never the quota journal.
/// Protection/entry preserve input order; scan work rotates by stable instrument key.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FairScheduler {
    last_scan: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleError {
    InvalidIdentity,
}
impl FairScheduler {
    pub fn assign(
        &mut self,
        symbols: &[Symbol],
        quota: u32,
    ) -> Result<Vec<Assignment>, ScheduleError> {
        let mut names = std::collections::BTreeSet::new();
        if symbols
            .iter()
            .any(|s| s.name.is_empty() || s.name.len() > 128 || !names.insert(&s.name))
        {
            return Err(ScheduleError::InvalidIdentity);
        }
        let mut ordered = symbols.to_vec();
        ordered.sort_by(|a, b| {
            rank(a.demand).cmp(&rank(b.demand)).then_with(|| {
                if a.demand == Demand::Scan {
                    let after =
                        |s: &Symbol| self.last_scan.as_ref().is_none_or(|last| s.name > *last);
                    after(b).cmp(&after(a)).then(a.name.cmp(&b.name))
                } else {
                    std::cmp::Ordering::Equal
                }
            })
        });
        let mut assigned = schedule(&ordered, quota);
        let protection_waiting = assigned
            .iter()
            .any(|s| s.demand == Demand::Protection && !s.served);
        if protection_waiting {
            for row in &mut assigned {
                if row.demand != Demand::Protection {
                    row.served = false;
                }
            }
        }
        if let Some(last) = assigned
            .iter()
            .rev()
            .find(|s| s.demand == Demand::Scan && s.served)
        {
            self.last_scan = Some(last.name.clone());
        }
        let by_name: std::collections::BTreeMap<_, _> =
            assigned.into_iter().map(|a| (a.name.clone(), a)).collect();
        Ok(symbols.iter().map(|s| by_name[&s.name].clone()).collect())
    }
}

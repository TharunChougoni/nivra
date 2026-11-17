//! Bounded account-equity observations for the authenticated workspace.
//! Display evidence only; original portfolio checkpoints remain authoritative.
use serde_json::{Value, json};
use std::collections::VecDeque;

pub(crate) struct EquitySeries {
    points: VecDeque<Value>,
    last_at: i64,
    interval_ms: i64,
    max_points: usize,
}

impl EquitySeries {
    pub(crate) fn new(interval_ms: u64, max_points: usize) -> Self {
        Self {
            points: VecDeque::new(),
            last_at: 0,
            interval_ms: interval_ms.min(i64::MAX as u64) as i64,
            max_points: max_points.max(1),
        }
    }

    pub(crate) fn reset(&mut self) {
        self.points.clear();
        self.last_at = 0;
    }

    /// Append one recorded checkpoint. Cash and equity are exact decimal strings.
    /// A repeated checkpoint replaces only the tail; an older one is ignored and
    /// never rewrites earlier observations.
    pub(crate) fn observe(&mut self, at: i64, cash: &str, equity: &str) {
        if at <= 0 || cash.is_empty() || equity.is_empty() {
            return;
        }
        let point = json!({
            "at": at,
            "cash": cash,
            "equity": equity,
            "mark_basis": "recorded_position_marks",
            "execution_authority": false,
        });
        if let Some(last) = self.points.back_mut() {
            if at == self.last_at {
                *last = point;
                return;
            }
            if at < self.last_at {
                return;
            }
        }
        if self.last_at > 0 && at.saturating_sub(self.last_at) < self.interval_ms {
            return;
        }
        self.last_at = at;
        self.points.push_back(point);
        while self.points.len() > self.max_points {
            self.points.pop_front();
        }
    }

    pub(crate) fn points(&self, from: Option<i64>, to: Option<i64>, limit: usize) -> Vec<Value> {
        let mut rows = self
            .points
            .iter()
            .filter(|point| {
                let at = point["at"].as_i64().unwrap_or(0);
                from.is_none_or(|from| at >= from) && to.is_none_or(|to| at <= to)
            })
            .cloned()
            .collect::<Vec<_>>();
        if rows.len() > limit {
            rows = rows.split_off(rows.len() - limit);
        }
        rows
    }

    pub(crate) fn last_at(&self) -> Option<i64> {
        self.points.back().and_then(|point| point["at"].as_i64())
    }
}

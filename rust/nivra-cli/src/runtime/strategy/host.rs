//! `StrategyHost`: manages a small pool of persistent strategy workers
//! (`WorkerKey = (source_sha256, params_sha256)`), running one live worker with
//! non-blocking hot swap and activation generation tracking, plus one
//! independent shadow slot.
#[cfg(test)]
use super::worker::WorkerState;
use super::worker::{Worker, WorkerConfig, WorkerKey};
use crate::runtime::{controller_events::Notify, wide::strategy_run::shadow::ShadowSlot};
use nivra_core::replay::digest;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

const MAX_POOL_WORKERS: usize = 2;
const RETIRE_GRACE_MS: i64 = 60_000;

pub(crate) struct StrategyHost {
    config: WorkerConfig,
    notify: Option<Notify>,
    workers: BTreeMap<WorkerKey, Worker>,
    startup_gate: Arc<Mutex<()>>,
    live: Option<WorkerKey>,
    desired_live: Option<WorkerKey>,
    activation_generation: u64,
    retiring: BTreeMap<WorkerKey, i64>,
    pub(crate) shadow: Option<ShadowSlot>,
    pending_live: Option<(Vec<u8>, Value)>,
}

impl StrategyHost {
    pub(crate) fn new(config: WorkerConfig, notify: Option<Notify>) -> Self {
        Self {
            config,
            notify,
            workers: BTreeMap::new(),
            startup_gate: Arc::default(),
            live: None,
            desired_live: None,
            activation_generation: 0,
            retiring: BTreeMap::new(),
            shadow: None,
            pending_live: None,
        }
    }

    pub(crate) fn activation_generation(&self) -> u64 {
        self.activation_generation
    }

    pub(crate) fn live_key(&self) -> Option<&WorkerKey> {
        self.live.as_ref()
    }

    pub(crate) fn desired_key(&self) -> Option<&WorkerKey> {
        self.desired_live.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn desired_starting(&self) -> bool {
        self.desired_live.as_ref().is_some_and(|key| {
            self.workers
                .get(key)
                .is_some_and(|w| matches!(w.state(), WorkerState::Starting))
        })
    }

    pub(crate) fn stderr_tail(&self, key: Option<&WorkerKey>) -> Option<String> {
        key.or(self.live.as_ref())
            .or(self.desired_live.as_ref())
            .and_then(|k| self.workers.get(k))
            .map(Worker::stderr_tail)
            .filter(|text| !text.is_empty())
    }

    pub(crate) fn pool_len(&self) -> usize {
        self.workers.len()
            + usize::from(
                self.shadow
                    .as_ref()
                    .is_some_and(ShadowSlot::worker_reserved),
            )
    }

    fn evict_unused(&mut self, now_ms: i64) {
        let expired: Vec<WorkerKey> = self
            .retiring
            .iter()
            .filter(|&(_, &until)| now_ms >= until)
            .map(|(key, _)| key.clone())
            .collect();
        for key in expired {
            self.retiring.remove(&key);
            if self.live.as_ref() != Some(&key)
                && self.desired_live.as_ref() != Some(&key)
                && let Some(mut worker) = self.workers.remove(&key)
            {
                worker.stop();
            }
        }
    }

    fn reserve_slot(&mut self) -> Result<(), &'static str> {
        if self.pool_len() < MAX_POOL_WORKERS {
            return Ok(());
        }
        let key = self
            .workers
            .keys()
            .find(|key| {
                self.live.as_ref() != Some(*key) && self.desired_live.as_ref() != Some(*key)
            })
            .cloned()
            .ok_or("strategy_worker_pool_full")?;
        self.retiring.remove(&key);
        if let Some(mut worker) = self.workers.remove(&key) {
            worker.stop();
        }
        Ok(())
    }

    /// Synchronize a reviewed choice with the pool.
    ///
    /// - `desired = None` (stand-down, disable, or blocked) revokes the live
    ///   worker immediately and bumps `activation_generation`.
    /// - `desired = Some(...)` starts the new worker in the background if not
    ///   present, keeps the old live worker until the new one is ready, and
    ///   flips `live` (bumping `activation_generation`) once ready.
    pub(crate) fn sync_desired(
        &mut self,
        desired: Option<(&[u8], &Value)>,
        now_ms: i64,
    ) -> Result<(), &'static str> {
        let Some((source, params)) = desired else {
            if self.live.is_some() || self.desired_live.is_some() {
                self.activation_generation = self.activation_generation.wrapping_add(1);
            }
            self.pending_live = None;
            if let Some(old) = self.live.take() {
                self.retiring.remove(&old);
                if let Some(mut worker) = self.workers.remove(&old) {
                    worker.stop();
                }
            }
            if let Some(old_desired) = self.desired_live.take() {
                self.retiring.remove(&old_desired);
                if let Some(mut worker) = self.workers.remove(&old_desired) {
                    worker.stop();
                }
            }
            self.evict_unused(now_ms);
            self.allow_shadow();
            return Ok(());
        };

        let source_sha256 = digest(source);
        let params_bytes = serde_json::to_vec(params).map_err(|_| "strategy_input_limit")?;
        let key = WorkerKey {
            source_sha256: source_sha256.clone(),
            params_sha256: digest(&params_bytes),
        };
        if self.desired_live.as_ref() != Some(&key) {
            self.desired_live = Some(key.clone());
            self.retiring.remove(&key);
            self.pending_live =
                (!self.workers.contains_key(&key)).then(|| (source.to_vec(), params.clone()));
            self.launch_desired(now_ms)?;
        }

        Ok(())
    }

    fn launch_desired(&mut self, now_ms: i64) -> Result<(), &'static str> {
        if self.pending_live.is_none() {
            return Ok(());
        }
        if let Some(shadow) = &self.shadow
            && self.workers.len() >= MAX_POOL_WORKERS - 1
            && shadow.permit_worker(false)
        {
            return Ok(());
        }
        self.evict_unused(now_ms);
        self.reserve_slot()?;
        let key = self
            .desired_live
            .clone()
            .ok_or("strategy_worker_unavailable")?;
        let (source, params) = self
            .pending_live
            .take()
            .ok_or("strategy_worker_unavailable")?;
        let worker = Worker::start_background(
            source,
            params,
            self.config.clone(),
            now_ms,
            self.notify.clone(),
            self.startup_gate.clone(),
        );
        self.workers.insert(key, worker);
        Ok(())
    }

    fn allow_shadow(&mut self) {
        let Some(shadow) = self.shadow.as_ref().filter(|s| s.top_n().is_some()) else {
            return;
        };
        if (self.live.is_none() || self.live == self.desired_live) && self.pending_live.is_none() {
            for (key, _) in std::mem::take(&mut self.retiring) {
                if let Some(mut worker) = self.workers.remove(&key) {
                    worker.stop();
                }
            }
            shadow.permit_worker(self.workers.len() < MAX_POOL_WORKERS);
        }
    }

    pub(crate) fn advance_activation(&mut self, now_ms: i64) -> Result<bool, &'static str> {
        self.evict_unused(now_ms);
        self.launch_desired(now_ms)?;
        let Some(desired) = self.desired_live.clone() else {
            self.allow_shadow();
            return Ok(false);
        };
        if self.pending_live.is_some() {
            return Ok(false);
        }
        let worker = self
            .workers
            .get_mut(&desired)
            .ok_or("strategy_worker_unavailable")?;
        let ready = worker.ensure_ready(now_ms);
        self.allow_shadow();
        if !ready? {
            return Ok(false);
        }
        if self.live.as_ref() != Some(&desired) {
            if let Some(old) = self.live.replace(desired) {
                self.retiring
                    .insert(old, now_ms.saturating_add(RETIRE_GRACE_MS));
            }
            self.activation_generation = self.activation_generation.wrapping_add(1);
        }
        self.allow_shadow();
        Ok(true)
    }

    pub(crate) fn time_until_due(&self, now_ms: i64) -> Option<Duration> {
        if self.pending_live.is_some() {
            return Some(Duration::from_millis(50));
        }
        self.desired_live
            .iter()
            .filter_map(|key| self.workers.get(key)?.time_until_due(now_ms))
            .min()
    }

    /// True when the active live worker is ready to accept an observation.
    pub(crate) fn live_ready(&mut self, now_ms: i64) -> Result<bool, &'static str> {
        self.advance_activation(now_ms)
    }

    /// Submit `observation_json` to the live worker. Returns `(generation, seq)`.
    pub(crate) fn submit_live(
        &mut self,
        observation_json: &[u8],
        now_ms: i64,
    ) -> Result<(u64, u64), &'static str> {
        let Some(live_key) = self.live.clone() else {
            return Err("strategy_worker_unavailable");
        };
        let worker = self
            .workers
            .get_mut(&live_key)
            .ok_or("strategy_worker_unavailable")?;
        let seq = worker.submit(observation_json, now_ms)?;
        Ok((self.activation_generation, seq))
    }

    /// Poll a specific live worker call non-blockingly.
    #[cfg(test)]
    pub(crate) fn is_call_finished(&mut self, key: &WorkerKey, now_ms: i64) -> bool {
        self.workers
            .get_mut(key)
            .is_none_or(|worker| worker.is_call_finished(now_ms))
    }

    pub(crate) fn poll_call(
        &mut self,
        key: &WorkerKey,
        expected_seq: u64,
        now_ms: i64,
    ) -> Option<super::worker::WorkerReply> {
        let worker = self.workers.get_mut(key)?;
        let mut reply = worker.poll(now_ms)?;
        if reply.seq != expected_seq {
            reply.outcome = Err("strategy_output_invalid");
        }
        Some(reply)
    }

    #[cfg(test)]
    pub(crate) fn wait_live_ready(&mut self, now_ms: i64) -> Result<(), &'static str> {
        if let Some(desired) = self.desired_live.clone()
            && let Some(worker) = self.workers.get_mut(&desired)
        {
            worker.wait_ready(now_ms)?;
            if self.live.as_ref() != Some(&desired) {
                let previous = self.live.replace(desired.clone());
                self.activation_generation = self.activation_generation.wrapping_add(1);
                if let Some(old) = previous
                    && old != desired
                {
                    self.retiring
                        .insert(old, now_ms.saturating_add(RETIRE_GRACE_MS));
                }
            }
        }
        Ok(())
    }
}

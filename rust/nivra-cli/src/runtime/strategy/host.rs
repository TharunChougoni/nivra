//! `StrategyHost`: manages a small pool of persistent strategy workers
//! (`WorkerKey = (source_sha256, params_sha256)`), running one live worker with
//! non-blocking hot swap and activation generation tracking, and reserving
//! shadow slots for follow-up shadow-testing tools.
#[cfg(test)]
use super::worker::WorkerState;
use super::worker::{Worker, WorkerConfig, WorkerKey};
use crate::runtime::strategy_library::review;
use nivra_core::replay::digest;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const MAX_POOL_WORKERS: usize = 4;
const RETIRE_GRACE_MS: i64 = 60_000;

/// Reserved shadow slot shape for follow-up research shadow tools.
#[allow(dead_code)] // Reserved for follow-up shadow research tools.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ShadowSlot {
    pub label: String,
    pub key: WorkerKey,
    pub until_ms: i64,
}

pub(crate) struct StrategyHost {
    config: WorkerConfig,
    workers: BTreeMap<WorkerKey, Worker>,
    reviewed_static: BTreeSet<String>,
    live: Option<WorkerKey>,
    desired_live: Option<WorkerKey>,
    activation_generation: u64,
    retiring: BTreeMap<WorkerKey, i64>,
    shadows: Vec<ShadowSlot>,
}

impl StrategyHost {
    pub(crate) fn new(config: WorkerConfig) -> Self {
        Self {
            config,
            workers: BTreeMap::new(),
            reviewed_static: BTreeSet::new(),
            live: None,
            desired_live: None,
            activation_generation: 0,
            retiring: BTreeMap::new(),
            shadows: Vec::new(),
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
    }

    fn verify_static(&mut self, source_sha256: &str, source: &[u8]) -> Result<(), &'static str> {
        if self.reviewed_static.contains(source_sha256) {
            return Ok(());
        }
        review::static_check(
            &self.config.python_path,
            source,
            self.config.env_dir.is_some(),
        )
        .map_err(|_| "strategy_review_unsafe")?;
        self.reviewed_static.insert(source_sha256.to_owned());
        Ok(())
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
                && !self.shadows.iter().any(|s| s.key == key)
                && let Some(mut worker) = self.workers.remove(&key)
            {
                worker.stop();
            }
        }
        while self.workers.len() > MAX_POOL_WORKERS {
            let candidate = self
                .retiring
                .iter()
                .min_by_key(|&(_, &until)| until)
                .map(|(key, _)| key.clone())
                .or_else(|| {
                    self.workers
                        .keys()
                        .find(|key| {
                            self.live.as_ref() != Some(*key)
                                && self.desired_live.as_ref() != Some(*key)
                                && !self.shadows.iter().any(|s| &s.key == *key)
                        })
                        .cloned()
                });
            let Some(key) = candidate else {
                break;
            };
            self.retiring.remove(&key);
            if let Some(mut worker) = self.workers.remove(&key) {
                worker.stop();
            }
        }
    }

    /// Synchronize the desired live strategy with the pool.
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
            if let Some(old) = self.live.take() {
                self.retiring.remove(&old);
                if !self.shadows.iter().any(|s| s.key == old)
                    && let Some(mut worker) = self.workers.remove(&old)
                {
                    worker.stop();
                }
            }
            if let Some(old_desired) = self.desired_live.take() {
                self.retiring.remove(&old_desired);
                if !self.shadows.iter().any(|s| s.key == old_desired)
                    && let Some(mut worker) = self.workers.remove(&old_desired)
                {
                    worker.stop();
                }
            }
            self.evict_unused(now_ms);
            return Ok(());
        };

        let source_sha256 = digest(source);
        let params_bytes = serde_json::to_vec(params).map_err(|_| "strategy_input_limit")?;
        let key = WorkerKey {
            source_sha256: source_sha256.clone(),
            params_sha256: digest(&params_bytes),
        };
        if let Err(reason) = self.verify_static(&source_sha256, source) {
            self.sync_desired(None, now_ms)?;
            return Err(reason);
        }

        if self.desired_live.as_ref() != Some(&key) {
            self.desired_live = Some(key.clone());
            self.retiring.remove(&key);
            if !self.workers.contains_key(&key) {
                self.evict_unused(now_ms);
                let worker = Worker::start(
                    key.clone(),
                    source.to_vec(),
                    params.clone(),
                    self.config.clone(),
                    now_ms,
                )?;
                self.workers.insert(key.clone(), worker);
            }
        }

        if let Some(worker) = self.workers.get_mut(&key) {
            let desired_ready = worker.ensure_ready(now_ms);
            if self.live.as_ref() != Some(&key) && matches!(desired_ready, Ok(true)) {
                let previous = self.live.replace(key.clone());
                self.activation_generation = self.activation_generation.wrapping_add(1);
                if let Some(old) = previous
                    && old != key
                {
                    self.retiring
                        .insert(old, now_ms.saturating_add(RETIRE_GRACE_MS));
                }
            } else if let Err(reason) = desired_ready
                && self.live.as_ref() == Some(&key)
            {
                return Err(reason);
            }
        }
        self.evict_unused(now_ms);
        Ok(())
    }

    /// True when the active live worker is `Ready` to accept an observation.
    pub(crate) fn live_ready(&mut self, now_ms: i64) -> Result<bool, &'static str> {
        self.evict_unused(now_ms);
        if let Some(desired) = self.desired_live.clone()
            && self.live.as_ref() != Some(&desired)
            && let Some(worker) = self.workers.get_mut(&desired)
            && matches!(worker.ensure_ready(now_ms), Ok(true))
        {
            let previous = self.live.replace(desired.clone());
            self.activation_generation = self.activation_generation.wrapping_add(1);
            if let Some(old) = previous
                && old != desired
            {
                self.retiring
                    .insert(old, now_ms.saturating_add(RETIRE_GRACE_MS));
            }
        }
        let Some(live_key) = self.live.clone() else {
            return match self
                .desired_live
                .as_ref()
                .and_then(|k| self.workers.get_mut(k))
            {
                Some(worker) => match worker.ensure_ready(now_ms) {
                    Ok(_) => Ok(false),
                    Err(reason) => Err(reason),
                },
                None => Ok(false),
            };
        };
        let Some(worker) = self.workers.get_mut(&live_key) else {
            return Ok(false);
        };
        worker.ensure_ready(now_ms)
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

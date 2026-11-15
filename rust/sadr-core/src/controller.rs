//! Offline daily controller. One primary book per broker, account, and NSE date.
//!
//! A halt stays halted. A committed effect is not repeated. This machine does not
//! start a session or authorize trading.

use crate::time::{ExchangeDate, LocalTime};
use serde::{Deserialize, Serialize};
use std::{error::Error, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    Boot,
    Reconcile,
    Sleep,
    Acquire,
    Prepare,
    Ready,
    Running,
    Closing,
    Report,
    ReviewQueued,
    Completed,
    Blocked,
    Halted,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DayKind {
    Trading,
    Closed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DayId {
    pub broker: String,
    pub account: String,
    pub date: ExchangeDate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Book {
    id: DayId,
    policy_hash: String,
    phase: Phase,
    generation: u64,
    reconciled: bool,
    pending: bool,
    effect_committed: bool,
    entry_permitted: bool,
    replayed: bool,
    model_attempt_retained: bool,
    refunded: bool,
}

impl Book {
    pub fn open(id: DayId, policy_hash: &str) -> Result<Self, ControlError> {
        if !alias_ok(&id.broker) || !alias_ok(&id.account) || !hash_ok(policy_hash) {
            return Err(ControlError::Identity);
        }
        Ok(Self {
            id,
            policy_hash: policy_hash.to_string(),
            phase: Phase::Boot,
            generation: 1,
            reconciled: false,
            pending: false,
            effect_committed: false,
            entry_permitted: false,
            replayed: false,
            model_attempt_retained: false,
            refunded: false,
        })
    }

    /// Prepare metadata for a later day of the same owner. This neither authorizes
    /// a session nor proves calendar coverage, flatness, grants or journal continuity.
    pub fn open_another(&self, id: DayId, policy_hash: &str) -> Result<Self, ControlError> {
        if self.id == id {
            return Err(ControlError::DuplicateDay);
        }
        if id.broker != self.id.broker || id.account != self.id.account || id.date <= self.id.date {
            return Err(ControlError::Identity);
        }
        if !matches!(self.phase, Phase::Completed | Phase::Skipped) || self.pending {
            return Err(ControlError::Phase);
        }
        Self::open(id, policy_hash)
    }

    pub fn id(&self) -> &DayId {
        &self.id
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn policy_hash(&self) -> &str {
        &self.policy_hash
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn reconciled(&self) -> bool {
        self.reconciled
    }

    pub fn effect_committed(&self) -> bool {
        self.effect_committed
    }

    pub fn entry_permitted(&self) -> bool {
        self.entry_permitted
    }

    pub fn replayed(&self) -> bool {
        self.replayed
    }

    pub fn model_attempt_retained(&self) -> bool {
        self.model_attempt_retained
    }

    pub fn refunded(&self) -> bool {
        self.refunded
    }

    pub fn trading_authorized(&self) -> bool {
        false
    }

    /// Validate restored durable state before it can participate in a transition.
    pub fn validate(&self) -> Result<(), ControlError> {
        if !alias_ok(&self.id.broker)
            || !alias_ok(&self.id.account)
            || !hash_ok(&self.policy_hash)
            || self.generation == 0
            || (self.entry_permitted && (self.phase != Phase::Running || !self.reconciled))
            || (self.phase == Phase::Completed && (self.pending || !self.reconciled))
            || (self.pending && self.effect_committed)
            || self.refunded
            || self.replayed
        {
            return Err(ControlError::Identity);
        }
        Ok(())
    }

    pub fn bind_policy(&mut self, policy_hash: &str) -> Result<(), ControlError> {
        self.require(Phase::Boot)?;
        if !hash_ok(policy_hash) {
            return Err(ControlError::Identity);
        }
        self.policy_hash = policy_hash.to_string();
        Ok(())
    }

    pub fn boot(&mut self) -> Result<(), ControlError> {
        self.require(Phase::Boot)?;
        self.phase = Phase::Reconcile;
        Ok(())
    }

    pub fn reconcile(&mut self) -> Result<(), ControlError> {
        self.require(Phase::Reconcile)?;
        self.reconciled = true;
        self.replayed = false;
        self.phase = Phase::Sleep;
        Ok(())
    }

    pub fn wake(&mut self) -> Result<(), ControlError> {
        self.require(Phase::Sleep)?;
        self.replayed = false;
        self.phase = Phase::Acquire;
        Ok(())
    }

    pub fn acquire(&mut self, file_ok: bool) -> Result<(), ControlError> {
        self.require(Phase::Acquire)?;
        self.phase = if file_ok {
            Phase::Prepare
        } else {
            self.entry_permitted = false;
            Phase::Blocked
        };
        Ok(())
    }

    pub fn prepare(&mut self, calendar_known: bool) -> Result<(), ControlError> {
        self.require(Phase::Prepare)?;
        self.phase = if calendar_known {
            Phase::Ready
        } else {
            self.entry_permitted = false;
            Phase::Blocked
        };
        Ok(())
    }

    pub fn begin_session(&mut self, kind: DayKind, now: LocalTime) -> Result<(), ControlError> {
        self.require(Phase::Ready)?;
        self.entry_permitted = false;
        self.phase = match kind {
            DayKind::Unknown => Phase::Blocked,
            DayKind::Closed => Phase::Skipped,
            DayKind::Trading if now >= clock("15:15") => Phase::Closing,
            DayKind::Trading if now >= clock("09:15") => Phase::Skipped,
            DayKind::Trading => {
                self.entry_permitted = true;
                Phase::Running
            }
        };
        Ok(())
    }

    pub fn close(&mut self) -> Result<(), ControlError> {
        if self.phase != Phase::Running && self.phase != Phase::Closing {
            return Err(ControlError::Phase);
        }
        self.entry_permitted = false;
        self.phase = Phase::Closing;
        Ok(())
    }

    pub fn report(&mut self) -> Result<(), ControlError> {
        self.require(Phase::Closing)?;
        if self.pending {
            return Err(ControlError::PendingEffect);
        }
        self.phase = Phase::Report;
        Ok(())
    }

    pub fn queue_review(&mut self) -> Result<(), ControlError> {
        self.require(Phase::Report)?;
        self.phase = Phase::ReviewQueued;
        Ok(())
    }

    pub fn sleep(&mut self) -> Result<(), ControlError> {
        if matches!(
            self.phase,
            Phase::Halted
                | Phase::Blocked
                | Phase::Skipped
                | Phase::ReviewQueued
                | Phase::Completed
        ) {
            self.replayed = false;
            if self.phase == Phase::ReviewQueued {
                self.phase = Phase::Completed;
            }
            return Ok(());
        }
        Err(ControlError::Phase)
    }

    pub fn halt(&mut self) {
        self.phase = Phase::Halted;
        self.entry_permitted = false;
    }

    pub fn reject_clock_jump(&mut self) {
        if !matches!(
            self.phase,
            Phase::Halted | Phase::Completed | Phase::Skipped
        ) {
            self.phase = Phase::Blocked;
        }
        self.entry_permitted = false;
    }

    pub fn fence(&mut self) {
        if let Some(generation) = self.generation.checked_add(1) {
            self.generation = generation;
        } else {
            self.halt();
        }
        self.entry_permitted = false;
    }

    pub fn accept_child(&self, generation: u64) -> Result<(), ControlError> {
        if self.phase == Phase::Halted {
            return Err(ControlError::Halted);
        }
        if generation == self.generation {
            Ok(())
        } else {
            Err(ControlError::StaleGeneration)
        }
    }

    pub fn note_intent(&mut self) -> Result<(), ControlError> {
        if self.phase == Phase::Halted || self.effect_committed {
            return Err(ControlError::Halted);
        }
        self.require(Phase::Running)?;
        if !self.entry_permitted || self.pending {
            return Err(ControlError::Phase);
        }
        self.pending = true;
        Ok(())
    }

    pub fn crash_before_commit(&mut self) {
        self.pending = false;
    }

    pub fn pending(&self) -> bool {
        self.pending
    }

    pub fn commit_effect(&mut self) -> Result<(), ControlError> {
        if self.phase == Phase::Halted {
            return Err(ControlError::Halted);
        }
        if self.effect_committed {
            return Err(ControlError::ExistingEffect);
        }
        self.require(Phase::Running)?;
        if !self.pending || !self.entry_permitted {
            return Err(ControlError::Phase);
        }
        self.pending = false;
        self.effect_committed = true;
        Ok(())
    }

    pub fn retain_unknown_model(&mut self) {
        self.model_attempt_retained = true;
        self.refunded = false;
    }

    fn require(&self, phase: Phase) -> Result<(), ControlError> {
        if self.phase == phase {
            Ok(())
        } else {
            Err(ControlError::Phase)
        }
    }
}

pub fn previous_trading_day(day: ExchangeDate, holidays: &[ExchangeDate]) -> Option<ExchangeDate> {
    let mut cursor = day.previous_day()?;
    for _ in 0..14 {
        if !cursor.is_weekend() && !holidays.contains(&cursor) {
            return Some(cursor);
        }
        cursor = cursor.previous_day()?;
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlError {
    Identity,
    DuplicateDay,
    Phase,
    Halted,
    ExistingEffect,
    StaleGeneration,
    PendingEffect,
}

impl fmt::Display for ControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Identity => "controller_identity_rejected",
            Self::DuplicateDay => "controller_duplicate_day",
            Self::Phase => "controller_phase_rejected",
            Self::Halted => "controller_halted",
            Self::ExistingEffect => "controller_effect_exists",
            Self::StaleGeneration => "controller_stale_generation",
            Self::PendingEffect => "controller_pending_effect_requires_reconciliation",
        })
    }
}

impl Error for ControlError {}

fn clock(text: &str) -> LocalTime {
    LocalTime::parse(text).expect("constant local time")
}

fn alias_ok(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_lowercase()
        && value.len() <= 32
        && chars.all(|char| {
            char.is_ascii_lowercase() || char.is_ascii_digit() || char == '_' || char == '-'
        })
}

fn hash_ok(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

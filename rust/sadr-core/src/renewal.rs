//! Offline refresh decisions. Unknown or rejected work blocks readiness and never falls back.

use std::{error::Error, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Unknown,
    Rejected,
    Accepted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    account: String,
    owner: Option<u64>,
    requests: u32,
    preserved: bool,
    readiness_blocked: bool,
    cancelled: bool,
}

impl Grant {
    pub fn open(account: &str) -> Result<Self, RenewalError> {
        if !account_ok(account) {
            return Err(RenewalError::Account);
        }
        Ok(Self {
            account: account.to_string(),
            owner: None,
            requests: 0,
            preserved: false,
            readiness_blocked: true,
            cancelled: false,
        })
    }

    pub fn account(&self) -> &str {
        &self.account
    }

    pub fn requests(&self) -> u32 {
        self.requests
    }

    pub fn preserved(&self) -> bool {
        self.preserved
    }

    pub fn readiness_blocked(&self) -> bool {
        self.readiness_blocked
    }

    pub fn cancelled(&self) -> bool {
        self.cancelled
    }

    pub fn fallback_enabled(&self) -> bool {
        false
    }

    pub fn trading_authorized(&self) -> bool {
        false
    }

    pub fn apply(&mut self, refresh: Refresh<'_>) -> Result<(), RenewalError> {
        if self.cancelled {
            return Err(RenewalError::Cancelled);
        }
        match refresh {
            Refresh::Mismatch { observed } => {
                if observed == self.account {
                    return Err(RenewalError::Account);
                }
                self.preserved = true;
                self.readiness_blocked = true;
            }
            Refresh::Cancel => {
                self.cancelled = true;
                self.preserved = true;
                self.readiness_blocked = true;
            }
            Refresh::Simultaneous { requester, other } => {
                if requester == other {
                    return Err(RenewalError::Owner);
                }
                self.preserved = true;
                self.readiness_blocked = true;
            }
            Refresh::Expire { requester, outcome } | Refresh::Rotate { requester, outcome } => {
                if self.owner.is_some_and(|owner| owner != requester) {
                    self.preserved = true;
                    self.readiness_blocked = true;
                    return Ok(());
                }
                if self.requests == 0 {
                    self.requests = 1;
                    self.owner = Some(requester);
                }
                self.preserved = true;
                self.readiness_blocked = outcome != Outcome::Accepted;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh<'a> {
    Expire { requester: u64, outcome: Outcome },
    Rotate { requester: u64, outcome: Outcome },
    Mismatch { observed: &'a str },
    Cancel,
    Simultaneous { requester: u64, other: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenewalError {
    Account,
    Owner,
    Cancelled,
}

impl fmt::Display for RenewalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Account => "renewal_account_rejected",
            Self::Owner => "renewal_owner_rejected",
            Self::Cancelled => "renewal_cancelled",
        })
    }
}

impl Error for RenewalError {}

fn account_ok(account: &str) -> bool {
    let mut chars = account.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_lowercase()
        && account.len() <= 32
        && chars.all(|char| {
            char.is_ascii_lowercase() || char.is_ascii_digit() || char == '_' || char == '-'
        })
}

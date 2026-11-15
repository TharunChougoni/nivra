//! Named phases of a paper book. The three persisted flags stay independent
//! booleans so sealed JSON keeps parsing; this type names every combination
//! and the transitions that combination may not take.

/// Control phase implied by `stopped`/`halted`/`closed` on a persisted book.
///
/// Every combination is representable because historic books can hold any
/// three-flag tuple. Carry of a simultaneously stopped and halted book is the
/// illegal *transition*, not an unreadable row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BookPhase {
    Live,
    Halted,
    Stopped,
    Closed,
    HaltedClosed,
    StoppedClosed,
    /// Persisted, but rollover carry refuses this combination.
    StoppedHalted,
    /// Persisted, but rollover carry refuses this combination.
    StoppedHaltedClosed,
}

impl BookPhase {
    pub const fn from_flags(stopped: bool, halted: bool, closed: bool) -> Self {
        match (stopped, halted, closed) {
            (false, false, false) => Self::Live,
            (false, true, false) => Self::Halted,
            (true, false, false) => Self::Stopped,
            (false, false, true) => Self::Closed,
            (false, true, true) => Self::HaltedClosed,
            (true, false, true) => Self::StoppedClosed,
            (true, true, false) => Self::StoppedHalted,
            (true, true, true) => Self::StoppedHaltedClosed,
        }
    }

    pub const fn stopped(self) -> bool {
        matches!(
            self,
            Self::Stopped | Self::StoppedClosed | Self::StoppedHalted | Self::StoppedHaltedClosed
        )
    }

    pub const fn halted(self) -> bool {
        matches!(
            self,
            Self::Halted | Self::HaltedClosed | Self::StoppedHalted | Self::StoppedHaltedClosed
        )
    }

    pub const fn closed(self) -> bool {
        matches!(
            self,
            Self::Closed | Self::HaltedClosed | Self::StoppedClosed | Self::StoppedHaltedClosed
        )
    }

    /// A book that is both stopped and halted cannot be carried forward.
    pub const fn carry_blocked(self) -> bool {
        matches!(self, Self::StoppedHalted | Self::StoppedHaltedClosed)
    }
}

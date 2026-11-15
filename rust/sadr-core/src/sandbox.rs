//! Dataset and import admission. A complete fixture is offline-only and does not spawn.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BundleCheck {
    pub warmup_bars: u32,
    pub required_warmup: u32,
    pub units_ok: bool,
    pub split_ok: bool,
    pub availability_ok: bool,
    pub hash_ok: bool,
    pub free_disk_bytes: u64,
    pub required_disk_bytes: u64,
    pub script_registered: bool,
    pub traverses: bool,
    pub symlink: bool,
    pub result_bytes: u64,
    pub max_result_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxDeny {
    Warmup,
    Units,
    Split,
    Availability,
    Hash,
    Disk,
    Script,
    Traversal,
    Symlink,
    Oversized,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxAdmission {
    reasons: Vec<SandboxDeny>,
}

impl SandboxAdmission {
    pub fn denied_before_spawn(&self) -> bool {
        !self.reasons.is_empty()
    }

    pub fn spawned(&self) -> bool {
        false
    }

    pub fn offline_admission(&self) -> bool {
        self.reasons.is_empty()
    }

    pub fn execution_authorized(&self) -> bool {
        false
    }

    pub fn reasons(&self) -> &[SandboxDeny] {
        &self.reasons
    }
}

pub fn admit(check: &BundleCheck) -> SandboxAdmission {
    let mut reasons = Vec::new();
    if check.warmup_bars < check.required_warmup {
        reasons.push(SandboxDeny::Warmup);
    }
    if !check.units_ok {
        reasons.push(SandboxDeny::Units);
    }
    if !check.split_ok {
        reasons.push(SandboxDeny::Split);
    }
    if !check.availability_ok {
        reasons.push(SandboxDeny::Availability);
    }
    if !check.hash_ok {
        reasons.push(SandboxDeny::Hash);
    }
    if check.free_disk_bytes < check.required_disk_bytes {
        reasons.push(SandboxDeny::Disk);
    }
    if !check.script_registered {
        reasons.push(SandboxDeny::Script);
    }
    if check.traverses {
        reasons.push(SandboxDeny::Traversal);
    }
    if check.symlink {
        reasons.push(SandboxDeny::Symlink);
    }
    if check.result_bytes > check.max_result_bytes {
        reasons.push(SandboxDeny::Oversized);
    }
    SandboxAdmission { reasons }
}

impl fmt::Display for SandboxDeny {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Warmup => "sandbox_warmup_missing",
            Self::Units => "sandbox_units_rejected",
            Self::Split => "sandbox_split_rejected",
            Self::Availability => "sandbox_availability_rejected",
            Self::Hash => "sandbox_hash_rejected",
            Self::Disk => "sandbox_disk_short",
            Self::Script => "sandbox_script_unregistered",
            Self::Traversal => "sandbox_traversal_rejected",
            Self::Symlink => "sandbox_symlink_rejected",
            Self::Oversized => "sandbox_result_oversized",
        })
    }
}

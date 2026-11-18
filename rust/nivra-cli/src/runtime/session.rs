//! Persistent orchestrator session: append-only framed log, operator notes and
//! per-turn action receipts.
//!
//! The log is one writer (the session orchestrator). Chat instances and the
//! dashboard only read it with [`read_frames`], which stops at the last complete
//! frame: a truncated tail never yields a partial record and never errors.
//!
//! Notes live in their own store, outside the compactable log. Every mutation
//! bumps the note-set version; a turn records the version it was built from and
//! its action commit is refused when the set changed (#143).
// The pi cutover left these modules read-only in production builds: every
// writer below is compiled for tests only, together with what it needs.

// ---------------------------------------------------------------------------
// Action receipts
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Operator notes
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Prompts, seeds and handoff
// ---------------------------------------------------------------------------

//! Bounded evidence for provider failures that happen before any provider turn.
//!
//! An outage can fail a digest turn every few seconds before the provider is
//! called (for example a catalog/auth failure). The original archive persisted
//! the full multi-megabyte request for every attempt, so one session grew by
//! gigabytes (#23). A pre-provider failure now keeps a bounded receipt
//! (reason, stage, sizes and hashes) and at most one representative request per
//! failure window. Original journals, books and failed artifacts are never
//! deleted: the first failure in a window keeps its full request and later
//! retries link to it.

//! Bounded, expiring, audited orchestrator directives.
//!
//! A directive is context for the next Luna or decide envelope. It never creates,
//! sizes or blocks an order. The execution path does not read this module.

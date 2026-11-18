//! Operator-selected orchestrator model binding.
//!
//! The reviewed planner policy names exact model/credential profiles and the
//! profile names it admits. A selection is a separate, versioned runtime setting
//! so a model-only switch never rewrites the sealed policy, merges role charges
//! or resets the persistent session. Requests are durable pending/active
//! revisions; the controller promotes a pending revision only at a turn boundary.

//! Preservation of unknown content payloads.
//!
//! Snapshots already carry their own stable content mapping in the state table, so no
//! separate registry encoding exists. What remains for this module is the contract rule that
//! unknown content keeps its identity, schema version, and raw payload across a save. World
//! format version 1 does not implement it: unknown content fails at restore time instead.

// Placeholder until the content system defines read-only handling of unknown payloads.

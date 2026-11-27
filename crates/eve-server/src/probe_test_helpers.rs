//! Test-only façade over active scanner probe runtime state (`#1541` Track H1a).
//!
//! Compiled under `#[cfg(any(test, feature = "testkit"))]` so both unit tests (`--lib`) and
//! integration tests (`--test it`) seed, mutate, and inspect active scanner probes through a
//! value-based seam without holding raw `ServerContext::active_scanner_probes` locks.

#![forbid(unsafe_code)]

use crate::router::{ActiveScannerProbe, ServerContext};
use std::collections::HashMap;

/// Inserts or replaces an [`ActiveScannerProbe`] entry keyed by `probe.probe_id`.
pub fn seed_probe(ctx: &ServerContext, probe: ActiveScannerProbe) {
    ctx.active_scanner_probes
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(probe.probe_id, probe);
}

/// Inserts or replaces a batch of [`ActiveScannerProbe`] entries keyed by each entry's `probe_id`.
pub fn seed_probes(ctx: &ServerContext, probes: impl IntoIterator<Item = ActiveScannerProbe>) {
    let mut map = ctx
        .active_scanner_probes
        .write()
        .unwrap_or_else(|e| e.into_inner());
    for probe in probes {
        map.insert(probe.probe_id, probe);
    }
}

/// Removes all tracked active scanner probes across all systems.
pub fn clear_probes(ctx: &ServerContext) {
    ctx.active_scanner_probes
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

/// Removes and returns the [`ActiveScannerProbe`] for `probe_id`, if present.
pub fn remove_probe(ctx: &ServerContext, probe_id: i64) -> Option<ActiveScannerProbe> {
    ctx.active_scanner_probes
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&probe_id)
}

/// Returns the total number of tracked active scanner probes.
pub fn probe_count(ctx: &ServerContext) -> usize {
    ctx.active_scanner_probes
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .len()
}

/// Returns `true` if `probe_id` is currently tracked as an active scanner probe.
pub fn contains_probe(ctx: &ServerContext, probe_id: i64) -> bool {
    ctx.active_scanner_probes
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(&probe_id)
}

/// Returns a cloned [`ActiveScannerProbe`] snapshot for `probe_id`, if present.
pub fn get_probe(ctx: &ServerContext, probe_id: i64) -> Option<ActiveScannerProbe> {
    ctx.active_scanner_probes
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(&probe_id)
        .cloned()
}

/// Returns a cloned snapshot of all tracked active scanner probes keyed by `probe_id`.
pub fn probes_snapshot(ctx: &ServerContext) -> HashMap<i64, ActiveScannerProbe> {
    ctx.active_scanner_probes
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Returns the sorted IDs of all tracked active scanner probes.
pub fn probe_ids(ctx: &ServerContext) -> Vec<i64> {
    let mut ids: Vec<i64> = ctx
        .active_scanner_probes
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .keys()
        .copied()
        .collect();
    ids.sort_unstable();
    ids
}

/// Mutates an existing [`ActiveScannerProbe`] in place if present, returning `true` if found.
pub fn mutate_probe(
    ctx: &ServerContext,
    probe_id: i64,
    f: impl FnOnce(&mut ActiveScannerProbe),
) -> bool {
    let mut map = ctx
        .active_scanner_probes
        .write()
        .unwrap_or_else(|e| e.into_inner());
    let Some(probe) = map.get_mut(&probe_id) else {
        return false;
    };
    f(probe);
    true
}

//! Persist-Reload Equivalence (PRE) check and world-state teardown invariants (`P2-0g`, `#908`).
//!
//! Provides:
//! 1. [`check_persist_reload_equivalence`] — reloads SQLite into a fresh [`WorldStateSnapshot`]
//!    and compares it against the live in-memory snapshot, ignoring transient runtime fields
//!    ([`TRANSIENT_SNAPSHOT_FIELDS`]), reporting the exact table and key of any mismatch.
//! 2. [`verify_dirty_hints_fast_path`] — verifies that syncing a snapshot with
//!    `dirty_hints.tracked = true` produces the exact same persisted SQLite state as a full scan.
//! 3. [`check_world_invariants`] — verifies teardown invariants across `WorldStateSnapshot`:
//!    - Every charge (`category_id == 8`) in a module slot flag (`11..=34`, `LoSlot0..=HiSlot7`)
//!      has a fitted non-charge module in the same `(location_id, flag)` (`#947` class guard).
//!    - Every item's `location_id` resolves to a valid container, owner, station, structure,
//!      solar system, celestial, or system location without self-parenting loops.
//! 4. Shrinking ratchet enforcement via [`WorldInvariantsRatchet`].

use eve_db::sqlite_store::{
    apply_migrations, load_snapshot_from_sqlite, sync_dirty_to_sqlite, TableFingerprints,
};
use eve_db::{SnapshotDirtyHints, WorldRepository, WorldStateSnapshot};
use eve_sde::SdeRegistry;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Transient `WorldStateSnapshot` fields excluded from Persist-Reload Equivalence (`#908`).
pub const TRANSIENT_SNAPSHOT_FIELDS: &[&str] = &[
    "sessions",
    "transient_item_attributes",
    "dirty_hints",
];

/// Module slot flag codes (`LoSlot0 = 11` through `HiSlot7 = 34`).
pub const MODULE_SLOT_FLAG_MIN: i32 = 11;
pub const MODULE_SLOT_FLAG_MAX: i32 = 34;

/// EVE charge category ID (`categoryID == 8`).
pub const CHARGE_CATEGORY_ID: u32 = 8;

/// A single table/key difference between a live snapshot and its SQLite reload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreDiffEntry {
    pub table: String,
    pub key: String,
    pub live_summary: String,
    pub reloaded_summary: String,
}

impl std::fmt::Display for PreDiffEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "PRE mismatch in table `{}` key `{}`: live={} vs reloaded={}",
            self.table, self.key, self.live_summary, self.reloaded_summary
        )
    }
}

/// World-state teardown invariant violation (`#908`, `#947`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum WorldInvariantViolation {
    /// A charge (`category_id == 8`) occupies a module slot (`flag` in `11..=34`) with no fitted
    /// non-charge module at the same `(location_id, flag)`.
    StrandedChargeInModuleSlot {
        charge_item_id: i64,
        charge_type_id: u32,
        location_id: i64,
        flag_code: i32,
    },
    /// An item's `location_id` is non-positive, self-referential, or does not resolve to any known
    /// world entity, station, solar system, or celestial.
    UnresolvedItemLocation {
        item_id: i64,
        type_id: u32,
        location_id: i64,
        reason: String,
    },
}

impl WorldInvariantViolation {
    pub fn ratchet_id(&self) -> String {
        match self {
            Self::StrandedChargeInModuleSlot {
                charge_item_id,
                charge_type_id,
                location_id,
                flag_code,
            } => format!(
                "StrandedChargeInModuleSlot:item={charge_item_id}:type={charge_type_id}:loc={location_id}:flag={flag_code}"
            ),
            Self::UnresolvedItemLocation {
                item_id,
                type_id,
                location_id,
                reason,
            } => format!(
                "UnresolvedItemLocation:item={item_id}:type={type_id}:loc={location_id}:{reason}"
            ),
        }
    }
}

/// Shrinking ratchet for pre-existing PRE diffs and world-invariant violations (`#908`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorldInvariantsRatchet {
    pub version: u32,
    pub allowed_pre_diff_tables: BTreeSet<String>,
    pub allowed_invariant_violations: BTreeSet<String>,
}

impl WorldInvariantsRatchet {
    pub fn default_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("ratchets")
            .join("world_invariants_ratchet.json")
    }

    pub fn load_default() -> Result<Self, String> {
        Self::load_from_path(&Self::default_path())
    }

    pub fn load_from_path(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        serde_json::from_str(&raw)
            .map_err(|e| format!("failed to parse {}: {e}", path.display()))
    }
}

/// Returns `true` when `EVOMU_PRE_CHECK=1` (or `true`) is set in the environment.
pub fn pre_check_enabled() -> bool {
    std::env::var("EVOMU_PRE_CHECK")
        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

fn compact_value_summary(v: Option<&Value>) -> String {
    match v {
        None => "<missing>".to_string(),
        Some(val) => {
            let s = val.to_string();
            if s.len() > 160 {
                format!("{}...", &s[..160])
            } else {
                s
            }
        }
    }
}

/// Compares a live [`WorldStateSnapshot`] against a SQLite-reloaded snapshot, ignoring
/// [`TRANSIENT_SNAPSHOT_FIELDS`], and returns every differing `(table, key)` entry.
pub fn diff_snapshots_ignore_transient(
    live: &WorldStateSnapshot,
    reloaded: &WorldStateSnapshot,
) -> Vec<PreDiffEntry> {
    let mut live_norm = live.clone();
    live_norm.reconcile_id_counters();
    let mut reloaded_norm = reloaded.clone();
    reloaded_norm.reconcile_id_counters();
    let live_val = serde_json::to_value(&live_norm).expect("serialize live WorldStateSnapshot");
    let reloaded_val =
        serde_json::to_value(&reloaded_norm).expect("serialize reloaded WorldStateSnapshot");

    let (Value::Object(live_map), Value::Object(reloaded_map)) = (live_val, reloaded_val) else {
        return Vec::new();
    };

    let mut all_tables = BTreeSet::new();
    all_tables.extend(live_map.keys().cloned());
    all_tables.extend(reloaded_map.keys().cloned());

    let mut diffs = Vec::new();
    for table in all_tables {
        if TRANSIENT_SNAPSHOT_FIELDS.contains(&table.as_str()) {
            continue;
        }
        let lv = live_map.get(&table);
        let rv = reloaded_map.get(&table);
        if lv == rv {
            continue;
        }
        match (lv, rv) {
            (Some(Value::Object(l_obj)), Some(Value::Object(r_obj))) => {
                let mut keys = BTreeSet::new();
                keys.extend(l_obj.keys().cloned());
                keys.extend(r_obj.keys().cloned());
                for k in keys {
                    if l_obj.get(&k) != r_obj.get(&k) {
                        diffs.push(PreDiffEntry {
                            table: table.clone(),
                            key: k.clone(),
                            live_summary: compact_value_summary(l_obj.get(&k)),
                            reloaded_summary: compact_value_summary(r_obj.get(&k)),
                        });
                    }
                }
            }
            (Some(Value::Array(l_arr)), Some(Value::Array(r_arr))) => {
                let max_len = l_arr.len().max(r_arr.len());
                for idx in 0..max_len {
                    if l_arr.get(idx) != r_arr.get(idx) {
                        diffs.push(PreDiffEntry {
                            table: table.clone(),
                            key: format!("[{idx}]"),
                            live_summary: compact_value_summary(l_arr.get(idx)),
                            reloaded_summary: compact_value_summary(r_arr.get(idx)),
                        });
                        break;
                    }
                }
            }
            _ => {
                diffs.push(PreDiffEntry {
                    table,
                    key: "$".to_string(),
                    live_summary: compact_value_summary(lv),
                    reloaded_summary: compact_value_summary(rv),
                });
            }
        }
    }
    diffs
}

fn sync_to_conn(conn: &mut rusqlite::Connection, snap: &WorldStateSnapshot, fp: &mut TableFingerprints) -> Result<(), String> {
    let tx = conn.transaction().map_err(|e| format!("begin sqlite tx: {e}"))?;
    sync_dirty_to_sqlite(&tx, snap, fp, false)?;
    tx.commit().map_err(|e| format!("commit sqlite tx: {e}"))
}

/// Executes the Persist-Reload Equivalence (PRE) check on `repo`:
/// Reloads the persisted [`WorldStateSnapshot`] directly from `repo`'s SQLite database and
/// compares it against the live in-memory snapshot, catching both unpersisted in-memory mutations
/// and round-trip persistence drift.
pub fn check_persist_reload_equivalence(
    repo: &WorldRepository,
) -> Result<Vec<PreDiffEntry>, String> {
    let live = repo.with_snapshot(|s| s.clone());
    let reloaded = repo.reload_snapshot_from_sqlite()?;
    Ok(diff_snapshots_ignore_transient(&live, &reloaded))
}

/// Verifies that syncing `snap` with `dirty_hints` produces the exact same SQLite snapshot as a
/// full unhinted scan (`dirty_hints.tracked = false`).
pub fn verify_dirty_hints_fast_path(
    snap: &WorldStateSnapshot,
) -> Result<Vec<PreDiffEntry>, String> {
    let mut conn_fast = rusqlite::Connection::open_in_memory()
        .map_err(|e| format!("open fast-path sqlite: {e}"))?;
    apply_migrations(&conn_fast)?;
    let mut fp_fast = TableFingerprints::default();
    let mut seeded_base = snap.clone();
    seeded_base.dirty_hints = SnapshotDirtyHints::default();
    sync_to_conn(&mut conn_fast, &seeded_base, &mut fp_fast)?;
    sync_to_conn(&mut conn_fast, snap, &mut fp_fast)?;
    let (reloaded_fast, _) = load_snapshot_from_sqlite(&conn_fast)?;

    let mut conn_full = rusqlite::Connection::open_in_memory()
        .map_err(|e| format!("open full-scan sqlite: {e}"))?;
    apply_migrations(&conn_full)?;
    let mut fp_full = TableFingerprints::default();
    let mut full_snap = snap.clone();
    full_snap.dirty_hints = SnapshotDirtyHints::default();
    sync_to_conn(&mut conn_full, &full_snap, &mut fp_full)?;
    let (reloaded_full, _) = load_snapshot_from_sqlite(&conn_full)?;

    Ok(diff_snapshots_ignore_transient(&reloaded_fast, &reloaded_full))
}

fn item_category_id(type_id: u32, sde: Option<&SdeRegistry>, fallback_sde: &SdeRegistry) -> Option<u32> {
    if let Some(reg) = sde {
        if let Some(t) = reg.types.get(&type_id) {
            return Some(t.category_id);
        }
    }
    fallback_sde.types.get(&type_id).map(|t| t.category_id)
}

fn location_resolves(
    location_id: i64,
    snap: &WorldStateSnapshot,
    sde: Option<&SdeRegistry>,
) -> bool {
    if location_id <= 0 {
        return false;
    }
    if snap.items.contains_key(&location_id)
        || snap.characters.contains_key(&location_id)
        || snap.corporations.contains_key(&location_id)
        || snap.structures.contains_key(&location_id)
    {
        return true;
    }
    if let Some(reg) = sde {
        if reg.stations.contains_key(&location_id)
            || (u32::try_from(location_id).is_ok_and(|id| reg.solar_systems.contains_key(&id)))
        {
            return true;
        }
    }
    // Well-known EVE ID ranges:
    // - 1..=10_000: system / junkyard / asset safety (2004) / universe roots
    // - 1_000_000..=2_000_000: NPC corporations
    // - 3_000_000..=4_000_000: NPC agents
    // - 10_000_000..=34_000_000: regions, constellations, solar systems
    // - 40_000_000..=55_000_000: celestials (planets, moons, stargates, asteroid belts)
    // - 60_000_000..=64_000_000: NPC and conquerable stations
    // - 66_000_000..=68_000_000: station office folders
    matches!(
        location_id,
        1..=10_000
            | 1_000_000..=2_000_000
            | 3_000_000..=4_000_000
            | 10_000_000..=34_000_000
            | 40_000_000..=55_000_000
            | 60_000_000..=64_000_000
            | 66_000_000..=68_000_000
    )
}

/// Checks world-state teardown invariants (`#908`, `#947`):
/// 1. Every charge (`category_id == 8`) in a module slot flag (`11..=34`, `LoSlot0..=HiSlot7`)
///    requires a fitted non-charge module (`category_id != 8`) at the same `(location_id, flag)`.
/// 2. Every item's `location_id` resolves to a valid container, owner, station, structure,
///    solar system, or celestial without self-loops.
pub fn check_world_invariants(
    snap: &WorldStateSnapshot,
    sde: Option<&SdeRegistry>,
) -> Vec<WorldInvariantViolation> {
    let fallback_sde = SdeRegistry::with_canonical_seed();
    let mut violations = Vec::new();

    for item in snap.items.values() {
        if item.location_id == item.item_id {
            violations.push(WorldInvariantViolation::UnresolvedItemLocation {
                item_id: item.item_id,
                type_id: item.type_id,
                location_id: item.location_id,
                reason: "item location_id is self-referential".to_string(),
            });
        } else if let Some(parent) = snap.items.get(&item.location_id) {
            if parent.location_id == item.item_id {
                violations.push(WorldInvariantViolation::UnresolvedItemLocation {
                    item_id: item.item_id,
                    type_id: item.type_id,
                    location_id: item.location_id,
                    reason: format!(
                        "2-cycle between item {} and parent {}",
                        item.item_id, parent.item_id
                    ),
                });
            }
        } else if !location_resolves(item.location_id, snap, sde) {
            violations.push(WorldInvariantViolation::UnresolvedItemLocation {
                item_id: item.item_id,
                type_id: item.type_id,
                location_id: item.location_id,
                reason: "location_id does not resolve to any known entity or location".to_string(),
            });
        }

        let flag_code = item.flag as i32;
        if (MODULE_SLOT_FLAG_MIN..=MODULE_SLOT_FLAG_MAX).contains(&flag_code)
            && item_category_id(item.type_id, sde, &fallback_sde) == Some(CHARGE_CATEGORY_ID)
        {
            let has_fitted_module = snap.items.values().any(|other| {
                other.item_id != item.item_id
                    && other.location_id == item.location_id
                    && other.flag == item.flag
                    && item_category_id(other.type_id, sde, &fallback_sde)
                        != Some(CHARGE_CATEGORY_ID)
            });
            if !has_fitted_module {
                violations.push(WorldInvariantViolation::StrandedChargeInModuleSlot {
                    charge_item_id: item.item_id,
                    charge_type_id: item.type_id,
                    location_id: item.location_id,
                    flag_code,
                });
            }
        }
    }

    violations.sort();
    violations.dedup();
    violations
}

/// Runs world-invariant checks and (when `force_pre` or `EVOMU_PRE_CHECK=1` is active)
/// Persist-Reload Equivalence and `SnapshotDirtyHints` fast-path checks, gating against
/// [`WorldInvariantsRatchet`].
pub fn assert_world_and_pre_guards(
    repo: &WorldRepository,
    sde: Option<&SdeRegistry>,
    force_pre: bool,
) {
    let ratchet = WorldInvariantsRatchet::load_default().unwrap_or_default();
    let snap = repo.with_snapshot(|s| s.clone());

    let inv_violations = check_world_invariants(&snap, sde);
    let unexpected_inv: Vec<_> = inv_violations
        .iter()
        .filter(|v| !ratchet.allowed_invariant_violations.contains(&v.ratchet_id()))
        .collect();
    assert!(
        unexpected_inv.is_empty(),
        "World-state teardown invariant violations (not in ratchet): {unexpected_inv:#?}"
    );

    if force_pre || pre_check_enabled() {
        let diffs = check_persist_reload_equivalence(repo)
            .expect("Persist-Reload Equivalence (PRE) SQLite round-trip must succeed");
        let unexpected_diffs: Vec<_> = diffs
            .iter()
            .filter(|d| !ratchet.allowed_pre_diff_tables.contains(&d.table))
            .collect();
        assert!(
            unexpected_diffs.is_empty(),
            "Persist-Reload Equivalence (PRE) check failed — first differing table/key: {} (total unexpected diffs: {unexpected_diffs:#?})",
            unexpected_diffs[0]
        );

        let fast_diffs = verify_dirty_hints_fast_path(&snap)
            .expect("SnapshotDirtyHints fast-path verification must succeed");
        let unexpected_fast: Vec<_> = fast_diffs
            .iter()
            .filter(|d| !ratchet.allowed_pre_diff_tables.contains(&d.table))
            .collect();
        assert!(
            unexpected_fast.is_empty(),
            "SnapshotDirtyHints fast-path diverged from full SQLite scan: {unexpected_fast:#?}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eve_db::{InventoryFlag, InventoryItemRecord};

    #[test]
    fn pre_check_catches_unpersisted_mutation_and_identifies_table_and_key() {
        let repo = WorldRepository::with_demo_seed();
        let clean_diffs = check_persist_reload_equivalence(&repo).expect("clean PRE check");
        assert!(
            clean_diffs.is_empty(),
            "Fresh SQLite-backed WorldRepository must have 0 PRE diffs, got {clean_diffs:#?}"
        );

        // Inject an unpersisted mutation directly into in-memory state bypassing SQLite sync:
        repo.inject_unpersisted_mutation_for_test(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_001) {
                ch.security_status = 9.99;
            }
        });

        let diffs = check_persist_reload_equivalence(&repo).expect("PRE check after mutation");
        assert!(
            diffs.iter().any(|d| d.table == "characters" && d.key == "90000001"),
            "PRE check must report exact differing table `characters` and key `90000001`, got {diffs:#?}"
        );
    }

    #[test]
    fn dirty_hints_fast_path_matches_full_sqlite_scan() {
        let repo = WorldRepository::with_demo_seed();
        repo.with_mut_snapshot(|snap| {
            snap.dirty_hints.tracked = true;
            snap.dirty_hints.touched_characters.insert(90_000_001);
            if let Some(ch) = snap.characters.get_mut(&90_000_001) {
                ch.free_skill_points += 1234.0;
            }
        });
        let snap = repo.with_snapshot(|s| s.clone());
        let diffs = verify_dirty_hints_fast_path(&snap).expect("verify_dirty_hints_fast_path");
        assert!(
            diffs.is_empty(),
            "SnapshotDirtyHints fast-path must match full SQLite scan, got {diffs:#?}"
        );
    }

    #[test]
    fn world_invariants_reject_stranded_charge_in_module_slot_947() {
        let repo = WorldRepository::with_demo_seed();
        let clean_snap = repo.with_snapshot(|s| s.clone());
        let initial_violations = check_world_invariants(&clean_snap, None);
        assert!(
            initial_violations.is_empty(),
            "Demo seed must satisfy all world invariants, got {initial_violations:#?}"
        );

        // Insert a charge (type 195 = Fusion S, category 8) in HiSlot0 (flag 27) on ship 100_000_001
        // without any non-charge module fitted in HiSlot0 (#947 class bug):
        let mut bad_snap = clean_snap.clone();
        bad_snap.items.retain(|_, it| !(it.location_id == 100_000_001 && it.flag == InventoryFlag::HiSlot0));
        bad_snap.items.insert(
            999_888_777,
            InventoryItemRecord {
                item_id: 999_888_777,
                type_id: 195,
                owner_id: 90_000_001,
                location_id: 100_000_001,
                flag: InventoryFlag::HiSlot0,
                quantity: 40,
                singleton: false,
                ..Default::default()
            },
        );
        let violations = check_world_invariants(&bad_snap, None);
        assert!(
            violations.contains(&WorldInvariantViolation::StrandedChargeInModuleSlot {
                charge_item_id: 999_888_777,
                charge_type_id: 195,
                location_id: 100_000_001,
                flag_code: 27,
            }),
            "Stranded charge in HiSlot0 without fitted module must be rejected (#947), got {violations:#?}"
        );
    }

    #[test]
    fn world_invariants_reject_unresolved_or_self_loop_item_location() {
        let repo = WorldRepository::with_demo_seed();
        let mut snap = repo.with_snapshot(|s| s.clone());
        snap.items.insert(
            999_888_778,
            InventoryItemRecord {
                item_id: 999_888_778,
                type_id: 34,
                owner_id: 90_000_001,
                location_id: 999_888_778, // self-loop!
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            999_888_779,
            InventoryItemRecord {
                item_id: 999_888_779,
                type_id: 34,
                owner_id: 90_000_001,
                location_id: 888_777_666_555, // non-existent container!
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                ..Default::default()
            },
        );
        let violations = check_world_invariants(&snap, None);
        assert!(
            violations.iter().any(|v| matches!(
                v,
                WorldInvariantViolation::UnresolvedItemLocation { item_id: 999_888_778, .. }
            )),
            "Self-referential item location_id must be rejected: {violations:#?}"
        );
        assert!(
            violations.iter().any(|v| matches!(
                v,
                WorldInvariantViolation::UnresolvedItemLocation { item_id: 999_888_779, .. }
            )),
            "Unresolved item location_id must be rejected: {violations:#?}"
        );
    }
}

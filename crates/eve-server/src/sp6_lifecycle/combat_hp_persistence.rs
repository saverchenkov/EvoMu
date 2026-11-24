//! Ship combat HP, capacitor, and item damage persistence helpers (`#585`, `#846`, `#917`, `#1170`).

#![forbid(unsafe_code)]

use super::{
    is_ship_stale_for_in_space_write_in_snap, sync_ship_item_damage_attrs_in_snap,
    PersistedCombatHp,
};
use eve_db::WorldRepository;
use eve_destiny::CombatState;
use std::sync::Arc;

/// Persists a ship ball's shield/armor/hull HP into `snap.sp6_lifecycle.ship_combat_hp` so damage
/// taken while disconnected or before a server restart is preserved upon reconnect (`plan:SP-6`).
pub fn persist_ship_combat_hp_in_db(
    db: &Arc<WorldRepository>,
    ball_id: i64,
    cs: &CombatState,
) {
    persist_ship_combat_hp_batch_in_db(db, [(ball_id, cs)]);
}

/// Persists multiple ship combat HPs into `snap.sp6_lifecycle.ship_combat_hp` AND mirrors hull/armor
/// deficits into `snap.items[ship_id].dynamic_attributes[DAMAGE | ARMOR_DAMAGE]` in a single mutation,
/// keeping all three ship HP representations synchronized (`#585`), and skipping snapshot write if unchanged.
pub fn persist_ship_combat_hp_batch_in_db<'a, I>(
    db: &Arc<WorldRepository>,
    updates: I,
) where
    I: IntoIterator<Item = (i64, &'a CombatState)>,
{
    persist_ship_combat_hp_batch_inner(db, updates, false);
}

/// Persists combat HPs only for balls backed by persisted `snap.items` or `snap.structures`,
/// preventing ephemeral NPC combat HP updates from triggering per-tick SQLite WAL writes.
pub fn persist_existing_ship_combat_hp_batch_in_db<'a, I>(
    db: &Arc<WorldRepository>,
    updates: I,
) where
    I: IntoIterator<Item = (i64, &'a CombatState)>,
{
    persist_ship_combat_hp_batch_inner(db, updates, true);
}

/// Persists combat HPs and capacitor for balls in `solar_system_id` backed by persisted entities,
/// refusing to overwrite ships that have already docked or jumped out of `solar_system_id` (`#846`, `#917` T-2).
pub fn persist_existing_ship_combat_hp_for_system_in_db<'a, I>(
    db: &Arc<WorldRepository>,
    solar_system_id: u32,
    updates: I,
) where
    I: IntoIterator<Item = (i64, &'a CombatState)>,
{
    persist_ship_combat_hp_batch_inner_for_system(db, Some(solar_system_id), updates, true);
}

pub(super) fn persist_ship_combat_hp_batch_inner<'a, I>(
    db: &Arc<WorldRepository>,
    updates: I,
    require_persisted_entity: bool,
) where
    I: IntoIterator<Item = (i64, &'a CombatState)>,
{
    persist_ship_combat_hp_batch_inner_for_system(db, None, updates, require_persisted_entity);
}

struct PreparedShipCombatHpWrite {
    ball_id: i64,
    hp: PersistedCombatHp,
    want_meta: Option<PersistedCombatHp>,
    meta_changed: bool,
    armor_dmg: f64,
    hull_dmg: f64,
}

fn prepare_ship_combat_hp_write_in_snap(
    snap: &eve_db::WorldStateSnapshot,
    bid: i64,
    cs: &CombatState,
    expected_system: Option<u32>,
    require_persisted_entity: bool,
) -> Option<PreparedShipCombatHpWrite> {
    if require_persisted_entity
        && !snap.items.contains_key(&bid)
        && !snap.structures.contains_key(&bid)
    {
        return None;
    }
    if is_ship_stale_for_in_space_write_in_snap(snap, bid, expected_system, None) {
        return None;
    }
    let new_hp = PersistedCombatHp {
        shield_hp: cs.shield_hp.max(0.0),
        armor_hp: cs.armor_hp.max(0.0),
        hull_hp: cs.structure_hp.max(0.0),
    };
    let cap_encoded = if bid > 0
        && cs.max_capacitor > 0.0
        && (cs.max_capacitor - cs.capacitor.max(0.0)) > 1e-6
    {
        cs.capacitor.clamp(0.0, cs.max_capacitor) + 1.0
    } else {
        0.0
    };
    let existing_meta =
        (bid > 0).then(|| snap.sp6_lifecycle.ship_combat_hp.get(&(-bid)).copied()).flatten();
    let prev_sys = existing_meta.map(|m| m.armor_hp).unwrap_or(0.0);
    let prev_epoch = existing_meta.map(|m| m.hull_hp).unwrap_or(0.0);
    let want_meta = if bid > 0 && (cap_encoded > 0.0 || prev_sys != 0.0 || prev_epoch != 0.0) {
        Some(PersistedCombatHp {
            shield_hp: cap_encoded,
            armor_hp: prev_sys,
            hull_hp: prev_epoch,
        })
    } else {
        None
    };
    let meta_changed = match (existing_meta, want_meta) {
        (None, None) => false,
        (Some(a), Some(b)) => {
            (a.shield_hp - b.shield_hp).abs() > 1e-6
                || a.armor_hp != b.armor_hp
                || a.hull_hp != b.hull_hp
        }
        _ => true,
    };
    let armor_dmg = if cs.max_armor_hp > 0.0 {
        (cs.max_armor_hp - cs.armor_hp).clamp(0.0, cs.max_armor_hp)
    } else {
        0.0
    };
    let hull_dmg = if cs.max_structure_hp > 0.0 {
        (cs.max_structure_hp - cs.structure_hp).clamp(0.0, cs.max_structure_hp)
    } else {
        0.0
    };
    let hp_changed = snap.sp6_lifecycle.ship_combat_hp.get(&bid) != Some(&new_hp);
    let item_changed = snap.items.get(&bid).is_some_and(|it| {
        let cur_a = it
            .dynamic_attributes
            .get(&eve_sde::attr_ids::ARMOR_DAMAGE)
            .copied();
        let cur_h = it
            .dynamic_attributes
            .get(&eve_sde::attr_ids::DAMAGE)
            .copied();
        let want_a = (armor_dmg > 1e-6).then_some(armor_dmg);
        let want_h = (hull_dmg > 1e-6).then_some(hull_dmg);
        let diff = |x: Option<f64>, y: Option<f64>| match (x, y) {
            (None, None) => false,
            (Some(a), Some(b)) => (a - b).abs() > 1e-6,
            _ => true,
        };
        diff(cur_a, want_a) || diff(cur_h, want_h)
    });
    if hp_changed || meta_changed || item_changed {
        Some(PreparedShipCombatHpWrite {
            ball_id: bid,
            hp: new_hp,
            want_meta,
            meta_changed,
            armor_dmg,
            hull_dmg,
        })
    } else {
        None
    }
}

fn apply_prepared_ship_combat_hp_write_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    write: PreparedShipCombatHpWrite,
    expected_system: Option<u32>,
) {
    if is_ship_stale_for_in_space_write_in_snap(snap, write.ball_id, expected_system, None) {
        return;
    }
    snap.sp6_lifecycle.ship_combat_hp.insert(write.ball_id, write.hp);
    if write.ball_id > 0 && write.meta_changed {
        if let Some(m) = write.want_meta {
            snap.sp6_lifecycle.ship_combat_hp.insert(-write.ball_id, m);
        } else {
            snap.sp6_lifecycle.ship_combat_hp.remove(&(-write.ball_id));
        }
    }
    sync_ship_item_damage_attrs_in_snap(snap, write.ball_id, write.armor_dmg, write.hull_dmg);
}

/// Returns `true` when persisting `cs` for `bid` would modify `snap` (`#1170`).
pub(crate) fn has_ship_combat_hp_change_in_snap(
    snap: &eve_db::WorldStateSnapshot,
    bid: i64,
    cs: &CombatState,
    expected_system: Option<u32>,
    require_persisted_entity: bool,
) -> bool {
    prepare_ship_combat_hp_write_in_snap(snap, bid, cs, expected_system, require_persisted_entity)
        .is_some()
}

/// Applies a single `CombatState` HP/capacitor update inside an active `WorldRepository::tx` (`#1170`).
pub(crate) fn apply_ship_combat_hp_update_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    bid: i64,
    cs: &CombatState,
    expected_system: Option<u32>,
    require_persisted_entity: bool,
) {
    if let Some(write) =
        prepare_ship_combat_hp_write_in_snap(snap, bid, cs, expected_system, require_persisted_entity)
    {
        apply_prepared_ship_combat_hp_write_in_snap(snap, write, expected_system);
    }
}

fn persist_ship_combat_hp_batch_inner_for_system<'a, I>(
    db: &Arc<WorldRepository>,
    expected_system: Option<u32>,
    updates: I,
    require_persisted_entity: bool,
) where
    I: IntoIterator<Item = (i64, &'a CombatState)>,
{
    let to_update: Vec<PreparedShipCombatHpWrite> = db.with_snapshot(|snap| {
        updates
            .into_iter()
            .filter_map(|(bid, cs)| {
                prepare_ship_combat_hp_write_in_snap(
                    snap,
                    bid,
                    cs,
                    expected_system,
                    require_persisted_entity,
                )
            })
            .collect()
    });

    if to_update.is_empty() {
        return;
    }

    let _ = db.try_with_mut_snapshot(|snap| -> Result<(), String> {
        for write in to_update {
            apply_prepared_ship_combat_hp_write_in_snap(snap, write, expected_system);
        }
        Ok(())
    });
}

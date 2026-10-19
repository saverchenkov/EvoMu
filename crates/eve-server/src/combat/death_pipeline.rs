//! `death_pipeline` — Canonical ship destruction, wreck spawning, 50% loot drop, pod ejection, insurance payout, and killmail generation (`C-3`).

#![forbid(unsafe_code)]

use crate::formulas::compute_insurance_quote;
use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, KillmailRecord, WalletJournalEntry};
use eve_destiny::{Ball, MovementMode};
use glam::DVec3;
use std::collections::HashMap;

/// Result summary of the completed death pipeline execution.
#[derive(Debug, Clone)]
pub struct DeathPipelineResult {
    pub wreck_item_id: i64,
    pub wreck_type_id: u32,
    pub loot_dropped_item_ids: Vec<i64>,
    pub loot_destroyed_item_ids: Vec<i64>,
    pub ejected_capsule_id: Option<i64>,
    pub insurance_payout_isk: f64,
    pub killmail_id: i64,
}

/// Executes the full EVE death pipeline when a ship's structure HP reaches 0:
/// 1. SDE wreck type resolution (`ctx.sde.wrecks` or fallback 26557).
/// 2. 50% loot drop / destroyed roll for all fitted and cargo items.
/// 3. Capsule/pod ejection with session ship update (or clone respawn if pod was destroyed).
/// 4. Insurance payout calculation and wallet balance crediting.
/// 5. Killmail generation and persistence.
/// 6. Physical wreck and capsule ball spawning in the solar system Ballpark.
pub async fn execute_death_pipeline(
    ctx: &ServerContext,
    sys_id: u32,
    victim_char_id: i64,
    victim_ship_id: i64,
    attacker_char_id: i64,
    victim_pos: DVec3,
    rng_seed: u64,
) -> Result<DeathPipelineResult, String> {
    ctx.invalidate_ship_fitting(victim_ship_id);

    let (victim_ship_type_id, base_price, insured_payout) = ctx.db.with_snapshot(|snap| {
        let ship = snap
            .items
            .get(&victim_ship_id)
            .ok_or_else(|| format!("Ship {victim_ship_id} not found in world repository"))?;
        let base = ctx
            .sde
            .types
            .get(&ship.type_id)
            .map(|t| t.base_price)
            .unwrap_or(230_000.0);
        Ok::<_, String>((ship.type_id, base, ship.insured_payout_isk))
    })?;

    let is_pod = victim_ship_type_id == 670;
    let wreck_type_id = if is_pod {
        // Pods leave no standard ship wreck (or corpse/biomass)
        26557
    } else {
        ctx.sde
            .wrecks
            .get(&victim_ship_type_id)
            .copied()
            .unwrap_or(26557)
    };

    let calculated_insurance_payout = if is_pod {
        0.0
    } else if insured_payout > 0.0 {
        insured_payout
    } else {
        // Basic insurance covers 40% of base hull price
        compute_insurance_quote(base_price, 0.40).1
    };

    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // Transactionally execute DB item transformations, loot rolls, insurance, and killmail
    let (
        wreck_item_id,
        dropped_ids,
        destroyed_ids,
        ejected_cap_id,
        killmail_id,
    ) = ctx.db.try_with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let wreck_id = snap.next_entity_id;

        // Create Wreck item in space
        snap.items.insert(
            wreck_id,
            InventoryItemRecord {
                item_id: wreck_id,
                type_id: wreck_type_id,
                owner_id: victim_char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );

        // Find all items contained inside or fitted to the victim ship
        let mut child_item_ids: Vec<i64> = snap
            .items
            .values()
            .filter(|i| i.location_id == victim_ship_id)
            .map(|i| i.item_id)
            .collect();
        child_item_ids.sort_unstable();

        let mut dropped = Vec::new();
        let mut destroyed = Vec::new();

        for iid in child_item_ids {
            // Deterministic 50% Loot Fairy roll: roll < 0.5 drops, >= 0.5 destroyed
            let roll = ((rng_seed ^ (iid as u64)).wrapping_mul(6364136223846793005) >> 32)
                as f64
                / 4294967296.0;
            if roll < 0.50 {
                // Item survives and is moved into the wreck
                if let Some(item) = snap.items.get_mut(&iid) {
                    item.location_id = wreck_id;
                    item.flag = InventoryFlag::Cargo;
                    item.version += 1;
                    dropped.push(iid);
                }
            } else {
                // Item destroyed
                snap.items.remove(&iid);
                destroyed.push(iid);
            }
        }

        // Delete the destroyed ship item
        snap.items.remove(&victim_ship_id);

        // Capsule / Pod handling
        let cap_id = if !is_pod {
            snap.next_entity_id += 1;
            let cid = snap.next_entity_id;
            snap.items.insert(
                cid,
                InventoryItemRecord {
                    item_id: cid,
                    type_id: 670,
                    owner_id: victim_char_id,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
            if let Some(vic) = snap.characters.get_mut(&victim_char_id) {
                vic.active_ship_id = cid;
                vic.version += 1;
                vic.system_epoch += 1;
            }
            Some(cid)
        } else {
            None
        };

        // Credit insurance payout to victim
        if calculated_insurance_payout > 0.0 {
            let payout_cents = IskCents::from_f64_isk(calculated_insurance_payout);
            if let Some(vic) = snap.characters.get_mut(&victim_char_id) {
                vic.balance = vic.balance.checked_add(payout_cents).unwrap_or(vic.balance);
            }
            snap.next_entity_id += 1;
            let jid = snap.next_entity_id;
            snap.wallet_journal.push(WalletJournalEntry {
                entry_id: jid,
                character_id: victim_char_id,
                entry_type: "insurance_payout".to_string(),
                amount: payout_cents,
                balance_after: snap
                    .characters
                    .get(&victim_char_id)
                    .map(|c| c.balance)
                    .unwrap_or(payout_cents),
                reason: format!("Insurance payout for loss of ship {victim_ship_id}"),
            });
        }

        // Create and persist Killmail
        snap.next_entity_id += 1;
        let kid = snap.next_entity_id;
        snap.killmails.insert(
            kid,
            KillmailRecord {
                kill_id: kid,
                victim_character_id: victim_char_id,
                attacker_character_id: attacker_char_id,
                solar_system_id: sys_id,
                ship_type_id: victim_ship_type_id,
                total_value_isk: base_price,
            },
        );

        Ok::<_, String>((wreck_id, dropped, destroyed, cap_id, kid))
    })?;

    // If pod was destroyed, respawn pilot at medical clone station
    if is_pod {
        let _ = ctx
            .db
            .respawn_character_at_home_station(victim_char_id, victim_ship_id, &ctx.sde);
    }

    // Remove destroyed victim ship ball from space
    let _ = sys_handle.remove_ball(victim_ship_id).await;

    // Spawn physical Wreck Ball in Ballpark
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: wreck_item_id,
                type_id: wreck_type_id,
                position: victim_pos,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 10_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            eve_destiny::CombatState {
                shield_hp: 0.0,
                max_shield_hp: 0.0,
                armor_hp: 0.0,
                max_armor_hp: 0.0,
                structure_hp: 500.0,
                max_structure_hp: 500.0,
                ..eve_destiny::CombatState::default()
            },
        )
        .await;

    // Spawn Capsule Ball if pilot ejected
    if let Some(cid) = ejected_cap_id {
        let _ = sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id: cid,
                    type_id: 670,
                    position: victim_pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 335.0,
                    inertia: 0.25,
                    mass: 16_000.0,
                    radius: 15.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                ctx.build_type_combat_state(670),
            )
            .await;
    }

    Ok(DeathPipelineResult {
        wreck_item_id,
        wreck_type_id,
        loot_dropped_item_ids: dropped_ids,
        loot_destroyed_item_ids: destroyed_ids,
        ejected_capsule_id: ejected_cap_id,
        insurance_payout_isk: calculated_insurance_payout,
        killmail_id,
    })
}

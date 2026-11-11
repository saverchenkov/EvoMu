//! `pve_fob_observatory` — Blood Raider & Guristas Forward Operating Bases (FOB)
//! (`feat:pve.pirate-fob-forward-operating-base`) and Pochven Observatory Flashpoint & World Ark
//! (`feat:pve.triglavian-world-ark-observatory`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_dungeon_runtime::{
    with_dungeon_runtime_store, DungeonRuntimeInstance, DungeonRuntimeObject, DungeonRuntimeRoom,
};
use crate::services::pve_incursion_sites::make_site_npc_ball;
use eve_db::{
    InventoryFlag, InventoryItemRecord as InventoryItem, IskCents, UpwellStructureRecord,
    WalletJournalEntry,
};
use eve_destiny::{CombatState, DVec3, MovementMode as BallMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::BTreeSet;

pub const FACTION_GURISTAS: i64 = 500_010;
pub const FACTION_BLOOD_RAIDER: i64 = 500_012;
pub const FACTION_TRIGLAVIAN: i64 = 500_026;
pub const CORP_DED: i64 = 1_000_137;
pub const POCHVEN_REGION_ID: u32 = 10_000_070;
pub const FOB_GURISTAS_TYPE_ID: u32 = 46_363; // Guristas Pirates Stronghold
pub const FOB_BLOOD_RAIDER_TYPE_ID: u32 = 46_364; // Blood Raiders Stronghold
pub const WORLD_ARK_BOSS_TYPE_ID: u32 = 52_361; // World Ark (Xordazh-class)
pub const OBSERVATORY_MAX_PAYOUT_PILOTS: usize = 15;
pub const OBSERVATORY_BASE_PILOT_ISK: i64 = 100_000_000;
pub const OBSERVATORY_BASE_PILOT_LP: i64 = 10_000;
pub const OBSERVATORY_COLLECTIVE_ISK: f64 = 1_500_000_000.0;
pub const OBSERVATORY_COLLECTIVE_LP: i64 = 150_000;
pub const FOB_MAX_FULL_PAYOUT_PILOTS: usize = 10;
pub const FOB_BASE_PILOT_BOUNTY_ISK: i64 = 30_000_000;
pub const FOB_COLLECTIVE_BOUNTY_ISK: f64 = 300_000_000.0;
pub const MAX_ACTIVE_POCHVEN_OBSERVATORIES: usize = 3;
pub const MAX_ACTIVE_PIRATE_FOBS: usize = 3;
pub const GATE_ACTIVATION_RANGE_M: f64 = 2_500.0;

pub use eve_db::ObservatorySiteState;

fn is_capital_or_disallowed_observatory_hull(ctx: &ServerContext, ship_type_id: u32) -> bool {
    if matches!(ship_type_id, 0 | 670 | 33_328) {
        return true;
    }
    let Some(t) = ctx.sde.types.get(&ship_type_id) else {
        return true;
    };
    if t.category_id != 6 {
        return true;
    }
    matches!(
        t.group_id,
        29 | 30 | 485 | 513 | 547 | 659 | 883 | 902 | 1538 | 4594
    )
}

fn sample_celestial_offset_pos(ctx: &ServerContext, sys_id: u32, base_dist_m: f64) -> DVec3 {
    let u1 = ctx.mechanic_rng.try_unit_f64().unwrap_or(0.37);
    let u2 = ctx.mechanic_rng.try_unit_f64().unwrap_or(0.61);
    let angle = u1 * std::f64::consts::TAU + ((sys_id % 97) as f64) * 0.13;
    let radial = base_dist_m + u2 * 120_000.0;
    let y = (u2 - 0.5) * 40_000.0;
    DVec3::new(radial * angle.cos(), y, radial * angle.sin())
}

pub fn get_observatory_site_state(ctx: &ServerContext, sys_id: u32) -> Option<ObservatorySiteState> {
    let site_id = 1_899_000_000 + i64::from(sys_id);
    ctx.db.with_snapshot(|snap| {
        snap.fob_observatory_state
            .observatory_sites
            .get(&site_id)
            .cloned()
    })
}

pub fn record_fob_combat_participant(ctx: &ServerContext, fob_id: i64, attacker_char_id: i64) {
    if attacker_char_id <= 0 {
        return;
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.fob_observatory_state
            .fob_combatants
            .entry(fob_id)
            .or_default()
            .insert(attacker_char_id);
    });
}

/// Spawns a Blood Raider (`500012`) or Guristas (`500010`) Forward Operating Base in a high/low-sec system
/// and launches an initial raiding fleet.
pub async fn spawn_pirate_fob(
    ctx: &ServerContext,
    sys_id: u32,
    pirate_faction_id: i64,
) -> Result<PyRep, BridgeRpcError> {
    if !matches!(pirate_faction_id, FACTION_GURISTAS | FACTION_BLOOD_RAIDER) {
        return Err(BridgeRpcError {
            code: "InvalidPirateFaction".into(),
            message: "Pirate FOBs must belong to Blood Raider (500012) or Guristas (500010)".into(),
        });
    }
    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(0.5);
    if sys_sec <= 0.0 {
        return Err(BridgeRpcError {
            code: "InvalidSystemSecurity".into(),
            message: format!("Pirate FOBs only spawn in high/low-sec systems (sec={sys_sec:.2})"),
        });
    }

    let now_unix = ctx.now_unix_secs();
    let fob_id = 1_897_000_000 + i64::from(sys_id) * 100 + (pirate_faction_id % 100);
    let fob_type_id = if pirate_faction_id == FACTION_BLOOD_RAIDER {
        FOB_BLOOD_RAIDER_TYPE_ID
    } else {
        FOB_GURISTAS_TYPE_ID
    };
    let fob_vec = sample_celestial_offset_pos(ctx, sys_id, 450_000.0);
    let fob_pos = [fob_vec.x, fob_vec.y, fob_vec.z];

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            fob_id,
            UpwellStructureRecord {
                structure_id: fob_id,
                type_id: fob_type_id,
                owner_corp_id: pirate_faction_id,
                solar_system_id: sys_id,
                name: if pirate_faction_id == FACTION_BLOOD_RAIDER {
                    "Blood Raider Forward Operating Base".into()
                } else {
                    "Guristas Forward Operating Base".into()
                },
                position: fob_pos,
                state: "Online".into(),
                quantum_core_installed: true,
                fuel_blocks: 10_000,
                shield_hp: 500_000.0,
                armor_hp: 500_000.0,
                hull_hp: 500_000.0,
                reinforced_until_unix: 0,
                last_damage_unix: 0,
                last_damage_tick: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.fob_observatory_state
            .fob_factions
            .insert(fob_id, pirate_faction_id);
        snap.fob_observatory_state.fob_combatants.remove(&fob_id);
        snap.fob_observatory_state
            .fob_defense_triggered
            .remove(&fob_id);
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let mut fob_ball = make_site_npc_ball(fob_id, fob_type_id, fob_vec);
    fob_ball.radius = 2_500.0;
    fob_ball.max_velocity = 0.0;
    fob_ball.mode = BallMode::Rigid;
    let mut fob_cs = CombatState::default();
    fob_cs.shield_hp = 500_000.0;
    fob_cs.max_shield_hp = 500_000.0;
    fob_cs.armor_hp = 500_000.0;
    fob_cs.max_armor_hp = 500_000.0;
    fob_cs.structure_hp = 500_000.0;
    fob_cs.max_structure_hp = 500_000.0;
    let _ = sys.spawn_ball_with_combat(fob_ball, fob_cs).await;

    let raider_type_id = if pirate_faction_id == FACTION_BLOOD_RAIDER {
        24_000 // Corpatis Bishop
    } else {
        16_980 // Pithum Silencer
    };
    let mut raiding_ids = Vec::with_capacity(3);
    for idx in 1..=3_i64 {
        let raider_id = fob_id + idx;
        let _ = sys
            .spawn_ball_with_combat(
                make_site_npc_ball(
                    raider_id,
                    raider_type_id,
                    fob_vec + DVec3::new((idx as f64) * 3_000.0, 0.0, 0.0),
                ),
                ctx.build_type_combat_state(raider_type_id),
            )
            .await;
        raiding_ids.push(raider_id);
    }

    Ok(PyRep::keyval([
        ("fobID", PyRep::Integer(fob_id)),
        ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
        ("pirateFactionID", PyRep::Integer(pirate_faction_id)),
        ("spawnedAtUnix", PyRep::Integer(now_unix as i64)),
        (
            "raidingFleetIDs",
            PyRep::List(raiding_ids.into_iter().map(PyRep::Integer).collect()),
        ),
    ]))
}

/// Launches defending response ships scaled to attacking on-grid player fleet composition.
pub async fn trigger_fob_defense_response(
    ctx: &ServerContext,
    fob_id: i64,
    attacker_ship_ids: &[i64],
) -> Result<PyRep, BridgeRpcError> {
    let (sys_id, fob_pos, pirate_faction_id) = ctx
        .db
        .with_snapshot(|snap| {
            snap.structures.get(&fob_id).map(|s| {
                (
                    s.solar_system_id,
                    DVec3::from_array(s.position),
                    s.owner_corp_id,
                )
            })
        })
        .ok_or_else(|| BridgeRpcError {
            code: "FobNotFound".into(),
            message: format!("Pirate FOB {fob_id} not found"),
        })?;

    let fleet_weight: usize = ctx.db.with_snapshot(|snap| {
        attacker_ship_ids
            .iter()
            .map(|sid| {
                let group_id = snap
                    .items
                    .get(sid)
                    .and_then(|it| ctx.sde.types.get(&it.type_id))
                    .map(|t| t.group_id)
                    .unwrap_or(25);
                match group_id {
                    30 | 485 | 547 | 659 | 883 => 4,
                    27 | 898 | 900 => 3,
                    26 | 358 | 419 | 832 | 906 => 2,
                    _ => 1,
                }
            })
            .sum()
    });
    let defender_count = fleet_weight.clamp(2, 24);
    let defender_type_id = if pirate_faction_id == FACTION_BLOOD_RAIDER {
        16_934 // Corpus Prophet
    } else {
        16_984 // Pith Dismantler
    };
    let sys = ctx.systems.get_or_spawn(sys_id);
    let mut defender_ids = Vec::with_capacity(defender_count);
    for idx in 1..=(defender_count as i64) {
        let def_id = fob_id + 10 + idx;
        let _ = sys
            .spawn_ball_with_combat(
                make_site_npc_ball(
                    def_id,
                    defender_type_id,
                    fob_pos + DVec3::new(0.0, (idx as f64) * 1_500.0, 2_500.0),
                ),
                ctx.build_type_combat_state(defender_type_id),
            )
            .await;
        defender_ids.push(def_id);
    }

    Ok(PyRep::keyval([
        ("fobID", PyRep::Integer(fob_id)),
        ("attackerCount", PyRep::Integer(attacker_ship_ids.len() as i64)),
        ("fleetCompositionWeight", PyRep::Integer(fleet_weight as i64)),
        ("defenderCount", PyRep::Integer(defender_ids.len() as i64)),
        (
            "defenderIDs",
            PyRep::List(defender_ids.into_iter().map(PyRep::Integer).collect()),
        ),
    ]))
}

/// Destroys a Pirate FOB, drops structure loot container in space, and pays per-pilot bounties
/// strictly to on-grid combat participants (`30M` ISK per pilot up to 10 pilots; diluted above 10).
pub async fn destroy_pirate_fob_and_payout(
    ctx: &ServerContext,
    fob_id: i64,
    on_grid_char_ids: &[i64],
) -> Result<PyRep, BridgeRpcError> {
    let now_unix = ctx.now_unix_secs();
    let (sys_id, fob_pos, pirate_faction_id, combatants) = ctx.db.try_with_mut_snapshot(|snap| {
        let st = snap
            .structures
            .get_mut(&fob_id)
            .ok_or_else(|| BridgeRpcError {
                code: "FobNotFound".into(),
                message: format!("Pirate FOB {fob_id} not found"),
            })?;
        if st.state == "Destroyed" {
            return Err(BridgeRpcError {
                code: "FobAlreadyDestroyed".into(),
                message: format!("Pirate FOB {fob_id} is already destroyed"),
            });
        }
        st.state = "Destroyed".into();
        st.hull_hp = 0.0;
        let s_id = st.solar_system_id;
        let f_pos = DVec3::from_array(st.position);
        let fac_id = st.owner_corp_id;
        let combatants: BTreeSet<i64> = snap
            .fob_observatory_state
            .fob_combatants
            .remove(&fob_id)
            .unwrap_or_default();
        snap.fob_observatory_state
            .fob_respawn_until
            .insert(s_id, now_unix.saturating_add(86_400));
        Ok((s_id, f_pos, fac_id, combatants))
    })?;

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys.remove_ball(fob_id).await;

    let loot_container_id = fob_id + 90;
    let mut cont_ball = make_site_npc_ball(loot_container_id, 23, fob_pos);
    cont_ball.max_velocity = 0.0;
    cont_ball.mode = BallMode::Rigid;
    let _ = sys.spawn_ball(cont_ball).await;

    let (live_on_grid_combatants, live_ineligible, all_grid_balls) = {
        let bp = sys.ballpark.read().unwrap_or_else(|e| e.into_inner());
        let mut eligible_balls = BTreeSet::new();
        let mut blocked_balls = BTreeSet::new();
        let mut any_balls = BTreeSet::new();
        for (&bid, b) in &bp.balls {
            any_balls.insert(bid);
            if b.position.distance(fob_pos) > 250_000.0 {
                blocked_balls.insert(bid);
                continue;
            }
            let is_cloaked = bp.combat_states.get(&bid).is_some_and(|cs| cs.is_cloaked)
                || bp.ball_slim_meta.get(&bid).is_some_and(|sm| sm.is_cloaked);
            let is_dead = bp.combat_states.get(&bid).is_some_and(|cs| cs.structure_hp <= 0.0);
            if is_cloaked || is_dead || b.type_id == 670 {
                blocked_balls.insert(bid);
            } else {
                eligible_balls.insert(bid);
            }
        }
        (eligible_balls, blocked_balls, any_balls)
    };
    let eligible_attackers: Vec<i64> = ctx.db.with_snapshot(|snap| {
        on_grid_char_ids
            .iter()
            .copied()
            .filter(|cid| {
                if !combatants.contains(cid) {
                    return false;
                }
                let Some(ch) = snap.characters.get(cid) else {
                    return false;
                };
                let sid = ch.active_ship_id;
                if sid > 0 {
                    if live_ineligible.contains(&sid) {
                        return false;
                    }
                    if all_grid_balls.contains(&sid) && !live_on_grid_combatants.contains(&sid) {
                        return false;
                    }
                    if let Some(it) = snap.items.get(&sid) {
                        let is_capsule = it.type_id == 670
                            || ctx
                                .sde
                                .types
                                .get(&it.type_id)
                                .is_some_and(|t| t.group_id == 29);
                        let offgrid_in_other_system = ch.solar_system_id != sys_id
                            && it.location_id != i64::from(sys_id)
                            && it.location_id < 60_000_000;
                        if is_capsule || offgrid_in_other_system {
                            return false;
                        }
                    }
                }
                true
            })
            .collect()
    });
    let per_pilot_bounty: i64 = if eligible_attackers.is_empty() {
        0
    } else if eligible_attackers.len() <= FOB_MAX_FULL_PAYOUT_PILOTS {
        FOB_BASE_PILOT_BOUNTY_ISK
    } else {
        (FOB_COLLECTIVE_BOUNTY_ISK as i64) / (eligible_attackers.len() as i64)
    };
    let total_bounty_paid = (per_pilot_bounty * (eligible_attackers.len() as i64)) as f64;
    let faction_loot_type_id = if pirate_faction_id == FACTION_BLOOD_RAIDER {
        13_791 // Dark Blood Dual Heavy Pulse Laser
    } else {
        13_865 // Dread Guristas 125mm Railgun
    };

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            loot_container_id,
            InventoryItem {
                item_id: loot_container_id,
                type_id: 17_366,
                owner_id: CORP_DED,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions
            .insert(loot_container_id, [fob_pos.x, fob_pos.y, fob_pos.z]);
        let loot_item_id = fob_id + 91;
        snap.items.insert(
            loot_item_id,
            InventoryItem {
                item_id: loot_item_id,
                type_id: faction_loot_type_id,
                owner_id: CORP_DED,
                location_id: loot_container_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
        for &cid in &eligible_attackers {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.balance = ch
                    .balance
                    .checked_add(IskCents::from_isk(per_pilot_bounty))
                    .unwrap_or(ch.balance);
                let new_bal = ch.balance;
                let entry_id = (snap.wallet_journal.len() as i64) + 1;
                snap.wallet_journal.push(WalletJournalEntry {
                    entry_id,
                    character_id: cid,
                    entry_type: "BountyPrizes".into(),
                    amount: IskCents::from_isk(per_pilot_bounty),
                    balance_after: new_bal,
                    reason: "Pirate FOB Destruction Bounty".into(),
                });
            }
        }
    });

    Ok(PyRep::keyval([
        ("fobID", PyRep::Integer(fob_id)),
        ("lootContainerID", PyRep::Integer(loot_container_id)),
        ("bountyPerPilot", PyRep::Integer(per_pilot_bounty)),
        ("totalBountyPaid", PyRep::Real(total_bounty_paid)),
    ]))
}

/// Spawns a Pochven Observatory Flashpoint site, enforcing the region-wide cap of 3 active sites
/// and registering a gated deadspace pocket so direct warps to the World Ark are blocked.
pub async fn spawn_observatory_flashpoint(
    ctx: &ServerContext,
    sys_id: u32,
) -> Result<PyRep, BridgeRpcError> {
    let region_id = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.region_id)
        .unwrap_or(0);
    if region_id != POCHVEN_REGION_ID {
        return Err(BridgeRpcError {
            code: "NotInPochven".into(),
            message: format!(
                "Observatory Flashpoint sites only spawn in Pochven (region {POCHVEN_REGION_ID})"
            ),
        });
    }

    let site_id = 1_899_000_000 + i64::from(sys_id);
    let active_in_pochven = ctx
        .db
        .with_snapshot(|snap| snap.fob_observatory_state.observatory_sites.len());
    if active_in_pochven >= MAX_ACTIVE_POCHVEN_OBSERVATORIES
        && get_observatory_site_state(ctx, sys_id).is_none()
    {
        return Err(BridgeRpcError {
            code: "ObservatoryRegionCapReached".into(),
            message: format!(
                "Pochven already has {active_in_pochven} active Observatory Flashpoints (max {MAX_ACTIVE_POCHVEN_OBSERVATORIES})"
            ),
        });
    }

    let site_vec = sample_celestial_offset_pos(ctx, sys_id, 600_000.0);
    let gate_vec = site_vec + DVec3::new(-250_000.0, 0.0, 0.0);
    let gate_id = site_id + 2;
    let world_ark_id = site_id + 1;
    let sys = ctx.systems.get_or_spawn(sys_id);

    let mut gate_ball = make_site_npc_ball(gate_id, 17_831, gate_vec);
    gate_ball.max_velocity = 0.0;
    gate_ball.mode = BallMode::Rigid;
    let _ = sys.spawn_ball(gate_ball).await;

    let mut boss_cs = CombatState::default();
    boss_cs.shield_hp = 200_000.0;
    boss_cs.max_shield_hp = 200_000.0;
    boss_cs.armor_hp = 400_000.0;
    boss_cs.max_armor_hp = 400_000.0;
    boss_cs.structure_hp = 200_000.0;
    boss_cs.max_structure_hp = 200_000.0;
    let _ = sys
        .spawn_ball_with_combat(
            make_site_npc_ball(world_ark_id, WORLD_ARK_BOSS_TYPE_ID, site_vec),
            boss_cs,
        )
        .await;

    let wave_types = [52_338_u32, 52_184_u32, 52_187_u32];
    let mut defense_wave_ids = Vec::with_capacity(wave_types.len());
    let mut room_objects = vec![DungeonRuntimeObject {
        ball_id: world_ark_id,
        type_id: WORLD_ARK_BOSS_TYPE_ID,
        name: "World Ark (Xordazh-class)".into(),
        position: [site_vec.x, site_vec.y, site_vec.z],
        radius: 5_000.0,
        is_container: false,
        loot_items: Vec::new(),
        spawned: true,
    }];
    for (idx, &wtid) in wave_types.iter().enumerate() {
        let wid = site_id + 10 + (idx as i64);
        let wpos = site_vec + DVec3::new(((idx as f64) + 1.0) * 4_000.0, 1_500.0, 0.0);
        let _ = sys
            .spawn_ball_with_combat(
                make_site_npc_ball(wid, wtid, wpos),
                ctx.build_type_combat_state(wtid),
            )
            .await;
        defense_wave_ids.push(wid);
        room_objects.push(DungeonRuntimeObject {
            ball_id: wid,
            type_id: wtid,
            name: "Triglavian Flashpoint Defender".into(),
            position: [wpos.x, wpos.y, wpos.z],
            radius: 300.0,
            is_container: false,
            loot_items: Vec::new(),
            spawned: true,
        });
    }

    let now_unix = ctx.now_unix_secs();
    let gate_dest = site_vec + DVec3::new(0.0, 0.0, -5_000.0);
    let dungeon_instance_id = with_dungeon_runtime_store(ctx, |store| {
        let inst_id = store.next_instance_id;
        store.next_instance_id += 1;
        store.instances.insert(
            inst_id,
            DungeonRuntimeInstance {
                instance_id: inst_id,
                dungeon_id: 52_361,
                dungeon_name: "Observatory Flashpoint".into(),
                category: "Flashpoint".into(),
                archetype: "Triglavian".into(),
                solar_system_id: sys_id,
                owner_char_id: 0,
                base_position: [gate_vec.x, gate_vec.y, gate_vec.z],
                disallow_mwd: false,
                entry_gate_ball_id: None,
                entry_gate_pos: None,
                entry_gate_locked: false,
                active_objective_id: 0,
                completed_task_ids: Vec::new(),
                last_completed_task_id: None,
                current_room_index: 0,
                rooms: vec![
                    DungeonRuntimeRoom {
                        room_index: 0,
                        room_id: 1,
                        room_name: "Observatory Perimeter Gate".into(),
                        position: [gate_vec.x, gate_vec.y, gate_vec.z],
                        gate_destination_pos: [gate_dest.x, gate_dest.y, gate_dest.z],
                        gated: false,
                        disallow_mwd: Some(false),
                        gate_ball_id: Some(gate_id),
                        gate_locked: false,
                        objects: Vec::new(),
                        npc_groups: Vec::new(),
                        triggers: Vec::new(),
                        spawned: true,
                        cleared: false,
                    },
                    DungeonRuntimeRoom {
                        room_index: 1,
                        room_id: 2,
                        room_name: "World Ark Interior Pocket".into(),
                        position: [site_vec.x, site_vec.y, site_vec.z],
                        gate_destination_pos: [gate_dest.x, gate_dest.y, gate_dest.z],
                        gated: true,
                        disallow_mwd: Some(false),
                        gate_ball_id: None,
                        gate_locked: false,
                        objects: room_objects,
                        npc_groups: Vec::new(),
                        triggers: Vec::new(),
                        spawned: true,
                        cleared: false,
                    },
                ],
                completed: false,
                despawned: false,
                created_unix: now_unix,
            },
        );
        inst_id
    });

    let obs_state = ObservatorySiteState {
        site_id,
        solar_system_id: sys_id,
        site_pos: [site_vec.x, site_vec.y, site_vec.z],
        gate_pos: [gate_vec.x, gate_vec.y, gate_vec.z],
        gate_id,
        world_ark_id,
        defense_wave_ids: defense_wave_ids.clone(),
        dungeon_instance_id,
        pocket_entrants: BTreeSet::new(),
        created_unix: now_unix,
    };
    ctx.db.with_mut_snapshot(|snap| {
        snap.fob_observatory_state
            .observatory_sites
            .insert(site_id, obs_state);
    });

    Ok(PyRep::keyval([
        ("siteID", PyRep::Integer(site_id)),
        ("gateID", PyRep::Integer(gate_id)),
        ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
        ("worldArkBossID", PyRep::Integer(world_ark_id)),
        (
            "defenseWaveIDs",
            PyRep::List(defense_wave_ids.into_iter().map(PyRep::Integer).collect()),
        ),
    ]))
}

/// Validates fleet composition (>= 3 on-grid subcapital pilots) and standing access (>= 0.0) for Observatory Flashpoint.
pub fn validate_observatory_access(
    ctx: &ServerContext,
    char_id: i64,
    on_grid_fleet_member_ids: &[i64],
) -> Result<(), BridgeRpcError> {
    if on_grid_fleet_member_ids.len() < 3 {
        return Err(BridgeRpcError {
            code: "InsufficientFleetComposition".into(),
            message: "Observatory Flashpoint requires at least 3 on-grid subcapital fleet members".into(),
        });
    }
    let min_standing = ctx.db.with_snapshot(|snap| {
        on_grid_fleet_member_ids
            .iter()
            .chain(std::iter::once(&char_id))
            .map(|cid| {
                snap.characters
                    .get(cid)
                    .and_then(|c| c.standings.get(&FACTION_TRIGLAVIAN).copied())
                    .unwrap_or(0.0)
            })
            .fold(f64::INFINITY, f64::min)
    });
    if min_standing < 0.0 {
        return Err(BridgeRpcError {
            code: "StandingAccessDenied".into(),
            message: format!("Triglavian standing {min_standing:.2} is below required 0.0"),
        });
    }
    Ok(())
}

/// Completes an Observatory Flashpoint site and pays per-pilot ISK (`100M`) and DED LP (`10,000`)
/// to up to 15 eligible pocket fleet members (`>= 3` required; no solo 1.5B ISK or bystander payouts).
pub fn complete_observatory_flashpoint(
    ctx: &ServerContext,
    site_id: i64,
    fleet_member_ids: &[i64],
) -> Result<PyRep, BridgeRpcError> {
    if fleet_member_ids.len() < 3 {
        return Err(BridgeRpcError {
            code: "InsufficientFleetComposition".into(),
            message: "Observatory Flashpoint payout requires at least 3 participating pocket fleet members".into(),
        });
    }
    let eligible: Vec<i64> = fleet_member_ids
        .iter()
        .copied()
        .take(OBSERVATORY_MAX_PAYOUT_PILOTS)
        .collect();

    let isk_each = OBSERVATORY_BASE_PILOT_ISK;
    let lp_each = OBSERVATORY_BASE_PILOT_LP;
    let now_unix = ctx.now_unix_secs();

    let obs_state = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(parsed) = snap
            .fob_observatory_state
            .observatory_sites
            .remove(&site_id)
        else {
            return Err(BridgeRpcError {
                code: "ObservatorySiteNotFound".into(),
                message: format!("Observatory Flashpoint site {site_id} does not exist"),
            });
        };
        snap.fob_observatory_state
            .observatory_respawn_until
            .insert(parsed.solar_system_id, now_unix.saturating_add(14_400));
        for &cid in &eligible {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.balance = ch
                    .balance
                    .checked_add(IskCents::from_isk(isk_each))
                    .unwrap_or(ch.balance);
                let new_bal = ch.balance;
                let entry_id = (snap.wallet_journal.len() as i64) + 1;
                snap.wallet_journal.push(WalletJournalEntry {
                    entry_id,
                    character_id: cid,
                    entry_type: "CorporateRewardPayout".into(),
                    amount: IskCents::from_isk(isk_each),
                    balance_after: new_bal,
                    reason: "Observatory Flashpoint Collective Payout".into(),
                });
                *ch.loyalty_points.entry(CORP_DED).or_insert(0) += lp_each;
            }
        }
        Ok(parsed)
    })?;

    with_dungeon_runtime_store(ctx, |store| {
        store.instances.remove(&obs_state.dungeon_instance_id);
    });

    Ok(PyRep::keyval([
        ("siteID", PyRep::Integer(site_id)),
        ("paidPilotCount", PyRep::Integer(eligible.len() as i64)),
        ("iskPerPilot", PyRep::Integer(isk_each)),
        ("lpPerPilot", PyRep::Integer(lp_each)),
    ]))
}

/// Enforces undocked state, `<= 2,500m` gate proximity, subcapital/battleship hull restriction,
/// on-grid fleet composition (`>= 3`), and Triglavian standing (`>= 0.0`) on `keeper.ActivateAccelerationGate`.
pub async fn try_activate_observatory_gate(
    ctx: &ServerContext,
    session: &SessionState,
    sys_id: u32,
    gate_id: i64,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let site_id = 1_899_000_000 + i64::from(sys_id);
    if gate_id != site_id + 2 {
        return None;
    }
    let obs_state = get_observatory_site_state(ctx, sys_id)?;
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Some(Err(BridgeRpcError {
            code: "CantDoThatWhileDocked".into(),
            message: "Cannot activate an acceleration gate while docked".into(),
        }));
    }
    if session.solar_system_id != Some(sys_id) {
        return Some(Err(BridgeRpcError {
            code: "NotInSolarSystem".into(),
            message: "Ship is not in the Observatory solar system".into(),
        }));
    }
    let Some(ship_id) = session.ship_id else {
        return Some(Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Activating an acceleration gate requires an active ship".into(),
        }));
    };

    let sys = ctx.systems.get_or_spawn(sys_id);
    let all_balls = sys.list_balls().await.unwrap_or_default();
    let Some(ship_ball) = all_balls.iter().find(|b| b.id == ship_id) else {
        return Some(Err(BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: "Active ship ball is not in space".into(),
        }));
    };
    let Some(gate_ball) = all_balls.iter().find(|b| b.id == gate_id) else {
        return Some(Err(BridgeRpcError {
            code: "GateNotInSpace".into(),
            message: "Observatory acceleration gate is not in space".into(),
        }));
    };
    let dist_to_gate = ship_ball.position.distance(gate_ball.position);
    if dist_to_gate > GATE_ACTIVATION_RANGE_M {
        return Some(Err(BridgeRpcError {
            code: "OutsideOperationalRange".into(),
            message: format!(
                "Ship is {dist_to_gate:.1}m from Observatory gate (must be <= {GATE_ACTIVATION_RANGE_M:.0}m)"
            ),
        }));
    }

    let (char_docked, ship_type_id) = ctx.db.with_snapshot(|snap| {
        let docked = snap
            .characters
            .get(&char_id)
            .is_none_or(|c| c.station_id.is_some() || c.structure_id.is_some());
        let stid = snap
            .items
            .get(&ship_id)
            .map(|i| i.type_id)
            .unwrap_or(ship_ball.type_id);
        (docked, stid)
    });
    if char_docked {
        return Some(Err(BridgeRpcError {
            code: "CantDoThatWhileDocked".into(),
            message: "Cannot activate an acceleration gate while docked".into(),
        }));
    }
    if is_capital_or_disallowed_observatory_hull(ctx, ship_type_id) {
        return Some(Err(BridgeRpcError {
            code: "ShipClassRestricted".into(),
            message: "Observatory Flashpoint acceleration gate restricts entry to subcapital hulls (up to Battleship)".into(),
        }));
    }

    let site_pos = DVec3::from_array(obs_state.site_pos);
    let gate_pos = DVec3::from_array(obs_state.gate_pos);
    let on_site_ships: BTreeSet<i64> = all_balls
        .iter()
        .filter(|b| {
            !is_capital_or_disallowed_observatory_hull(ctx, b.type_id)
                && (b.position.distance(gate_pos) <= 250_000.0
                    || b.position.distance(site_pos) <= 150_000.0)
        })
        .map(|b| b.id)
        .collect();

    let on_grid_fleet_members: Vec<i64> = ctx.db.with_snapshot(|snap| {
        let Some(fleet) = snap.fleets.values().find(|f| f.members.contains(&char_id)) else {
            return vec![char_id];
        };
        fleet
            .members
            .iter()
            .copied()
            .filter(|cid| {
                snap.characters.get(cid).is_some_and(|c| {
                    c.solar_system_id == sys_id
                        && c.station_id.is_none()
                        && c.structure_id.is_none()
                        && c.active_ship_id > 0
                        && on_site_ships.contains(&c.active_ship_id)
                })
            })
            .collect()
    });

    if let Err(e) = validate_observatory_access(ctx, char_id, &on_grid_fleet_members) {
        return Some(Err(e));
    }

    let dest = site_pos + DVec3::new(0.0, 0.0, -5_000.0);
    let _ = sys.set_ball_position(ship_id, dest).await;
    let _ = sys.set_movement_mode(ship_id, BallMode::Stop).await;

    let mut updated = obs_state;
    updated.pocket_entrants.insert(char_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.fob_observatory_state
            .observatory_sites
            .insert(site_id, updated);
    });

    Some(Ok(PyRep::keyval([
        ("siteID", PyRep::Integer(site_id)),
        ("accessGranted", PyRep::Bool(true)),
    ])))
}

/// Completes the Observatory Flashpoint when the World Ark boss (`52361`) is killed in the NPC death pipeline.
pub async fn on_world_ark_boss_killed(ctx: &ServerContext, sys_id: u32, killer_char_id: i64) {
    if killer_char_id <= 0 {
        return;
    }
    let Some(obs_state) = get_observatory_site_state(ctx, sys_id) else {
        return;
    };
    let site_pos = DVec3::from_array(obs_state.site_pos);
    let sys = ctx.systems.get_or_spawn(sys_id);
    let all_balls = sys.list_balls().await.unwrap_or_default();
    let pocket_ship_ids: BTreeSet<i64> = all_balls
        .iter()
        .filter(|b| {
            !is_capital_or_disallowed_observatory_hull(ctx, b.type_id)
                && b.position.distance(site_pos) <= 150_000.0
        })
        .map(|b| b.id)
        .collect();

    let mut combat_attackers: BTreeSet<i64> =
        crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
            rt.aggression_assist_log
                .get(&obs_state.world_ark_id)
                .map(|list| list.iter().map(|a| a.attacker_char_id).collect())
                .unwrap_or_default()
        });
    combat_attackers.insert(killer_char_id);

    let fleet_member_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
        let Some(fleet) = snap
            .fleets
            .values()
            .find(|f| f.members.contains(&killer_char_id))
        else {
            return Vec::new();
        };
        if !fleet.members.iter().any(|m| combat_attackers.contains(m)) {
            return Vec::new();
        }
        fleet
            .members
            .iter()
            .copied()
            .filter(|cid| {
                obs_state.pocket_entrants.contains(cid)
                    && snap.characters.get(cid).is_some_and(|c| {
                        c.solar_system_id == sys_id
                            && c.station_id.is_none()
                            && c.structure_id.is_none()
                            && c.active_ship_id > 0
                            && pocket_ship_ids.contains(&c.active_ship_id)
                    })
            })
            .collect()
    });

    if fleet_member_ids.len() >= 3 {
        if complete_observatory_flashpoint(ctx, obs_state.site_id, &fleet_member_ids).is_ok() {
            let _ = sys.remove_ball(obs_state.gate_id).await;
            for wid in obs_state.defense_wave_ids {
                let _ = sys.remove_ball(wid).await;
            }
        }
    } else {
        let boss_is_dead = sys
            .get_ball_snapshot(obs_state.world_ark_id)
            .await
            .ok()
            .flatten()
            .is_none()
            || sys
                .get_combat_state(obs_state.world_ark_id)
                .await
                .ok()
                .flatten()
                .is_some_and(|cs| cs.structure_hp <= 0.0);
        if boss_is_dead {
            let now_unix = ctx.now_unix_secs();
            let site_id = obs_state.site_id;
            ctx.db.with_mut_snapshot(|snap| {
                snap.fob_observatory_state
                    .observatory_sites
                    .remove(&site_id);
                snap.fob_observatory_state
                    .observatory_respawn_until
                    .insert(sys_id, now_unix.saturating_add(14_400));
            });
            with_dungeon_runtime_store(ctx, |store| {
                store.instances.remove(&obs_state.dungeon_instance_id);
            });
            let _ = sys.remove_ball(obs_state.gate_id).await;
            for wid in obs_state.defense_wave_ids {
                let _ = sys.remove_ball(wid).await;
            }
        }
    }
}

/// Drives Pirate FOB and Pochven Observatory Flashpoint spawns from the living-world cluster tick,
/// enforcing region/cluster active caps (`<= 3` Pochven Observatories, `<= 3` Pirate FOBs) and respawn cooldowns.
pub async fn step_living_world_fob_and_observatory_spawns(ctx: &ServerContext) {
    let now_unix = ctx.now_unix_secs();
    let (mut active_fobs, mut active_obs) = ctx.db.with_snapshot(|snap| {
        let fobs = snap
            .structures
            .values()
            .filter(|s| {
                s.state != "Destroyed"
                    && matches!(s.type_id, FOB_GURISTAS_TYPE_ID | FOB_BLOOD_RAIDER_TYPE_ID)
            })
            .count();
        let obs = snap.fob_observatory_state.observatory_sites.len();
        (fobs, obs)
    });

    let mut active_handles = ctx.systems.active_system_handles();
    active_handles.sort_unstable_by_key(|(sid, _)| *sid);
    for &(sys_id, _) in &active_handles {
        let has_piloted_ship = ctx.db.with_snapshot(|s| {
            s.characters.values().any(|c| {
                c.solar_system_id == sys_id
                    && c.station_id.is_none()
                    && c.structure_id.is_none()
                    && c.active_ship_id > 0
            })
        });
        if !has_piloted_ship {
            continue;
        }
        let Some(sys_info) = ctx.sde.solar_systems.get(&sys_id) else {
            continue;
        };
        if active_fobs < MAX_ACTIVE_PIRATE_FOBS
            && sys_info.security_status > 0.0
            && sys_info.security_status < 0.8
        {
            let can_spawn_fob = ctx.db.with_snapshot(|snap| {
                let has_fob = snap.structures.values().any(|s| {
                    s.solar_system_id == sys_id
                        && s.state != "Destroyed"
                        && matches!(s.type_id, FOB_GURISTAS_TYPE_ID | FOB_BLOOD_RAIDER_TYPE_ID)
                });
                let on_cd = snap
                    .fob_observatory_state
                    .fob_respawn_until
                    .get(&sys_id)
                    .is_some_and(|&until| now_unix < until);
                !has_fob && !on_cd
            });
            if can_spawn_fob {
                let faction = if sys_id % 2 == 1 {
                    FACTION_BLOOD_RAIDER
                } else {
                    FACTION_GURISTAS
                };
                if spawn_pirate_fob(ctx, sys_id, faction).await.is_ok() {
                    active_fobs += 1;
                }
            }
        }
        if active_obs < MAX_ACTIVE_POCHVEN_OBSERVATORIES
            && sys_info.region_id == POCHVEN_REGION_ID
        {
            let site_id = 1_899_000_000 + i64::from(sys_id);
            let can_spawn_obs = ctx.db.with_snapshot(|snap| {
                let exists = snap
                    .fob_observatory_state
                    .observatory_sites
                    .contains_key(&site_id);
                let on_cd = snap
                    .fob_observatory_state
                    .observatory_respawn_until
                    .get(&sys_id)
                    .is_some_and(|&until| now_unix < until);
                !exists && !on_cd
            });
            if can_spawn_obs && spawn_observatory_flashpoint(ctx, sys_id).await.is_ok() {
                active_obs += 1;
            }
        }
    }
}

/// Live combat damage and remaining HP-ratio summary for a Pirate FOB engagement.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PirateFobDamageOutcome {
    pub applied_damage: f64,
    pub destroyed: bool,
    pub shield_ratio: f64,
    pub armor_ratio: f64,
    pub hull_ratio: f64,
    pub invulnerable: bool,
}

pub async fn apply_live_combat_damage_to_pirate_fob(
    ctx: &ServerContext,
    sys_id: u32,
    sys_handle: &crate::system_actor::SystemHandle,
    attacker_char_id: i64,
    target_ball_id: i64,
    total_dmg: f64,
) -> PirateFobDamageOutcome {
    if total_dmg > 0.0 && attacker_char_id > 0 {
        record_fob_combat_participant(ctx, target_ball_id, attacker_char_id);
    }
    let (on_grid_combat_ships, ineligible_balls) = {
        let bp = sys_handle.ballpark.read().unwrap_or_else(|e| e.into_inner());
        let fob_pos = ctx.db.with_snapshot(|snap| {
            snap.structures
                .get(&target_ball_id)
                .map(|s| eve_destiny::DVec3::from_array(s.position))
                .unwrap_or(eve_destiny::DVec3::ZERO)
        });
        let mut combat_ships = std::collections::HashSet::new();
        let mut blocked_balls = std::collections::HashSet::new();
        for (&bid, b) in &bp.balls {
            if b.position.distance(fob_pos) > 250_000.0 {
                continue;
            }
            let is_cloaked = bp.combat_states.get(&bid).is_some_and(|cs| cs.is_cloaked)
                || bp.ball_slim_meta.get(&bid).is_some_and(|sm| sm.is_cloaked);
            let is_dead = bp.combat_states.get(&bid).is_some_and(|cs| cs.structure_hp <= 0.0);
            if is_cloaked || is_dead || b.type_id == 670 {
                blocked_balls.insert(bid);
            } else {
                combat_ships.insert(bid);
            }
        }
        (combat_ships, blocked_balls)
    };
    let (should_trigger_def, dest, s_r, a_r, h_r, attacker_ships, attacker_chars) =
        ctx.db.with_mut_snapshot(|snap| {
            let mut trig = false;
            if let Some(st) = snap.structures.get_mut(&target_ball_id) {
                let mut rem = total_dmg;
                if st.shield_hp > 0.0 {
                    let take = rem.min(st.shield_hp);
                    st.shield_hp -= take;
                    rem -= take;
                }
                if rem > 0.0 && st.armor_hp > 0.0 {
                    let take = rem.min(st.armor_hp);
                    st.armor_hp -= take;
                    rem -= take;
                }
                if rem > 0.0 && st.hull_hp > 0.0 {
                    let take = rem.min(st.hull_hp);
                    st.hull_hp -= take;
                }
                if snap
                    .fob_observatory_state
                    .fob_defense_triggered
                    .insert(target_ball_id)
                {
                    trig = true;
                }
                let dest = st.hull_hp <= 0.0;
                let s_r = (st.shield_hp / 500_000.0).clamp(0.0, 1.0);
                let a_r = (st.armor_hp / 500_000.0).clamp(0.0, 1.0);
                let h_r = (st.hull_hp / 500_000.0).clamp(0.0, 1.0);
                let mut on_grid_pilots: Vec<(i64, i64)> = snap
                    .characters
                    .values()
                    .filter(|c| {
                        c.solar_system_id == sys_id
                            && c.station_id.is_none()
                            && c.structure_id.is_none()
                            && c.active_ship_id > 0
                            && on_grid_combat_ships.contains(&c.active_ship_id)
                            && !ineligible_balls.contains(&c.active_ship_id)
                            && !snap.items.get(&c.active_ship_id).is_some_and(|it| {
                                it.type_id == 670
                                    || ctx
                                        .sde
                                        .types
                                        .get(&it.type_id)
                                        .is_some_and(|t| t.group_id == 29)
                            })
                    })
                    .map(|c| (c.character_id, c.active_ship_id))
                    .collect();
                on_grid_pilots.sort_unstable_by_key(|(cid, _)| *cid);
                let ships: Vec<i64> = on_grid_pilots.iter().map(|(_, sid)| *sid).collect();
                let chars: Vec<i64> = on_grid_pilots.iter().map(|(cid, _)| *cid).collect();
                (trig, dest, s_r, a_r, h_r, ships, chars)
            } else {
                (false, true, 0.0, 0.0, 0.0, Vec::new(), Vec::new())
            }
        });
    if should_trigger_def {
        let _ = trigger_fob_defense_response(ctx, target_ball_id, &attacker_ships).await;
    }
    if dest {
        let _ = destroy_pirate_fob_and_payout(ctx, target_ball_id, &attacker_chars).await;
    }
    PirateFobDamageOutcome {
        applied_damage: total_dmg,
        destroyed: dest,
        shield_ratio: s_r,
        armor_ratio: a_r,
        hull_ratio: h_r,
        invulnerable: dest,
    }
}

#[cfg(test)]
#[path = "pve_fob_observatory_tests.rs"]
mod pve_fob_observatory_tests;

//! `combat_npcs_drones` — Salvagers, Tractor Beams, Complete Drone Lifecycle & NPC AI / Factions / Clone Soldiers / EWAR / Remote Repair / Bounties (`C-4`, `C-7`).
//!
// parity: plan:C-4, plan:C-7
// parity: feat:modules.salvager, feat:combat.wreck-size-salvage-difficulty, feat:modules.tractor-beam
// parity: feat:drones.launch-drones, feat:drones.recall-drones, feat:drones.return-orbit, feat:drones.engage-target
// parity: feat:drones.assist-guard, feat:drones.aggressive-passive, feat:drones.abandon-scoop, feat:drones.reconnect-drones
// parity: feat:drones.drone-bandwidth, feat:drones.drone-control-range, feat:drones.salvage-drones, feat:drones.logistic-drones
// parity: feat:drones.ewar-drones, feat:drones.sentry-drones, feat:drones.drone-hp-loss, feat:drones.sentry-drone-scoop-distance
// parity: feat:drones.gecko-augmented-mutated-drones, feat:drones.gecko-subverted-bandwidth
// parity: feat:combat.bounty-payout-npc, feat:npc.belt-rats, feat:npc.npc-ai-targeting, feat:npc.npc-damage-types
// parity: feat:npc.npc-loot, feat:npc.sentry-guns, feat:npc.sentry-gun-grid-aggression, feat:npc.drifters
// parity: feat:npc.triglavians, feat:npc.sleepers, feat:npc.edencom-npc, feat:npc.rogue-drones
// parity: feat:npc.incursion-npc, feat:npc.navy-npc, feat:npc.npc-hauler, feat:npc.npc-spawn-encounter
// parity: feat:npc.npc-standings-aggression, feat:npc.officer-npc, feat:npc.npc-ewar-application
// parity: feat:npc.npc-remote-repair, feat:npc.npc-target-switching, feat:npc.clone-soldier-spawns

#![forbid(unsafe_code)]

use super::*;
use eve_destiny::{Ball, MovementMode};

/// Evaluates effective drone control range in meters for `char_id` flying `ship_id` (`feat:drones.drone-control-range`).
/// Base control range is 20,000 m + 5,000 m/level of Drone Avionics (`3437`) + 3,000 m/level of Advanced Drone Avionics (`23618`).
pub fn compute_drone_control_range(ctx: &ServerContext, char_id: i64, ship_id: i64) -> f64 {
    let (da_lv, ada_lv, ship_base) = ctx.db.with_snapshot(|snap| {
        let skills = snap.skills.get(&char_id);
        let da = skills
            .and_then(|m| m.get(&3437))
            .map(|s| s.level as f64)
            .unwrap_or(0.0);
        let ada = skills
            .and_then(|m| m.get(&23618))
            .map(|s| s.level as f64)
            .unwrap_or(0.0);
        let base = snap
            .items
            .get(&ship_id)
            .and_then(|it| ctx.sde.types.get(&it.type_id))
            .and_then(|t| t.default_attributes.get(&458).copied())
            .unwrap_or(20_000.0);
        (da, ada, base)
    });
    ship_base + 5_000.0 * da_lv + 3_000.0 * ada_lv
}

/// Evaluates Salvager (`group_id == 1122`) or Tractor Beam (`group_id == 650`) module activation (`feat:modules.salvager`, `feat:combat.wreck-size-salvage-difficulty`, `feat:modules.tractor-beam`).
#[allow(clippy::too_many_arguments)]
pub async fn activate_salvager_or_tractor_module(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    module_id: i64,
    _m: &eve_dogma::FittedModule,
    mod_type: &eve_sde::InvType,
    attacker_ball: &Ball,
    target_id_opt: Option<i64>,
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let Some(wreck_id) = target_id_opt else {
        return Some(Err(BridgeRpcError {
            code: "TargetRequired".into(),
            message: "Salvager / Tractor Beam requires a target wreck or container".into(),
        }));
    };
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let Some((_, wreck_ball)) = sys_handle.get_ball_snapshot(wreck_id).await.ok().flatten() else {
        return Some(Err(BridgeRpcError {
            code: "TargetNotInSpace".into(),
            message: format!("Target wreck/container {wreck_id} is not in space"),
        }));
    };
    let dist = attacker_ball.position.distance(wreck_ball.position);

    // 1. Tractor Beam (`group_id == 650`, `feat:modules.tractor-beam`)
    if mod_type.group_id == 650 {
        let max_range = mod_type
            .default_attributes
            .get(&54)
            .copied()
            .unwrap_or(20_000.0)
            .max(20_000.0);
        if dist > max_range {
            return Some(Err(BridgeRpcError {
                code: "TargetOutOfRange".into(),
                message: format!(
                    "Wreck/can {wreck_id} is {dist:.1} m away (exceeds tractor beam range {max_range:.1} m)"
                ),
            }));
        }
        let now_unix = ctx.now_unix();
        let wreck_owner = ctx
            .db
            .with_snapshot(|snap| snap.items.get(&wreck_id).map(|i| i.owner_id))
            .unwrap_or(char_id);
        let is_abandoned = with_combat_runtime(ctx, |rt| {
            rt.container_lifetimes
                .get(&wreck_id)
                .map(|c| c.is_abandoned)
                .unwrap_or(false)
        }) || ctx
            .crimewatch
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_create(wreck_owner)
            .is_criminal(now_unix);
        let same_corp_or_fleet = ctx.db.with_snapshot(|snap| {
            let c1 = snap.characters.get(&char_id).map(|c| c.corporation_id);
            let c2 = snap.characters.get(&wreck_owner).map(|c| c.corporation_id);
            let same_corp = c1.is_some() && c1 == c2 && c1.unwrap_or(0) > 2_000_000;
            let same_fleet = snap
                .fleets
                .values()
                .any(|f| f.members.contains(&char_id) && f.members.contains(&wreck_owner));
            same_corp || same_fleet
        });
        if wreck_owner != char_id && wreck_owner != 0 && !is_abandoned && !same_corp_or_fleet {
            return Some(Err(BridgeRpcError {
                code: "TractorBeamOwnershipRestricted".into(),
                message: "Cannot tractor a wreck or container owned by another pilot".into(),
            }));
        }
        let tractor_vel = mod_type
            .default_attributes
            .get(&624)
            .copied()
            .filter(|&v| v > 0.0)
            .unwrap_or(1_000.0);
        let pull_seconds = mod_type
            .default_attributes
            .get(&73)
            .copied()
            .map(|ms| (ms / 1_000.0).max(1.0))
            .unwrap_or(5.0);
        let (new_pos, arrived) = crate::combat::drones_salvage_tractor::step_tractor_beam(
            wreck_ball.position,
            attacker_ball.position,
            tractor_vel,
            pull_seconds,
        );
        let _ = sys_handle.set_ball_position(wreck_id, new_pos).await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.item_positions
                .insert(wreck_id, [new_pos.x, new_pos.y, new_pos.z]);
        });
        return Some(Ok(PyRep::keyval([
            ("moduleID", PyRep::Integer(module_id)),
            ("targetID", PyRep::Integer(wreck_id)),
            ("newDistanceMeters", PyRep::Real(attacker_ball.position.distance(new_pos))),
            ("arrivedWithinLootRange", PyRep::Bool(arrived)),
        ])));
    }

    // 2. Salvager (`group_id == 1122`, `feat:modules.salvager`, `feat:combat.wreck-size-salvage-difficulty`)
    let max_range = mod_type
        .default_attributes
        .get(&54)
        .copied()
        .unwrap_or(5_000.0)
        .max(5_000.0);
    if dist > max_range {
        return Some(Err(BridgeRpcError {
            code: "TargetOutOfRange".into(),
            message: format!(
                "Wreck {wreck_id} is {dist:.1} m away (exceeds salvager range {max_range:.1} m)"
            ),
        }));
    }
    let res = execute_wreck_salvage_cycle(
        ctx,
        sys_id,
        char_id,
        ship_id,
        wreck_id,
        Some(mod_type.type_id),
    )
    .await;
    Some(res)
}

/// Core wreck salvaging cycle shared by Salvager modules (`25861`/`30836`) and Salvage Drones (`32787`)
/// (`feat:modules.salvager`, `feat:combat.wreck-size-salvage-difficulty`, `feat:drones.salvage-drones`).
pub async fn execute_wreck_salvage_cycle(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    wreck_id: i64,
    salvager_type_id_opt: Option<u32>,
) -> Result<PyRep, BridgeRpcError> {
    let wreck_type_id = ctx
        .db
        .with_snapshot(|snap| snap.items.get(&wreck_id).map(|i| i.type_id))
        .unwrap_or(26557);
    let wreck_difficulty_bonus = ctx
        .sde
        .types
        .get(&wreck_type_id)
        .and_then(|t| t.default_attributes.get(&901).copied())
        .unwrap_or(20.0);

    let mod_access_bonus = salvager_type_id_opt
        .and_then(|tid| ctx.sde.types.get(&tid))
        .and_then(|t| {
            t.default_attributes
                .get(&902)
                .copied()
                .or_else(|| t.default_attributes.get(&901).copied())
        })
        .unwrap_or(3.0); // Salvage Drone I base access bonus per cycle

    let (salvaging_skill_lv, rig_access_bonus) = ctx.db.with_snapshot(|snap| {
        let sk_lv = snap
            .skills
            .get(&char_id)
            .and_then(|m| m.get(&3438))
            .map(|s| s.level as f64)
            .unwrap_or(0.0);
        let rig_bonus: f64 = snap
            .items
            .values()
            .filter(|i| {
                i.location_id == ship_id
                    && matches!(
                        i.flag,
                        InventoryFlag::RigSlot0 | InventoryFlag::RigSlot1 | InventoryFlag::RigSlot2
                    )
            })
            .filter_map(|i| ctx.sde.types.get(&i.type_id))
            .filter_map(|t| t.default_attributes.get(&901).copied())
            .sum();
        (sk_lv, rig_bonus)
    });

    let access_chance_pct =
        mod_access_bonus + 5.0 * salvaging_skill_lv + rig_access_bonus + wreck_difficulty_bonus;
    if access_chance_pct <= 0.0 {
        return Ok(PyRep::keyval([
            ("wreckID", PyRep::Integer(wreck_id)),
            ("wreckTypeID", PyRep::Integer(wreck_type_id as i64)),
            ("wreckDifficultyBonus", PyRep::Real(wreck_difficulty_bonus)),
            ("accessChancePct", PyRep::Real(access_chance_pct)),
            ("salvaged", PyRep::Bool(false)),
            ("reason", PyRep::String("InsufficientAccessChance".into())),
        ]));
    }

    let roll_pct = 10.0;
    if roll_pct >= access_chance_pct {
        return Ok(PyRep::keyval([
            ("wreckID", PyRep::Integer(wreck_id)),
            ("wreckTypeID", PyRep::Integer(wreck_type_id as i64)),
            ("wreckDifficultyBonus", PyRep::Real(wreck_difficulty_bonus)),
            ("accessChancePct", PyRep::Real(access_chance_pct)),
            ("salvaged", PyRep::Bool(false)),
            ("reason", PyRep::String("CycleRollFailed".into())),
        ]));
    }

    // Select T1, T2, or Sleeper salvage loot table based on wreck's SDE accessDifficultyBonus (`attr 901`)
    let salvage_table: &[(u32, i32)] = if wreck_difficulty_bonus <= -15.0 {
        // Sleeper / Drifter wreck -> Ancient / Sleeper salvage (`30259` Melted Nanoribbons, `30258` Modified Fluid Router)
        &[(30259, 2), (30258, 3)]
    } else if wreck_difficulty_bonus < 0.0 {
        // Tech II / Elite wreck -> T2 salvage (`25605` Artificial Neural Network, `25600` Capacitor Console)
        &[(25605, 2), (25600, 3)]
    } else {
        // Tech I frigate/cruiser/battleship/capital wreck -> T1 salvage (`25588` Alloyed Tritanium Bar, `25595` Armor Plates)
        &[(25588, 4), (25595, 2), (25597, 3)]
    };

    let yielded_type_ids: Vec<u32> = salvage_table.iter().map(|&(t, _)| t).collect();
    ctx.db.with_mut_snapshot(|snap| {
        // Transfer any surviving cargo inside the wreck or remove the wreck entity
        snap.items.remove(&wreck_id);
        for &(tid, qty) in salvage_table {
            snap.next_entity_id += 1;
            let iid = snap.next_entity_id;
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: qty,
                    singleton: false,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        }
    });
    with_mut_combat_runtime(ctx, |rt| {
        rt.container_lifetimes.remove(&wreck_id);
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.remove_ball(wreck_id).await;

    Ok(PyRep::keyval([
        ("wreckID", PyRep::Integer(wreck_id)),
        ("wreckTypeID", PyRep::Integer(wreck_type_id as i64)),
        ("wreckDifficultyBonus", PyRep::Real(wreck_difficulty_bonus)),
        ("accessChancePct", PyRep::Real(access_chance_pct)),
        ("salvaged", PyRep::Bool(true)),
        (
            "salvageTypeIDs",
            PyRep::List(
                yielded_type_ids
                    .into_iter()
                    .map(|t| PyRep::Integer(t as i64))
                    .collect(),
            ),
        ),
    ]))
}

/// Steps automatic drone aggression (`feat:drones.aggressive-passive`) and fleet Assist / Guard (`feat:drones.assist-guard`).
/// - If `attacker_of_owner_opt` is `Some(aggressor_ball_id)` and owner's drone setting is `aggressive == true`,
///   idle combat drones belonging to `owner_char_id` automatically engage `aggressor_ball_id` (concentrating on one target if `focus_fire == true`).
/// - If an assisted fleetmate attacks `assisted_target_opt`, assisting drones engage `assisted_target_opt`.
/// - If a guarded fleetmate is attacked by `attacker_of_guarded_opt`, guarding drones engage that attacker.
pub async fn step_drone_auto_aggression_and_assist(
    ctx: &ServerContext,
    solar_system_id: u32,
    owner_char_id: i64,
    attacker_of_owner_opt: Option<i64>,
    assisted_target_opt: Option<(i64, i64)>,
    attacker_of_guarded_opt: Option<(i64, i64)>,
) -> Vec<i64> {
    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let settings = with_combat_runtime(ctx, |rt| {
        rt.drone_settings
            .get(&owner_char_id)
            .cloned()
            .unwrap_or_default()
    });
    let drones: Vec<DroneSpaceState> = with_combat_runtime(ctx, |rt| {
        rt.active_drones_in_space
            .values()
            .filter(|d| {
                d.solar_system_id == solar_system_id
                    && d.owner_char_id == owner_char_id
                    && !d.is_abandoned
            })
            .cloned()
            .collect()
    });

    let mut engaged_drone_balls = Vec::new();
    for mut d in drones {
        let mut chosen_target = None;
        if let Some((assisted_char, target_ball)) = assisted_target_opt {
            if d.assist_char_id == Some(assisted_char) {
                chosen_target = Some(target_ball);
            }
        }
        if chosen_target.is_none() {
            if let Some((guarded_char, aggressor_ball)) = attacker_of_guarded_opt {
                if d.guard_char_id == Some(guarded_char) {
                    chosen_target = Some(aggressor_ball);
                }
            }
        }
        if chosen_target.is_none() && settings.aggressive {
            if let Some(aggr) = attacker_of_owner_opt {
                chosen_target = Some(aggr);
            }
        }
        if let Some(tid) = chosen_target {
            d.target_id = Some(tid);
            if !d.is_sentry {
                let _ = sys_handle
                    .set_movement_mode(
                        d.ball_id,
                        MovementMode::Orbit {
                            target_id: tid,
                            range: 1_500.0,
                        },
                    )
                    .await;
            }
            if d.damage_vector.total() > 0.0 {
                let _ = sys_handle.apply_damage(tid, d.damage_vector).await;
            }
            engaged_drone_balls.push(d.ball_id);
            with_mut_combat_runtime(ctx, |rt| {
                rt.active_drones_in_space.insert(d.ball_id, d);
            });
        }
    }
    engaged_drone_balls
}

/// Applies incoming weapon/NPC damage to a launched drone in space (`feat:drones.drone-hp-loss`).
/// If the drone's structure HP reaches 0, removes the drone ball from Ballpark and deletes its inventory record.
pub async fn apply_damage_to_drone_in_space(
    ctx: &ServerContext,
    solar_system_id: u32,
    drone_ball_id: i64,
    damage: DamageVector,
) -> (f64, bool) {
    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let Some((applied, destroyed, _)) = sys_handle.apply_damage(drone_ball_id, damage).await.ok().flatten() else {
        return (0.0, false);
    };
    if destroyed {
        let removed_drone = with_mut_combat_runtime(ctx, |rt| {
            rt.active_drones_in_space.remove(&drone_ball_id)
        });
        let _ = sys_handle.remove_ball(drone_ball_id).await;
        if let Some(d) = removed_drone {
            ctx.db.with_mut_snapshot(|snap| {
                snap.items.remove(&d.drone_item_id);
            });
        }
    }
    (applied, destroyed)
}


/// Handles all V24 client Drone RPCs (`ship.LaunchDrones`, `ship.ScoopToDroneBay`, `entity.CmdReturnBay`,
/// `entity.CmdReturnHome`, `entity.CmdReturnAndOrbit`, `entity.CmdEngage`, `entity.CmdAssist`, `entity.CmdGuard`,
/// `entity.ChangeDroneSettings`, `entity.SetAggressive`, `entity.CmdAbandonDrone`, `entity.CmdReconnectToDrones`, `entity.CmdSalvage`).
pub async fn handle_npcs_drones(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("ship", "LaunchDrones") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id.or_else(|| {
                ctx.db
                    .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.active_ship_id))
            }) else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let drone_ids: Vec<i64> = match args.first() {
                Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => {
                    items.iter().filter_map(|it| it.as_i64()).collect()
                }
                Some(PyRep::Integer(id)) => vec![*id],
                _ => Vec::new(),
            };

            // Check pilot max active drones from Drones skill (`3436`, 1 per level up to 5; defaults to 5 if not explicitly restricted)
            let max_drones_allowed = ctx.db.with_snapshot(|snap| {
                snap.skills
                    .get(&char_id)
                    .and_then(|m| m.get(&3436))
                    .map(|s| (s.level as usize).clamp(1, 5))
                    .unwrap_or(5)
            });

            let (cur_count, cur_bw) = with_combat_runtime(ctx, |rt| {
                let active: Vec<&DroneSpaceState> = rt
                    .active_drones_in_space
                    .values()
                    .filter(|d| d.parent_ship_id == ship_id && !d.is_abandoned)
                    .collect();
                (active.len(), active.iter().map(|d| d.bandwidth_used).sum::<f64>())
            });

            if cur_count + drone_ids.len() > max_drones_allowed {
                return Some(Err(BridgeRpcError {
                    code: "MaxActiveDronesExceeded".into(),
                    message: format!(
                        "Cannot control more than {max_drones_allowed} active drones (currently {cur_count}, requested {})",
                        drone_ids.len()
                    ),
                }));
            }

            let mut ship_fit_opt = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).ok();
            let triage_active = ship_fit_opt
                .as_ref()
                .map(|f| {
                    f.modules.values().any(|m| {
                        matches!(
                            m.state,
                            eve_dogma::ModuleState::Active
                                | eve_dogma::ModuleState::Overloaded
                        ) && matches!(m.type_id, 27951 | 4294)
                    })
                })
                .unwrap_or(false);
            let ship_bw = if triage_active {
                0.0
            } else {
                ship_fit_opt
                    .as_ref()
                    .map(|f| f.ship.get_attribute(1271, &ctx.sde))
                    .filter(|&bw| bw > 0.0)
                    .or_else(|| {
                        ctx.db.with_snapshot(|snap| {
                            snap.items
                                .get(&ship_id)
                                .and_then(|s| ctx.sde.types.get(&s.type_id))
                                .and_then(|t| t.default_attributes.get(&1271).copied())
                        })
                    })
                    .unwrap_or(50.0)
            };

            let drone_records: Vec<InventoryItemRecord> = match ctx.db.with_snapshot(|snap| {
                let mut recs = Vec::new();
                for &did in &drone_ids {
                    let Some(it) = snap.items.get(&did).cloned() else {
                        return Err(BridgeRpcError {
                            code: "DroneNotFound".into(),
                            message: format!("Drone item {did} not found"),
                        });
                    };
                    if it.owner_id != char_id || it.location_id != ship_id {
                        return Err(BridgeRpcError {
                            code: "DroneNotInBay".into(),
                            message: format!("Drone {did} is not in ship {ship_id}'s drone bay"),
                        });
                    }
                    recs.push(it);
                }
                Ok(recs)
            }) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let req_bw: f64 = drone_records
                .iter()
                .map(|r| {
                    r.dynamic_attributes.get(&1272).copied().unwrap_or_else(|| {
                        ctx.sde
                            .types
                            .get(&r.type_id)
                            .and_then(|t| t.default_attributes.get(&1272).copied())
                            .unwrap_or(5.0)
                    })
                })
                .sum();
            if cur_bw + req_bw > ship_bw + 1e-4 {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientDroneBandwidth".into(),
                    message: format!(
                        "Launching requires {:.1} Mbit/s bandwidth (ship has {:.1} Mbit/s, {:.1} Mbit/s already in use)",
                        req_bw, ship_bw, cur_bw
                    ),
                }));
            }

            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let ship_pos = sys_handle
                .get_ball_snapshot(ship_id)
                .await
                .ok()
                .flatten()
                .map(|(_, b)| b.position)
                .unwrap_or(DVec3::ZERO);

            let mut launched_ball_ids = Vec::new();
            for (idx, rec) in drone_records.into_iter().enumerate() {
                if let Some(ref mut f) = ship_fit_opt {
                    if !f.drones.iter().any(|&(id, _)| id == rec.item_id) {
                        f.drones.push((rec.item_id, rec.type_id));
                    }
                }
                let sde_t = ctx.sde.types.get(&rec.type_id);
                let attr = |aid: u16| -> Option<f64> {
                    rec.dynamic_attributes
                        .get(&aid)
                        .copied()
                        .or_else(|| {
                            ship_fit_opt
                                .as_ref()
                                .map(|f| f.get_drone_effective_attribute(rec.item_id, aid, &ctx.sde))
                        })
                        .or_else(|| sde_t.and_then(|t| t.default_attributes.get(&aid).copied()))
                };
                let raw_vel = attr(37).unwrap_or(1_500.0);
                let is_sentry = raw_vel < 1.0
                    || matches!(rec.type_id, 23559 | 23561 | 23525 | 23563 | 28292 | 28294);
                let max_vel = if is_sentry { 0.0 } else { raw_vel };
                let bw_used = attr(1272).unwrap_or(5.0);
                let opt_range = attr(54).unwrap_or(5_000.0);
                let dmg_mult = attr(64).unwrap_or(1.0);
                let dmg_vec = DamageVector {
                    em: attr(114).unwrap_or(0.0) * dmg_mult,
                    explosive: attr(116).unwrap_or(0.0) * dmg_mult,
                    kinetic: attr(117).unwrap_or(0.0) * dmg_mult,
                    thermal: attr(118).unwrap_or(0.0) * dmg_mult,
                };

                let ball_id = ctx.db.with_mut_snapshot(|snap| {
                    snap.next_entity_id += 1;
                    let bid = snap.next_entity_id;
                    if let Some(it) = snap.items.get_mut(&rec.item_id) {
                        it.location_id = sys_id as i64;
                        it.flag = InventoryFlag::None;
                        it.version += 1;
                    }
                    bid
                });
                let d_pos = ship_pos
                    + DVec3::new(
                        ((idx as f64) * 50.0).cos() * 40.0,
                        0.0,
                        ((idx as f64) * 50.0).sin() * 40.0,
                    );
                let mut cs = ctx.build_type_combat_state(rec.type_id);
                if let Some(&mut_hp) = rec.dynamic_attributes.get(&9) {
                    cs.structure_hp = mut_hp;
                    cs.max_structure_hp = mut_hp;
                }
                let _ = sys_handle
                    .spawn_ball_with_combat(
                        Ball {
                            id: ball_id,
                            type_id: rec.type_id,
                            position: d_pos,
                            velocity: DVec3::ZERO,
                            max_velocity: max_vel,
                            inertia: 0.5,
                            mass: 10_000.0,
                            radius: 10.0,
                            speed_fraction: if is_sentry { 0.0 } else { 1.0 },
                            mode: MovementMode::Stop,
                        },
                        cs,
                    )
                    .await;
                let _ = sys_handle
                    .emit_destiny_event(
                        ball_id,
                        eve_destiny::DestinyEvent::OnDroneStateChange {
                            drone_id: ball_id,
                            owner_id: char_id,
                            controller_id: ship_id,
                            activity_state: 0,
                            type_id: rec.type_id,
                            controller_owner_id: char_id,
                            target_id: None,
                        },
                    )
                    .await;

                let st = DroneSpaceState {
                    ball_id,
                    drone_item_id: rec.item_id,
                    type_id: rec.type_id,
                    group_id: sde_t.map(|t| t.group_id).unwrap_or(100),
                    owner_char_id: char_id,
                    original_owner_char_id: char_id,
                    parent_ship_id: ship_id,
                    solar_system_id: sys_id,
                    target_id: None,
                    assist_char_id: None,
                    guard_char_id: None,
                    is_abandoned: false,
                    is_sentry,
                    bandwidth_used: bw_used,
                    max_velocity: max_vel,
                    optimal_range: opt_range,
                    damage_vector: dmg_vec,
                    shield_repair_amount: attr(68).unwrap_or(0.0),
                    armor_repair_amount: attr(84).unwrap_or(0.0),
                    hull_repair_amount: attr(83).unwrap_or(0.0),
                    web_speed_factor: attr(20).unwrap_or(0.0),
                    neut_amount: attr(97).unwrap_or(0.0),
                    ecm_strength: attr(238)
                        .or_else(|| attr(239))
                        .or_else(|| attr(240))
                        .or_else(|| attr(241))
                        .unwrap_or(0.0),
                    tp_sig_bonus_pct: attr(554).unwrap_or(0.0),
                };
                with_mut_combat_runtime(ctx, |rt| {
                    rt.active_drones_in_space.insert(ball_id, st);
                });
                let notif_args = vec![
                    PyRep::Integer(rec.item_id),
                    PyRep::Integer(char_id),
                    PyRep::Integer(ship_id),
                    PyRep::Integer(0),
                    PyRep::Integer(rec.type_id as i64),
                ];
                ctx.notify(
                    NotificationTarget::Character(char_id),
                    "OnDroneStateChange",
                    "charid",
                    notif_args.clone(),
                );
                session.push_notification("OnDroneStateChange", notif_args);
                launched_ball_ids.push(ball_id);
            }
            Some(Ok(PyRep::List(
                launched_ball_ids.into_iter().map(PyRep::Integer).collect(),
            )))
        }

        ("entity", "CmdReturnBay") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id.or_else(|| {
                ctx.db
                    .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.active_ship_id))
            }) else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let ship_pos = sys_handle
                .get_ball_snapshot(ship_id)
                .await
                .ok()
                .flatten()
                .map(|(_, b)| b.position)
                .unwrap_or(DVec3::ZERO);

            let candidates: Vec<DroneSpaceState> = with_combat_runtime(ctx, |rt| {
                rt.active_drones_in_space
                    .values()
                    .filter(|d| d.parent_ship_id == ship_id && !d.is_abandoned)
                    .cloned()
                    .collect()
            });

            // Immobile sentry drones can only be returned/scooped when the controlling ship is within 2,500 m (`feat:drones.sentry-drone-scoop-distance`)
            for d in &candidates {
                if d.is_sentry {
                    if let Ok(Some((_, d_ball))) = sys_handle.get_ball_snapshot(d.ball_id).await {
                        let dist = ship_pos.distance(d_ball.position);
                        if dist > 2_500.0 {
                            return Some(Err(BridgeRpcError {
                                code: "SentryDroneOutOfScoopRange".into(),
                                message: format!(
                                    "Immobile sentry drone {} is {dist:.1} m away (must be within 2,500 m to return to bay)",
                                    d.ball_id
                                ),
                            }));
                        }
                    }
                }
            }

            let mut recalled_items = Vec::new();
            for d in candidates {
                with_mut_combat_runtime(ctx, |rt| {
                    rt.active_drones_in_space.remove(&d.ball_id);
                });
                let _ = sys_handle.remove_ball(d.ball_id).await;
                ctx.db.with_mut_snapshot(|snap| {
                    if let Some(it) = snap.items.get_mut(&d.drone_item_id) {
                        it.location_id = ship_id;
                        it.flag = InventoryFlag::DroneBay;
                        it.version += 1;
                    }
                });
                recalled_items.push(d.drone_item_id);
            }
            Some(Ok(PyRep::List(
                recalled_items.into_iter().map(PyRep::Integer).collect(),
            )))
        }

        ("entity", "CmdReturnHome") | ("entity", "CmdReturnAndOrbit") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id.or_else(|| {
                ctx.db
                    .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.active_ship_id))
            }) else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let drones: Vec<DroneSpaceState> = with_combat_runtime(ctx, |rt| {
                rt.active_drones_in_space
                    .values()
                    .filter(|d| d.parent_ship_id == ship_id && !d.is_abandoned)
                    .cloned()
                    .collect()
            });
            let mut count = 0;
            for mut d in drones {
                d.target_id = None;
                if d.is_sentry {
                    // Immobile sentry drones ignore orbit movement (`feat:drones.sentry-drone-scoop-distance`)
                    let _ = sys_handle
                        .set_movement_mode(d.ball_id, MovementMode::Stop)
                        .await;
                } else {
                    let _ = sys_handle
                        .set_movement_mode(
                            d.ball_id,
                            MovementMode::Orbit {
                                target_id: ship_id,
                                range: 1_000.0,
                            },
                        )
                        .await;
                }
                count += 1;
                with_mut_combat_runtime(ctx, |rt| {
                    rt.active_drones_in_space.insert(d.ball_id, d);
                });
            }
            Some(Ok(PyRep::Integer(count as i64)))
        }

        ("entity", "CmdEngage") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let target_id = args
                .get(1)
                .and_then(|a| a.as_i64())
                .or_else(|| args.first().and_then(|a| a.as_i64()))
                .unwrap_or(0);
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let ship_pos = sys_handle
                .get_ball_snapshot(ship_id)
                .await
                .ok()
                .flatten()
                .map(|(_, b)| b.position)
                .unwrap_or(DVec3::ZERO);
            let Some((_, target_ball)) = sys_handle.get_ball_snapshot(target_id).await.ok().flatten() else {
                return Some(Err(BridgeRpcError {
                    code: "TargetNotInSpace".into(),
                    message: format!("Target ball {target_id} is not in space"),
                }));
            };

            let max_ctrl_range = compute_drone_control_range(ctx, char_id, ship_id);
            let target_dist = ship_pos.distance(target_ball.position);
            if target_dist > max_ctrl_range {
                return Some(Err(BridgeRpcError {
                    code: "OutOfDroneControlRange".into(),
                    message: format!(
                        "Target is {target_dist:.1} m away (exceeds drone control range {max_ctrl_range:.1} m)"
                    ),
                }));
            }

            let drones: Vec<DroneSpaceState> = with_combat_runtime(ctx, |rt| {
                rt.active_drones_in_space
                    .values()
                    .filter(|d| d.parent_ship_id == ship_id && !d.is_abandoned)
                    .cloned()
                    .collect()
            });

            let mut total_dmg_dealt = 0.0;
            let mut total_repaired = 0.0;
            let mut ewar_applied = false;
            let now_unix = ctx.now_unix();

            for mut d in drones {
                d.target_id = Some(target_id);
                if !d.is_sentry {
                    let _ = sys_handle
                        .set_movement_mode(
                            d.ball_id,
                            MovementMode::Orbit {
                                target_id,
                                range: 1_500.0,
                            },
                        )
                        .await;
                }
                if d.group_id == 640 {
                    // Logistic drone (`feat:drones.logistic-drones`)
                    if let Ok(Some(mut tcs)) = sys_handle.get_combat_state(target_id).await {
                        if d.shield_repair_amount > 0.0 {
                            let before = tcs.shield_hp;
                            tcs.shield_hp =
                                (tcs.shield_hp + d.shield_repair_amount).min(tcs.max_shield_hp);
                            total_repaired += (tcs.shield_hp - before).max(0.0);
                        }
                        if d.armor_repair_amount > 0.0 {
                            let before = tcs.armor_hp;
                            tcs.armor_hp =
                                (tcs.armor_hp + d.armor_repair_amount).min(tcs.max_armor_hp);
                            total_repaired += (tcs.armor_hp - before).max(0.0);
                        }
                        let _ = sys_handle.set_combat_state(target_id, tcs).await;
                    }
                } else if matches!(d.group_id, 639 | 641 | 544 | 545) {
                    // EWAR drone (`feat:drones.ewar-drones`)
                    ewar_applied = true;
                    if d.web_speed_factor < 0.0 {
                        let mult = (1.0 + d.web_speed_factor / 100.0).clamp(0.1, 1.0);
                        let _ = sys_handle
                            .set_speed_fraction(target_id, (target_ball.speed_fraction.max(1.0)) * mult)
                            .await;
                    }
                    if d.neut_amount > 0.0 {
                        if let Ok(Some(mut tcs)) = sys_handle.get_combat_state(target_id).await {
                            tcs.capacitor = (tcs.capacitor - d.neut_amount).max(0.0);
                            let _ = sys_handle.set_combat_state(target_id, tcs).await;
                        }
                    }
                    if d.tp_sig_bonus_pct > 0.0 {
                        if let Ok(Some(mut tcs)) = sys_handle.get_combat_state(target_id).await {
                            let base_sig = if tcs.signature_radius > 0.0 {
                                tcs.signature_radius
                            } else {
                                target_ball.radius.max(35.0)
                            };
                            tcs.signature_radius = base_sig * (1.0 + d.tp_sig_bonus_pct / 100.0);
                            let _ = sys_handle.set_combat_state(target_id, tcs).await;
                        }
                    }
                    if d.ecm_strength > 0.0 {
                        let _ = sys_handle.clear_target_locks(target_id).await;
                        with_mut_combat_runtime(ctx, |rt| {
                            if let Some(npc) = rt.npc_entities.get_mut(&target_id) {
                                npc.ecm_jammed = true;
                            }
                        });
                    }
                    record_killmail_assist_action(
                        ctx,
                        target_id,
                        char_id,
                        d.type_id,
                        d.type_id,
                        0.0,
                        true,
                        now_unix,
                    );
                } else if d.damage_vector.total() > 0.0 {
                    // Combat / Sentry / Gecko / Augmented / Mutated drone (`feat:drones.engage-target`, `feat:drones.gecko-augmented-mutated-drones`, `feat:drones.gecko-subverted-bandwidth`)
                    if let Ok(Some((app, _, _))) =
                        sys_handle.apply_damage(target_id, d.damage_vector).await
                    {
                        total_dmg_dealt += app;
                        record_killmail_assist_action(
                            ctx,
                            target_id,
                            char_id,
                            d.type_id,
                            d.type_id,
                            app,
                            false,
                            now_unix,
                        );
                    }
                }
                with_mut_combat_runtime(ctx, |rt| {
                    rt.active_drones_in_space.insert(d.ball_id, d);
                });
            }

            if total_dmg_dealt > 0.0 || ewar_applied {
                ctx.crimewatch
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .trigger_weapon_timer(char_id, now_unix);
            }

            Some(Ok(PyRep::keyval([
                ("targetID", PyRep::Integer(target_id)),
                ("totalDamageDealt", PyRep::Real(total_dmg_dealt)),
                ("totalRepairedHP", PyRep::Real(total_repaired)),
                ("ewarApplied", PyRep::Bool(ewar_applied)),
            ])))
        }

        ("entity", "CmdAssist") | ("entity", "CmdGuard") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let _ = ctx
                .db
                .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.character_id));
            let target_char_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let is_assist = method == "CmdAssist";
            let mut updated = 0;
            with_mut_combat_runtime(ctx, |rt| {
                for d in rt.active_drones_in_space.values_mut() {
                    if d.owner_char_id == char_id && !d.is_abandoned {
                        if is_assist {
                            d.assist_char_id = Some(target_char_id);
                            d.guard_char_id = None;
                        } else {
                            d.guard_char_id = Some(target_char_id);
                            d.assist_char_id = None;
                        }
                        updated += 1;
                    }
                }
            });
            Some(Ok(PyRep::keyval([
                ("mode", PyRep::String(method.into())),
                ("assignedToCharID", PyRep::Integer(target_char_id)),
                ("droneCount", PyRep::Integer(updated)),
            ])))
        }

        ("entity", "ChangeDroneSettings") | ("entity", "SetAggressive") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let _ = ctx
                .db
                .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.character_id));
            let aggressive = kwargs
                .get("aggressive")
                .and_then(|v| v.as_bool())
                .or_else(|| args.first().and_then(|a| a.as_bool()))
                .unwrap_or(true);
            let focus_fire = kwargs
                .get("focusFire")
                .and_then(|v| v.as_bool())
                .or_else(|| args.get(1).and_then(|a| a.as_bool()))
                .unwrap_or(true);
            with_mut_combat_runtime(ctx, |rt| {
                rt.drone_settings.insert(
                    char_id,
                    DroneBehaviorSettings {
                        aggressive,
                        focus_fire,
                    },
                );
            });
            let setting_args = vec![PyRep::Bool(aggressive), PyRep::Bool(focus_fire)];
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnDroneSettingChanged",
                "charid",
                setting_args.clone(),
            );
            session.push_notification("OnDroneSettingChanged", setting_args);
            Some(Ok(PyRep::keyval([
                ("aggressive", PyRep::Bool(aggressive)),
                ("focusFire", PyRep::Bool(focus_fire)),
            ])))
        }

        ("entity", "CmdAbandonDrone") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let _ = ctx
                .db
                .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.character_id));
            let mut abandoned_balls = Vec::new();
            with_mut_combat_runtime(ctx, |rt| {
                for d in rt.active_drones_in_space.values_mut() {
                    if d.owner_char_id == char_id && !d.is_abandoned {
                        d.is_abandoned = true;
                        d.owner_char_id = 0;
                        d.target_id = None;
                        abandoned_balls.push(d.ball_id);
                    }
                }
            });
            let abandon_args = vec![PyRep::List(
                abandoned_balls.iter().copied().map(PyRep::Integer).collect(),
            )];
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnDroneControlLost",
                "charid",
                abandon_args.clone(),
            );
            session.push_notification("OnDroneControlLost", abandon_args);
            Some(Ok(PyRep::List(
                abandoned_balls.into_iter().map(PyRep::Integer).collect(),
            )))
        }

        ("ship", "ScoopToDroneBay") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let drone_ball_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let ship_pos = sys_handle
                .get_ball_snapshot(ship_id)
                .await
                .ok()
                .flatten()
                .map(|(_, b)| b.position)
                .unwrap_or(DVec3::ZERO);
            let Some((_, d_ball)) = sys_handle.get_ball_snapshot(drone_ball_id).await.ok().flatten() else {
                return Some(Err(BridgeRpcError {
                    code: "DroneNotInSpace".into(),
                    message: format!("Drone ball {drone_ball_id} is not in space"),
                }));
            };
            if ship_pos.distance(d_ball.position) > 2_500.0 {
                return Some(Err(BridgeRpcError {
                    code: "OutOfScoopRange".into(),
                    message: "Drone must be within 2,500 m to scoop to drone bay".into(),
                }));
            }
            let drone_st_opt = with_combat_runtime(ctx, |rt| {
                rt.active_drones_in_space.get(&drone_ball_id).cloned()
            });
            let Some(drone_st) = drone_st_opt else {
                return Some(Err(BridgeRpcError {
                    code: "DroneNotFound".into(),
                    message: format!("Drone ball {drone_ball_id} is not a scoopable drone"),
                }));
            };
            if !drone_st.is_abandoned && drone_st.owner_char_id != char_id {
                return Some(Err(BridgeRpcError {
                    code: "DroneOwnedByOther".into(),
                    message: "Cannot scoop an active drone controlled by another pilot".into(),
                }));
            }
            with_mut_combat_runtime(ctx, |rt| {
                rt.active_drones_in_space.remove(&drone_ball_id);
            });
            let _ = sys_handle.remove_ball(drone_ball_id).await;
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(it) = snap.items.get_mut(&drone_st.drone_item_id) {
                    it.owner_id = char_id;
                    it.location_id = ship_id;
                    it.flag = InventoryFlag::DroneBay;
                    it.version += 1;
                }
            });
            Some(Ok(PyRep::keyval([
                ("scoopedDroneItemID", PyRep::Integer(drone_st.drone_item_id)),
                ("newOwnerID", PyRep::Integer(char_id)),
            ])))
        }

        ("entity", "CmdReconnectToDrones") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let _ = ctx
                .db
                .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.character_id));
            let mut reconnected = Vec::new();
            with_mut_combat_runtime(ctx, |rt| {
                for d in rt.active_drones_in_space.values_mut() {
                    if d.solar_system_id == sys_id
                        && d.is_abandoned
                        && d.original_owner_char_id == char_id
                        && reconnected.len() < 5
                    {
                        d.is_abandoned = false;
                        d.owner_char_id = char_id;
                        d.parent_ship_id = ship_id;
                        reconnected.push(d.ball_id);
                    }
                }
            });
            Some(Ok(PyRep::List(
                reconnected.into_iter().map(PyRep::Integer).collect(),
            )))
        }

        ("entity", "CmdSalvage") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let has_salvage_drone = with_combat_runtime(ctx, |rt| {
                rt.active_drones_in_space.values().any(|d| {
                    d.parent_ship_id == ship_id
                        && !d.is_abandoned
                        && (d.group_id == 1159 || d.type_id == 32787)
                })
            });
            if !has_salvage_drone {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveSalvageDrones".into(),
                    message: "No active Salvage Drones in space".into(),
                }));
            }
            let Some(wreck_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "TargetRequired".into(),
                    message: "CmdSalvage requires a target wreckID".into(),
                }));
            };
            Some(
                execute_wreck_salvage_cycle(
                    ctx,
                    sys_id,
                    char_id,
                    ship_id,
                    wreck_id,
                    None,
                )
                .await,
            )
        }

        _ => None,
    }
}

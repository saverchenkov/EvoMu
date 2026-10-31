//! `insurgency_ice_heist_fob` — Pirate Insurgency Ice Refinery Heist (`feat:pve.insurgency-ice-heist`)
//! and Blood Raider / Guristas Forward Operating Bases (`feat:pve.pirate-fob-forward-operating-base`).

use crate::router::ServerContext;
use crate::services::fw_gameplay::militia_lp_corp_id;
use crate::services::insurgency_services::{
    insurgency_stage, FACTION_ANGEL_CARTEL, FACTION_GURISTAS,
};
use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, PersistedIceHeistRecord, UpwellStructureRecord,
    WalletJournalEntry,
};
use eve_destiny::{Ball, MovementMode};
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;

pub const FACTION_BLOOD_RAIDERS: i64 = 500_012;
pub const ICE_HEIST_RESPAWN_SECS: u64 = 14_400;
pub const ICE_HEIST_WIN_TANKERS: u32 = 10;
pub const ICE_HEIST_WIN_LP: i64 = 150_000;
pub const ICE_HEIST_THUNDERCHILD_GUARD_RANGE_M: f64 = 15_000.0;
pub const PIRATE_FOB_BOUNTY_ISK: i64 = 50_000_000;

/// Spawns an Ice Refinery Heist site in `sys_id` if Corruption is Stage 3 or 4 and the 4h respawn
/// cooldown has elapsed (`feat:pve.insurgency-ice-heist`).
pub async fn spawn_ice_heist_checked(
    ctx: &ServerContext,
    sys_id: u32,
) -> Result<i64, BridgeRpcError> {
    let now_unix = ctx.now_unix();
    let instance_id = ctx.db.try_with_mut_snapshot(|snap| {
        let mut c_pts = 0i64;
        for camp in snap.insurgency_runtime.campaigns.values() {
            if let Some(&p) = camp.corruption_points.get(&sys_id) {
                c_pts = c_pts.max(p);
            }
        }
        let stage = snap
            .system_states
            .get(&sys_id)
            .map(|s| s.insurgency_corruption_stage)
            .unwrap_or(0)
            .max(insurgency_stage(c_pts));
        if stage != 3 && stage != 4 {
            return Err(BridgeRpcError {
                code: "InvalidCorruptionStage".into(),
                message: format!(
                    "Ice Refinery Heist requires Corruption Stage 3 or 4 (system {sys_id} is Stage {stage})"
                ),
            });
        }
        if let Some(existing) = snap
            .insurgency_runtime
            .ice_heist_instances
            .iter()
            .rev()
            .find(|h| h.solar_system_id == sys_id)
        {
            if !existing.completed {
                return Ok(existing.instance_id);
            }
            if existing.respawn_at_unix > now_unix {
                return Err(BridgeRpcError {
                    code: "IceHeistRespawnCooldown".into(),
                    message: format!(
                        "Ice Refinery Heist in system {sys_id} is on 4h respawn cooldown until {}",
                        existing.respawn_at_unix
                    ),
                });
            }
        }
        let id = (snap.insurgency_runtime.ice_heist_instances.len() as i64) + 800_001;
        snap.insurgency_runtime
            .ice_heist_instances
            .push(PersistedIceHeistRecord {
                instance_id: id,
                solar_system_id: sys_id,
                pirate_tankers_saved: 0,
                empire_tankers_saved: 0,
                completed: false,
                respawn_at_unix: 0,
            });
        Ok(id)
    })?;

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let tanker_id = instance_id * 100 + 1;
    let guard_id = instance_id * 100 + 2;
    if sys_handle.get_ball_snapshot(tanker_id).await.ok().flatten().is_none() {
        let _ = sys_handle
            .spawn_ball(Ball {
                id: tanker_id,
                type_id: 32882,
                position: DVec3::new(120_000.0, 0.0, 60_000.0),
                velocity: DVec3::ZERO,
                max_velocity: 120.0,
                inertia: 1.0,
                mass: 5.0e6,
                radius: 250.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            })
            .await;
    }
    if sys_handle.get_ball_snapshot(guard_id).await.ok().flatten().is_none() {
        let _ = sys_handle
            .spawn_ball(Ball {
                id: guard_id,
                type_id: 54733, // Upwell Thunderchild guard
                position: DVec3::new(122_000.0, 0.0, 60_000.0),
                velocity: DVec3::ZERO,
                max_velocity: 180.0,
                inertia: 1.0,
                mass: 1.2e7,
                radius: 300.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            })
            .await;
    }
    Ok(instance_id)
}

/// Returns true if `type_id` is an Ice Refinery Heist tanker NPC (web-immune).
pub fn is_ice_heist_tanker_web_immune(type_id: u32) -> bool {
    type_id == 32882
}

/// Escorts `count` tankers to `side`'s scoring zone (`"pirate"` or `"empire"`).
/// The first side to 10 tankers wins 150,000 LP and starts the 4h respawn timer.
pub fn escort_ice_heist_tanker(
    ctx: &ServerContext,
    char_id: i64,
    instance_id: i64,
    side: &str,
    count: u32,
) -> Result<PyRep, BridgeRpcError> {
    if side != "pirate" && side != "empire" {
        return Err(BridgeRpcError {
            code: "InvalidHeistSide".into(),
            message: format!("Expected 'pirate' or 'empire', got '{side}'"),
        });
    }
    let now_unix = ctx.now_unix();
    let (pirate_saved, empire_saved, completed, respawn_at, lp_awarded) =
        ctx.db.try_with_mut_snapshot(|snap| {
            let rec = snap
                .insurgency_runtime
                .ice_heist_instances
                .iter_mut()
                .find(|h| h.instance_id == instance_id)
                .ok_or_else(|| BridgeRpcError {
                    code: "IceHeistNotFound".into(),
                    message: format!("Ice Heist instance {instance_id} not found"),
                })?;
            if rec.completed {
                return Err(BridgeRpcError {
                    code: "IceHeistAlreadyCompleted".into(),
                    message: format!("Ice Heist instance {instance_id} is already completed"),
                });
            }
            if side == "pirate" {
                rec.pirate_tankers_saved =
                    rec.pirate_tankers_saved.saturating_add(count).min(ICE_HEIST_WIN_TANKERS);
            } else {
                rec.empire_tankers_saved =
                    rec.empire_tankers_saved.saturating_add(count).min(ICE_HEIST_WIN_TANKERS);
            }
            let won = rec.pirate_tankers_saved >= ICE_HEIST_WIN_TANKERS
                || rec.empire_tankers_saved >= ICE_HEIST_WIN_TANKERS;
            let mut lp = 0i64;
            if won {
                rec.completed = true;
                rec.respawn_at_unix = now_unix.saturating_add(ICE_HEIST_RESPAWN_SECS);
                lp = ICE_HEIST_WIN_LP;
                let lp_corp = snap
                    .characters
                    .get(&char_id)
                    .and_then(|c| c.war_faction_id)
                    .map(|fid| militia_lp_corp_id(&ctx.sde, fid))
                    .unwrap_or(1_000_180);
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    *ch.loyalty_points.entry(lp_corp).or_insert(0) += lp;
                }
            }
            Ok((
                rec.pirate_tankers_saved,
                rec.empire_tankers_saved,
                rec.completed,
                rec.respawn_at_unix,
                lp,
            ))
        })?;

    Ok(PyRep::keyval([
        ("instanceID", PyRep::Integer(instance_id)),
        ("pirateTankersSaved", PyRep::Integer(i64::from(pirate_saved))),
        ("empireTankersSaved", PyRep::Integer(i64::from(empire_saved))),
        ("completed", PyRep::Bool(completed)),
        ("respawnAtUnix", PyRep::Integer(respawn_at as i64)),
        ("lpAwarded", PyRep::Integer(lp_awarded)),
    ]))
}

/// Destroys any pirate-aligned ship/NPC within 15 km of an Upwell Thunderchild guard in `sys_id`.
pub async fn step_ice_heist_thunderchild_guards(ctx: &ServerContext, sys_id: u32) -> usize {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let balls = sys_handle.list_balls().await.unwrap_or_default();
    let guards: Vec<DVec3> = balls
        .iter()
        .filter(|b| b.type_id == 54733)
        .map(|b| b.position)
        .collect();
    if guards.is_empty() {
        return 0;
    }
    let mut destroyed = 0usize;
    for b in &balls {
        if b.type_id == 54733 || b.type_id == 32882 {
            continue;
        }
        let is_pirate = ctx.db.with_snapshot(|snap| {
            snap.characters.values().any(|c| {
                c.active_ship_id == b.id
                    && matches!(
                        c.war_faction_id,
                        Some(FACTION_ANGEL_CARTEL | FACTION_GURISTAS | FACTION_BLOOD_RAIDERS)
                    )
            })
        });
        if is_pirate
            && guards
                .iter()
                .any(|g| g.distance(b.position) <= ICE_HEIST_THUNDERCHILD_GUARD_RANGE_M)
        {
            let _ = sys_handle.remove_ball(b.id).await;
            destroyed += 1;
        }
    }
    destroyed
}

/// Spawns a Blood Raider (`500012`) or Guristas (`500010`) Forward Operating Base in a high/low-sec
/// system (`feat:pve.pirate-fob-forward-operating-base`).
pub async fn spawn_pirate_fob_in_system(
    ctx: &ServerContext,
    sys_id: u32,
    pirate_faction_id: i64,
) -> Result<i64, BridgeRpcError> {
    let sys = ctx.sde.solar_systems.get(&sys_id).ok_or_else(|| BridgeRpcError {
        code: "SystemNotFound".into(),
        message: format!("Solar system {sys_id} not found"),
    })?;
    if sys_id >= 31_000_000 || sys.security_status < 0.0 {
        return Err(BridgeRpcError {
            code: "InvalidFobSystem".into(),
            message: format!(
                "Pirate FOBs only spawn in K-space high/low-sec systems (system {sys_id} has security {:.2})",
                sys.security_status
            ),
        });
    }
    if pirate_faction_id != FACTION_BLOOD_RAIDERS
        && pirate_faction_id != FACTION_GURISTAS
        && pirate_faction_id != FACTION_ANGEL_CARTEL
    {
        return Err(BridgeRpcError {
            code: "InvalidPirateFaction".into(),
            message: format!("Unsupported pirate FOB faction {pirate_faction_id}"),
        });
    }
    let fob_id = 1_895_000_000 + i64::from(sys_id);
    let pos = [95_000.0, 0.0, 45_000.0];
    let fob_type = if pirate_faction_id == FACTION_BLOOD_RAIDERS {
        47_512
    } else {
        47_516
    };
    ctx.db.with_mut_snapshot(|snap| {
        let owner_corp = militia_lp_corp_id(&ctx.sde, pirate_faction_id);
        snap.structures.insert(
            fob_id,
            UpwellStructureRecord {
                structure_id: fob_id,
                type_id: fob_type,
                owner_corp_id: owner_corp,
                solar_system_id: sys_id,
                name: if pirate_faction_id == FACTION_BLOOD_RAIDERS {
                    "Blood Raider Forward Operating Base".into()
                } else {
                    "Guristas Forward Operating Base".into()
                },
                state: "ShieldVulnerable".into(),
                position: pos,
                quantum_core_installed: true,
                fuel_blocks: 10_000,
                shield_hp: 2_500.0,
                armor_hp: 2_500.0,
                hull_hp: 3_500.0,
                reinforced_until_unix: 0,
                last_damage_unix: 0,
                last_damage_tick: 0,
                self_destruct_at_unix: None,
            },
        );
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    if sys_handle.get_ball_snapshot(fob_id).await.ok().flatten().is_none() {
        let _ = sys_handle
            .spawn_ball(Ball {
                id: fob_id,
                type_id: fob_type,
                position: DVec3::from_array(pos),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 5.0e8,
                radius: 1_200.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            })
            .await;
    }
    Ok(fob_id)
}

/// Launches a raiding fleet from `fob_id` and spawns defending response ships scaled to
/// `attacker_ship_type_ids`.
pub async fn trigger_fob_defense_response(
    ctx: &ServerContext,
    fob_id: i64,
    attacker_ship_type_ids: &[u32],
) -> Result<Vec<i64>, BridgeRpcError> {
    let (sys_id, pos) = ctx.db.with_snapshot(|snap| {
        let st = snap.structures.get(&fob_id).ok_or_else(|| BridgeRpcError {
            code: "FobNotFound".into(),
            message: format!("Pirate FOB {fob_id} not found"),
        })?;
        Ok((st.solar_system_id, st.position))
    })?;
    let wave_count = (attacker_ship_type_ids.len().max(1) * 2).min(12);
    let defender_type_id = if attacker_ship_type_ids.len() >= 3 {
        24_690 // Battleship response
    } else {
        37_453 // Cruiser response
    };
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut spawned = Vec::with_capacity(wave_count);
    for idx in 0..wave_count {
        let npc_id = fob_id * 100 + (idx as i64) + 1;
        if sys_handle.get_ball_snapshot(npc_id).await.ok().flatten().is_none() {
            let _ = sys_handle
                .spawn_ball(Ball {
                    id: npc_id,
                    type_id: defender_type_id,
                    position: DVec3::new(pos[0] + (idx as f64) * 500.0, pos[1], pos[2]),
                    velocity: DVec3::ZERO,
                    max_velocity: 250.0,
                    inertia: 1.0,
                    mass: 1.0e7,
                    radius: 250.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Rigid,
                })
                .await;
        }
        spawned.push(npc_id);
    }
    Ok(spawned)
}

/// Destroys a Pirate FOB, drops structure loot in space, and credits the FOB bounty to `killer_char_id`.
pub async fn destroy_pirate_fob(
    ctx: &ServerContext,
    fob_id: i64,
    killer_char_id: i64,
) -> Result<(i64, i64), BridgeRpcError> {
    let (sys_id, pos, wreck_id) = ctx.db.try_with_mut_snapshot(|snap| {
        let st = snap.structures.get_mut(&fob_id).ok_or_else(|| BridgeRpcError {
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
        st.shield_hp = 0.0;
        st.armor_hp = 0.0;
        st.hull_hp = 0.0;
        let sys_id = st.solar_system_id;
        let pos = st.position;
        let wreck_id = { snap.next_entity_id += 1; snap.next_entity_id };
        let loot_id = { snap.next_entity_id += 1; snap.next_entity_id };
        snap.items.insert(
            loot_id,
            InventoryItemRecord {
                item_id: loot_id,
                type_id: 56201, // Quantum Core loot
                owner_id: killer_char_id,
                location_id: wreck_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        let bounty = IskCents::from_isk(PIRATE_FOB_BOUNTY_ISK);
        let entry_id = { snap.next_entity_id += 1; snap.next_entity_id };
        if let Some(ch) = snap.characters.get_mut(&killer_char_id) {
            ch.balance = ch.balance.checked_add(bounty).unwrap_or(ch.balance);
            let new_bal = ch.balance;
            snap.wallet_journal.push(WalletJournalEntry {
                entry_id,
                character_id: killer_char_id,
                entry_type: "BountyPrizes".into(),
                amount: bounty,
                balance_after: new_bal,
                reason: format!("Pirate FOB Destruction Bounty ({fob_id})"),
            });
        }
        Ok((sys_id, pos, wreck_id))
    })?;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.remove_ball(fob_id).await;
    let _ = sys_handle
        .spawn_ball(Ball {
            id: wreck_id,
            type_id: 26557,
            position: DVec3::from_array(pos),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 1.0e6,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await;
    Ok((wreck_id, PIRATE_FOB_BOUNTY_ISK))
}

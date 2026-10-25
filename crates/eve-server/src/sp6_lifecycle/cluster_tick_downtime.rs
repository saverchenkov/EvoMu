//! Daily downtime wreck/mission maintenance (`feat:lifecycle.wreck-downtime`, `feat:lifecycle.mission-downtime-respawn`).

use crate::router::ServerContext;
use crate::services::combat_crimewatch::with_mut_combat_runtime;
use super::with_mut_sp6_state;
use eve_destiny::{Ball, MovementMode, SlimItemMeta};
use glam::DVec3;
use std::collections::HashMap;

/// Daily downtime maintenance: purges stale NPC wrecks (`feat:lifecycle.wreck-downtime`)
/// and resets uncompleted ("Accepted") missions by despawning remaining NPCs and restoring
/// initial room/NPC state (`feat:lifecycle.mission-downtime-respawn`).
// parity: feat:lifecycle.wreck-downtime, feat:lifecycle.mission-downtime-respawn
pub fn cleanup_npc_wrecks_and_reset_missions_on_downtime(ctx: &ServerContext) -> (usize, usize) {
    let mut removed_wreck_balls: Vec<(u32, i64)> = Vec::new();
    let mut removed_mission_npcs: Vec<(u32, i64)> = Vec::new();
    let mut missions_reset_count: usize = 0;

    with_mut_sp6_state(&ctx.db, |st, snap| {
        let npc_wreck_ids: Vec<(i64, u32)> = snap
            .items
            .values()
            .filter(|i| {
                let is_wreck = i.type_id == 26557
                    || ctx
                        .sde
                        .types
                        .get(&i.type_id)
                        .is_some_and(|t| t.group_id == 186);
                let is_npc_wreck = st
                    .space_object_meta
                    .get(&i.item_id)
                    .is_some_and(|m| m.is_npc_wreck);
                is_wreck && is_npc_wreck && (30_000_000..=34_000_000).contains(&i.location_id)
            })
            .map(|i| (i.item_id, i.location_id as u32))
            .collect();

        for (wid, sys_id) in npc_wreck_ids {
            snap.items.remove(&wid);
            snap.item_positions.remove(&wid);
            st.space_object_meta.remove(&wid);
            st.ship_combat_hp.remove(&wid);
            snap.items.retain(|_, child| child.location_id != wid);
            removed_wreck_balls.push((sys_id, wid));
        }

        // Despawn surviving mission NPCs and reset uncompleted ("Accepted") missions (`feat:lifecycle.mission-downtime-respawn`)
        for mission in snap.active_missions.values_mut() {
            if mission.state == "Accepted" {
                let target_count = (mission.spawned_npc_ball_ids.len() as u32)
                    .max(mission.npcs_remaining)
                    .max(4);
                for &old_bid in &mission.spawned_npc_ball_ids {
                    removed_mission_npcs.push((mission.solar_system_id, old_bid));
                }
                mission.spawned_npc_ball_ids.clear();
                mission.npcs_remaining = target_count;
                mission.current_room_id = 1;
                let key = (mission.character_id, mission.agent_id as u32);
                if !st.missions_awaiting_downtime_respawn.contains(&key) {
                    st.missions_awaiting_downtime_respawn.push(key);
                }
                missions_reset_count += 1;
            }
        }
    });

    let wrecks_removed_count = removed_wreck_balls.len();
    if !removed_wreck_balls.is_empty() || !removed_mission_npcs.is_empty() {
        with_mut_combat_runtime(ctx, |rt| {
            for &(_, wid) in &removed_wreck_balls {
                rt.container_lifetimes.remove(&wid);
                rt.npc_entities.remove(&wid);
            }
            for &(_, bid) in &removed_mission_npcs {
                rt.npc_entities.remove(&bid);
            }
        });
    }
    for (sys_id, ball_id) in removed_wreck_balls.into_iter().chain(removed_mission_npcs) {
        let handle = ctx.systems.get_or_spawn(sys_id);
        handle.try_remove_ball(ball_id);
    }

    (wrecks_removed_count, missions_reset_count)
}

/// Spawns the initial wave of SDE-backed pirate NPCs when a pilot warps to their accepted mission pocket
/// after daily downtime reset (`feat:lifecycle.mission-downtime-respawn`).
pub fn spawn_mission_wave_on_pocket_entry_if_needed(
    ctx: &ServerContext,
    char_id: i64,
    solar_system_id: u32,
    pocket_origin: DVec3,
) -> usize {
    let default_npc_tid =
        crate::services::agents_missions::agents_missions_helpers::resolve_mission_npc_type(ctx)
            .max(10_019);
    let mut spawned_npcs: Vec<(u32, i64, u32, DVec3)> = Vec::new();

    with_mut_sp6_state(&ctx.db, |st, snap| {
        for (&(cid, aid), mission) in snap.active_missions.iter_mut() {
            if cid != char_id
                || mission.state != "Accepted"
                || mission.solar_system_id != solar_system_id
            {
                continue;
            }
            let awaiting = st.missions_awaiting_downtime_respawn.contains(&(cid, aid));
            let needs_respawn =
                awaiting || (mission.spawned_npc_ball_ids.is_empty() && mission.npcs_remaining > 0);
            if !needs_respawn {
                continue;
            }
            st.missions_awaiting_downtime_respawn
                .retain(|&k| k != (cid, aid));
            let wave_count = mission.npcs_remaining.max(4);
            mission.npcs_remaining = wave_count;
            mission.current_room_id = 1;
            mission.spawned_npc_ball_ids.clear();
            for idx in 0..wave_count {
                snap.next_entity_id += 1;
                let npc_bid = snap.next_entity_id;
                mission.spawned_npc_ball_ids.push(npc_bid);
                let angle = (idx as f64) * std::f64::consts::TAU / (wave_count as f64);
                let pos =
                    pocket_origin + DVec3::new(angle.cos() * 4_500.0, 0.0, angle.sin() * 4_500.0);
                spawned_npcs.push((solar_system_id, npc_bid, default_npc_tid, pos));
            }
        }
    });

    let count = spawned_npcs.len();
    for (sys_id, npc_bid, npc_type_id, pos) in spawned_npcs {
        let handle = ctx.systems.get_or_spawn(sys_id);
        let sde_type = ctx.sde.types.get(&npc_type_id);
        let (group_id, category_id, faction_id, max_vel, mass, radius, inertia, opt_range, bounty, dmg_vec) =
            sde_type
                .map(|t| {
                    let mv = t.default_attributes.get(&37).copied().unwrap_or(320.0);
                    let agi = t.default_attributes.get(&70).copied().unwrap_or(2.0);
                    let opt = t.default_attributes.get(&54).copied().unwrap_or(10_000.0);
                    let bty = t.default_attributes.get(&481).copied().unwrap_or(25_000.0);
                    let mult = t.default_attributes.get(&64).copied().unwrap_or(1.0).max(0.5);
                    let em = t.default_attributes.get(&114).copied().unwrap_or(12.0) * mult;
                    let ex = t.default_attributes.get(&116).copied().unwrap_or(12.0) * mult;
                    let ki = t.default_attributes.get(&117).copied().unwrap_or(15.0) * mult;
                    let th = t.default_attributes.get(&118).copied().unwrap_or(15.0) * mult;
                    (
                        t.group_id,
                        t.category_id,
                        t.faction_id.unwrap_or(500_010),
                        mv.max(150.0),
                        t.mass.max(100_000.0),
                        t.radius.max(30.0),
                        agi.max(0.5),
                        opt.max(5_000.0),
                        bty.max(10_000.0),
                        eve_destiny::DamageVector::new(em, th, ki, ex),
                    )
                })
                .unwrap_or((
                    546,
                    11,
                    500_010,
                    320.0,
                    1_200_000.0,
                    45.0,
                    2.0,
                    10_000.0,
                    25_000.0,
                    eve_destiny::DamageVector::new(12.0, 15.0, 15.0, 12.0),
                ));
        let npc_corp_id = ctx
            .sde
            .npc_corporations
            .values()
            .find(|c| c.faction_id == Some(faction_id))
            .map(|c| c.corporation_id as i64)
            .unwrap_or(1_000_127);
        let cs = ctx.build_type_combat_state(npc_type_id);
        handle.try_spawn_ball_with_meta(
            Ball {
                id: npc_bid,
                type_id: npc_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: max_vel,
                inertia,
                mass,
                radius,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
            SlimItemMeta {
                group_id,
                category_id,
                owner_id: faction_id as i64,
                char_id: None,
                corp_id: npc_corp_id,
                alliance_id: -1,
                security_status: -5.0,
                is_cloaked: false,
            },
        );
        with_mut_combat_runtime(ctx, |rt| {
            rt.npc_entities.insert(
                npc_bid,
                crate::services::combat_crimewatch::ExtendedNpcEntity {
                    ball_id: npc_bid,
                    type_id: npc_type_id,
                    faction_id,
                    archetype: "Pirate".into(),
                    solar_system_id: sys_id,
                    spawn_group_id: 1,
                    target_id: None,
                    aggression_threat: HashMap::new(),
                    threat_table: HashMap::new(),
                    bounty_isk: bounty,
                    optimal_range: opt_range,
                    orbit_range: (opt_range * 0.6).clamp(2_500.0, 15_000.0),
                    damage_vector: dmg_vec,
                    entropic_cycles: 0,
                    drifter_overshield_hp: 0.0,
                    drifter_doomsday_fired: false,
                    web_speed_factor: 0.0,
                    web_range: 0.0,
                    scram_strength: 0.0,
                    scram_range: 0.0,
                    neut_amount: 0.0,
                    ecm_jammed: false,
                    remote_shield_rep: 0.0,
                    remote_armor_rep: 0.0,
                    remote_rep_range: 0.0,
                    tag_drop_type_id: None,
                    custom_cargo_loot: None,
                    spawn_position: pos,
                    sleeper_wh_class: 0,
                },
            );
        });
    }
    count
}

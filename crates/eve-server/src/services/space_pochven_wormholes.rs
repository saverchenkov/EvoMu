//! `space_pochven_wormholes` — Pochven entry/exit wormholes and non-deadspace geometry.
//!
//! Implements:
//! - `feat:navigation.pochven-wormholes`:
//!   - Wormholes connecting Pochven and k-space/w-space.
//!   - Cosmic signature sits ~30 km from the actual wormhole ball (non-deadspace grid).
//!   - Exit K162 opens when scanned from the Pochven side.
//!   - Guard NPCs spawned: Triglavians (48087, 48092) outside Pochven, Pochven mix
//!     (48087 Triglavian, 47958 Drifter, 48245 Sleeper, 47876 Rogue Drone) inside Pochven.

#![forbid(unsafe_code)]

use glam::DVec3;
use eve_db::CosmicSignatureRecord;
use eve_destiny::{Ball, MovementMode};
use crate::router::ServerContext;
use super::space_docking_cloaking::{make_wh_sig, pick_system_celestial_position, wormhole_exit_item_id_for_signature};
use eve_sde::SolarSystemId;

pub fn is_pochven_wormhole(sde: &eve_sde::SdeRegistry, sig: &CosmicSignatureRecord) -> bool {
    matches!(
        sig.wormhole_type_id,
        Some(56_026 | 56_535..=56_565 | 92_287..=92_289 | 97_881..=97_882)
    ) || sde.solar_systems.get(&sig.solar_system_id).map(|s| s.region_id) == Some(10_000_070)
        || sig
            .destination_system_id
            .and_then(|d| sde.solar_systems.get(&d))
            .map(|s| s.region_id)
            == Some(10_000_070)
}

pub fn pochven_wormhole_ball_position(
    sde: &eve_sde::SdeRegistry,
    sig: &CosmicSignatureRecord,
) -> DVec3 {
    let base = DVec3::from_array(sig.position);
    if is_pochven_wormhole(sde, sig) {
        base + DVec3::new(30_000.0, 0.0, 0.0)
    } else {
        base
    }
}

pub fn pochven_static_wormholes(
    sys_id: SolarSystemId,
    seq: u32,
    now_unix: u64,
) -> Vec<CosmicSignatureRecord> {
    let mut out = vec![make_wh_sig(
        format!("PCH-{}", 100 + ((sys_id + seq * 19) % 900)),
        sys_id,
        "Wormhole C729".into(),
        [11.0, 2.0, -3.5],
        56_026,
        30_000_144,
        now_unix,
        43_200,
        1_000_000_000.0,
        410_000_000.0,
    )];
    if seq == 0 {
        out.push(make_wh_sig(
            format!("PCW-{}", 200 + ((sys_id + 23) % 700)),
            sys_id,
            "Wormhole R081".into(),
            [-9.5, 3.0, 5.5],
            56_541,
            31_001_500,
            now_unix,
            43_200,
            1_000_000_000.0,
            375_000_000.0,
        ));
    }
    out
}

pub async fn open_pochven_wormhole_and_spawn_guards(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    sig: &CosmicSignatureRecord,
    wh_item_id: i64,
) -> (DVec3, Vec<i64>) {
    let is_pochven_side = ctx.sde.solar_systems.get(&sys_id).map(|s| s.region_id) == Some(10_000_070);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    if let Ok(Some((_, ball))) = sys_handle.get_ball_snapshot(wh_item_id).await {
        let guard_types: &[u32] = if is_pochven_side {
            &[48_087, 47_958, 48_245, 47_876]
        } else {
            &[48_087, 48_092]
        };
        let guard_ids = guard_types.iter().enumerate().map(|(idx, _)| wh_item_id.saturating_add(10 + idx as i64)).collect();
        return (ball.position, guard_ids);
    }
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(list) = snap.cosmic_signatures.get_mut(&sys_id) {
            if let Some(s) = list.iter_mut().find(|s| s.signature_id == sig.signature_id) {
                s.signal_strength = 1.0;
            }
        }
    });
    let wh_pos = pochven_wormhole_ball_position(&ctx.sde, sig);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: wh_item_id,
            type_id: sig.wormhole_type_id.unwrap_or(56_026),
            position: wh_pos,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: sig.remaining_mass_kg.max(1.0),
            radius: 1500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await;
    let is_pochven_side = ctx.sde.solar_systems.get(&sys_id).map(|s| s.region_id) == Some(10_000_070);
    if is_pochven_side {
        if let Some(dest_sys) = sig.destination_system_id {
            let exit_id = wormhole_exit_item_id_for_signature(dest_sys, &sig.signature_id);
            let (exit_sig_pos, _) = pick_system_celestial_position(ctx, dest_sys, (wh_item_id as u32).wrapping_add(1));
            let k162_sig_id = format!("K162-{}", sig.signature_id);
            ctx.db.with_mut_snapshot(|snap| {
                let d_list = snap.cosmic_signatures.entry(dest_sys).or_default();
                if !d_list.iter().any(|s| s.signature_id == k162_sig_id) {
                    d_list.push(CosmicSignatureRecord {
                        signature_id: k162_sig_id,
                        solar_system_id: dest_sys,
                        sig_group: "Wormhole".into(),
                        type_name: "Unstable Wormhole (K162)".into(),
                        position: exit_sig_pos,
                        signal_strength: 0.0,
                        wormhole_type_id: Some(26_272),
                        destination_system_id: Some(sys_id),
                        created_unix: sig.created_unix,
                        lifetime_seconds: sig.lifetime_seconds,
                        max_mass_kg: sig.max_mass_kg,
                        remaining_mass_kg: sig.remaining_mass_kg,
                        max_jump_mass_kg: sig.max_jump_mass_kg,
                        collapsed: false,
                    });
                }
            });
            let exit_ball_pos = DVec3::from_array(exit_sig_pos) + DVec3::new(30_000.0, 0.0, 0.0);
            let dest_handle = ctx.systems.get_or_spawn(dest_sys);
            let _ = dest_handle
                .spawn_ball(Ball {
                    id: exit_id,
                    type_id: 26_272,
                    position: exit_ball_pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: sig.remaining_mass_kg.max(1.0),
                    radius: 1500.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Rigid,
                })
                .await;
            for (idx, npc_tid) in [48_087_u32, 48_092].into_iter().enumerate() {
                let gid = exit_id.saturating_add(10 + idx as i64);
                let g_pos = exit_ball_pos + DVec3::new(2_000.0 + (idx as f64) * 1_500.0, 0.0, 0.0);
                let _ = dest_handle
                    .spawn_ball_with_combat(
                        Ball {
                            id: gid,
                            type_id: npc_tid,
                            position: g_pos,
                            velocity: DVec3::ZERO,
                            max_velocity: 350.0,
                            inertia: 1.0,
                            mass: 1_200_000.0,
                            radius: 65.0,
                            speed_fraction: 0.0,
                            mode: MovementMode::Stop,
                        },
                        ctx.build_type_combat_state(npc_tid),
                    )
                    .await;
                register_pochven_guard_npc(ctx, gid, npc_tid, dest_sys, g_pos);
            }
        }
    }
    let guard_types: &[u32] = if is_pochven_side {
        &[48_087, 47_958, 48_245, 47_876]
    } else {
        &[48_087, 48_092]
    };
    let mut guard_ids = Vec::with_capacity(guard_types.len());
    for (idx, &npc_tid) in guard_types.iter().enumerate() {
        let gid = wh_item_id.saturating_add(10 + idx as i64);
        let g_pos = wh_pos + DVec3::new(2_000.0 + (idx as f64) * 1_500.0, 0.0, 0.0);
        let _ = sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id: gid,
                    type_id: npc_tid,
                    position: g_pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 350.0,
                    inertia: 1.0,
                    mass: 1_200_000.0,
                    radius: 65.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                ctx.build_type_combat_state(npc_tid),
            )
            .await;
        register_pochven_guard_npc(ctx, gid, npc_tid, sys_id, g_pos);
        guard_ids.push(gid);
    }
    (wh_pos, guard_ids)
}

fn register_pochven_guard_npc(
    ctx: &ServerContext,
    gid: i64,
    tid: u32,
    sys_id: SolarSystemId,
    pos: DVec3,
) {
    let fac = if tid == 48_087 || tid == 48_092 { 500_026 } else { 500_024 };
    let archetype = if fac == 500_026 { "Triglavian" } else { "PochvenHostile" };
    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        rt.npc_entities.insert(
            gid,
            crate::services::combat_crimewatch::ExtendedNpcEntity {
                ball_id: gid,
                type_id: tid,
                faction_id: fac,
                archetype: archetype.into(),
                solar_system_id: sys_id,
                spawn_group_id: 888_001,
                target_id: None,
                aggression_threat: std::collections::HashMap::new(),
                threat_table: std::collections::HashMap::new(),
                bounty_isk: 50_000.0,
                optimal_range: 15_000.0,
                orbit_range: 10_000.0,
                damage_vector: eve_destiny::DamageVector {
                    em: 50.0,
                    thermal: 50.0,
                    kinetic: 50.0,
                    explosive: 50.0,
                },
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
                spawn_position: pos,
                sleeper_wh_class: 0,
                custom_cargo_loot: None,
            },
        );
    });
}

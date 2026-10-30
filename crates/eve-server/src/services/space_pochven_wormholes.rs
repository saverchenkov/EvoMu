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
use eve_proto::BridgeRpcError;
use crate::router::ServerContext;
use super::space_docking_cloaking::{make_wh_sig, pick_system_celestial_position, wormhole_exit_item_id_for_signature};
use eve_sde::SolarSystemId;
use std::collections::{BTreeMap, BTreeSet};

pub const POCHVEN_REGION_ID: u32 = 10_000_070;

pub fn is_pochven_wormhole(sde: &eve_sde::SdeRegistry, sig: &CosmicSignatureRecord) -> bool {
    matches!(
        sig.wormhole_type_id,
        Some(56_026 | 56_535..=56_565 | 92_287..=92_289 | 97_881..=97_882)
    ) || sde.solar_systems.get(&sig.solar_system_id).map(|s| s.region_id) == Some(POCHVEN_REGION_ID)
        || sig
            .destination_system_id
            .and_then(|d| sde.solar_systems.get(&d))
            .map(|s| s.region_id)
            == Some(POCHVEN_REGION_ID)
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

/// Returns all reachable K-space solar systems within `max_ly` Euclidean light-years of `origin_sys_id`
/// using SDE `galactic_pos_ly` coordinates (`feat:navigation.pochven-filaments`, `feat:navigation.pochven-wormholes`).
pub fn pochven_kspace_neighbors_within_ly(
    sde: &eve_sde::SdeRegistry,
    origin_sys_id: SolarSystemId,
    max_ly: f64,
) -> Vec<SolarSystemId> {
    let Some(origin_sys) = sde.solar_systems.get(&origin_sys_id) else {
        return Vec::new();
    };
    if origin_sys.galactic_pos_ly == [0.0; 3] {
        return Vec::new();
    }
    let origin_ly = DVec3::from_array(origin_sys.galactic_pos_ly);
    let mut out: Vec<SolarSystemId> = sde
        .solar_systems
        .values()
        .filter(|s| {
            s.solar_system_id != origin_sys_id
                && s.solar_system_id < 31_000_000
                && s.solar_system_id != 30_100_000
                && !matches!(
                    s.region_id,
                    POCHVEN_REGION_ID | 10_000_004 | 10_000_017 | 10_000_019 | 10_001_000
                )
                && s.galactic_pos_ly != [0.0; 3]
                && origin_ly.distance(DVec3::from_array(s.galactic_pos_ly)) <= max_ly
        })
        .map(|s| s.solar_system_id)
        .collect();
    out.sort_unstable();
    out
}

/// Returns a Pochven solar system within `max_ly` Euclidean light-years of K-space system `k_sys_id`,
/// or `None` if `k_sys_id` is outside Pochven home-region proximity.
pub fn nearest_pochven_system_within_ly(
    sde: &eve_sde::SdeRegistry,
    k_sys_id: SolarSystemId,
    max_ly: f64,
    seq: u32,
) -> Option<SolarSystemId> {
    let k_sys = sde.solar_systems.get(&k_sys_id)?;
    if k_sys.region_id == POCHVEN_REGION_ID
        || k_sys_id >= 31_000_000
        || k_sys_id == 30_100_000
        || k_sys.galactic_pos_ly == [0.0; 3]
    {
        return None;
    }
    let k_ly = DVec3::from_array(k_sys.galactic_pos_ly);
    let mut matches: Vec<SolarSystemId> = sde
        .solar_systems
        .values()
        .filter(|s| {
            s.region_id == POCHVEN_REGION_ID
                && s.galactic_pos_ly != [0.0; 3]
                && k_ly.distance(DVec3::from_array(s.galactic_pos_ly)) <= max_ly
        })
        .map(|s| s.solar_system_id)
        .collect();
    matches.sort_unstable();
    if matches.is_empty() {
        None
    } else {
        let idx = ((k_sys_id as usize).wrapping_add(seq as usize)) % matches.len();
        Some(matches[idx])
    }
}

/// Derives `(border_systems, internal_systems, home_systems)` in Pochven (`region_id == 10_000_070`)
/// directly from SDE constellation membership and stargate jump graph topology.
pub fn pochven_topology_pools(
    sde: &eve_sde::SdeRegistry,
) -> (Vec<SolarSystemId>, Vec<SolarSystemId>, Vec<SolarSystemId>) {
    let mut poch_const: BTreeMap<SolarSystemId, u32> = BTreeMap::new();
    for s in sde.solar_systems.values() {
        if s.region_id == POCHVEN_REGION_ID {
            poch_const.insert(s.solar_system_id, s.constellation_id);
        }
    }
    let mut adj: BTreeMap<SolarSystemId, BTreeSet<SolarSystemId>> = BTreeMap::new();
    for &sid in poch_const.keys() {
        adj.entry(sid).or_default();
    }
    for j in sde.jumps.values() {
        if poch_const.contains_key(&j.from_solar_system_id)
            && poch_const.contains_key(&j.to_solar_system_id)
        {
            adj.entry(j.from_solar_system_id)
                .or_default()
                .insert(j.to_solar_system_id);
            adj.entry(j.to_solar_system_id)
                .or_default()
                .insert(j.from_solar_system_id);
        }
    }
    let mut border = Vec::new();
    let mut home = Vec::new();
    let mut internal = Vec::new();
    for (&sid, &cid) in &poch_const {
        let neighbors = adj.get(&sid);
        let is_border = neighbors.is_some_and(|ns| {
            ns.iter()
                .any(|n| poch_const.get(n).is_some_and(|&nc| nc != cid))
        });
        let is_home = !is_border
            && neighbors.is_some_and(|ns| {
                ns.len() == 2
                    && ns
                        .iter()
                        .all(|n| adj.get(n).is_some_and(|n_adj| n_adj.len() == 3))
            });
        if is_border {
            border.push(sid);
        } else if is_home {
            home.push(sid);
        } else {
            internal.push(sid);
        }
    }
    (border, internal, home)
}

/// Resolves SDE-backed candidate destination systems for Pochven entry and exit filaments (`56065..=56081`).
pub fn resolve_pochven_filament_candidates(
    ctx: &ServerContext,
    fil_type_id: u32,
    old_sys: SolarSystemId,
    char_id: i64,
) -> Result<Vec<SolarSystemId>, BridgeRpcError> {
    let (border, internal, home) = pochven_topology_pools(&ctx.sde);
    let krai_systems = |krai_name: &str| -> Vec<SolarSystemId> {
        let mut v: Vec<SolarSystemId> = ctx
            .sde
            .solar_systems
            .values()
            .filter(|s| {
                s.region_id == POCHVEN_REGION_ID
                    && ctx
                        .sde
                        .constellations
                        .get(&s.constellation_id)
                        .is_some_and(|c| c.name.eq_ignore_ascii_case(krai_name))
            })
            .map(|s| s.solar_system_id)
            .collect();
        v.sort_unstable();
        v
    };
    let mut candidates: Vec<SolarSystemId> = match fil_type_id {
        // Glorification-1/5/15 'Devana' Filament -> Triglavian Minor Victory systems
        56_065..=56_067 => {
            let mut v: Vec<SolarSystemId> =
                crate::services::pve_special_sites::triglavian_minor_victory_systems()
                    .iter()
                    .copied()
                    .filter(|sid| ctx.sde.solar_systems.contains_key(sid))
                    .collect();
            v.sort_unstable();
            v
        }
        // Proximity-5/15 'Extraction' Filament -> K-space systems within <= 5.0 LY of Pochven origin
        56_068..=56_069 => pochven_kspace_neighbors_within_ly(&ctx.sde, old_sys, 5.0),
        // Cladistic-5/15 'Krai Perun' / 'Krai Veles' / 'Krai Svarog' Filaments
        56_070..=56_071 => krai_systems("Krai Perun"),
        56_072..=56_073 => krai_systems("Krai Veles"),
        56_074..=56_075 => krai_systems("Krai Svarog"),
        // Border-5/15 'Pochven' Filament -> 6 Pochven border systems
        56_076..=56_077 => border,
        // Internal-5/15 'Pochven' Filament -> 18 Pochven internal systems
        56_078..=56_079 => internal,
        // Home-5/15 'Pochven' Filament -> 3 Pochven clade home systems
        56_080..=56_081 => home.clone(),
        _ => border,
    };
    let trig_standing = ctx.db.with_snapshot(|s| {
        s.characters
            .get(&char_id)
            .and_then(|c| c.standings.get(&500_026).copied())
            .unwrap_or(0.0)
    });
    if trig_standing < 6.0 {
        if matches!(fil_type_id, 56_080..=56_081) {
            return Err(BridgeRpcError {
                code: "SystemCheck_JumpFailed_StandingsTooLow".into(),
                message: "Triglavian standings below 6.0".into(),
            });
        }
        candidates.retain(|s| !home.contains(s));
    }
    Ok(candidates)
}

pub fn pochven_static_wormholes(
    sde: &eve_sde::SdeRegistry,
    sys_id: SolarSystemId,
    seq: u32,
    now_unix: u64,
) -> Vec<CosmicSignatureRecord> {
    let k_neighbors = pochven_kspace_neighbors_within_ly(sde, sys_id, 5.0);
    let k_dest = if !k_neighbors.is_empty() {
        let idx = ((sys_id as usize).wrapping_mul(7).wrapping_add(seq as usize)) % k_neighbors.len();
        k_neighbors[idx]
    } else {
        let mut fallback: Vec<SolarSystemId> = sde
            .solar_systems
            .values()
            .filter(|s| s.solar_system_id < 31_000_000 && s.region_id != POCHVEN_REGION_ID)
            .map(|s| s.solar_system_id)
            .collect();
        fallback.sort_unstable();
        fallback
            .get(((sys_id as usize).wrapping_add(seq as usize)) % fallback.len().max(1))
            .copied()
            .unwrap_or_else(|| sde.solar_systems.keys().copied().next().unwrap_or(sys_id))
    };
    let mut c4_systems: Vec<SolarSystemId> = sde
        .solar_systems
        .values()
        .filter(|s| s.wormhole_class_id == Some(4))
        .map(|s| s.solar_system_id)
        .collect();
    c4_systems.sort_unstable();
    let c4_dest = if !c4_systems.is_empty() {
        let idx = ((sys_id as usize).wrapping_mul(11).wrapping_add(seq as usize)) % c4_systems.len();
        c4_systems[idx]
    } else {
        sde.solar_systems
            .values()
            .find(|s| s.wormhole_class_id.is_some())
            .map(|s| s.solar_system_id)
            .unwrap_or(sys_id)
    };
    vec![
        make_wh_sig(
            format!("PCH-{}", 100 + ((sys_id + seq * 19) % 900)),
            sys_id,
            "Wormhole C729".into(),
            [11.0, 2.0, -3.5],
            56_026,
            k_dest,
            now_unix,
            43_200,
            1_000_000_000.0,
            410_000_000.0,
        ),
        make_wh_sig(
            format!("PCW-{}", 200 + ((sys_id + seq * 23) % 700)),
            sys_id,
            "Wormhole R081".into(),
            [-9.5, 3.0, 5.5],
            56_541,
            c4_dest,
            now_unix,
            43_200,
            1_000_000_000.0,
            375_000_000.0,
        ),
    ]
}

pub async fn ensure_pochven_side_guards(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    anchor_id: i64,
    wh_pos: DVec3,
    is_pochven_side: bool,
) -> Vec<i64> {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let guard_types: &[u32] = if is_pochven_side {
        &[48_087, 47_958, 48_245, 47_876]
    } else {
        &[48_087, 48_092]
    };
    let mut guard_ids = Vec::with_capacity(guard_types.len());
    for (idx, &npc_tid) in guard_types.iter().enumerate() {
        let gid = anchor_id.saturating_add(10 + idx as i64);
        guard_ids.push(gid);
        if sys_handle
            .get_ball_snapshot(gid)
            .await
            .ok()
            .flatten()
            .is_some()
        {
            continue;
        }
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
    }
    guard_ids
}

pub async fn open_pochven_wormhole_and_spawn_guards(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    sig: &CosmicSignatureRecord,
    wh_item_id: i64,
) -> (DVec3, Vec<i64>) {
    let is_pochven_side = ctx.sde.solar_systems.get(&sys_id).map(|s| s.region_id) == Some(10_000_070);
    let wh_pos = pochven_wormhole_ball_position(&ctx.sde, sig);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    if sys_handle
        .get_ball_snapshot(wh_item_id)
        .await
        .ok()
        .flatten()
        .is_none()
    {
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
    }
    if is_pochven_side {
        if let Some(dest_sys) = sig.destination_system_id {
            let exit_id = wormhole_exit_item_id_for_signature(dest_sys, &sig.signature_id);
            let (exit_sig_pos, _) = pick_system_celestial_position(ctx, dest_sys, (wh_item_id as u32).wrapping_add(1));
            let k162_sig_id = format!("K162-{}", sig.signature_id);
            let mut added_k162: Vec<CosmicSignatureRecord> = Vec::new();
            ctx.db.with_mut_snapshot(|snap| {
                let d_list = snap.cosmic_signatures.entry(dest_sys).or_default();
                if !d_list.iter().any(|s| s.signature_id == k162_sig_id) {
                    let rec = CosmicSignatureRecord {
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
                    };
                    added_k162.push(rec.clone());
                    d_list.push(rec);
                }
            });
            if !added_k162.is_empty() {
                crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_signature_delta(
                    ctx,
                    dest_sys,
                    &added_k162,
                    &[],
                );
            }
            let exit_ball_pos = DVec3::from_array(exit_sig_pos) + DVec3::new(30_000.0, 0.0, 0.0);
            let dest_handle = ctx.systems.get_or_spawn(dest_sys);
            if dest_handle
                .get_ball_snapshot(exit_id)
                .await
                .ok()
                .flatten()
                .is_none()
            {
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
            }
            let _ = ensure_pochven_side_guards(ctx, dest_sys, exit_id, exit_ball_pos, false).await;
        }
    }
    let guard_ids = ensure_pochven_side_guards(ctx, sys_id, wh_item_id, wh_pos, is_pochven_side).await;
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

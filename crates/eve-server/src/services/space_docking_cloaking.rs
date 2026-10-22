//! Wormhole profiles, cosmic signatures/anomalies, cosmic wormhole jumps, and `scanMgr` probe scanning handlers.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::space_destiny::{
    record_session_change_timer, wormhole_item_id_for_signature, AU_IN_METERS,
};
use eve_db::types::{CosmicAnomalyRecord, CosmicSignatureRecord, InventoryFlag};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SolarSystemId;
use glam::DVec3;
use std::collections::HashMap;

const BEYONCE_SVC: &str = "beyonce";

pub fn system_wormhole_profile(sys_id: SolarSystemId) -> (u8, bool, bool, &'static str) {
    let is_wh = (31_000_000..=31_002_605).contains(&sys_id);
    let is_thera = sys_id == 31_000_005;
    let is_shattered = is_thera || (is_wh && (sys_id % 10 == 0));
    let wh_class = if is_thera {
        12
    } else if is_shattered {
        13
    } else if is_wh {
        (((sys_id - 31_000_000) % 6) + 1) as u8
    } else {
        0
    };
    let env_effect = if !is_wh || is_thera {
        "None"
    } else {
        match sys_id % 6 {
            0 => "Pulsar",
            1 => "Wolf-Rayet",
            2 => "Magnetar",
            3 => "Red Giant",
            4 => "Cataclysmic Variable",
            _ => "Black Hole",
        }
    };
    (wh_class, is_thera, is_shattered, env_effect)
}

#[allow(clippy::too_many_arguments)]
fn make_wh_sig(
    signature_id: String,
    solar_system_id: SolarSystemId,
    type_name: String,
    pos_au: [f64; 3],
    wormhole_type_id: u32,
    destination_system_id: SolarSystemId,
    created_unix: u64,
    lifetime_seconds: u64,
    max_mass_kg: f64,
    max_jump_mass_kg: f64,
) -> CosmicSignatureRecord {
    CosmicSignatureRecord {
        signature_id,
        solar_system_id,
        sig_group: "Wormhole".into(),
        type_name,
        position: [
            pos_au[0] * AU_IN_METERS,
            pos_au[1] * AU_IN_METERS,
            pos_au[2] * AU_IN_METERS,
        ],
        signal_strength: 0.0,
        wormhole_type_id: Some(wormhole_type_id),
        destination_system_id: Some(destination_system_id),
        created_unix,
        lifetime_seconds,
        max_mass_kg,
        remaining_mass_kg: max_mass_kg,
        max_jump_mass_kg,
        collapsed: false,
    }
}

fn make_site_sig(
    signature_id: String,
    solar_system_id: SolarSystemId,
    sig_group: &str,
    type_name: &str,
    pos_au: [f64; 3],
    created_unix: u64,
) -> CosmicSignatureRecord {
    CosmicSignatureRecord {
        signature_id,
        solar_system_id,
        sig_group: sig_group.into(),
        type_name: type_name.into(),
        position: [
            pos_au[0] * AU_IN_METERS,
            pos_au[1] * AU_IN_METERS,
            pos_au[2] * AU_IN_METERS,
        ],
        signal_strength: 0.0,
        wormhole_type_id: None,
        destination_system_id: None,
        created_unix,
        lifetime_seconds: 86_400,
        max_mass_kg: 0.0,
        remaining_mass_kg: 0.0,
        max_jump_mass_kg: 0.0,
        collapsed: false,
    }
}

pub(crate) fn static_wormhole_template(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    now_unix: u64,
    seq: u32,
) -> Vec<CosmicSignatureRecord> {
    if sys_id == 31_000_005 {
        let hs_wh = ctx.sde.wormhole_types.get(&30647);
        let ls_wh = ctx.sde.wormhole_types.get(&30648);
        let ns_wh = ctx.sde.wormhole_types.get(&30649);
        vec![
            make_wh_sig(
                format!("THS-{}", 100 + ((sys_id + seq) % 900)),
                sys_id,
                hs_wh
                    .map(|w| w.name.clone())
                    .unwrap_or_else(|| "Wormhole B449".into()),
                [10.0, 2.0, -3.0],
                30647,
                30_000_142,
                now_unix,
                hs_wh
                    .map(|w| (w.max_stable_time_min * 60.0) as u64)
                    .unwrap_or(57_600),
                hs_wh.map(|w| w.max_stable_mass).unwrap_or(2_000_000_000.0),
                375_000_000.0,
            ),
            make_wh_sig(
                format!("TLS-{}", 200 + ((sys_id + seq) % 700)),
                sys_id,
                ls_wh.map(|w| w.name.clone()).unwrap_or_else(|| "Wormhole N944".into()),
                [-9.0, 4.0, 6.0],
                30648,
                30_002_813,
                now_unix,
                ls_wh.map(|w| (w.max_stable_time_min * 60.0) as u64).unwrap_or(86_400),
                ls_wh.map(|w| w.max_stable_mass).unwrap_or(3_000_000_000.0),
                375_000_000.0,
            ),
            make_wh_sig(
                format!("TNS-{}", 300 + ((sys_id + seq) % 600)),
                sys_id,
                ns_wh.map(|w| w.name.clone()).unwrap_or_else(|| "Wormhole S199".into()),
                [6.0, -8.0, 5.0],
                30649,
                30_002_904,
                now_unix,
                ns_wh.map(|w| (w.max_stable_time_min * 60.0) as u64).unwrap_or(57_600),
                ns_wh.map(|w| w.max_stable_mass).unwrap_or(3_000_000_000.0),
                375_000_000.0,
            ),
        ]
    } else if (31_000_000..=31_002_605).contains(&sys_id) {
        // DERIVED: static (`WST-...`) and wandering (`WND-...`) wormhole connection templates per W-space class
        let (wh_class, _, is_shattered, _) = system_wormhole_profile(sys_id);
        let (wh_type_id, dest_sys, wnd_type_id, wnd_dest_sys): (u32, u32, u32, u32) = match wh_class {
            1 => (30579, 30_000_142, 30584, 30_002_813),
            2 => (30583, 31_000_001, 30579, 30_000_142),
            3 => (30584, 30_002_813, 30642, 31_000_003),
            4 => (30642, 31_000_003, 30643, 31_000_004),
            5 => (30643, 31_000_004, 30645, 31_000_006),
            6 => (30645, 31_000_006, 30643, 31_000_004),
            _ => (30660, 30_000_142, 30584, 30_002_813),
        };
        let wh_info = ctx.sde.wormhole_types.get(&wh_type_id);
        let max_mass = wh_info.map(|w| w.max_stable_mass).unwrap_or(2_000_000_000.0);
        let max_jump = if is_shattered { 62_000_000.0 } else { wh_info.map(|w| w.max_jump_mass).unwrap_or(375_000_000.0) };
        let life_s = wh_info.map(|w| (w.max_stable_time_min * 60.0) as u64).unwrap_or(86_400);
        let mut out = Vec::new();
        if seq == 0 {
            let wnd_info = ctx.sde.wormhole_types.get(&wnd_type_id);
            out.push(make_wh_sig(
                format!("WND-{}", 200 + ((sys_id + 13) % 700)),
                sys_id,
                wnd_info.map(|w| w.name.clone()).unwrap_or_else(|| "Wandering Wormhole".into()),
                [-8.0, 5.0, 3.0],
                wnd_type_id,
                wnd_dest_sys,
                now_unix,
                wnd_info.map(|w| (w.max_stable_time_min * 60.0) as u64).unwrap_or(57_600),
                wnd_info.map(|w| w.max_stable_mass).unwrap_or(1_000_000_000.0),
                max_jump,
            ));
        }
        out.push(make_wh_sig(
            format!("WST-{}", 100 + ((sys_id + seq * 17) % 900)),
            sys_id,
            wh_info.map(|w| w.name.clone()).unwrap_or_else(|| "Unstable Wormhole".into()),
            [12.0, 2.5, -4.0],
            wh_type_id,
            dest_sys,
            now_unix,
            life_s,
            max_mass,
            max_jump,
        ));
        out
    } else {
        let sig_prefix = if seq == 0 { "JMM" } else { "STA" };
        vec![make_wh_sig(
            format!("{sig_prefix}-{}", 100 + ((sys_id + seq * 37) % 900)),
            sys_id,
            "Unstable Wormhole (K162)".into(),
            [12.0, 2.5, -4.0],
            26272,
            31_000_005,
            now_unix,
            86_400,
            2_000_000_000.0,
            375_000_000.0,
        )]
    }
}

pub async fn ensure_system_signatures_and_anomalies(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
) -> (Vec<CosmicSignatureRecord>, Vec<CosmicAnomalyRecord>) {
    let now_unix = ctx.now_unix();
    let (sigs, anomalies) = ctx
        .db
        .try_with_mut_snapshot(|snap| {
            let list = snap.cosmic_signatures.entry(sys_id).or_insert_with(|| {
                let mut v = static_wormhole_template(ctx, sys_id, now_unix, 0);
                v.extend([
                    make_site_sig(
                        format!("REL-{}", 200 + (sys_id % 700)),
                        sys_id,
                        "Relic Site",
                        "Ruined Sansha Relic Site",
                        [-8.0, 1.0, 15.0],
                        now_unix,
                    ),
                    make_site_sig(
                        format!("DAT-{}", 300 + (sys_id % 600)),
                        sys_id,
                        "Data Site",
                        "Local Information Repository",
                        [5.0, -11.0, 3.0],
                        now_unix,
                    ),
                    make_site_sig(
                        format!("CBT-{}", 400 + (sys_id % 500)),
                        sys_id,
                        "Combat Site",
                        "Pirate Fleet Base",
                        [-14.0, 0.0, -8.0],
                        now_unix,
                    ),
                    make_site_sig(
                        format!("GAS-{}", 500 + (sys_id % 400)),
                        sys_id,
                        "Gas Site",
                        "Amber Cytoserocin Nebula",
                        [9.0, -4.0, 7.0],
                        now_unix,
                    ),
                    make_site_sig(
                        format!("ORE-{}", 600 + (sys_id % 300)),
                        sys_id,
                        "Ore Site",
                        "Rare Ore Site (Jaspet & Hemorphite)",
                        [-11.0, 6.0, -13.0],
                        now_unix,
                    ),
                ]);
                v
            });
            let sigs_clone = list.clone();
            let anoms = snap
                .cosmic_anomalies
                .entry(sys_id)
                .or_insert_with(|| {
                    vec![CosmicAnomalyRecord {
                        anomaly_id: format!("ANO-{}", 100 + (sys_id % 900)),
                        solar_system_id: sys_id,
                        name: "Dense Veldspar Deposit".into(),
                        anomaly_type: "Ore".into(),
                        position: [
                            4.0 * AU_IN_METERS,
                            3.0 * AU_IN_METERS,
                            -2.0 * AU_IN_METERS,
                        ],
                        signal_strength: 1.0,
                        ore_types: vec![1230, 1228],
                        remaining_volume: 50_000.0,
                        created_unix: now_unix,
                        lifetime_seconds: 86_400,
                        cleared: false,
                    }]
                })
                .clone();
            Ok::<_, std::convert::Infallible>((sigs_clone, anoms))
        })
        .unwrap();

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for sig in &sigs {
        if sig.sig_group == "Wormhole" && !sig.collapsed {
            let wh_item_id = wormhole_item_id_for_signature(sys_id, &sig.signature_id);
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
                        type_id: sig.wormhole_type_id.unwrap_or(26272),
                        position: DVec3::from_array(sig.position),
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
        }
    }

    (sigs, anomalies)
}

pub fn compute_wormhole_stages(
    wh: &CosmicSignatureRecord,
    now_unix: u64,
) -> (&'static str, f64, &'static str, i64, bool) {
    let elapsed = now_unix.saturating_sub(wh.created_unix);
    let lifetime_pct = if wh.lifetime_seconds == 0 || elapsed >= wh.lifetime_seconds {
        0.0
    } else {
        (wh.lifetime_seconds - elapsed) as f64 / wh.lifetime_seconds as f64
    };
    let (lifetime_stage, wormhole_age) = if lifetime_pct <= 0.0 {
        ("Collapsed_Expired", 3)
    } else if lifetime_pct < 0.25 {
        ("ReachingEndOfLifetime", 2)
    } else {
        ("Healthy", 1)
    };

    let mass_pct = if wh.max_mass_kg > 0.0 {
        wh.remaining_mass_kg / wh.max_mass_kg
    } else {
        0.0
    };
    let (mass_stage, wormhole_size) = if wh.remaining_mass_kg <= 0.0 {
        ("Collapsed_Exhausted", 0.0)
    } else if mass_pct < 0.10 {
        ("Critical", 0.1)
    } else if mass_pct < 0.50 {
        ("Destabilized", 0.5)
    } else {
        ("Stable", 1.0)
    };
    let is_collapsed = wh.collapsed || lifetime_pct <= 0.0 || wh.remaining_mass_kg <= 0.0;
    (
        mass_stage,
        wormhole_size,
        lifetime_stage,
        wormhole_age,
        is_collapsed,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_cosmic_wormhole_jump(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    old_sys: SolarSystemId,
    ship_id: i64,
    current_ball: Ball,
    wh_arg_i64: Option<i64>,
    wh_arg_str: Option<&str>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let _ = ensure_system_signatures_and_anomalies(ctx, old_sys).await;
    let has_matching_sig = ctx.db.with_snapshot(|snap| {
        snap.cosmic_signatures.get(&old_sys).is_some_and(|sigs| {
            sigs.iter().any(|s| {
                s.sig_group == "Wormhole"
                    && (wh_arg_str == Some(s.signature_id.as_str())
                        || wh_arg_i64
                            == Some(wormhole_item_id_for_signature(old_sys, &s.signature_id)))
            })
        })
    });
    if !has_matching_sig && wh_arg_str.is_none() {
        return None;
    }

    let ship_mass = current_ball.mass;
    let now_unix = ctx.now_unix();
    let old_handle = ctx.systems.get_or_spawn(old_sys);

    let mut collapsed_ball_to_remove: Option<i64> = None;
    let mut replacement_to_spawn: Option<CosmicSignatureRecord> = None;

    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(sigs) = snap.cosmic_signatures.get_mut(&old_sys) else {
            return Err(BridgeRpcError {
                code: "WormholeNotFound".into(),
                message: "No signatures in system".into(),
            });
        };
        let next_seq = sigs.len() as u32 + 1;
        let Some(wh_idx) = sigs.iter().position(|s| {
            s.sig_group == "Wormhole"
                && (wh_arg_str == Some(s.signature_id.as_str())
                    || wh_arg_i64
                        == Some(wormhole_item_id_for_signature(old_sys, &s.signature_id)))
        }) else {
            return Err(BridgeRpcError {
                code: "WormholeNotFound".into(),
                message: format!("Wormhole not found in system {old_sys}"),
            });
        };

        let wh_id = wormhole_item_id_for_signature(old_sys, &sigs[wh_idx].signature_id);
        let sig_id_str = sigs[wh_idx].signature_id.clone();
        let dest_sys = sigs[wh_idx].destination_system_id.unwrap_or(31_000_005);

        let pol_expiry = snap
            .wh_polarizations
            .get(&(char_id, dest_sys as i64))
            .copied()
            .unwrap_or(0);
        if pol_expiry > now_unix {
            return Err(BridgeRpcError {
                code: "WormholePolarized".into(),
                message: format!(
                    "Character is polarized for system {} for another {} seconds",
                    dest_sys,
                    pol_expiry.saturating_sub(now_unix)
                ),
            });
        }

        let wh_pos = DVec3::from_array(sigs[wh_idx].position);
        let dist = current_ball.position.distance(wh_pos);
        if dist > 2_500.0 {
            return Err(BridgeRpcError {
                code: "OutsideWormholeJumpRange".into(),
                message: format!(
                    "Ship is {:.1} m from wormhole (must be <= 2500 m)",
                    dist
                ),
            });
        }

        let mut spawn_replacement = |sigs_vec: &mut Vec<CosmicSignatureRecord>| {
            let replacements = static_wormhole_template(ctx, old_sys, now_unix, next_seq);
            if let Some(mut rep) = replacements.into_iter().next() {
                rep.destination_system_id = Some(dest_sys);
                replacement_to_spawn = Some(rep.clone());
                sigs_vec.push(rep);
            }
        };

        let elapsed = now_unix.saturating_sub(sigs[wh_idx].created_unix);
        if elapsed >= sigs[wh_idx].lifetime_seconds || sigs[wh_idx].collapsed {
            sigs[wh_idx].collapsed = true;
            collapsed_ball_to_remove = Some(wh_id);
            spawn_replacement(sigs);
            let code = if elapsed >= sigs[wh_idx].lifetime_seconds {
                "WormholeCollapsed"
            } else {
                "WormholeMassCollapsed"
            };
            return Err(BridgeRpcError {
                code: code.into(),
                message: "Wormhole has collapsed".into(),
            });
        }

        let (dest_class, dest_is_thera, dest_is_shattered, _) = system_wormhole_profile(dest_sys);
        let max_jump = sigs[wh_idx].max_jump_mass_kg;
        if (max_jump > 0.0 && ship_mass > max_jump)
            || ((dest_is_thera || dest_is_shattered || (1..=4).contains(&dest_class))
                && ship_mass > 375_000_000.0)
        {
            return Err(BridgeRpcError {
                code: "ShipTooHeavyForWormhole".into(),
                message: format!(
                    "Ship mass {:.1} kg exceeds wormhole jump limit {:.1} kg",
                    ship_mass, max_jump
                ),
            });
        }

        if sigs[wh_idx].remaining_mass_kg <= 0.0 {
            sigs[wh_idx].collapsed = true;
            collapsed_ball_to_remove = Some(wh_id);
            spawn_replacement(sigs);
            return Err(BridgeRpcError {
                code: "WormholeMassCollapsed".into(),
                message: "Wormhole lacks remaining mass capacity and has collapsed".into(),
            });
        }

        sigs[wh_idx].remaining_mass_kg -= ship_mass;
        if sigs[wh_idx].remaining_mass_kg <= 0.0 {
            sigs[wh_idx].collapsed = true;
            collapsed_ball_to_remove = Some(wh_id);
            spawn_replacement(sigs);
        }

        let (mass_stage, wh_size, life_stage, wh_age, is_collapsed) =
            compute_wormhole_stages(&sigs[wh_idx], now_unix);
        let rem_mass = sigs[wh_idx].remaining_mass_kg;

        snap.wh_polarizations
            .insert((char_id, dest_sys as i64), now_unix + 300);

        Ok((
            dest_sys,
            wh_id,
            sig_id_str,
            rem_mass,
            max_jump,
            mass_stage,
            wh_size,
            life_stage,
            wh_age,
            is_collapsed,
        ))
    });

    if let Some(rem_id) = collapsed_ball_to_remove {
        let _ = old_handle.remove_ball(rem_id).await;
    }
    let mut rep_sig_id: Option<String> = None;
    let mut rep_wh_id: Option<i64> = None;
    if let Some(rep) = replacement_to_spawn {
        let new_wh_id = wormhole_item_id_for_signature(old_sys, &rep.signature_id);
        rep_sig_id = Some(rep.signature_id.clone());
        rep_wh_id = Some(new_wh_id);
        let _ = old_handle
            .spawn_ball(Ball {
                id: new_wh_id,
                type_id: rep.wormhole_type_id.unwrap_or(26272),
                position: DVec3::from_array(rep.position),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: rep.remaining_mass_kg.max(1.0),
                radius: 1500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            })
            .await;
    }

    match res {
        Ok((
            dest_sys_id,
            wh_item_id,
            sig_id_str,
            remaining_mass,
            max_jump_mass,
            mass_stage,
            wh_size,
            life_stage,
            wh_age,
            is_collapsed,
        )) => {
            let _ = old_handle.remove_ball(ship_id).await;
            let updated_ch = match ctx
                .db
                .set_character_solar_system(char_id, dest_sys_id, &ctx.sde)
            {
                Ok(c) => c,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "SystemTransitionFailed".into(),
                        message: e.to_string(),
                    }))
                }
            };
            ctx.sync_character_local_chat(char_id, Some(old_sys), dest_sys_id);

            session.solar_system_id = Some(updated_ch.solar_system_id);
            session.constellation_id = Some(updated_ch.constellation_id);
            session.region_id = Some(updated_ch.region_id);
            session.station_id = None;
            session.structure_id = None;

            let _ = ensure_system_signatures_and_anomalies(ctx, dest_sys_id).await;
            let (dest_wh_class, dest_is_thera, dest_is_shattered, env_effect) =
                system_wormhole_profile(dest_sys_id);

            let new_handle = ctx.systems.get_or_spawn(dest_sys_id);
            let mut new_ball = current_ball;
            new_ball.position = DVec3::new(10_000.0, 0.0, 0.0);
            new_ball.velocity = DVec3::ZERO;
            new_ball.speed_fraction = 0.0;
            new_ball.mode = MovementMode::Stop;

            let mut combat_state = ctx.build_ship_combat_state(char_id, ship_id, new_ball.type_id);
            combat_state.is_cloaked = true;
            combat_state.gate_cloak_remaining_ticks = 60;
            combat_state.undock_invulnerable_remaining_ticks = 60;
            combat_state.sync_invuln_with_undock_timer = true;
            match env_effect {
                "Pulsar" => {
                    combat_state.max_shield_hp *= 1.30;
                    combat_state.shield_hp = combat_state.max_shield_hp;
                    combat_state.signature_radius *= 1.30;
                }
                "Wolf-Rayet" => {
                    combat_state.max_armor_hp *= 1.30;
                    combat_state.armor_hp = combat_state.max_armor_hp;
                    combat_state.signature_radius *= 0.85;
                }
                "Black Hole" => {
                    new_ball.max_velocity *= 1.30;
                    new_ball.inertia *= 1.15;
                }
                "Cataclysmic Variable" => {
                    combat_state.max_capacitor *= 1.30;
                    combat_state.capacitor = combat_state.max_capacitor;
                }
                _ => {}
            }
            let mut slim_meta = ctx.build_ship_slim_meta(char_id, new_ball.type_id);
            slim_meta.is_cloaked = true;
            let spawn_tick = new_handle
                .spawn_ball_with_meta(new_ball, combat_state, slim_meta)
                .await
                .unwrap_or(0);
            record_session_change_timer(
                ctx,
                char_id,
                now_unix,
                dest_sys_id,
                spawn_tick,
                "wormhole_jump",
            );

            let new_beyonce_id = match ctx.bind_object(session, BEYONCE_SVC, dest_sys_id as i64) {
                Ok(b) => b,
                Err(e) => return Some(Err(e)),
            };

            Some(Ok(PyRep::keyval([
                ("success", PyRep::Bool(true)),
                ("solarSystemID", PyRep::Integer(dest_sys_id as i64)),
                ("destinationSystemID", PyRep::Integer(dest_sys_id as i64)),
                ("beyonceBoundID", PyRep::Integer(new_beyonce_id as i64)),
                ("wormholeID", PyRep::Integer(wh_item_id)),
                ("signatureID", PyRep::String(sig_id_str)),
                ("remainingMassKg", PyRep::Real(remaining_mass)),
                ("maxJumpMassKg", PyRep::Real(max_jump_mass)),
                ("massStage", PyRep::String(mass_stage.into())),
                ("wormholeSize", PyRep::Real(wh_size)),
                ("lifetimeStage", PyRep::String(life_stage.into())),
                ("wormholeAge", PyRep::Integer(wh_age)),
                ("collapsed", PyRep::Bool(is_collapsed)),
                (
                    "replacementSignatureID",
                    rep_sig_id.map(PyRep::String).unwrap_or(PyRep::None),
                ),
                (
                    "replacementWormholeID",
                    rep_wh_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                ),
                ("wormholeClass", PyRep::Integer(dest_wh_class as i64)),
                ("isThera", PyRep::Bool(dest_is_thera)),
                ("isShattered", PyRep::Bool(dest_is_shattered)),
                ("delayedLocal", PyRep::Bool(dest_wh_class > 0)),
                ("environmentalEffect", PyRep::String(env_effect.into())),
            ])))
        }
        Err(e) => Some(Err(e)),
    }
}

pub(crate) async fn handle_scan_and_wormhole_extras(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    // parity: feat:scanning.probe-launch, feat:scanning.probe-recall, feat:scanning.probe-scan-math
    // parity: rpc:scanMgr.GetSystemScanMgr
    match (service, method) {
        ("scanMgr", "GetSystemScanMgr") => handle_get_system_scan_mgr(ctx, session, service, args).await,
        ("scanMgr", "GetAnomalies") => handle_get_anomalies(ctx, session, bound_entry).await,
        ("scanMgr", "RequestScans") => handle_request_scans(ctx, session, bound_entry, args, _kwargs).await,
        ("scanMgr", "RecoverProbes") => handle_recover_probes(ctx, session, bound_entry, args).await,
        ("scanMgr", "ReconnectToLostProbes") => handle_reconnect_probes(ctx, session, bound_entry).await,
        ("scanMgr", "DestroyProbe") => handle_destroy_probe(ctx, session, bound_entry, args).await,
        _ => None,
    }
}

fn format_anomalies(anomalies: Vec<CosmicAnomalyRecord>) -> Vec<PyRep> {
    anomalies
        .into_iter()
        .filter(|a| !a.cleared)
        .map(|a| {
            PyRep::keyval([
                ("anomalyID", PyRep::String(a.anomaly_id)),
                ("name", PyRep::String(a.name)),
                ("anomalyType", PyRep::String(a.anomaly_type)),
                ("signalStrength", PyRep::Real(1.0)),
                ("resolved", PyRep::Bool(true)),
                ("x", PyRep::Real(a.position[0])),
                ("y", PyRep::Real(a.position[1])),
                ("z", PyRep::Real(a.position[2])),
            ])
        })
        .collect()
}

async fn handle_get_system_scan_mgr(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, _char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let sys_id = args
        .first()
        .and_then(|a| a.as_i64())
        .map(|v| v as u32)
        .or(session.solar_system_id)
        .unwrap_or(30_000_142);
    let now_unix = ctx.now_unix();
    let (sigs, anomalies) = ensure_system_signatures_and_anomalies(ctx, sys_id).await;
    let bound_id = match ctx.bind_object(session, service, sys_id as i64) {
        Ok(b) => b,
        Err(e) => return Some(Err(e)),
    };
    let (wh_class, is_thera, is_shattered, env_effect) = system_wormhole_profile(sys_id);

    let sig_items: Vec<PyRep> = sigs
        .into_iter()
        .filter(|s| !s.collapsed)
        .map(|s| {
            let wh_item_id = wormhole_item_id_for_signature(sys_id, &s.signature_id);
            let (mass_stage, wh_size, life_stage, wh_age, is_collapsed) =
                compute_wormhole_stages(&s, now_unix);
            PyRep::keyval([
                ("signatureID", PyRep::String(s.signature_id)),
                ("itemID", PyRep::Integer(wh_item_id)),
                ("targetID", PyRep::Integer(wh_item_id)),
                (
                    "group",
                    PyRep::String(if s.signal_strength >= 0.25 {
                        s.sig_group
                    } else {
                        "Unknown".into()
                    }),
                ),
                (
                    "type",
                    PyRep::String(if s.signal_strength >= 1.0 {
                        s.type_name
                    } else {
                        "Unidentified Signature".into()
                    }),
                ),
                ("signalStrength", PyRep::Real(s.signal_strength)),
                (
                    "typeID",
                    s.wormhole_type_id
                        .map(|t| PyRep::Integer(t as i64))
                        .unwrap_or(PyRep::None),
                ),
                (
                    "destinationSystemID",
                    s.destination_system_id
                        .map(|d| PyRep::Integer(d as i64))
                        .unwrap_or(PyRep::None),
                ),
                ("massStage", PyRep::String(mass_stage.into())),
                ("wormholeSize", PyRep::Real(wh_size)),
                ("lifetimeStage", PyRep::String(life_stage.into())),
                ("wormholeAge", PyRep::Integer(wh_age)),
                ("remainingMassKg", PyRep::Real(s.remaining_mass_kg)),
                ("maxJumpMassKg", PyRep::Real(s.max_jump_mass_kg)),
                ("collapsed", PyRep::Bool(is_collapsed)),
                ("x", PyRep::Real(s.position[0])),
                ("y", PyRep::Real(s.position[1])),
                ("z", PyRep::Real(s.position[2])),
            ])
        })
        .collect();

    let anom_items = format_anomalies(anomalies);

    Some(Ok(PyRep::keyval([
        ("boundID", PyRep::Integer(bound_id as i64)),
        ("solarSystemID", PyRep::Integer(sys_id as i64)),
        ("wormholeClass", PyRep::Integer(wh_class as i64)),
        ("isThera", PyRep::Bool(is_thera)),
        ("isShattered", PyRep::Bool(is_shattered)),
        ("delayedLocal", PyRep::Bool(wh_class > 0)),
        (
            "allowsCapitalShips",
            PyRep::Bool(wh_class == 0 || (wh_class >= 5 && !is_shattered)),
        ),
        ("environmentalEffect", PyRep::String(env_effect.into())),
        ("signatures", PyRep::List(sig_items)),
        ("anomalies", PyRep::List(anom_items)),
    ])))
}

async fn handle_get_anomalies(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if let Err(e) = ServerContext::require_character(session) {
        return Some(Err(e));
    }
    let sys_id = bound_entry
        .map(|b| b.object_id as u32)
        .or(session.solar_system_id)
        .unwrap_or(30_000_142);
    let (_, anomalies) = ensure_system_signatures_and_anomalies(ctx, sys_id).await;
    Some(Ok(PyRep::List(format_anomalies(anomalies))))
}

async fn handle_request_scans(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let has_active_probes = ctx.active_scanner_probes.read().ok().is_some_and(|map| {
        map.values()
            .any(|p| p.owner_char_id == char_id && p.state != 0)
    });
    if !has_active_probes
        && bound_entry.is_none()
        && (args.is_empty() || matches!(args.first(), Some(PyRep::Real(_))))
        && _kwargs.is_empty()
    {
        return None;
    }

    let sys_id = bound_entry
        .map(|b| b.object_id as u32)
        .or(session.solar_system_id)
        .unwrap_or(30_000_142);
    let _ = ensure_system_signatures_and_anomalies(ctx, sys_id).await;
    let now_unix = ctx.now_unix();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    let override_range_step = args
        .get(1)
        .and_then(|a| a.as_i64())
        .map(|v| (v as i32).clamp(1, 8));

    let mut moved_balls: Vec<(i64, DVec3)> = Vec::new();
    if let Ok(mut probe_map) = ctx.active_scanner_probes.write() {
        for p in probe_map.values_mut() {
            if p.owner_char_id == char_id
                && now_unix
                    >= p.launched_at_unix
                        .saturating_add(p.max_flight_time_s.max(0.0) as u64)
            {
                p.state = 0;
            }
        }

        if let Some(PyRep::Dict(entries)) = args.first() {
            for (k, v) in entries {
                let Some(pid) = k.as_i64() else { continue };
                let Some(probe) = probe_map.get_mut(&pid) else {
                    continue;
                };
                if probe.owner_char_id != char_id || probe.state == 0 {
                    continue;
                }
                let find_in_v = |key: &str| -> Option<&PyRep> {
                    match v {
                        PyRep::KeyVal(kv) => {
                            kv.iter().find(|(ek, _)| *ek == key).map(|(_, ev)| ev)
                        }
                        PyRep::Dict(kv) => kv
                            .iter()
                            .find(|(ek, _)| ek.as_str() == Some(key))
                            .map(|(_, ev)| ev),
                        _ => None,
                    }
                };
                if let Some(PyRep::Tuple(c) | PyRep::List(c)) =
                    find_in_v("pos").or_else(|| find_in_v("destination"))
                {
                    if c.len() >= 3 {
                        probe.position = [
                            c[0].as_f64().unwrap_or(probe.position[0]),
                            c[1].as_f64().unwrap_or(probe.position[1]),
                            c[2].as_f64().unwrap_or(probe.position[2]),
                        ];
                        probe.destination = probe.position;
                        moved_balls.push((pid, DVec3::from_array(probe.position)));
                    }
                } else if let (Some(x), Some(y), Some(z)) = (
                    find_in_v("x").and_then(|a| a.as_f64()),
                    find_in_v("y").and_then(|a| a.as_f64()),
                    find_in_v("z").and_then(|a| a.as_f64()),
                ) {
                    probe.position = [x, y, z];
                    probe.destination = probe.position;
                    moved_balls.push((pid, DVec3::from_array(probe.position)));
                }
                if let Some(step) = find_in_v("rangeStep").and_then(|a| a.as_i64()) {
                    probe.range_step = (step as i32).clamp(1, 8);
                    probe.scan_range_m =
                        0.25 * AU_IN_METERS * 2.0_f64.powi(probe.range_step - 1);
                } else if let Some(rad_m) = find_in_v("scanRange").and_then(|a| a.as_f64()) {
                    probe.scan_range_m = rad_m.max(0.25 * AU_IN_METERS);
                    let au = probe.scan_range_m / (0.25 * AU_IN_METERS);
                    probe.range_step = (au.log2().round() as i32 + 1).clamp(1, 8);
                }
            }
        } else if let Some(PyRep::List(list)) = args.first() {
            let mut owner_probe_ids: Vec<i64> = probe_map
                .values()
                .filter(|p| {
                    p.owner_char_id == char_id
                        && p.solar_system_id == sys_id
                        && p.state != 0
                })
                .map(|p| p.probe_id)
                .collect();
            owner_probe_ids.sort_unstable();
            for (idx, item) in list.iter().enumerate() {
                let Some(&pid) = owner_probe_ids.get(idx) else {
                    break;
                };
                let Some(probe) = probe_map.get_mut(&pid) else {
                    continue;
                };
                if let PyRep::List(coords) | PyRep::Tuple(coords) = item {
                    if coords.len() >= 3 {
                        probe.position = [
                            coords[0].as_f64().unwrap_or(0.0),
                            coords[1].as_f64().unwrap_or(0.0),
                            coords[2].as_f64().unwrap_or(0.0),
                        ];
                        probe.destination = probe.position;
                        moved_balls.push((pid, DVec3::from_array(probe.position)));
                    }
                    if let Some(step) = coords
                        .get(3)
                        .and_then(|a| a.as_i64())
                        .map(|s| (s as i32).clamp(1, 8))
                        .or(override_range_step)
                    {
                        probe.range_step = step;
                        probe.scan_range_m = 0.25 * AU_IN_METERS * 2.0_f64.powi(step - 1);
                    }
                }
            }
        } else if let Some(step) = override_range_step {
            for p in probe_map.values_mut() {
                if p.owner_char_id == char_id && p.solar_system_id == sys_id && p.state != 0 {
                    p.range_step = step;
                    p.scan_range_m = 0.25 * AU_IN_METERS * 2.0_f64.powi(step - 1);
                }
            }
        }
    }

    for (pid, new_pos) in moved_balls {
        if let Ok(Some((_, mut b))) = sys_handle.get_ball_snapshot(pid).await {
            b.position = new_pos;
            let _ = sys_handle.spawn_ball(b).await;
        }
    }

    let active_probes: Vec<crate::router::ActiveScannerProbe> = ctx
        .active_scanner_probes
        .read()
        .ok()
        .map(|map| {
            map.values()
                .filter(|p| {
                    p.owner_char_id == char_id
                        && p.solar_system_id == sys_id
                        && p.state != 0
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();

    if active_probes.is_empty() {
        return Some(Err(BridgeRpcError {
            code: "NoActiveProbes".into(),
            message: "No active scanner probes in space".into(),
        }));
    }

    let (astro_lvl, rangefinding_lvl) = ctx.db.with_snapshot(|snap| {
        let sk = snap.skills.get(&char_id);
        let a = sk.and_then(|m| m.get(&3412)).map(|s| s.level).unwrap_or(3);
        let r = sk.and_then(|m| m.get(&25810)).map(|s| s.level).unwrap_or(0);
        (a, r)
    });
    let skill_mult = (1.0 + 0.05 * astro_lvl as f64) * (1.0 + 0.05 * rangefinding_lvl as f64);

    let scan_results = ctx
        .db
        .try_with_mut_snapshot(|snap| {
            let sigs = snap.cosmic_signatures.entry(sys_id).or_default();
            let mut out = Vec::new();
            for sig in sigs.iter_mut() {
                if sig.collapsed {
                    continue;
                }
                let sig_pos = DVec3::from_array(sig.position);
                let mut covering_probes = 0usize;
                let mut total_effective_strength = 0.0;

                for probe in &active_probes {
                    let p_pos = DVec3::from_array(probe.position);
                    let dist = p_pos.distance(sig_pos);
                    let radius = probe.scan_range_m.max(0.25 * AU_IN_METERS);
                    if dist <= radius {
                        covering_probes += 1;
                        let step_factor = 0.5_f64.powi(probe.range_step - 1);
                        let dist_factor = (-(dist / radius).powi(2)).exp();
                        let probe_s = probe.base_sensor_strength
                            * skill_mult
                            * step_factor
                            * dist_factor;
                        total_effective_strength += probe_s;
                    }
                }

                let trilateration_cap = match covering_probes {
                    0 => 0.0,
                    1 => 0.24,
                    2 => 0.49,
                    3 => 0.74,
                    _ => 1.0,
                };
                let raw_strength = (total_effective_strength / 25.0).min(trilateration_cap);
                sig.signal_strength = raw_strength.clamp(0.0, trilateration_cap);
                let resolved = sig.signal_strength >= 1.0;
                let wh_item_id = wormhole_item_id_for_signature(sys_id, &sig.signature_id);
                out.push((sig.clone(), wh_item_id, covering_probes, resolved));
            }
            Ok::<_, std::convert::Infallible>(out)
        })
        .unwrap();

    let mut rows = Vec::new();
    for (sig, wh_item_id, covering_probes, resolved) in scan_results {
        if resolved {
            ctx.mark_signature_scanned(char_id, sys_id, wh_item_id);
        }
        let (mass_stage, wh_size, life_stage, wh_age, is_collapsed) =
            compute_wormhole_stages(&sig, now_unix);
        rows.push(PyRep::keyval([
            ("id", PyRep::String(sig.signature_id.clone())),
            ("signatureID", PyRep::String(sig.signature_id)),
            ("itemID", PyRep::Integer(wh_item_id)),
            ("targetID", PyRep::Integer(wh_item_id)),
            (
                "group",
                PyRep::String(if sig.signal_strength >= 0.25 {
                    sig.sig_group
                } else {
                    "Unknown".into()
                }),
            ),
            (
                "type",
                PyRep::String(if resolved {
                    sig.type_name
                } else {
                    "Unidentified Signature".into()
                }),
            ),
            ("signalStrength", PyRep::Real(sig.signal_strength)),
            ("certainty", PyRep::Real(sig.signal_strength)),
            ("deviation", PyRep::Real(if resolved { 0.0 } else { (1.0 - sig.signal_strength) * 0.25 * AU_IN_METERS })),
            ("coveringProbes", PyRep::Integer(covering_probes as i64)),
            ("resolved", PyRep::Bool(resolved)),
            ("massStage", PyRep::String(mass_stage.into())),
            ("wormholeSize", PyRep::Real(wh_size)),
            ("lifetimeStage", PyRep::String(life_stage.into())),
            ("wormholeAge", PyRep::Integer(wh_age)),
            ("x", if resolved { PyRep::Real(sig.position[0]) } else { PyRep::None }),
            ("y", if resolved { PyRep::Real(sig.position[1]) } else { PyRep::None }),
            ("z", if resolved { PyRep::Real(sig.position[2]) } else { PyRep::None }),
            ("collapsed", PyRep::Bool(is_collapsed)),
        ]));
    }

    let _ = ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnSystemScanStarted",
        "charid",
        vec![
            PyRep::Integer(now_unix as i64),
            PyRep::Integer(4000),
            PyRep::Integer(active_probes.len() as i64),
        ],
    );
    let _ = ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnSystemScanStopped",
        "charid",
        vec![
            PyRep::Integer(active_probes.len() as i64),
            PyRep::List(rows.clone()),
            PyRep::List(Vec::new()),
        ],
    );

    Some(Ok(PyRep::List(rows)))
}

async fn handle_recover_probes(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let sys_id = bound_entry
        .map(|b| b.object_id as u32)
        .or(session.solar_system_id)
        .unwrap_or(30_000_142);

    let mut target_probe_ids: Vec<i64> = Vec::new();
    if let Some(PyRep::List(list) | PyRep::Tuple(list)) = args.first() {
        for item in list {
            if let Some(pid) = item.as_i64() {
                target_probe_ids.push(pid);
            }
        }
    } else if let Some(pid) = args.first().and_then(|a| a.as_i64()) {
        target_probe_ids.push(pid);
    }

    let now_unix = ctx.now_unix();
    let recovered_probes: Vec<crate::router::ActiveScannerProbe> =
        if let Ok(mut map) = ctx.active_scanner_probes.write() {
            for p in map.values_mut() {
                if p.owner_char_id == char_id
                    && now_unix >= p.launched_at_unix.saturating_add(p.max_flight_time_s.max(0.0) as u64)
                {
                    p.state = 0;
                }
            }
            let ids: Vec<i64> = map
                .values()
                .filter(|p| {
                    p.owner_char_id == char_id
                        && p.state != 0
                        && (target_probe_ids.is_empty() || target_probe_ids.contains(&p.probe_id))
                })
                .map(|p| p.probe_id)
                .collect();
            ids.into_iter().filter_map(|id| map.remove(&id)).collect()
        } else {
            Vec::new()
        };

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for p in &recovered_probes {
        let _ = sys_handle.remove_ball(p.probe_id).await;
    }

    let count = recovered_probes.len() as i32;
    let probe_type_id = recovered_probes
        .first()
        .map(|p| p.probe_type_id)
        .unwrap_or(30013);
    let launcher_id_opt = recovered_probes.first().map(|p| p.launcher_module_id);
    let ship_id_opt = session
        .ship_id
        .or_else(|| recovered_probes.first().map(|p| p.ship_id));

    let (to_launcher, to_cargo) = if count > 0 {
        ctx.db.with_mut_snapshot(|snap| {
            let mut remaining = count;
            let mut in_launcher = 0i32;
            let mut in_cargo = 0i32;
            let add_stack = |items: &mut HashMap<i64, eve_db::InventoryItemRecord>,
                             next_id: &mut i64,
                             ship_id: i64,
                             flag: InventoryFlag,
                             qty: i32,
                             exclude_id: i64| {
                if let Some(st) = items.values_mut().find(|i| {
                    i.owner_id == char_id
                        && i.location_id == ship_id
                        && i.flag == flag
                        && i.item_id != exclude_id
                        && i.type_id == probe_type_id
                }) {
                    st.quantity += qty;
                    st.version += 1;
                } else {
                    *next_id += 1;
                    let new_id = *next_id;
                    items.insert(
                        new_id,
                        eve_db::InventoryItemRecord {
                            item_id: new_id,
                            type_id: probe_type_id,
                            owner_id: char_id,
                            location_id: ship_id,
                            flag,
                            quantity: qty,
                            singleton: false,
                            version: 1,
                            ..Default::default()
                        },
                    );
                }
            };
            if let (Some(ship_id), Some(launcher_id)) = (ship_id_opt, launcher_id_opt) {
                if let Some(li) = snap.items.get(&launcher_id).cloned().filter(|i| i.location_id == ship_id) {
                    let cur_clip = snap
                        .items
                        .values()
                        .find(|i| {
                            i.owner_id == char_id
                                && i.location_id == ship_id
                                && i.flag == li.flag
                                && i.item_id != launcher_id
                                && i.type_id == probe_type_id
                        })
                        .map(|i| i.quantity)
                        .unwrap_or(0);
                    let add = (8 - cur_clip).max(0).min(remaining);
                    if add > 0 {
                        add_stack(&mut snap.items, &mut snap.next_entity_id, ship_id, li.flag, add, launcher_id);
                        remaining -= add;
                        in_launcher += add;
                    }
                }
            }
            if remaining > 0 {
                if let Some(ship_id) = ship_id_opt {
                    add_stack(&mut snap.items, &mut snap.next_entity_id, ship_id, InventoryFlag::Cargo, remaining, 0);
                    in_cargo += remaining;
                }
            }
            (in_launcher, in_cargo)
        })
    } else {
        (0, 0)
    };

    let rec_ids: Vec<PyRep> = recovered_probes
        .iter()
        .map(|p| PyRep::Integer(p.probe_id))
        .collect();
    Some(Ok(PyRep::keyval([
        ("recoveredProbeIDs", PyRep::List(rec_ids)),
        ("recoveredCount", PyRep::Integer(count as i64)),
        ("returnedToLauncher", PyRep::Integer(to_launcher as i64)),
        ("returnedToCargo", PyRep::Integer(to_cargo as i64)),
    ])))
}

async fn handle_reconnect_probes(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let sys_id = bound_entry
        .map(|b| b.object_id as u32)
        .or(session.solar_system_id)
        .unwrap_or(30_000_142);
    let now_unix = ctx.now_unix();
    let ship_id = session.ship_id.unwrap_or(0);

    let mut reconnected = Vec::new();
    if let Ok(mut map) = ctx.active_scanner_probes.write() {
        for p in map.values_mut() {
            if p.owner_char_id == char_id && p.solar_system_id == sys_id {
                if now_unix
                    >= p.launched_at_unix
                        .saturating_add(p.max_flight_time_s.max(0.0) as u64)
                {
                    p.state = 0;
                } else {
                    p.state = 1;
                    if ship_id != 0 {
                        p.ship_id = ship_id;
                    }
                    reconnected.push(PyRep::Integer(p.probe_id));
                }
            }
        }
    }
    let count = reconnected.len() as i64;
    Some(Ok(PyRep::keyval([
        ("reconnectedProbeIDs", PyRep::List(reconnected)),
        ("reconnectedCount", PyRep::Integer(count)),
    ])))
}

async fn handle_destroy_probe(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let sys_id = bound_entry
        .map(|b| b.object_id as u32)
        .or(session.solar_system_id)
        .unwrap_or(30_000_142);
    let target_pid = args.first().and_then(|a| a.as_i64());

    let destroyed_ids: Vec<i64> = if let Ok(mut map) = ctx.active_scanner_probes.write() {
        let ids: Vec<i64> = map
            .values()
            .filter(|p| {
                p.owner_char_id == char_id
                    && (target_pid.is_none() || target_pid == Some(p.probe_id))
            })
            .map(|p| p.probe_id)
            .collect();
        for id in &ids {
            map.remove(id);
        }
        ids
    } else {
        Vec::new()
    };

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for pid in &destroyed_ids {
        let _ = sys_handle.remove_ball(*pid).await;
    }

    Some(Ok(PyRep::keyval([
        (
            "destroyedProbeID",
            destroyed_ids
                .first()
                .copied()
                .map(PyRep::Integer)
                .unwrap_or(PyRep::None),
        ),
        ("destroyedCount", PyRep::Integer(destroyed_ids.len() as i64)),
        ("destroyed", PyRep::Bool(!destroyed_ids.is_empty())),
    ])))
}

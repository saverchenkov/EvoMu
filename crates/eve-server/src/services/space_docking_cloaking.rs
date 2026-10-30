//! Wormhole profiles, cosmic signatures/anomalies, cosmic wormhole jumps, and `scanMgr` probe scanning re-exports.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::space_destiny::{
    record_session_change_timer, wormhole_item_id_for_signature, AU_IN_METERS,
};
pub use crate::services::wormhole_lifecycle_ops::*;
use eve_db::types::{CosmicAnomalyRecord, CosmicSignatureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SolarSystemId;
use glam::DVec3;

#[path = "space_probe_scanning.rs"]
pub mod space_probe_scanning;
pub use space_probe_scanning::*;

const BEYONCE_SVC: &str = "beyonce";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WormholeBuckets {
    pub destination_class: &'static str,
    pub max_ship_size: &'static str,
    pub lifetime_bucket: &'static str,
    pub mass_stability_bucket: &'static str,
    pub lifetime_description: &'static str,
    pub mass_stability_description: &'static str,
}

// parity: feat:wormholes.mass-reduction-and-destabilization
pub fn derive_wormhole_buckets(
    wh: &CosmicSignatureRecord,
    now_unix: u64,
    sde: &eve_sde::SdeRegistry,
) -> WormholeBuckets {
    derive_wormhole_buckets_raw(
        wh.destination_system_id.unwrap_or(0),
        wh.max_jump_mass_kg,
        wh.remaining_mass_kg,
        wh.max_mass_kg,
        wh.created_unix,
        wh.lifetime_seconds,
        wh.collapsed,
        now_unix,
        sde,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn derive_wormhole_buckets_raw(
    dest_sys: u32,
    max_jump_mass_kg: f64,
    remaining_mass_kg: f64,
    max_mass_kg: f64,
    created_unix: u64,
    lifetime_seconds: u64,
    collapsed: bool,
    now_unix: u64,
    sde: &eve_sde::SdeRegistry,
) -> WormholeBuckets {
    let wh_class_id = sde.solar_systems.get(&dest_sys).and_then(|s| {
        s.wormhole_class_id.or_else(|| {
            sde.constellations
                .get(&s.constellation_id)
                .and_then(|c| c.wormhole_class_id)
                .or_else(|| sde.regions.get(&s.region_id).and_then(|r| r.wormhole_class_id))
        })
    });

    // DERIVED: Destination classification resolved from SDE solar system, constellation, or region wormholeClassID
    let dest_class = match wh_class_id {
        Some(12) => "Thera",
        Some(14..=18) => "Drifter",
        Some(25) => "Triglavian",
        Some(1..=3 | 13) => "C1-3",
        Some(4..=5) => "C4-5",
        Some(6) => "C6",
        Some(7) => "High-Sec",
        Some(8) => "Low-Sec",
        Some(9..=11) => "Null-Sec",
        _ => {
            let sec = sde
                .solar_systems
                .get(&dest_sys)
                .map(|s| s.security_status)
                .unwrap_or(0.0);
            if sec >= 0.45 {
                "High-Sec"
            } else if sec > 0.0 {
                "Low-Sec"
            } else {
                "Null-Sec"
            }
        }
    };

    // DERIVED: Ship size category bucket thresholds derived from mass
    let max_ship_size = if max_jump_mass_kg > 0.0 && max_jump_mass_kg <= 20_000_000.0 {
        "Small"
    } else if max_jump_mass_kg > 0.0 && max_jump_mass_kg <= 62_000_000.0 {
        "Medium"
    } else if max_jump_mass_kg > 0.0 && max_jump_mass_kg <= 375_000_000.0 {
        "Large"
    } else if max_jump_mass_kg > 375_000_000.0 {
        "Extra Large"
    } else {
        "Large"
    };

    // DERIVED: Lifetime stage bucket intervals derived from elapsed time
    let elapsed = now_unix.saturating_sub(created_unix);
    let remaining_seconds = lifetime_seconds.saturating_sub(elapsed);
    let lifetime_bucket = if collapsed
        || lifetime_seconds == 0
        || elapsed >= lifetime_seconds
        || remaining_seconds == 0
    {
        "expired"
    } else if remaining_seconds < 3_600 {
        "<1h"
    } else if remaining_seconds < 4 * 3_600 {
        "<4h"
    } else {
        "<1 day"
    };

    // Show-info lifetime strings for the 3 remaining-lifetime bands (>24h, 4h-24h, <4h).
    let lifetime_description = if remaining_seconds > 24 * 3_600 && !collapsed {
        "This wormhole is beginning to decay, and probably won't last another day"
    } else if remaining_seconds >= 4 * 3_600 && !collapsed {
        "This wormhole is not yet beginning to decay"
    } else {
        "This wormhole is reaching the end of its natural lifetime"
    };

    // DERIVED: Mass stability bucket threshold derived from mass ratio
    let mass_ratio = if max_mass_kg > 0.0 {
        remaining_mass_kg / max_mass_kg
    } else {
        0.0
    };
    let (mass_stability_bucket, mass_stability_description) =
        if remaining_mass_kg <= 0.0 || mass_ratio < 0.10 {
            (
                "<10%",
                "This wormhole has had its stability critically disrupted",
            )
        } else if mass_ratio < 0.50 {
            (
                "<50%",
                "This wormhole has had its stability reduced by ships passing through it",
            )
        } else {
            (
                ">50%",
                "This wormhole has not yet had its stability significantly disrupted",
            )
        };

    WormholeBuckets {
        destination_class: dest_class,
        max_ship_size,
        lifetime_bucket,
        mass_stability_bucket,
        lifetime_description,
        mass_stability_description,
    }
}

pub use super::space_pochven_wormholes::{is_pochven_wormhole, pochven_wormhole_ball_position};

pub async fn ensure_system_signatures_and_anomalies(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
) -> (Vec<CosmicSignatureRecord>, Vec<CosmicAnomalyRecord>) {
    let now_unix = ctx.now_unix();
    let mut expired_balls_to_despawn: Vec<(SolarSystemId, i64)> = Vec::new();
    let mut expired_sig_ids: Vec<String> = Vec::new();
    let mut collapsed_mirror_deltas: Vec<(SolarSystemId, String)> = Vec::new();
    let mut added_sigs: Vec<CosmicSignatureRecord> = Vec::new();
    let mut expired_anom_ids: Vec<String> = Vec::new();
    let is_drifter_sys = is_drifter_system(sys_id);
    let sys_wh_class = system_wormhole_profile_sde(&ctx.sde, sys_id).0;
    let (sigs, anomalies, materialized_exits) = ctx
        .db
        .try_with_mut_snapshot(|snap| {
            let mut freshly_seeded_drifter: Option<Vec<CosmicSignatureRecord>> = None;
            let needs_seed = match snap.cosmic_signatures.get(&sys_id) {
                None => true,
                Some(existing) => {
                    !existing.is_empty()
                        && existing.iter().all(|s| s.signature_id.starts_with("K162-"))
                }
            };
            let list = snap.cosmic_signatures.entry(sys_id).or_default();
            if needs_seed {
                let mut v = static_wormhole_template(ctx, sys_id, now_unix, 0);
                if is_drifter_sys {
                    freshly_seeded_drifter = Some(v.clone());
                } else {
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
                }
                list.extend(v);
            }

            let mut spawned_replacements = Vec::new();
            let mut collapsed_parents = Vec::new();
            let base_len = list.len() as u32;
            for (idx, sig) in list.iter_mut().enumerate() {
                if !sig.collapsed
                    && sig.lifetime_seconds > 0
                    && now_unix >= sig.created_unix.saturating_add(sig.lifetime_seconds)
                {
                    sig.collapsed = true;
                    expired_sig_ids.push(sig.signature_id.clone());
                    if sig.sig_group == "Wormhole" {
                        let ent_id = wormhole_item_id_for_signature(sys_id, &sig.signature_id);
                        expired_balls_to_despawn.push((sys_id, ent_id));
                        if let Some(dest_sys) = sig.destination_system_id {
                            let exit_id =
                                wormhole_exit_item_id_for_signature(dest_sys, &sig.signature_id);
                            expired_balls_to_despawn.push((dest_sys, exit_id));
                            collapsed_mirror_deltas
                                .push((dest_sys, format!("K162-{}", sig.signature_id)));
                        }
                        collapsed_parents.push(sig.clone());
                        if let Some(rep) = build_static_replacement_wormhole(
                            ctx,
                            sys_id,
                            sig,
                            now_unix,
                            base_len + (idx as u32) + 1,
                        ) {
                            spawned_replacements.push(rep);
                        }
                    }
                }
            }
            added_sigs = spawned_replacements.clone();
            list.extend(spawned_replacements);
            let sigs_clone = list.clone();

            for parent in &collapsed_parents {
                sync_k162_mirror_in_snap(snap, parent);
            }
            if let Some(drifter_v) = freshly_seeded_drifter {
                seed_drifter_kspace_beacons_in_snap(ctx, snap, sys_id, &drifter_v);
            }

            let mut mat_exits = std::collections::HashMap::new();
            for sig in &sigs_clone {
                if sig.sig_group == "Wormhole"
                    && !sig.collapsed
                    && !sig.signature_id.starts_with("K162-")
                {
                    if let Some(d_sys) = sig.destination_system_id {
                        let k162_id = format!("K162-{}", sig.signature_id);
                        if let Some(k_rec) = snap
                            .cosmic_signatures
                            .get(&d_sys)
                            .and_then(|l| l.iter().find(|s| s.signature_id == k162_id && !s.collapsed))
                        {
                            mat_exits.insert(
                                sig.signature_id.clone(),
                                (
                                    pochven_wormhole_ball_position(&ctx.sde, k_rec),
                                    k_rec.wormhole_type_id.unwrap_or(26_272),
                                ),
                            );
                        }
                    }
                }
            }

            let anom_list = snap
                .cosmic_anomalies
                .entry(sys_id)
                .or_insert_with(|| {
                    if sys_wh_class == 0 {
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
                    } else {
                        Vec::new()
                    }
                });
            for anom in anom_list.iter_mut() {
                if !anom.cleared
                    && anom.lifetime_seconds > 0
                    && now_unix >= anom.created_unix.saturating_add(anom.lifetime_seconds)
                {
                    anom.cleared = true;
                    expired_anom_ids.push(anom.anomaly_id.clone());
                }
            }
            let anoms = anom_list.clone();
            Ok::<_, std::convert::Infallible>((sigs_clone, anoms, mat_exits))
        })
        .unwrap();

    crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_signature_delta(
        ctx,
        sys_id,
        &added_sigs,
        &expired_sig_ids,
    );
    for (m_sys, m_sig_id) in collapsed_mirror_deltas {
        crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_signature_delta(
            ctx,
            m_sys,
            &[],
            &[m_sig_id],
        );
    }
    crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_anomaly_delta(
        ctx,
        sys_id,
        &[],
        &expired_anom_ids,
    );

    for (b_sys, b_id) in expired_balls_to_despawn {
        let h = ctx.systems.get_or_spawn(b_sys);
        let _ = h.remove_ball(b_id).await;
    }

    let is_pochven_sys =
        ctx.sde.solar_systems.get(&sys_id).map(|s| s.region_id) == Some(10_000_070);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for sig in &sigs {
        if sig.sig_group == "Wormhole" && !sig.collapsed {
            let wh_item_id = wormhole_item_id_for_signature(sys_id, &sig.signature_id);
            let ball_pos = pochven_wormhole_ball_position(&ctx.sde, sig);
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
                        position: ball_pos,
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
            if is_pochven_wormhole(&ctx.sde, sig) {
                if is_pochven_sys && sig.signal_strength >= 1.0 {
                    let _ = super::space_pochven_wormholes::open_pochven_wormhole_and_spawn_guards(
                        ctx,
                        sys_id,
                        sig,
                        wh_item_id,
                    )
                    .await;
                } else {
                    let _ = super::space_pochven_wormholes::ensure_pochven_side_guards(
                        ctx,
                        sys_id,
                        wh_item_id,
                        ball_pos,
                        is_pochven_sys,
                    )
                    .await;
                }
            }
            if sys_wh_class == 0 && is_drifter_wormhole(&ctx.sde, sig) {
                let _ =
                    ensure_drifter_kspace_guards(ctx, sys_id, sig, wh_item_id, ball_pos).await;
            }
            // Destination K162 exit balls only materialize once a ship first jumps through
            // (or when a Pochven wormhole is scanned to 1.0 from Pochven, or for Drifter K-space beacons).
            if !sig.signature_id.starts_with("K162-") {
                if let Some(&(exit_pos, exit_type_id)) = materialized_exits.get(&sig.signature_id) {
                    if let Some(dest_sys) = sig.destination_system_id {
                        let exit_id =
                            wormhole_exit_item_id_for_signature(dest_sys, &sig.signature_id);
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
                                    type_id: exit_type_id,
                                    position: exit_pos,
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
            }
        }
    }

    (sigs, anomalies)
}

async fn sweep_expired_wormholes(
    ctx: &ServerContext,
    filter_sys: Option<SolarSystemId>,
    now_unix: u64,
) {
    let has_expired = ctx.db.with_snapshot(|snap| {
        snap.cosmic_signatures.iter().any(|(&sys_id, list)| {
            list.iter().any(|sig| {
                sig.sig_group == "Wormhole"
                    && !sig.collapsed
                    && sig.lifetime_seconds > 0
                    && now_unix >= sig.created_unix.saturating_add(sig.lifetime_seconds)
                    && filter_sys.is_none_or(|t| {
                        sys_id == t || sig.destination_system_id == Some(t)
                    })
            })
        })
    });
    if !has_expired {
        return;
    }

    let mut expired_balls_to_despawn: Vec<(SolarSystemId, i64)> = Vec::new();
    let mut new_wormholes_to_spawn: Vec<(SolarSystemId, CosmicSignatureRecord)> = Vec::new();
    let mut collapsed_parents: Vec<CosmicSignatureRecord> = Vec::new();
    let mut sig_deltas: Vec<(SolarSystemId, Vec<CosmicSignatureRecord>, Vec<String>)> = Vec::new();

    ctx.db.with_mut_snapshot(|snap| {
        let sys_ids: Vec<SolarSystemId> = snap.cosmic_signatures.keys().copied().collect();
        for sys_id in sys_ids {
            let Some(list) = snap.cosmic_signatures.get_mut(&sys_id) else {
                continue;
            };
            let mut spawned_replacements = Vec::new();
            let mut removed_ids = Vec::new();
            let base_len = list.len() as u32;
            for (idx, sig) in list.iter_mut().enumerate() {
                let matches_scope = filter_sys.is_none_or(|t| {
                    sys_id == t || sig.destination_system_id == Some(t)
                });
                if matches_scope
                    && sig.sig_group == "Wormhole"
                    && !sig.collapsed
                    && !sig.signature_id.starts_with("K162-")
                    && sig.lifetime_seconds > 0
                    && now_unix >= sig.created_unix.saturating_add(sig.lifetime_seconds)
                {
                    sig.collapsed = true;
                    removed_ids.push(sig.signature_id.clone());
                    let ent_id = wormhole_item_id_for_signature(sys_id, &sig.signature_id);
                    expired_balls_to_despawn.push((sys_id, ent_id));
                    if let Some(dest_sys) = sig.destination_system_id {
                        let exit_id =
                            wormhole_exit_item_id_for_signature(dest_sys, &sig.signature_id);
                        expired_balls_to_despawn.push((dest_sys, exit_id));
                        sig_deltas.push((
                            dest_sys,
                            Vec::new(),
                            vec![format!("K162-{}", sig.signature_id)],
                        ));
                    }
                    collapsed_parents.push(sig.clone());
                    if let Some(rep) = build_static_replacement_wormhole(
                        ctx,
                        sys_id,
                        sig,
                        now_unix,
                        base_len + (idx as u32) + 1,
                    ) {
                        new_wormholes_to_spawn.push((sys_id, rep.clone()));
                        spawned_replacements.push(rep);
                    }
                } else if matches_scope
                    && sig.sig_group == "Wormhole"
                    && !sig.collapsed
                    && sig.signature_id.starts_with("K162-")
                    && sig.lifetime_seconds > 0
                    && now_unix >= sig.created_unix.saturating_add(sig.lifetime_seconds)
                {
                    sig.collapsed = true;
                    removed_ids.push(sig.signature_id.clone());
                    let orig_id = sig.signature_id.trim_start_matches("K162-");
                    let exit_id = wormhole_exit_item_id_for_signature(sys_id, orig_id);
                    expired_balls_to_despawn.push((sys_id, exit_id));
                }
            }
            if !spawned_replacements.is_empty() || !removed_ids.is_empty() {
                sig_deltas.push((sys_id, spawned_replacements.clone(), removed_ids));
            }
            list.extend(spawned_replacements);
        }
        for parent in &collapsed_parents {
            sync_k162_mirror_in_snap(snap, parent);
        }
    });

    for (d_sys, added, removed) in sig_deltas {
        crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_signature_delta(
            ctx, d_sys, &added, &removed,
        );
    }
    for (b_sys, b_id) in expired_balls_to_despawn {
        let h = ctx.systems.get_or_spawn(b_sys);
        let _ = h.remove_ball(b_id).await;
    }
    for (sys_id, rep) in new_wormholes_to_spawn {
        let new_wh_id = wormhole_item_id_for_signature(sys_id, &rep.signature_id);
        let wh_pos = super::space_pochven_wormholes::pochven_wormhole_ball_position(&ctx.sde, &rep);
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_handle
            .spawn_ball(Ball {
                id: new_wh_id,
                type_id: rep.wormhole_type_id.unwrap_or(26272),
                position: wh_pos,
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
}

/// SystemActor periodic step wormhole expiry sweep for `sys_id` (`feat:wormholes.lifetime-decay-stages`).
pub async fn collapse_expired_wormholes_in_system(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    now_unix: u64,
) {
    sweep_expired_wormholes(ctx, Some(sys_id), now_unix).await;
}

/// Background cluster wormhole expiry sweep:
/// despawns both entrance and exit balls of expired wormholes and spawns static replacement wormholes.
pub async fn expire_and_respawn_wormholes_in_cluster(ctx: &ServerContext, now_unix: u64) {
    sweep_expired_wormholes(ctx, None, now_unix).await;
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

// parity: feat:wormholes.mass-reduction-and-destabilization
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
    let now_unix = ctx.now_unix();
    if let Some(active_timer) =
        crate::services::space_destiny::check_session_change_timer_active(ctx, char_id, now_unix, None)
    {
        ctx.notify(crate::router::NotificationTarget::Character(char_id), "OnWormholeJumpCancel", "charid", vec![]);
        return Some(Err(BridgeRpcError {
            code: "SessionChangeInProgress".into(),
            message: format!(
                "Cannot jump wormhole while 10s session-change timer ({}) is active",
                active_timer.reason
            ),
        }));
    }

    let has_explicit_expired = ctx.db.with_snapshot(|snap| {
        snap.cosmic_signatures.get(&old_sys).is_some_and(|sigs| {
            sigs.iter().any(|s| {
                s.sig_group == "Wormhole"
                    && (wh_arg_str == Some(s.signature_id.as_str())
                        || wh_arg_i64
                            == Some(wormhole_item_id_for_signature(old_sys, &s.signature_id)))
            })
        }) || snap.cosmic_signatures.iter().any(|(_, sigs)| {
            sigs.iter().any(|s| {
                s.sig_group == "Wormhole"
                    && s.destination_system_id == Some(old_sys)
                    && (wh_arg_i64
                        == Some(wormhole_exit_item_id_for_signature(old_sys, &s.signature_id))
                        || wh_arg_str == Some(format!("K162-{}", s.signature_id).as_str()))
            })
        })
    });
    if !has_explicit_expired {
        let _ = ensure_system_signatures_and_anomalies(ctx, old_sys).await;
    }
    let has_matching_sig = ctx.db.with_snapshot(|snap| {
        snap.cosmic_signatures.get(&old_sys).is_some_and(|sigs| {
            sigs.iter().any(|s| {
                s.sig_group == "Wormhole"
                    && (wh_arg_str == Some(s.signature_id.as_str())
                        || wh_arg_i64
                            == Some(wormhole_item_id_for_signature(old_sys, &s.signature_id)))
            })
        }) || snap.cosmic_signatures.iter().any(|(_, sigs)| {
            sigs.iter().any(|s| {
                s.sig_group == "Wormhole"
                    && s.destination_system_id == Some(old_sys)
                    && (wh_arg_i64
                        == Some(wormhole_exit_item_id_for_signature(old_sys, &s.signature_id))
                        || wh_arg_str == Some(format!("K162-{}", s.signature_id).as_str()))
            })
        })
    });
    if !has_matching_sig && wh_arg_str.is_none() {
        return None;
    }

    let (ship_mass, prop_mass_add) = compute_effective_ship_jump_mass(ctx, ship_id, current_ball.mass);
    let old_handle = ctx.systems.get_or_spawn(old_sys);
    let exit_ball_pos_opt = if let Some(arg_id) = wh_arg_i64 {
        old_handle
            .get_ball_snapshot(arg_id)
            .await
            .ok()
            .flatten()
            .map(|(_, b)| b.position)
    } else {
        None
    };

    let mut collapsed_balls_to_remove: Vec<(SolarSystemId, i64)> = Vec::new();
    let mut replacement_to_spawn: Option<(SolarSystemId, CosmicSignatureRecord)> = None;
    let mut collapsed_sig_delta: Option<(SolarSystemId, String)> = None;
    let mut collapsed_mirror_delta: Option<(SolarSystemId, String)> = None;
    let mut k162_to_materialize: Option<(SolarSystemId, i64, CosmicSignatureRecord, DVec3)> = None;

    let res = ctx.db.try_with_mut_snapshot(|snap| {
        // Resolve canonical parent signature even if jumping via materialized `K162-<orig_id>` string or `exit_wh_id`.
        let mut reverse_parent_opt: Option<(SolarSystemId, String)> = None;
        let mut local_k162_pos_opt: Option<DVec3> = None;
        if let Some(local_sigs) = snap.cosmic_signatures.get(&old_sys) {
            if let Some(local_k162) = local_sigs.iter().find(|s| {
                s.sig_group == "Wormhole"
                    && s.signature_id.starts_with("K162-")
                    && (wh_arg_str == Some(s.signature_id.as_str())
                        || wh_arg_i64
                            == Some(wormhole_item_id_for_signature(old_sys, &s.signature_id)))
            }) {
                local_k162_pos_opt = Some(pochven_wormhole_ball_position(&ctx.sde, local_k162));
                let orig_id = local_k162.signature_id[5..].to_string();
                if let Some(parent_sys) = local_k162.destination_system_id {
                    if snap.cosmic_signatures.get(&parent_sys).is_some_and(|ps| {
                        ps.iter().any(|p| p.sig_group == "Wormhole" && p.signature_id == orig_id)
                    }) {
                        reverse_parent_opt = Some((parent_sys, orig_id));
                    }
                }
            }
        }
        if reverse_parent_opt.is_none() {
            let has_non_k162_local = snap.cosmic_signatures.get(&old_sys).is_some_and(|sigs| {
                sigs.iter().any(|s| {
                    s.sig_group == "Wormhole"
                        && !s.signature_id.starts_with("K162-")
                        && (wh_arg_str == Some(s.signature_id.as_str())
                            || wh_arg_i64
                                == Some(wormhole_item_id_for_signature(old_sys, &s.signature_id)))
                })
            });
            if !has_non_k162_local {
                for (&psys, sigs) in &snap.cosmic_signatures {
                    if let Some(parent) = sigs.iter().find(|s| {
                        s.sig_group == "Wormhole"
                            && !s.signature_id.starts_with("K162-")
                            && s.destination_system_id == Some(old_sys)
                            && (wh_arg_i64
                                == Some(wormhole_exit_item_id_for_signature(old_sys, &s.signature_id))
                                || wh_arg_str
                                    == Some(format!("K162-{}", s.signature_id).as_str()))
                    }) {
                        reverse_parent_opt = Some((psys, parent.signature_id.clone()));
                        break;
                    }
                }
            }
        }

        let (sig_host_sys, is_reverse_exit_jump, target_parent_sig_id) =
            if let Some((psys, orig_id)) = reverse_parent_opt {
                (psys, true, Some(orig_id))
            } else {
                (old_sys, false, None)
            };
        let ship_type_id = snap
            .items
            .get(&ship_id)
            .map(|i| i.type_id)
            .unwrap_or(current_ball.type_id);

        let (
            dest_sys,
            wh_id,
            exit_wh_id,
            sig_id_str,
            rem_mass,
            max_jump,
            mass_stage,
            wh_size,
            life_stage,
            wh_age,
            is_collapsed,
            buckets,
            parent_snapshot,
        ) = {
            let Some(sigs) = snap.cosmic_signatures.get_mut(&sig_host_sys) else {
                return Err(BridgeRpcError {
                    code: "WormholeNotFound".into(),
                    message: "No signatures in system".into(),
                });
            };
            let next_seq = sigs.len() as u32 + 1;
            let Some(wh_idx) = sigs.iter().position(|s| {
                s.sig_group == "Wormhole"
                    && if let Some(ref pid) = target_parent_sig_id {
                        s.signature_id == *pid
                    } else {
                        wh_arg_str == Some(s.signature_id.as_str())
                            || wh_arg_i64
                                == Some(wormhole_item_id_for_signature(old_sys, &s.signature_id))
                    }
            }) else {
                return Err(BridgeRpcError {
                    code: "WormholeNotFound".into(),
                    message: format!("Wormhole not found in system {old_sys}"),
                });
            };

            let sig_id_str = sigs[wh_idx].signature_id.clone();
            let (wh_id, dest_sys, exit_wh_id) = if is_reverse_exit_jump {
                let local_exit_id = wormhole_exit_item_id_for_signature(old_sys, &sig_id_str);
                let parent_entrance_id = wormhole_item_id_for_signature(sig_host_sys, &sig_id_str);
                (local_exit_id, sig_host_sys, parent_entrance_id)
            } else {
                let entrance_id = wormhole_item_id_for_signature(old_sys, &sig_id_str);
                let d_sys = sigs[wh_idx].destination_system_id.unwrap_or(31_000_005);
                let exit_id = wormhole_exit_item_id_for_signature(d_sys, &sig_id_str);
                (entrance_id, d_sys, exit_id)
            };

            let pol_expiry = snap.wh_polarizations.get(&(char_id, wh_id)).copied().unwrap_or(0);
            if pol_expiry > now_unix {
                return Err(BridgeRpcError {
                    code: "WormholePolarized".into(),
                    message: format!(
                        "Character is polarized for wormhole {} for another {} seconds",
                        wh_id,
                        pol_expiry.saturating_sub(now_unix)
                    ),
                });
            }

            let wh_pos = exit_ball_pos_opt
                .or(local_k162_pos_opt)
                .unwrap_or_else(|| pochven_wormhole_ball_position(&ctx.sde, &sigs[wh_idx]));
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

            let mut spawn_replacement = |sigs_vec: &mut Vec<CosmicSignatureRecord>, idx: usize| {
                if let Some(rep) = build_static_replacement_wormhole(
                    ctx,
                    sig_host_sys,
                    &sigs_vec[idx],
                    now_unix,
                    next_seq,
                ) {
                    replacement_to_spawn = Some((sig_host_sys, rep.clone()));
                    sigs_vec.push(rep);
                }
            };

            let elapsed = now_unix.saturating_sub(sigs[wh_idx].created_unix);
            if elapsed >= sigs[wh_idx].lifetime_seconds || sigs[wh_idx].collapsed {
                let was_already_collapsed = sigs[wh_idx].collapsed;
                sigs[wh_idx].collapsed = true;
                collapsed_balls_to_remove.push((old_sys, wh_id));
                collapsed_balls_to_remove.push((dest_sys, exit_wh_id));
                if !was_already_collapsed {
                    collapsed_sig_delta = Some((sig_host_sys, sig_id_str.clone()));
                    if !is_reverse_exit_jump {
                        collapsed_mirror_delta = Some((dest_sys, format!("K162-{sig_id_str}")));
                    } else {
                        collapsed_mirror_delta = Some((old_sys, format!("K162-{sig_id_str}")));
                    }
                    spawn_replacement(sigs, wh_idx);
                }
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

            let (origin_class, _, _, _) = system_wormhole_profile_sde(&ctx.sde, old_sys);
            let (dest_class, dest_is_thera, dest_is_shattered, _) =
                system_wormhole_profile_sde(&ctx.sde, dest_sys);
            let max_jump = sigs[wh_idx].max_jump_mass_kg;
            if is_small_ship_wormhole(
                sigs[wh_idx].wormhole_type_id,
                max_jump,
                origin_class,
                dest_class,
            ) {
                check_small_ship_wormhole_eligibility(&ctx.sde, ship_type_id, ship_mass)?;
            } else if (max_jump > 0.0 && ship_mass > max_jump)
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
                collapsed_sig_delta = Some((sig_host_sys, sig_id_str.clone()));
                collapsed_mirror_delta = Some((
                    if is_reverse_exit_jump { old_sys } else { dest_sys },
                    format!("K162-{sig_id_str}"),
                ));
                collapsed_balls_to_remove.push((old_sys, wh_id));
                collapsed_balls_to_remove.push((dest_sys, exit_wh_id));
                spawn_replacement(sigs, wh_idx);
                return Err(BridgeRpcError {
                    code: "WormholeMassCollapsed".into(),
                    message: "Wormhole lacks remaining mass capacity and has collapsed".into(),
                });
            }

            sigs[wh_idx].remaining_mass_kg -= ship_mass;
            if sigs[wh_idx].remaining_mass_kg <= 0.0 {
                sigs[wh_idx].collapsed = true;
                collapsed_sig_delta = Some((sig_host_sys, sig_id_str.clone()));
                collapsed_mirror_delta = Some((
                    if is_reverse_exit_jump { old_sys } else { dest_sys },
                    format!("K162-{sig_id_str}"),
                ));
                collapsed_balls_to_remove.push((old_sys, wh_id));
                collapsed_balls_to_remove.push((dest_sys, exit_wh_id));
                spawn_replacement(sigs, wh_idx);
            }

            let (mass_stage, wh_size, life_stage, wh_age, is_collapsed) =
                compute_wormhole_stages(&sigs[wh_idx], now_unix);
            let buckets = derive_wormhole_buckets(&sigs[wh_idx], now_unix, &ctx.sde);
            let rem_mass = sigs[wh_idx].remaining_mass_kg;
            let parent_snapshot = sigs[wh_idx].clone();

            (
                dest_sys,
                wh_id,
                exit_wh_id,
                sig_id_str,
                rem_mass,
                max_jump,
                mass_stage,
                wh_size,
                life_stage,
                wh_age,
                is_collapsed,
                buckets,
                parent_snapshot,
            )
        };

        // Materialize destination K162 signature on first jump through (`feat:navigation.wormhole-k162-exit-materialization`)
        if !is_reverse_exit_jump && !is_collapsed && !parent_snapshot.signature_id.starts_with("K162-") {
            if let Some((k_exit_id, k_rec, k_pos)) =
                materialize_k162_exit_in_snap(ctx, snap, &parent_snapshot, dest_sys)
            {
                k162_to_materialize = Some((dest_sys, k_exit_id, k_rec, k_pos));
            }
        }
        sync_k162_mirror_in_snap(snap, &parent_snapshot);

        snap.wh_polarizations.insert((char_id, dest_sys as i64), now_unix + 300);
        snap.wh_polarizations.insert((char_id, wh_id), now_unix + 300);

        Ok((dest_sys, wh_id, exit_wh_id, sig_id_str, rem_mass, max_jump, mass_stage, wh_size, life_stage, wh_age, is_collapsed, buckets))
    });

    for (b_sys, rem_id) in &collapsed_balls_to_remove {
        let h = ctx.systems.get_or_spawn(*b_sys);
        let _ = h.remove_ball(*rem_id).await;
    }
    if let Some((host_sys, collapsed_id)) = collapsed_sig_delta {
        let added_vec: Vec<CosmicSignatureRecord> = replacement_to_spawn
            .as_ref()
            .filter(|(s, _)| *s == host_sys)
            .map(|(_, r)| r.clone())
            .into_iter()
            .collect();
        crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_signature_delta(
            ctx,
            host_sys,
            &added_vec,
            &[collapsed_id],
        );
    }
    if let Some((m_sys, m_id)) = collapsed_mirror_delta {
        crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_signature_delta(
            ctx,
            m_sys,
            &[],
            &[m_id],
        );
    }
    if let Some((d_sys, k_exit_id, k_rec, k_pos)) = k162_to_materialize {
        let dh = ctx.systems.get_or_spawn(d_sys);
        if dh.get_ball_snapshot(k_exit_id).await.ok().flatten().is_none() {
            let _ = dh
                .spawn_ball(Ball {
                    id: k_exit_id,
                    type_id: 26272,
                    position: k_pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: k_rec.remaining_mass_kg.max(1.0),
                    radius: 1500.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Rigid,
                })
                .await;
        }
        if is_pochven_wormhole(&ctx.sde, &k_rec) {
            let d_is_poch = ctx.sde.solar_systems.get(&d_sys).map(|s| s.region_id) == Some(10_000_070);
            let _ = super::space_pochven_wormholes::ensure_pochven_side_guards(
                ctx, d_sys, k_exit_id, k_pos, d_is_poch,
            )
            .await;
        }
        crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_signature_delta(
            ctx,
            d_sys,
            &[k_rec],
            &[],
        );
    }
    let mut rep_sig_id: Option<String> = None;
    let mut rep_wh_id: Option<i64> = None;
    let mut rep_wh_type_id: Option<u32> = None;
    if let Some((rep_host_sys, rep)) = replacement_to_spawn {
        let new_wh_id = wormhole_item_id_for_signature(rep_host_sys, &rep.signature_id);
        rep_sig_id = Some(rep.signature_id.clone());
        rep_wh_id = Some(new_wh_id);
        rep_wh_type_id = rep.wormhole_type_id;
        let rep_handle = ctx.systems.get_or_spawn(rep_host_sys);
        let _ = rep_handle
            .spawn_ball(Ball {
                id: new_wh_id,
                type_id: rep.wormhole_type_id.unwrap_or(26272),
                position: pochven_wormhole_ball_position(&ctx.sde, &rep),
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
        Ok((dest_sys_id, wh_item_id, exit_wh_id, sig_id_str, remaining_mass, max_jump_mass, mass_stage, wh_size, life_stage, wh_age, is_collapsed, buckets)) => {
            let prev_cs = old_handle.get_combat_state(ship_id).await.ok().flatten();
            crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(ctx, char_id, ship_id, old_sys);
            crate::services::space_destiny::space_warp_jump::deactivate_ship_modules_on_leave_space(ctx, char_id, ship_id);
            let _ = old_handle.remove_ball(ship_id).await;
            let updated_ch = match ctx.db.set_character_solar_system(char_id, dest_sys_id, &ctx.sde) {
                Ok(c) => c,
                Err(e) => return Some(Err(BridgeRpcError { code: "SystemTransitionFailed".into(), message: e.to_string() })),
            };
            ctx.sync_character_local_chat(char_id, Some(old_sys), dest_sys_id);

            session.solar_system_id = Some(updated_ch.solar_system_id);
            session.constellation_id = Some(updated_ch.constellation_id);
            session.region_id = Some(updated_ch.region_id);
            session.station_id = None;
            session.structure_id = None;

            let _ = ensure_system_signatures_and_anomalies(ctx, dest_sys_id).await;
            let (dest_wh_class, dest_is_thera, dest_is_shattered, env_effect) =
                system_wormhole_profile_sde(&ctx.sde, dest_sys_id);

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
            if let Some((_, _, _, eff_tid)) =
                eve_dogma::resolve_system_wormhole_phenomenon(&ctx.sde, dest_sys_id, None)
            {
                let mut eff_attrs = std::collections::HashMap::from([
                    (eve_sde::attr_ids::SHIELD_CAPACITY, combat_state.max_shield_hp),
                    (eve_sde::attr_ids::ARMOR_HP, combat_state.max_armor_hp),
                    (eve_sde::attr_ids::SIGNATURE_RADIUS, combat_state.signature_radius),
                    (eve_sde::attr_ids::CAPACITOR_CAPACITY, combat_state.max_capacitor),
                    (eve_sde::attr_ids::MAX_VELOCITY, new_ball.max_velocity),
                    (eve_sde::attr_ids::AGILITY, new_ball.inertia),
                ]);
                if eve_dogma::apply_wormhole_phenomenon_to_attributes(
                    &ctx.sde,
                    eff_tid,
                    6,
                    false,
                    &mut eff_attrs,
                ) {
                    combat_state.max_shield_hp =
                        *eff_attrs.get(&eve_sde::attr_ids::SHIELD_CAPACITY).unwrap_or(&combat_state.max_shield_hp);
                    combat_state.shield_hp = combat_state.max_shield_hp;
                    combat_state.max_armor_hp =
                        *eff_attrs.get(&eve_sde::attr_ids::ARMOR_HP).unwrap_or(&combat_state.max_armor_hp);
                    combat_state.armor_hp = combat_state.max_armor_hp;
                    combat_state.signature_radius =
                        *eff_attrs.get(&eve_sde::attr_ids::SIGNATURE_RADIUS).unwrap_or(&combat_state.signature_radius);
                    combat_state.max_capacitor =
                        *eff_attrs.get(&eve_sde::attr_ids::CAPACITOR_CAPACITY).unwrap_or(&combat_state.max_capacitor);
                    combat_state.capacitor = combat_state.max_capacitor;
                    new_ball.max_velocity =
                        *eff_attrs.get(&eve_sde::attr_ids::MAX_VELOCITY).unwrap_or(&new_ball.max_velocity);
                    new_ball.inertia =
                        *eff_attrs.get(&eve_sde::attr_ids::AGILITY).unwrap_or(&new_ball.inertia);
                }
            } else {
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
            }
            if let Some(pcs) = prev_cs {
                let s_frac = (pcs.shield_hp / pcs.max_shield_hp.max(1.0)).clamp(0.0, 1.0);
                let a_frac = (pcs.armor_hp / pcs.max_armor_hp.max(1.0)).clamp(0.0, 1.0);
                let h_frac = (pcs.structure_hp / pcs.max_structure_hp.max(1.0)).clamp(0.0, 1.0);
                let c_frac = (pcs.capacitor / pcs.max_capacitor.max(1.0)).clamp(0.0, 1.0);
                combat_state.shield_hp = combat_state.max_shield_hp * s_frac;
                combat_state.armor_hp = combat_state.max_armor_hp * a_frac;
                combat_state.structure_hp = combat_state.max_structure_hp * h_frac;
                combat_state.capacitor = combat_state.max_capacitor * c_frac;
            }
            let mut slim_meta = ctx.build_ship_slim_meta(char_id, new_ball.type_id);
            slim_meta.is_cloaked = true;
            let spawn_tick = new_handle.spawn_ball_with_meta(new_ball, combat_state, slim_meta).await.unwrap_or(0);
            record_session_change_timer(ctx, char_id, now_unix, dest_sys_id, spawn_tick, "wormhole_jump");

            let new_beyonce_id = match ctx.bind_object(session, BEYONCE_SVC, dest_sys_id as i64) {
                Ok(b) => b,
                Err(e) => return Some(Err(e)),
            };

            Some(Ok(PyRep::keyval([
                ("success", PyRep::Bool(true)), ("solarSystemID", PyRep::Integer(dest_sys_id as i64)),
                ("destinationSystemID", PyRep::Integer(dest_sys_id as i64)), ("beyonceBoundID", PyRep::Integer(new_beyonce_id as i64)),
                ("wormholeID", PyRep::Integer(wh_item_id)), ("exitWormholeID", PyRep::Integer(exit_wh_id)),
                ("signatureID", PyRep::String(sig_id_str)), ("shipJumpMassKg", PyRep::Real(ship_mass)),
                ("propulsionMassAdditionKg", PyRep::Real(prop_mass_add)), ("remainingMassKg", PyRep::Real(remaining_mass)),
                ("maxJumpMassKg", PyRep::Real(max_jump_mass)), ("massStage", PyRep::String(mass_stage.into())),
                ("massStabilityBucket", PyRep::String(buckets.mass_stability_bucket.into())),
                ("massStabilityDescription", PyRep::String(buckets.mass_stability_description.into())),
                ("wormholeSize", PyRep::Real(wh_size)), ("lifetimeStage", PyRep::String(life_stage.into())),
                ("lifetimeBucket", PyRep::String(buckets.lifetime_bucket.into())),
                ("lifetimeDescription", PyRep::String(buckets.lifetime_description.into())),
                ("wormholeAge", PyRep::Integer(wh_age)), ("collapsed", PyRep::Bool(is_collapsed)),
                ("replacementSignatureID", rep_sig_id.map(PyRep::String).unwrap_or(PyRep::None)),
                ("replacementWormholeID", rep_wh_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("replacementWormholeTypeID", rep_wh_type_id.map(|t| PyRep::Integer(t as i64)).unwrap_or(PyRep::None)),
                ("wormholeClass", PyRep::Integer(dest_wh_class as i64)), ("isThera", PyRep::Bool(dest_is_thera)),
                ("isShattered", PyRep::Bool(dest_is_shattered)), ("delayedLocal", PyRep::Bool(dest_wh_class > 0)),
                ("environmentalEffect", PyRep::String(env_effect.into())),
            ])))
        }
        Err(e) => {
            ctx.notify(crate::router::NotificationTarget::Character(char_id), "OnWormholeJumpCancel", "charid", vec![]);
            Some(Err(e))
        }
    }
}

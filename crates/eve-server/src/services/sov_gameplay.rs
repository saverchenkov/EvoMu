//! `sov_gameplay` — Live Equinox Sovereignty, Mercenary Den, Orbital Skyhook, Entosis & Flex Structure Mechanics (`SO-5`, `Q-8`).
//!
//! Exposes ONLY live server protocol, combat, module, and cluster-tick entrypoints:
//! - Orbital Skyhook anchoring, 1 Hz linear reagent maturity & ship-link silo theft (`sov_gameplay_skyhook`).
//! - Mercenary Den `structureDeployment.Anchor` (`handle_anchor_mercenary_den_rpc`), `beyonce.CallComponentFromClient`
//!   (`handle_mercden_component_call`), `structureControl.AttackStructure` (`apply_mercenary_den_weapon_damage`),
//!   Remote Shield Booster repair (`apply_mercenary_den_shield_repair`), and MTO combat completion (`on_space_entity_destroyed`).
//! - ADM-scaled vulnerability windows (`apply_adm_scaled_vulnerability_window_in_snap`), live Entosis Link activation
//!   validation (`precheck_entosis_activation`, `register_entosis_capture_target`), and elapsed-time ADM-scaled
//!   Entosis Hub reinforcement & Constellation Command Node capture (`tick_entosis_captures`).
//! - Sovereignty Hub combat/prospecting upgrade anomaly spawning, Flex structure upgrade gates
//!   (`require_flex_sov_upgrade_online`), and Tenebrex Cyno Jammer 5-minute spool-up & reinforcement disable
//!   (`is_tenebrex_jammer_active_in_snap`).

#![forbid(unsafe_code)]

#[path = "sov_gameplay_skyhook.rs"]
pub mod sov_gameplay_skyhook;
pub use sov_gameplay_skyhook::*;

#[path = "sov_gameplay_entosis.rs"]
pub mod sov_gameplay_entosis;
pub use sov_gameplay_entosis::*;

use crate::router::{NotificationTarget, ServerContext};
use crate::services::quasar_sov_hub::{id_field, push_notice, PendingNotices};
use crate::services::quasar_sov_mercden::{
    encode_evolution_simulation, push_definitions_updated_notice, refresh_activities, simulate_den,
    MERCENARY_DEN_ABSOLUTE_MAX, TYPE_MERCENARY_DEN_MANAGEMENT,
};
use crate::services::sov_services::{emit_sov_structures_updated, TYPE_ORBITAL_SKYHOOK};
use eve_db::{InventoryFlag, InventoryItemRecord, PersistedMercenaryDen, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::quasar::encode_bytes_field;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::sync::{Arc, RwLock};

/// SDE type ID for Mercenary Den (`85230`).
pub const TYPE_MERCENARY_DEN: u32 = 85_230;
/// SDE type ID for Infomorph Decryption Key (`85746`).
pub const TYPE_INFOMORPH_DECRYPTION_KEY: u32 = 85_746;
/// Maximum anchoring & extraction distance from a Temperate planet's Orbital Skyhook (`10,000 m`).
pub const MERC_DEN_MAX_SKYHOOK_DIST_M: f64 = 10_000.0;
/// Mercenary Den reinforcement duration (24 hours = `86,400 s`).
pub const MERC_DEN_REINFORCEMENT_SECS: u64 = 24 * 3_600;
/// Mercenary Den post-reinforcement vulnerability window (6 hours = `21,600 s`).
pub const MERC_DEN_VULNERABILITY_WINDOW_SECS: u64 = 6 * 3_600;
/// Base shield & hull HP of an anchored Mercenary Den.
pub const MERC_DEN_BASE_HP: f64 = 5_000.0;

pub(crate) fn flush_pending_quasar(ctx: &ServerContext, pending: &PendingNotices, now_unix: u64) {
    let items: Vec<_> = pending
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .drain(..)
        .collect();
    for (scope, msg) in items {
        let _ = ctx.quasar_gateway.publish_notice(scope, msg, now_unix);
    }
}

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

// =========================================================================
// 1. Mercenary Den Anchoring, Component Extraction, Combat & Repair
// =========================================================================

/// Live `structureDeployment.Anchor` handler for `TYPE_MERCENARY_DEN` (`85230`).
pub async fn handle_anchor_mercenary_den_rpc(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: "Cannot anchor a Mercenary Den while docked".into(),
        });
    }
    let sys_id = session.solar_system_id.ok_or_else(|| BridgeRpcError {
        code: "NoSolarSystem".into(),
        message: "No active solar system".into(),
    })?;
    let ship_id = session.ship_id.filter(|&id| id > 0).ok_or_else(|| BridgeRpcError {
        code: "NoActiveShip".into(),
        message: "Active ship required in space".into(),
    })?;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, ship_ball) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .ok()
        .flatten()
        .ok_or_else(|| BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: format!("Ship {ship_id} is not present in space"),
        })?;

    if ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .is_none_or(|s| s.security_status > 0.0 || s.solar_system_id >= 31_000_000)
    {
        return Err(BridgeRpcError {
            code: "SovereigntyNullsecRequired".into(),
            message: "Mercenary Dens can only be anchored in sovereign nullsec".into(),
        });
    }

    let explicit_skyhook_id = args.get(1).and_then(|a| a.as_i64()).filter(|&v| v > 0);
    let explicit_planet_id = args.get(2).and_then(|a| a.as_i64()).filter(|&v| v > 0);

    // Resolve host Skyhook in system and measure live ship-to-Skyhook distance.
    let candidates: Vec<(i64, DVec3, Option<i64>)> = ctx.db.with_snapshot(|snap| {
        snap.structures
            .values()
            .filter(|st| {
                st.type_id == TYPE_ORBITAL_SKYHOOK
                    && st.solar_system_id == sys_id
                    && st.state != "Destroyed"
                    && explicit_skyhook_id.is_none_or(|want| st.structure_id == want)
            })
            .map(|st| {
                let pid = snap.sov_runtime.skyhook_planets.get(&st.structure_id).copied();
                (st.structure_id, DVec3::from_array(st.position), pid)
            })
            .collect()
    });
    if candidates.is_empty() {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnMercenaryDenRegistrationInvalid",
            "charid",
            vec![PyRep::Integer(explicit_skyhook_id.unwrap_or(0)), PyRep::String("SkyhookNotFound".into())],
        );
        return Err(BridgeRpcError {
            code: "SkyhookNotFound".into(),
            message: "No active Orbital Skyhook found in this solar system".into(),
        });
    }

    let mut chosen: Option<(i64, DVec3, Option<i64>, f64)> = None;
    for (sh_id, fallback_pos, pid) in candidates {
        let sh_pos = sys_handle
            .get_ball_snapshot(sh_id)
            .await
            .ok()
            .flatten()
            .map(|(_, b)| b.position)
            .unwrap_or(fallback_pos);
        let dist = (ship_ball.position.distance(sh_pos) - ship_ball.radius - 500.0).max(0.0);
        if chosen.as_ref().is_none_or(|c| dist < c.3) {
            chosen = Some((sh_id, sh_pos, pid, dist));
        }
    }
    let (skyhook_id, sh_pos, skyhook_planet_opt, dist_m) = chosen.expect("non-empty candidates");
    if dist_m > MERC_DEN_MAX_SKYHOOK_DIST_M {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnMercenaryDenRegistrationInvalid",
            "charid",
            vec![PyRep::Integer(skyhook_id), PyRep::String("MercenaryDenTooFarFromSkyhook".into())],
        );
        return Err(BridgeRpcError {
            code: "MercenaryDenTooFarFromSkyhook".into(),
            message: format!(
                "Ship is {dist_m:.0} m from Orbital Skyhook {skyhook_id} (maximum {MERC_DEN_MAX_SKYHOOK_DIST_M:.0} m)"
            ),
        });
    }

    let planet_id = match (skyhook_planet_opt, explicit_planet_id) {
        (Some(sp), Some(ep)) if sp != ep => {
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnMercenaryDenRegistrationInvalid",
                "charid",
                vec![PyRep::Integer(skyhook_id), PyRep::String("DenPlanetMismatch".into())],
            );
            return Err(BridgeRpcError {
                code: "DenPlanetMismatch".into(),
                message: format!("Planet {ep} does not match Skyhook {skyhook_id} planet {sp}"),
            });
        }
        (Some(sp), _) => sp,
        (None, Some(ep)) => ep,
        (None, None) => {
            return Err(BridgeRpcError {
                code: "SkyhookPlanetUnknown".into(),
                message: format!("Orbital Skyhook {skyhook_id} has no bound planet"),
            });
        }
    };
    let is_temperate = ctx
        .sde
        .celestials_in_system(sys_id)
        .iter()
        .any(|c| c.celestial_id == planet_id && c.type_id == 11);
    if !is_temperate {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnMercenaryDenRegistrationInvalid",
            "charid",
            vec![PyRep::Integer(skyhook_id), PyRep::String("PlanetNotTemperate".into())],
        );
        return Err(BridgeRpcError {
            code: "PlanetNotTemperate".into(),
            message: format!("Planet {planet_id} is not a Temperate planet (type 11)"),
        });
    }

    let now = ctx.now_unix();
    let pending: PendingNotices = Arc::new(RwLock::new(Vec::new()));
    let den_pos = DVec3::new(sh_pos.x + 2_500.0, sh_pos.y, sh_pos.z);

    let den_id_res = ctx.db.try_with_mut_snapshot(|snap| {
        if snap
            .system_states
            .get(&sys_id)
            .and_then(|s| s.sov_alliance_id)
            .is_none()
        {
            return Err(BridgeRpcError {
                code: "SovereigntyNotClaimed".into(),
                message: format!("System {sys_id} does not have claimed sovereignty"),
            });
        }
        let skill_lvl = snap
            .skills
            .get(&char_id)
            .and_then(|m| m.get(&TYPE_MERCENARY_DEN_MANAGEMENT))
            .map(|r| u64::from(r.level))
            .unwrap_or(0)
            .min(MERCENARY_DEN_ABSOLUTE_MAX);
        let owned = snap
            .sov_runtime
            .mercenary_dens
            .values()
            .filter(|d| d.owner_char_id == char_id)
            .count() as u64;
        if skill_lvl == 0 || owned >= skill_lvl {
            return Err(BridgeRpcError {
                code: "MercenaryDenCapExceeded".into(),
                message: format!(
                    "Character {char_id} owns {owned} Mercenary Dens (Mercenary Den Management cap: {skill_lvl})"
                ),
            });
        }
        let item_id = snap
            .items
            .values()
            .find(|i| {
                i.location_id == ship_id
                    && i.type_id == TYPE_MERCENARY_DEN
                    && i.quantity > 0
                    && matches!(i.flag, InventoryFlag::Cargo | InventoryFlag::Hangar | InventoryFlag::None)
            })
            .map(|i| i.item_id)
            .ok_or_else(|| BridgeRpcError {
                code: "MissingMercenaryDenItem".into(),
                message: "Ship cargo does not contain a Mercenary Den (85230)".into(),
            })?;
        if let Some(it) = snap.items.get_mut(&item_id) {
            it.quantity -= 1;
            if it.quantity <= 0 {
                snap.items.remove(&item_id);
            }
        }

        snap.next_entity_id += 1;
        let den_id = snap.next_entity_id;
        let pos_arr = [den_pos.x, den_pos.y, den_pos.z];
        snap.structures.insert(
            den_id,
            UpwellStructureRecord {
                structure_id: den_id,
                type_id: TYPE_MERCENARY_DEN,
                owner_corp_id: 0,
                solar_system_id: sys_id,
                name: format!("Mercenary Den ({char_id})"),
                position: pos_arr,
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: MERC_DEN_BASE_HP,
                armor_hp: MERC_DEN_BASE_HP,
                hull_hp: MERC_DEN_BASE_HP,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        let den = PersistedMercenaryDen {
            den_id,
            owner_char_id: char_id,
            skyhook_id,
            solar_system_id: sys_id,
            planet_id,
            type_id: TYPE_MERCENARY_DEN,
            enabled: true,
            cargo_extraction_enabled: true,
            development_level: 0,
            anarchy_level: 0,
            simulated_at_unix: now,
            infomorphs: 0,
            next_activity_generation_unix: now,
        };
        push_definitions_updated_notice(&pending, &den);
        snap.sov_runtime.mercenary_dens.insert(den_id, den);
        snap.sov_runtime
            .mercenary_den_reinforcement
            .insert(den_id, (0, 0, 100));
        snap.sov_runtime.mercenary_den_tax_pct.entry(skyhook_id).or_insert(0);
        refresh_activities(snap, &pending, den_id, now);
        Ok(den_id)
    });

    let den_id = match den_id_res {
        Ok(id) => id,
        Err(err) => {
            if err.code == "MercenaryDenCapExceeded" {
                ctx.notify(
                    NotificationTarget::Character(char_id),
                    "OnMercenaryDenRegistrationInvalid",
                    "charid",
                    vec![PyRep::Integer(skyhook_id), PyRep::String(err.code.clone())],
                );
            }
            return Err(err);
        }
    };

    let _ = sys_handle
        .spawn_ball(Ball {
            id: den_id,
            type_id: TYPE_MERCENARY_DEN,
            position: den_pos,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 1.0e7,
            radius: 150.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await;
    flush_pending_quasar(ctx, &pending, now);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnMercenaryDenRegistrationSuccess",
        "charid",
        vec![PyRep::Integer(den_id), PyRep::Integer(skyhook_id)],
    );
    emit_sov_structures_updated(ctx, sys_id, vec![PyRep::Integer(den_id)]);
    Ok(PyRep::keyval([
        ("structureID", PyRep::Integer(den_id)),
        ("denID", PyRep::Integer(den_id)),
        ("skyhookID", PyRep::Integer(skyhook_id)),
        ("ownerCharID", PyRep::Integer(char_id)),
        ("typeID", PyRep::Integer(TYPE_MERCENARY_DEN as i64)),
    ]))
}

/// Live `beyonce.CallComponentFromClient` handler for `comp_name == "mercenaryDen"`.
pub async fn handle_mercden_component_call(
    ctx: &ServerContext,
    _session: &SessionState,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    den_id: i64,
    comp_name: &str,
    comp_method: &str,
    _args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    if !matches!(
        comp_method,
        "extract_infomorphs_to_ship" | "ExtractInfomorphsToShip" | "extract_infomorphs"
    ) {
        return Err(BridgeRpcError {
            code: "UnsupportedComponentCall".into(),
            message: format!("Unsupported Mercenary Den component method {comp_name}.{comp_method}"),
        });
    }
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, ship_ball) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .ok()
        .flatten()
        .ok_or_else(|| BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: format!("Ship {ship_id} is not in space in system {sys_id}"),
        })?;
    let den_pos = ctx
        .db
        .with_snapshot(|s| s.structures.get(&den_id).map(|st| (st.solar_system_id, st.position)))
        .ok_or_else(|| BridgeRpcError {
            code: "MercenaryDenNotFound".into(),
            message: format!("Mercenary Den {den_id} not found"),
        })?;
    if den_pos.0 != sys_id {
        return Err(BridgeRpcError {
            code: "MercenaryDenNotInSystem".into(),
            message: format!("Mercenary Den {den_id} is not in system {sys_id}"),
        });
    }
    let live_den_pos = sys_handle
        .get_ball_snapshot(den_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b.position)
        .unwrap_or_else(|| DVec3::from_array(den_pos.1));
    let dist = (ship_ball.position.distance(live_den_pos) - ship_ball.radius - 150.0).max(0.0);
    if dist > MERC_DEN_MAX_SKYHOOK_DIST_M {
        return Err(BridgeRpcError {
            code: "MercenaryDenOutOfRange".into(),
            message: format!("Ship is {dist:.0} m from Mercenary Den {den_id} (max 10,000 m)"),
        });
    }

    let now = ctx.now_unix();
    let pending: PendingNotices = Arc::new(RwLock::new(Vec::new()));
    let (owner_share, tax_share) = ctx.db.try_with_mut_snapshot(|snap| {
        let reinf = snap
            .sov_runtime
            .mercenary_den_reinforcement
            .get(&den_id)
            .copied()
            .unwrap_or((0, 0, 100));
        let den_check = snap
            .sov_runtime
            .mercenary_dens
            .get(&den_id)
            .cloned()
            .ok_or_else(|| BridgeRpcError {
                code: "MercenaryDenNotFound".into(),
                message: format!("Mercenary Den {den_id} not found"),
            })?;
        if den_check.owner_char_id != char_id {
            return Err(BridgeRpcError {
                code: "NotMercenaryDenOwner".into(),
                message: "Only the owning character may extract Infomorph Decryption Keys".into(),
            });
        }
        if !den_check.enabled || reinf.2 < 50 || now < reinf.0 {
            return Err(BridgeRpcError {
                code: "MercenaryDenReinforced".into(),
                message: "Mercenary Den is reinforced and inaccessible until shields reach >= 50%".into(),
            });
        }
        let den_mut = snap.sov_runtime.mercenary_dens.get_mut(&den_id).expect("checked");
        if simulate_den(den_mut, now) {
            let mut n = Vec::new();
            id_field(1, den_id as u64, &mut n);
            encode_bytes_field(2, &encode_evolution_simulation(den_mut), &mut n);
            push_notice(
                &pending,
                den_mut.solar_system_id,
                "eve_public.sovereignty.mercenaryden.api.EvolutionStateChangedNotice",
                n,
            );
        }
        let total = den_mut.infomorphs;
        if total == 0 {
            return Err(BridgeRpcError {
                code: "NoInfomorphsAvailable".into(),
                message: "Mercenary Den currently holds 0 Infomorph Decryption Keys".into(),
            });
        }
        den_mut.infomorphs = 0;
        let skyhook_id = den_mut.skyhook_id;
        let tax_pct = u32::from(
            snap.sov_runtime
                .mercenary_den_tax_pct
                .get(&skyhook_id)
                .copied()
                .unwrap_or(0)
                .min(100),
        );
        let tax_qty = (total.saturating_mul(tax_pct)) / 100;
        let owner_qty = total.saturating_sub(tax_qty);

        if owner_qty > 0 {
            snap.next_entity_id += 1;
            let item_id = snap.next_entity_id;
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id: TYPE_INFOMORPH_DECRYPTION_KEY,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: owner_qty as i32,
                    ..Default::default()
                },
            );
        }
        if tax_qty > 0 {
            let sov_corp = snap
                .structures
                .get(&skyhook_id)
                .map(|s| s.owner_corp_id)
                .unwrap_or(0);
            snap.next_entity_id += 1;
            let tax_item_id = snap.next_entity_id;
            snap.items.insert(
                tax_item_id,
                InventoryItemRecord {
                    item_id: tax_item_id,
                    type_id: TYPE_INFOMORPH_DECRYPTION_KEY,
                    owner_id: sov_corp.max(char_id),
                    location_id: skyhook_id,
                    flag: InventoryFlag::Hangar,
                    quantity: tax_qty as i32,
                    ..Default::default()
                },
            );
        }
        Ok((owner_qty, tax_qty))
    })?;
    flush_pending_quasar(ctx, &pending, now);
    Ok(PyRep::keyval([
        ("denID", PyRep::Integer(den_id)),
        ("ownerShare", PyRep::Integer(owner_share as i64)),
        ("taxShare", PyRep::Integer(tax_share as i64)),
        ("typeID", PyRep::Integer(TYPE_INFOMORPH_DECRYPTION_KEY as i64)),
    ]))
}

/// Applies live weapon damage (`structureControl.AttackStructure`) to a Mercenary Den:
/// - Depleting shield puts the den into a 24h reinforcement with a 6h vulnerability window and disables it.
/// - Attacks during the 24h reinforcement or outside the 6h window are rejected.
/// - Depleting hull inside the 6h vulnerability window destroys the den and rolls a 50% drop per stored stack.
pub async fn apply_mercenary_den_weapon_damage(
    ctx: &ServerContext,
    den_id: i64,
    damage: f64,
) -> Result<PyRep, BridgeRpcError> {
    let now = ctx.now_unix();
    let pending: PendingNotices = Arc::new(RwLock::new(Vec::new()));
    enum Outcome {
        Damaged { state: String, shield_hp: f64, hull_hp: f64, reinf_until: u64, vuln_end: u64 },
        Destroyed { sys_id: u32, owner_cid: i64, wreck_id: i64, wreck_pos: [f64; 3], dropped_stacks: usize, dropped_qty: u32 },
    }
    let outcome = ctx.db.try_with_mut_snapshot(|snap| {
        let (mut reinf_until, mut vuln_end, _) = snap
            .sov_runtime
            .mercenary_den_reinforcement
            .get(&den_id)
            .copied()
            .unwrap_or((0, 0, 100));
        if reinf_until > 0 && now < reinf_until {
            return Err(BridgeRpcError {
                code: "MercenaryDenReinforced".into(),
                message: format!("Mercenary Den {den_id} is reinforced until {reinf_until}"),
            });
        }
        if vuln_end > 0 && now >= vuln_end {
            return Err(BridgeRpcError {
                code: "MercenaryDenOutsideVulnerabilityWindow".into(),
                message: format!("Mercenary Den {den_id} 6h vulnerability window expired at {vuln_end}"),
            });
        }
        let st = snap.structures.get_mut(&den_id).ok_or_else(|| BridgeRpcError {
            code: "MercenaryDenNotFound".into(),
            message: format!("Mercenary Den {den_id} not found"),
        })?;
        if st.state == "Destroyed" {
            return Err(BridgeRpcError {
                code: "StructureDestroyed".into(),
                message: format!("Mercenary Den {den_id} is already destroyed"),
            });
        }
        st.last_damage_unix = now;
        let sys_id = st.solar_system_id;
        let pos_arr = st.position;

        // Phase 1: Shield layer before reinforcement (or while awaiting >=50% repair outside active window).
        if reinf_until == 0 {
            st.shield_hp = (st.shield_hp - damage.max(0.0)).max(0.0);
            let mut shield_pct = ((st.shield_hp / MERC_DEN_BASE_HP) * 100.0).round().clamp(0.0, 100.0) as u8;
            if st.shield_hp <= 0.0 {
                reinf_until = now + MERC_DEN_REINFORCEMENT_SECS;
                vuln_end = reinf_until + MERC_DEN_VULNERABILITY_WINDOW_SECS;
                shield_pct = 0;
                st.state = "HullReinforced".into();
                st.reinforced_until_unix = reinf_until;
                if let Some(den_mut) = snap.sov_runtime.mercenary_dens.get_mut(&den_id) {
                    let _ = simulate_den(den_mut, now);
                    den_mut.enabled = false;
                    let mut n = Vec::new();
                    id_field(1, den_id as u64, &mut n);
                    encode_bytes_field(2, &encode_evolution_simulation(den_mut), &mut n);
                    push_notice(
                        &pending,
                        sys_id,
                        "eve_public.sovereignty.mercenaryden.api.EvolutionStateChangedNotice",
                        n,
                    );
                }
            }
            let st_s = st.state.clone();
            let s_hp = st.shield_hp;
            let h_hp = st.hull_hp;
            snap.sov_runtime
                .mercenary_den_reinforcement
                .insert(den_id, (reinf_until, vuln_end, shield_pct));
            return Ok(Outcome::Damaged { state: st_s, shield_hp: s_hp, hull_hp: h_hp, reinf_until, vuln_end });
        }

        // Phase 2: Inside the 6-hour post-reinforcement vulnerability window (`reinf_until <= now < vuln_end`).
        st.state = "HullVulnerable".into();
        st.hull_hp = (st.hull_hp - damage.max(0.0)).max(0.0);
        if st.hull_hp > 0.0 {
            let st_s = st.state.clone();
            let s_hp = st.shield_hp;
            let h_hp = st.hull_hp;
            return Ok(Outcome::Damaged { state: st_s, shield_hp: s_hp, hull_hp: h_hp, reinf_until, vuln_end });
        }

        // Destroyed! Roll 50% drop chance independently for each stored Infomorph Decryption Key stack.
        st.state = "Destroyed".into();
        let removed_den = snap.sov_runtime.mercenary_dens.remove(&den_id);
        snap.sov_runtime.mercenary_den_reinforcement.remove(&den_id);
        snap.sov_runtime.mercenary_activities.retain(|_, a| a.den_id != den_id);
        let owner_cid = removed_den.as_ref().map(|d| d.owner_char_id).unwrap_or(0);

        let mut stack_sizes: Vec<u32> = Vec::new();
        let item_ids: Vec<i64> = snap
            .items
            .values()
            .filter(|i| i.location_id == den_id && i.type_id == TYPE_INFOMORPH_DECRYPTION_KEY && i.quantity > 0)
            .map(|i| i.item_id)
            .collect();
        for iid in item_ids {
            if let Some(it) = snap.items.remove(&iid) {
                stack_sizes.push(it.quantity as u32);
            }
        }
        if let Some(den_rec) = &removed_den {
            let mut rem_info = den_rec.infomorphs;
            while rem_info > 0 {
                let chunk = rem_info.min(50);
                stack_sizes.push(chunk);
                rem_info -= chunk;
            }
        }

        snap.next_entity_id += 1;
        let wreck_id = snap.next_entity_id;
        snap.items.insert(
            wreck_id,
            InventoryItemRecord {
                item_id: wreck_id,
                type_id: 26_561,
                owner_id: owner_cid.max(1),
                location_id: i64::from(sys_id),
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions.insert(wreck_id, pos_arr);

        let mut dropped_stacks = 0usize;
        let mut dropped_qty = 0u32;
        for (idx, qty) in stack_sizes.into_iter().enumerate() {
            let roll = splitmix64((den_id as u64) ^ ((idx as u64).wrapping_mul(0x9E37_79B9)) ^ u64::from(qty));
            if (roll & 1) == 0 {
                dropped_stacks += 1;
                dropped_qty += qty;
                snap.next_entity_id += 1;
                let lid = snap.next_entity_id;
                snap.items.insert(
                    lid,
                    InventoryItemRecord {
                        item_id: lid,
                        type_id: TYPE_INFOMORPH_DECRYPTION_KEY,
                        owner_id: owner_cid.max(1),
                        location_id: wreck_id,
                        flag: InventoryFlag::Cargo,
                        quantity: qty as i32,
                        ..Default::default()
                    },
                );
            }
        }
        Ok(Outcome::Destroyed { sys_id, owner_cid, wreck_id, wreck_pos: pos_arr, dropped_stacks, dropped_qty })
    })?;

    flush_pending_quasar(ctx, &pending, now);
    match outcome {
        Outcome::Damaged { state, shield_hp, hull_hp, reinf_until, vuln_end } => Ok(PyRep::keyval([
            ("structureID", PyRep::Integer(den_id)),
            ("state", PyRep::String(state)),
            ("shieldHP", PyRep::Real(shield_hp)),
            ("hullHP", PyRep::Real(hull_hp)),
            ("reinforcedUntil", PyRep::Integer(reinf_until as i64)),
            ("vulnerabilityEnds", PyRep::Integer(vuln_end as i64)),
            ("vulnerabilityEndsAt", PyRep::Integer(vuln_end as i64)),
        ])),
        Outcome::Destroyed { sys_id, owner_cid, wreck_id, wreck_pos, dropped_stacks, dropped_qty } => {
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let _ = sys_handle.remove_ball(den_id).await;
            let _ = sys_handle
                .spawn_ball(Ball {
                    id: wreck_id,
                    type_id: 26_561,
                    position: DVec3::from_array(wreck_pos),
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 0.0,
                    mass: 50_000.0,
                    radius: 40.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Rigid,
                })
                .await;
            if owner_cid > 0 {
                ctx.notify(
                    NotificationTarget::Character(owner_cid),
                    "OnMyMercenaryDenRemoved",
                    "charid",
                    vec![PyRep::Integer(den_id)],
                );
            }
            emit_sov_structures_updated(ctx, sys_id, vec![PyRep::Integer(den_id)]);
            Ok(PyRep::keyval([
                ("structureID", PyRep::Integer(den_id)),
                ("state", PyRep::String("Destroyed".into())),
                ("wreckID", PyRep::Integer(wreck_id)),
                ("droppedStacks", PyRep::Integer(dropped_stacks as i64)),
                ("droppedQuantity", PyRep::Integer(dropped_qty as i64)),
            ]))
        }
    }
}

/// Applies Remote Shield Booster (`group_id == 41 | 1697`) repair to a Mercenary Den after its 24h
/// reinforcement timer expires, re-enabling the den once shields reach `>= 50%`.
pub async fn apply_mercenary_den_shield_repair(
    ctx: &ServerContext,
    sys_id: u32,
    ship_id: i64,
    den_id: i64,
    repair_hp: f64,
) -> Result<(u8, bool), BridgeRpcError> {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, ship_ball) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .ok()
        .flatten()
        .ok_or_else(|| BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: "Ship must be in space to repair Mercenary Den shields".into(),
        })?;
    let den_pos_opt = ctx
        .db
        .with_snapshot(|s| s.structures.get(&den_id).map(|st| (st.solar_system_id, st.position)));
    let Some((den_sys, pos_arr)) = den_pos_opt else {
        return Err(BridgeRpcError {
            code: "MercenaryDenNotFound".into(),
            message: format!("Mercenary Den {den_id} not found"),
        });
    };
    if den_sys != sys_id {
        return Err(BridgeRpcError {
            code: "TargetNotInSystem".into(),
            message: "Mercenary Den is not in the same solar system".into(),
        });
    }
    let den_pos = sys_handle
        .get_ball_snapshot(den_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b.position)
        .unwrap_or_else(|| DVec3::from_array(pos_arr));
    if ship_ball.position.distance(den_pos) > 50_000.0 {
        return Err(BridgeRpcError {
            code: "TargetOutOfRange".into(),
            message: "Mercenary Den is outside Remote Shield Booster range".into(),
        });
    }

    let now = ctx.now_unix();
    let pending: PendingNotices = Arc::new(RwLock::new(Vec::new()));
    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let (reinf_until, vuln_end, _) = snap
            .sov_runtime
            .mercenary_den_reinforcement
            .get(&den_id)
            .copied()
            .unwrap_or((0, 0, 100));
        if reinf_until > 0 && now < reinf_until {
            return Err(BridgeRpcError {
                code: "MercenaryDenStillInReinforcement".into(),
                message: format!("Mercenary Den {den_id} cannot be repaired while reinforced until {reinf_until}"),
            });
        }
        let st = snap.structures.get_mut(&den_id).ok_or_else(|| BridgeRpcError {
            code: "MercenaryDenNotFound".into(),
            message: format!("Mercenary Den {den_id} not found"),
        })?;
        st.shield_hp = (st.shield_hp + repair_hp.max(500.0)).min(MERC_DEN_BASE_HP);
        let pct = ((st.shield_hp / MERC_DEN_BASE_HP) * 100.0).round().clamp(0.0, 100.0) as u8;
        let mut enabled_now = false;
        if pct >= 50 {
            st.state = "ShieldVulnerable".into();
            st.reinforced_until_unix = 0;
            snap.sov_runtime.mercenary_den_reinforcement.insert(den_id, (0, 0, pct));
            if let Some(den_mut) = snap.sov_runtime.mercenary_dens.get_mut(&den_id) {
                den_mut.enabled = true;
                den_mut.simulated_at_unix = now;
                enabled_now = true;
                let mut n = Vec::new();
                id_field(1, den_id as u64, &mut n);
                encode_bytes_field(2, &encode_evolution_simulation(den_mut), &mut n);
                push_notice(
                    &pending,
                    sys_id,
                    "eve_public.sovereignty.mercenaryden.api.EvolutionStateChangedNotice",
                    n,
                );
            }
        } else {
            snap.sov_runtime
                .mercenary_den_reinforcement
                .insert(den_id, (reinf_until, vuln_end, pct));
        }
        Ok((pct, enabled_now))
    })?;
    flush_pending_quasar(ctx, &pending, now);
    Ok(res)
}

/// Dispatches high-slot weapon damage to Mercenary Dens, Skyhooks, or FW/Insurgency structures,
/// consuming weapon ammo charges and stamping `last_damage_tick`.
pub async fn dispatch_special_structure_attack(
    ctx: &ServerContext,
    char_id: i64,
    structure_id: i64,
    st_type_id: u32,
    is_fw_ins: bool,
    volley_dmg: f64,
    tick: u64,
    charges: &[i64],
) -> Result<PyRep, BridgeRpcError> {
    let rep = if st_type_id == TYPE_MERCENARY_DEN {
        apply_mercenary_den_weapon_damage(ctx, structure_id, volley_dmg).await?
    } else if st_type_id == crate::services::sov_services::TYPE_ORBITAL_SKYHOOK {
        apply_skyhook_weapon_damage(ctx, structure_id, volley_dmg)?
    } else if is_fw_ins {
        crate::services::fw_gameplay::apply_fw_or_insurgency_structure_damage(ctx, char_id, structure_id, volley_dmg).await?
    } else {
        return Err(BridgeRpcError {
            code: "InvalidSpecialStructure".into(),
            message: format!("Structure {structure_id} is not a special target"),
        });
    };
    ctx.db.with_mut_snapshot(|snap| {
        for cid in charges {
            if let Some(ch) = snap.items.get_mut(cid) {
                if ch.quantity > 1 {
                    ch.quantity -= 1;
                    ch.version += 1;
                } else {
                    snap.items.remove(cid);
                }
            }
        }
        if let Some(s) = snap.structures.get_mut(&structure_id) {
            s.last_damage_tick = tick;
        }
    });
    Ok(rep)
}

/// Applies secondary side-effects (suspect timers in corrupted Stage-5 high-sec, cyno jammer state notices)
/// after an Upwell structure takes normal damage.
pub fn handle_normal_structure_damage_aftermath(
    ctx: &ServerContext,
    caller_sys: u32,
    sys_sec: f64,
    char_id: i64,
    ship_id: i64,
    st_type_id: u32,
    out_state: &str,
    hs_stage5_suspect: bool,
    now: u64,
) {
    if hs_stage5_suspect {
        if let Ok(mut cw) = ctx.crimewatch.write() {
            let _ = cw.handle_combat_aggression(char_id, 0, ship_id, caller_sys, sys_sec, false, now);
        }
    }
    if st_type_id == TYPE_TENEBREX_CYNO_JAMMER
        && matches!(out_state, "ArmorReinforced" | "HullReinforced" | "Destroyed")
    {
        ctx.notify(
            crate::router::NotificationTarget::SolarSystem(caller_sys),
            "OnCynoJammerChanged",
            "solarsystemid",
            vec![PyRep::Integer(caller_sys as i64), PyRep::Integer(0)],
        );
    }
}

pub async fn handle_confirm_skyhook_sov_management(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let Some(skyhook_id) = args.first().and_then(|a| a.as_i64()) else {
        return Err(BridgeRpcError {
            code: "InvalidArguments".into(),
            message: "ConfirmSkyhookSovManagement requires skyhookID".into(),
        });
    };
    let tax_pct = args
        .get(1)
        .and_then(|a| a.as_i64())
        .unwrap_or(0)
        .clamp(0, 100) as u8;

    let is_authorized = ctx.db.with_snapshot(|snap| {
        let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
        let Some(corp) = snap.corporations.get(&corp_id) else {
            return false;
        };
        let role = corp.member_roles.get(&char_id).copied().unwrap_or(session.corp_role);
        let has_role = char_id == corp.ceo_id
            || eve_db::corp_roles::has_role(
                role,
                eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER,
            );
        if !has_role {
            return false;
        }
        let Some(st) = snap.structures.get(&skyhook_id) else {
            return false;
        };
        st.owner_corp_id == corp_id
            || snap
                .system_states
                .get(&st.solar_system_id)
                .and_then(|s| s.sov_alliance_id)
                .is_some_and(|aid| corp.alliance_id == Some(aid))
    });

    if !is_authorized {
        return Err(BridgeRpcError {
            code: "CorpRoleDenied".into(),
            message: "Only owner corporation directors or sovereign alliance directors may configure Skyhook management and tax".into(),
        });
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.mercenary_den_tax_pct.insert(skyhook_id, tax_pct);
    });

    Ok(PyRep::keyval([
        ("skyhookID", PyRep::Integer(skyhook_id)),
        ("taxPercent", PyRep::Integer(tax_pct as i64)),
    ]))
}

pub fn try_handle_sov_hub_data_hack(
    ctx: &ServerContext,
    char_id: i64,
    target_id: i64,
    module_id: i64,
    base_coherence: f64,
    base_strength: f64,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let sov_hub_opt = ctx.db.with_snapshot(|snap| {
        let st = snap.structures.get(&target_id)?;
        if st.type_id != crate::services::sov_services::TYPE_SOVEREIGNTY_HUB {
            return None;
        }
        let caller_char = snap.characters.get(&char_id)?;
        let caller_corp = caller_char.corporation_id;
        let caller_aid = snap.corporations.get(&caller_corp).and_then(|c| c.alliance_id);
        let hub_corp = st.owner_corp_id;
        let hub_aid = snap.corporations.get(&hub_corp).and_then(|c| c.alliance_id);
        let is_friendly = caller_corp == hub_corp || (caller_aid.is_some() && caller_aid == hub_aid);
        Some((st.solar_system_id, is_friendly))
    });
    let (hub_sys, is_friendly) = sov_hub_opt?;
    if is_friendly {
        return Some(Err(BridgeRpcError {
            code: "CannotHackFriendlySovHub".into(),
            message: "Cannot hack your own corporation or alliance Sovereignty Hub".into(),
        }));
    }
    let skills = ctx.character_skill_levels(char_id);
    let hack_lvl = skills.get(&3405).copied().unwrap_or(0).max(skills.get(&3408).copied().unwrap_or(0));
    if hack_lvl == 0 {
        return Some(Err(BridgeRpcError {
            code: "HackingSkillRequired".into(),
            message: "Hacking (3405) or Archaeology (3408) skill required to operate analyzer".into(),
        }));
    }
    let coherence = base_coherence + 10.0 * (hack_lvl as f64);
    let strength = base_strength + 2.0 * (hack_lvl as f64);
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnHackingStart",
        "charid",
        vec![PyRep::keyval([
            ("moduleID", PyRep::Integer(module_id)),
            ("containerID", PyRep::Integer(target_id)),
        ])],
    );
    crate::services::sov_services::record_hub_hack(ctx, hub_sys, target_id, 3_600);
    Some(Ok(PyRep::keyval([
        ("hubID", PyRep::Integer(target_id)),
        ("hacked", PyRep::Bool(true)),
        ("virusCoherence", PyRep::Real(coherence)),
        ("virusStrength", PyRep::Real(strength)),
        ("disabledDurationSeconds", PyRep::Integer(3_600)),
    ])))
}



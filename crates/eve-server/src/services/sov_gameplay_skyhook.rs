//! `sov_gameplay_skyhook` — Orbital Skyhook Anchoring, 1 Hz Reagent Maturity & Silo Ship-Link (`Q-8`, `SO-5`).
//!
//! Implements live V24 Orbital Skyhook mechanics:
//! - `structureDeployment.Anchor` (`handle_anchor_skyhook_rpc`) for `TYPE_ORBITAL_SKYHOOK` (`81080`).
//! - Hourly planetary reagent harvesting (`SKYHOOK_HOURLY_REAGENT_YIELD`) and exact 1 Hz linear
//!   reagent maturity (`advance_skyhook_reagents_in_snap`) with sub-unit remainder tracking so
//!   `86,400` 1-second cluster ticks mature the exact same quantity as a single 24-hour step.
//! - V24 `beyonce.CallComponentFromClient` handlers (`handle_skyhook_component_call`) for:
//!   - `linkWithShip.InitiateLink` / `InitiateLinkQuick`: enforces same-system presence, `<= 15 km`
//!     range, open theft vulnerability window, `<= 10%` Skyhook shield HP, Cruiser+ or Upwell Hauler
//!     hull class, `1,000 m/s` max velocity cap, warp/cloak/dock/tether lock (`is_ship_skyhook_linked`),
//!     and 10-minute (`600s`) active link countdown resolved in `tick_skyhooks`.
//!   - `orbitalSkyhook.is_character_authorised` & `ExtractReagentsToShip` for authorized corporation
//!     directors/station managers extracting matured reagents from the secure bay into cargo.

#![forbid(unsafe_code)]

use crate::quasar_gateway::NoticeScope;
use crate::router::{NotificationTarget, ServerContext};
use crate::services::sov_services::{
    TYPE_MAGMATIC_GAS, TYPE_ORBITAL_SKYHOOK, TYPE_SUPERIONIC_ICE,
};
use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord, WorldStateSnapshot};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, GoogleAny, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;

/// Skyhook reagent maturity cycle (24 hours = 86,400 seconds).
pub const SKYHOOK_MATURITY_CYCLE_SECS: u64 = 24 * 3_600;
/// DERIVED (synthetic): matches the 50 units / 3,600s planetary reagent harvest rate in `quasar_sov_resources.rs`.
pub const SKYHOOK_HOURLY_REAGENT_YIELD: u64 = 50;
/// Orbital Skyhook Reagent Silo ship-link duration (10 minutes = 600 seconds, `Orbital_Skyhook.md`).
pub const SKYHOOK_SILO_LINK_DURATION_SECS: u64 = 600;
/// Maximum ship-to-Skyhook range to initiate and maintain a Reagent Silo link (`15,000 m`).
pub const SKYHOOK_SILO_LINK_RANGE_M: f64 = 15_000.0;
/// Maximum ship speed while linked to an Orbital Skyhook Reagent Silo (`1,000 m/s`).
pub const SKYHOOK_SILO_LINK_MAX_SPEED_MS: f64 = 1_000.0;
/// Base shield HP assigned to an anchored Orbital Skyhook (`10%` threshold = `1,000.0` HP).
pub const SKYHOOK_BASE_SHIELD_HP: f64 = 10_000.0;

/// Maps SDE planet type to extracted Colony Reagent:
/// Ice (`12`) -> Superionic Ice (`81144`), Lava (`2015`) -> Magmatic Gas (`81143`).
pub fn planet_reagent_type(planet_type_id: u32) -> Option<u32> {
    match planet_type_id {
        12 => Some(TYPE_SUPERIONIC_ICE),
        2015 => Some(TYPE_MAGMATIC_GAS),
        _ => None,
    }
}

pub(crate) fn extract_extra_var(extra: Option<&PyRep>, key: &str) -> Option<i64> {
    match extra {
        Some(PyRep::Dict(map)) => map
            .iter()
            .find(|(k, _)| k.as_str().is_some_and(|s| s.eq_ignore_ascii_case(key)))
            .and_then(|(_, v)| v.as_i64()),
        Some(PyRep::KeyVal(kv)) => kv
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .and_then(|(_, v)| v.as_i64()),
        _ => None,
    }
}

/// Advances hourly planetary reagent harvesting into `insecure_qty` and linear 24h maturity from
/// `insecure_qty` into `secure_qty` using persisted `(batch_qty, rem)` remainder state so 1 Hz
/// ticks never integer-truncate maturity progress.
pub fn advance_skyhook_reagents_in_snap(
    snap: &mut WorldStateSnapshot,
    skyhook_id: i64,
    now_unix: u64,
) -> Option<(u32, u64, u64)> {
    let is_active = snap
        .sov_runtime
        .skyhook_active
        .get(&skyhook_id)
        .copied()
        .unwrap_or(true)
        && snap
            .structures
            .get(&skyhook_id)
            .is_none_or(|st| st.state != "Destroyed");

    let mut harvested_now = 0u64;
    if is_active && snap.sov_runtime.skyhook_reagent_bays.contains_key(&skyhook_id) {
        let bay_ts = snap
            .sov_runtime
            .skyhook_reagent_bays
            .get(&skyhook_id)
            .map(|b| b.3)
            .unwrap_or(now_unix);
        let last_h = snap
            .sov_runtime
            .skyhook_last_harvest_unix
            .entry(skyhook_id)
            .or_insert(if bay_ts > 0 { bay_ts } else { now_unix });
        if now_unix >= last_h.saturating_add(3_600) {
            let hours = (now_unix - *last_h) / 3_600;
            *last_h = last_h.saturating_add(hours * 3_600);
            harvested_now = hours.saturating_mul(SKYHOOK_HOURLY_REAGENT_YIELD);
        }
    }

    let (reagent_type, mut secure_qty, mut insecure_qty, mut last_ts) =
        snap.sov_runtime.skyhook_reagent_bays.get(&skyhook_id).copied()?;
    if last_ts == 0 {
        last_ts = now_unix;
    }
    let elapsed = now_unix.saturating_sub(last_ts);
    let (mut batch_qty, mut rem) = snap
        .sov_runtime
        .skyhook_maturity_rem
        .get(&skyhook_id)
        .copied()
        .unwrap_or((0, 0));

    if elapsed > 0 && insecure_qty > 0 {
        if batch_qty == 0 {
            batch_qty = insecure_qty;
        }
        let cycle = u128::from(SKYHOOK_MATURITY_CYCLE_SECS);
        let numer = u128::from(batch_qty)
            .saturating_mul(u128::from(elapsed))
            .saturating_add(rem);
        let matured = (numer / cycle).min(u128::from(insecure_qty)) as u64;
        insecure_qty -= matured;
        secure_qty = secure_qty.saturating_add(matured);
        if insecure_qty == 0 {
            batch_qty = 0;
            rem = 0;
        } else {
            rem = numer % cycle;
        }
    }
    if harvested_now > 0 {
        insecure_qty = insecure_qty.saturating_add(harvested_now);
        batch_qty = insecure_qty;
    }
    last_ts = now_unix;

    snap.sov_runtime
        .skyhook_reagent_bays
        .insert(skyhook_id, (reagent_type, secure_qty, insecure_qty, last_ts));
    snap.sov_runtime
        .skyhook_maturity_rem
        .insert(skyhook_id, (batch_qty, rem));
    snap.sov_runtime
        .skyhook_reagents
        .insert(skyhook_id, (reagent_type, secure_qty.saturating_add(insecure_qty)));
    Some((reagent_type, secure_qty, insecure_qty))
}

/// Returns `true` while `ship_id` has an active 10-minute Skyhook Reagent Silo link (`SkyhookLinkActive`).
pub fn is_ship_skyhook_linked(ctx: &ServerContext, ship_id: i64, now_unix: u64) -> bool {
    ctx.db.with_snapshot(|snap| {
        snap.sov_runtime
            .skyhook_silo_links
            .values()
            .any(|&(_, sid, completes_at, _)| sid == ship_id && now_unix < completes_at)
    })
}

/// Checks whether a ship hull (`ship_type_id`) is eligible to link with an Orbital Skyhook Reagent Silo
/// (Cruisers and larger combat hulls, Industrials, Transport Ships, and Upwell Haulers; rejects capsules,
/// shuttles, corvettes, frigates, and destroyers).
fn is_eligible_skyhook_link_hull(ctx: &ServerContext, ship_type_id: u32) -> bool {
    let Some(t) = ctx.sde.types.get(&ship_type_id) else {
        return false;
    };
    if t.category_id != 6 {
        return false;
    }
    !matches!(
        t.group_id,
        29 | 31 | 237 | 25 | 324 | 830 | 831 | 834 | 893 | 1283 | 1527 | 2001 | 420 | 541 | 1305 | 1534
    )
}

/// Live `structureDeployment.Anchor` handler for `TYPE_ORBITAL_SKYHOOK` (`81080`).
pub async fn handle_anchor_skyhook_rpc(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: "Cannot anchor an Orbital Skyhook while docked".into(),
        });
    }
    let corp_id = session.corporation_id.unwrap_or(0);
    let sys_id = session.solar_system_id.ok_or_else(|| BridgeRpcError {
        code: "NoSolarSystem".into(),
        message: "No active solar system".into(),
    })?;
    let ship_id = session.ship_id.ok_or_else(|| BridgeRpcError {
        code: "ShipNotInSpace".into(),
        message: "Active ship required".into(),
    })?;

    let has_role = ctx.db.with_snapshot(|s| {
        crate::services::station_structure::structure_lifecycle_rpc::has_structure_manager_role(
            s, session, char_id, corp_id,
        )
    });
    if !has_role {
        return Err(BridgeRpcError {
            code: "InsufficientCorpRoles".into(),
            message: "Anchoring an Orbital Skyhook requires Director or Station Manager role".into(),
        });
    }

    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(1.0);
    let flat_args: Vec<PyRep> = match args.first() {
        Some(PyRep::Tuple(items)) | Some(PyRep::List(items)) if items.len() >= 5 => items.clone(),
        _ => args.to_vec(),
    };
    let planet_id = if flat_args.len() >= 7 {
        extract_extra_var(flat_args.get(6), "planetID")
            .or_else(|| extract_extra_var(flat_args.get(6), "planet_id"))
            .unwrap_or_else(|| flat_args.get(1).and_then(|a| a.as_i64()).unwrap_or(0))
    } else {
        let a0 = flat_args.first().and_then(|a| a.as_i64()).unwrap_or(0);
        if a0 == TYPE_ORBITAL_SKYHOOK as i64
            || ctx.db.with_snapshot(|s| {
                s.items
                    .get(&a0)
                    .is_some_and(|i| i.type_id == TYPE_ORBITAL_SKYHOOK)
            })
        {
            flat_args.get(1).and_then(|a| a.as_i64()).unwrap_or(0)
        } else {
            a0
        }
    };

    let celestial = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.celestial_id == planet_id && c.group_id == 7)
        .ok_or_else(|| BridgeRpcError {
            code: "InvalidPlanet".into(),
            message: format!("Planet {planet_id} does not exist in solar system {sys_id}"),
        })?;
    let planet_type_id = celestial.type_id;

    let ship_pos = ctx
        .systems
        .get_or_spawn(sys_id)
        .get_ball_snapshot(ship_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b.position)
        .ok_or_else(|| BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: format!("Active ship {ship_id} is not present in space in system {sys_id}"),
        })?;
    let planet_anchor_pos = DVec3::new(
        celestial.position.x + 150_000.0,
        celestial.position.y,
        celestial.position.z,
    );
    if ship_pos.distance(planet_anchor_pos) > 500_000.0
        && ship_pos.distance(celestial.position) > 500_000.0
    {
        return Err(BridgeRpcError {
            code: "TooFarFromPlanet".into(),
            message: format!("Ship must be within 500 km of planet {planet_id} orbital anchor point"),
        });
    }

    let now = ctx.now_unix();
    let skyhook_id = ctx.db.try_with_mut_snapshot(|snap| {
        let sov_alliance = snap
            .system_states
            .get(&sys_id)
            .and_then(|s| s.sov_alliance_id)
            .unwrap_or(0);
        if sys_sec > 0.0 || sov_alliance == 0 {
            return Err(BridgeRpcError {
                code: "CannotAnchorSkyhookInEmpireSpace".into(),
                message: format!(
                    "Orbital Skyhooks can only be anchored in sovereign null-sec solar systems (system {sys_id}, security {sys_sec:.2})"
                ),
            });
        }
        let corp_alliance = snap
            .corporations
            .get(&corp_id)
            .and_then(|c| c.alliance_id)
            .unwrap_or(0);
        if corp_alliance != sov_alliance {
            return Err(BridgeRpcError {
                code: "NotSovereigntyHolder".into(),
                message: format!(
                    "Corporation {corp_id} (alliance {corp_alliance}) does not hold sovereignty in system {sys_id} (held by {sov_alliance})"
                ),
            });
        }
        if snap
            .sov_runtime
            .skyhook_planets
            .iter()
            .any(|(&sh_id, &pid)| pid == planet_id && snap.structures.contains_key(&sh_id))
        {
            return Err(BridgeRpcError {
                code: "SkyhookAlreadyExistsAtPlanet".into(),
                message: format!("Planet {planet_id} already has an anchored Orbital Skyhook"),
            });
        }
        let item_id = snap
            .items
            .values()
            .find(|i| {
                (i.owner_id == char_id || i.owner_id == corp_id)
                    && i.location_id == ship_id
                    && i.type_id == TYPE_ORBITAL_SKYHOOK
                    && i.quantity >= 1
            })
            .map(|i| i.item_id)
            .ok_or_else(|| BridgeRpcError {
                code: "MissingSkyhookItem".into(),
                message: format!(
                    "Anchoring requires Orbital Skyhook item (type {TYPE_ORBITAL_SKYHOOK}) in ship cargo"
                ),
            })?;
        if let Some(it) = snap.items.get_mut(&item_id) {
            it.quantity -= 1;
            if it.quantity <= 0 {
                snap.items.remove(&item_id);
            }
        }
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        let pos_arr = [planet_anchor_pos.x, planet_anchor_pos.y, planet_anchor_pos.z];
        snap.structures.insert(
            id,
            UpwellStructureRecord {
                structure_id: id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: format!("Orbital Skyhook (Planet {planet_id})"),
                position: pos_arr,
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 200,
                shield_hp: SKYHOOK_BASE_SHIELD_HP,
                armor_hp: SKYHOOK_BASE_SHIELD_HP,
                hull_hp: SKYHOOK_BASE_SHIELD_HP,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.sov_runtime.skyhook_active.insert(id, true);
        snap.sov_runtime.skyhook_planets.insert(id, planet_id);
        snap.sov_runtime.skyhook_last_harvest_unix.insert(id, now);
        if let Some(reagent_type) = planet_reagent_type(planet_type_id) {
            snap.sov_runtime.skyhook_reagents.insert(id, (reagent_type, 0));
            snap.sov_runtime
                .skyhook_reagent_bays
                .insert(id, (reagent_type, 0, 0, now));
            snap.sov_runtime.skyhook_maturity_rem.insert(id, (0, 0));
        }
        let chosen_hour_offset = snap
            .corp_governance
            .get(&corp_id)
            .map(|g| u64::from(g.reinforce_hour) * 3_600)
            .unwrap_or(22 * 3_600);
        snap.sov_runtime
            .skyhook_theft_windows
            .insert(id, (now + chosen_hour_offset, now + chosen_hour_offset + 7_200));
        Ok(id)
    })?;

    let _ = ctx
        .systems
        .get_or_spawn(sys_id)
        .spawn_ball(Ball {
            id: skyhook_id,
            type_id: TYPE_ORBITAL_SKYHOOK,
            position: planet_anchor_pos,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 1.0e8,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await;
    crate::services::sov_services::emit_sov_structures_updated(
        ctx,
        sys_id,
        vec![PyRep::Integer(skyhook_id)],
    );
    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnSkyhookFirstActivation",
        "solarsystemid",
        vec![PyRep::Integer(skyhook_id), PyRep::Integer(planet_id)],
    );
    Ok(PyRep::keyval([
        ("structureID", PyRep::Integer(skyhook_id)),
        ("typeID", PyRep::Integer(TYPE_ORBITAL_SKYHOOK as i64)),
    ]))
}

/// Applies live weapon damage (`dogmaIM.Activate`) to an Orbital Skyhook's shield/silo,
/// emitting `OnSkyhookSiloDamageUpdated` and exposing it for ship-link theft once shield drops `<= 10%`
/// inside an open theft vulnerability window.
pub fn apply_skyhook_weapon_damage(
    ctx: &ServerContext,
    skyhook_id: i64,
    damage: f64,
) -> Result<PyRep, BridgeRpcError> {
    let now = ctx.now_unix();
    let (sys_id, shield_hp, shield_pct, hackable_now, state_str) =
        ctx.db.try_with_mut_snapshot(|snap| {
            let window = snap.sov_runtime.skyhook_theft_windows.get(&skyhook_id).copied();
            let in_window = window.is_some_and(|(s, e)| s <= now && now < e);
            let st = snap.structures.get_mut(&skyhook_id).ok_or_else(|| BridgeRpcError {
                code: "StructureNotFound".into(),
                message: format!("Orbital Skyhook {skyhook_id} not found"),
            })?;
            if st.state == "Destroyed" {
                return Err(BridgeRpcError {
                    code: "StructureDestroyed".into(),
                    message: format!("Orbital Skyhook {skyhook_id} is destroyed"),
                });
            }
            if st.state == "ArmorReinforced" && now < st.reinforced_until_unix {
                return Err(BridgeRpcError {
                    code: "StructureInvulnerable".into(),
                    message: format!("Orbital Skyhook {skyhook_id} is reinforced"),
                });
            }
            st.last_damage_unix = now;
            if st.shield_hp > 0.0 {
                st.shield_hp = (st.shield_hp - damage.max(0.0)).max(0.0);
                // If inside the reagent theft window, keep the Skyhook in ShieldVulnerable at >= 1 HP
                // until a separate hull/armor siege occurs, or transition to ArmorReinforced if 0 HP outside window.
                if st.shield_hp <= 0.0 && !in_window {
                    st.state = "ArmorReinforced".into();
                    st.reinforced_until_unix = now + 86_400;
                }
            }
            let sys_id = st.solar_system_id;
            let s_hp = st.shield_hp;
            let s_pct = (s_hp / SKYHOOK_BASE_SHIELD_HP).clamp(0.0, 1.0);
            let st_s = st.state.clone();
            let hackable = in_window && s_pct <= 0.10;
            if hackable {
                snap.sov_runtime.skyhook_hackable.insert(skyhook_id, true);
            }
            Ok((sys_id, s_hp, s_pct, hackable, st_s))
        })?;

    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnSkyhookSiloDamageUpdated",
        "solarsystemid",
        vec![PyRep::Integer(skyhook_id), PyRep::Real(shield_pct)],
    );
    if hackable_now {
        crate::services::sov_services::emit_skyhook_can_be_hacked(ctx, sys_id, skyhook_id);
    }
    Ok(PyRep::keyval([
        ("structureID", PyRep::Integer(skyhook_id)),
        ("state", PyRep::String(state_str)),
        ("shieldHP", PyRep::Real(shield_hp)),
        ("shieldPct", PyRep::Real(shield_pct)),
        ("hackable", PyRep::Bool(hackable_now)),
    ]))
}

/// Handles V24 `beyonce.CallComponentFromClient` calls targeting an Orbital Skyhook (`linkWithShip` & `orbitalSkyhook`).
pub async fn handle_skyhook_component_call(
    ctx: &ServerContext,
    session: &SessionState,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    skyhook_id: i64,
    comp_name: &str,
    comp_method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, mut ship_ball) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .ok()
        .flatten()
        .ok_or_else(|| BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: format!("Ship {ship_id} is not present in space in system {sys_id}"),
        })?;
    let cs = sys_handle
        .get_combat_state(ship_id)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    if cs.is_cloaked {
        return Err(BridgeRpcError {
            code: "CannotLinkWhileCloaked".into(),
            message: "Cannot link with Orbital Skyhook while cloaked".into(),
        });
    }

    let skyhook_st = ctx
        .db
        .with_snapshot(|snap| snap.structures.get(&skyhook_id).cloned())
        .filter(|st| st.type_id == TYPE_ORBITAL_SKYHOOK && st.state != "Destroyed")
        .ok_or_else(|| BridgeRpcError {
            code: "SkyhookNotFound".into(),
            message: format!("Orbital Skyhook {skyhook_id} not found"),
        })?;
    if skyhook_st.solar_system_id != sys_id {
        return Err(BridgeRpcError {
            code: "TargetNotInSameSystem".into(),
            message: format!(
                "Orbital Skyhook {skyhook_id} is in system {}, caller is in {sys_id}",
                skyhook_st.solar_system_id
            ),
        });
    }
    let skyhook_pos = if let Some((_, b)) = sys_handle.get_ball_snapshot(skyhook_id).await.ok().flatten() {
        b.position
    } else {
        DVec3::from_array(skyhook_st.position)
    };
    let dist = (ship_ball.position.distance(skyhook_pos) - ship_ball.radius - 500.0).max(0.0);
    if dist > SKYHOOK_SILO_LINK_RANGE_M {
        return Err(BridgeRpcError {
            code: "OutOfSkyhookLinkRange".into(),
            message: format!(
                "Ship is {dist:.1} m from Orbital Skyhook {skyhook_id} (must be <= {SKYHOOK_SILO_LINK_RANGE_M:.0} m)"
            ),
        });
    }

    let now = ctx.now_unix();
    match (comp_name, comp_method) {
        ("linkWithShip", "InitiateLink" | "InitiateLinkQuick") => {
            if !is_eligible_skyhook_link_hull(ctx, ship_ball.type_id) {
                return Err(BridgeRpcError {
                    code: "IneligibleShipForSkyhookLink".into(),
                    message: format!(
                        "Ship hull type {} cannot link to a Skyhook Reagent Silo (requires Cruiser+ or Upwell Hauler)",
                        ship_ball.type_id
                    ),
                });
            }
            let max_shield = SKYHOOK_BASE_SHIELD_HP;
            if skyhook_st.shield_hp > max_shield * 0.10 + 1e-6 {
                return Err(BridgeRpcError {
                    code: "SkyhookSiloShieldTooHigh".into(),
                    message: format!(
                        "Orbital Skyhook {skyhook_id} shield HP ({:.1}/{max_shield:.1}) must be damaged to <= 10% before Reagent Silo linking",
                        skyhook_st.shield_hp
                    ),
                });
            }
            let completes_at = now + SKYHOOK_SILO_LINK_DURATION_SECS;
            let orig_max_vel = ship_ball.max_velocity;
            ctx.db.try_with_mut_snapshot(|snap| {
                let _ = advance_skyhook_reagents_in_snap(snap, skyhook_id, now);
                let window_open = snap
                    .sov_runtime
                    .skyhook_theft_windows
                    .get(&skyhook_id)
                    .copied()
                    .is_some_and(|(start, end)| start <= now && now <= end)
                    || snap
                        .sov_runtime
                        .skyhook_hackable
                        .get(&skyhook_id)
                        .copied()
                        .unwrap_or(false);
                if !window_open {
                    return Err(BridgeRpcError {
                        code: "SkyhookNotVulnerableToTheft".into(),
                        message: format!(
                            "Skyhook {skyhook_id} is outside its reagent theft vulnerability window"
                        ),
                    });
                }
                if let Some(&(_, existing_ship, until, _)) =
                    snap.sov_runtime.skyhook_silo_links.get(&skyhook_id)
                {
                    if now < until {
                        return Err(BridgeRpcError {
                            code: "SkyhookAlreadyLinked".into(),
                            message: format!(
                                "Skyhook {skyhook_id} is already linked by ship {existing_ship}"
                            ),
                        });
                    }
                }
                let insec = snap
                    .sov_runtime
                    .skyhook_reagent_bays
                    .get(&skyhook_id)
                    .map(|b| b.2)
                    .unwrap_or(0);
                if insec == 0 {
                    return Err(BridgeRpcError {
                        code: "InsecureReagentBayEmpty".into(),
                        message: format!(
                            "Skyhook {skyhook_id} insecure bay has no maturing reagents to steal"
                        ),
                    });
                }
                snap.sov_runtime.skyhook_silo_links.insert(
                    skyhook_id,
                    (char_id, ship_id, completes_at, orig_max_vel),
                );
                Ok(())
            })?;

            if ship_ball.max_velocity > SKYHOOK_SILO_LINK_MAX_SPEED_MS {
                ship_ball.max_velocity = SKYHOOK_SILO_LINK_MAX_SPEED_MS;
                let _ = sys_handle.spawn_ball(ship_ball).await;
            }
            crate::services::sov_services::emit_skyhook_can_be_hacked(ctx, sys_id, skyhook_id);
            Ok(PyRep::keyval([
                ("skyhookID", PyRep::Integer(skyhook_id)),
                ("shipID", PyRep::Integer(ship_id)),
                ("linked", PyRep::Bool(true)),
                (
                    "linkDurationSeconds",
                    PyRep::Integer(SKYHOOK_SILO_LINK_DURATION_SECS as i64),
                ),
                ("completesAtUnix", PyRep::Integer(completes_at as i64)),
                ("maxVelocityCap", PyRep::Real(SKYHOOK_SILO_LINK_MAX_SPEED_MS)),
            ]))
        }
        ("orbitalSkyhook", "is_character_authorised") => {
            let corp_id = session.corporation_id.unwrap_or(0);
            let authorised = corp_id == skyhook_st.owner_corp_id
                && ctx.db.with_snapshot(|s| {
                    crate::services::station_structure::structure_lifecycle_rpc::has_structure_manager_role(
                        s, session, char_id, corp_id,
                    )
                });
            Ok(PyRep::Bool(authorised))
        }
        ("orbitalSkyhook", "ExtractReagentsToShip") => {
            let corp_id = session.corporation_id.unwrap_or(0);
            let authorised = corp_id == skyhook_st.owner_corp_id
                && ctx.db.with_snapshot(|s| {
                    crate::services::station_structure::structure_lifecycle_rpc::has_structure_manager_role(
                        s, session, char_id, corp_id,
                    )
                });
            if !authorised {
                return Err(BridgeRpcError {
                    code: "NotAuthorisedForSkyhookExtraction".into(),
                    message: "Only owning corporation Directors or Station Managers may extract secured Skyhook reagents".into(),
                });
            }
            let req_qty_opt = args.get(4).and_then(|a| a.as_i64()).filter(|&q| q > 0).map(|q| q as u64);
            let (reagent_tid, extracted, rem_sec) = ctx.db.try_with_mut_snapshot(|snap| {
                let _ = advance_skyhook_reagents_in_snap(snap, skyhook_id, now);
                let entry = snap
                    .sov_runtime
                    .skyhook_reagent_bays
                    .get_mut(&skyhook_id)
                    .ok_or_else(|| BridgeRpcError {
                        code: "SkyhookReagentBayNotFound".into(),
                        message: format!("Skyhook {skyhook_id} has no reagent bay"),
                    })?;
                let take = req_qty_opt.unwrap_or(entry.1).min(entry.1);
                if take == 0 {
                    return Err(BridgeRpcError {
                        code: "NoSecuredReagentsAvailable".into(),
                        message: format!("Skyhook {skyhook_id} has 0 secured reagents available"),
                    });
                }
                entry.1 -= take;
                let r_tid = entry.0;
                let rem_s = entry.1;
                let insec = entry.2;
                snap.sov_runtime
                    .skyhook_reagents
                    .insert(skyhook_id, (r_tid, rem_s + insec));
                snap.next_entity_id += 1;
                let item_id = snap.next_entity_id;
                snap.items.insert(
                    item_id,
                    InventoryItemRecord {
                        item_id,
                        type_id: r_tid,
                        owner_id: char_id,
                        location_id: ship_id,
                        flag: InventoryFlag::Cargo,
                        quantity: take as i32,
                        ..Default::default()
                    },
                );
                Ok((r_tid, take, rem_s))
            })?;
            Ok(PyRep::keyval([
                ("skyhookID", PyRep::Integer(skyhook_id)),
                ("reagentTypeID", PyRep::Integer(reagent_tid as i64)),
                ("extractedQuantity", PyRep::Integer(extracted as i64)),
                ("remainingSecureQuantity", PyRep::Integer(rem_sec as i64)),
            ]))
        }
        _ => Err(BridgeRpcError {
            code: "UnsupportedComponentCall".into(),
            message: format!("Unsupported Skyhook component call {comp_name}.{comp_method}"),
        }),
    }
}

/// Advances all Orbital Skyhook reagent bays and resolves active 10-minute Reagent Silo links.
pub(crate) async fn tick_skyhooks(ctx: &ServerContext, now_unix: u64) {
    let (skyhook_ids, links): (Vec<i64>, Vec<(i64, (i64, i64, u64, f64))>) =
        ctx.db.with_snapshot(|snap| {
            let mut ids: Vec<i64> = snap.sov_runtime.skyhook_reagent_bays.keys().copied().collect();
            for &id in snap.sov_runtime.skyhook_active.keys() {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
            ids.sort_unstable();
            let l: Vec<_> = snap
                .sov_runtime
                .skyhook_silo_links
                .iter()
                .map(|(&k, &v)| (k, v))
                .collect();
            (ids, l)
        });

    let newly_hackable: Vec<(u32, i64)> = ctx.db.with_mut_snapshot(|snap| {
        let mut opened = Vec::new();
        for id in skyhook_ids {
            let _ = advance_skyhook_reagents_in_snap(snap, id, now_unix);
            let window = snap
                .sov_runtime
                .skyhook_theft_windows
                .entry(id)
                .or_insert_with(|| (now_unix, now_unix + 7_200));
            if now_unix >= window.1 {
                let interval = 302_400; // ~3.5 days offset
                let mut start = window.0 + interval;
                if start + 7_200 <= now_unix {
                    start += ((now_unix - (start + 7_200)) / interval + 1) * interval;
                }
                *window = (start, start + 7_200);
            }
            let (start, end) = *window;
            let open_now = start <= now_unix && now_unix < end;
            let was_open = snap.sov_runtime.skyhook_hackable.get(&id).copied().unwrap_or(false);
            if open_now != was_open {
                snap.sov_runtime.skyhook_hackable.insert(id, open_now);
                if open_now {
                    if let Some(st) = snap.structures.get(&id) {
                        opened.push((st.solar_system_id, id));
                    }
                }
            }
        }
        opened
    });
    for (sys_id, sh_id) in newly_hackable {
        crate::services::sov_services::emit_skyhook_can_be_hacked(ctx, sys_id, sh_id);
    }

    for (skyhook_id, (hacker_cid, ship_id, completes_at, orig_vel)) in links {
        let Some((sys_id, pos_arr)) = ctx.db.with_snapshot(|snap| {
            snap.structures
                .get(&skyhook_id)
                .filter(|s| s.state != "Destroyed")
                .map(|s| (s.solar_system_id, s.position))
        }) else {
            ctx.db.with_mut_snapshot(|snap| {
                snap.sov_runtime.skyhook_silo_links.remove(&skyhook_id);
            });
            continue;
        };
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let skyhook_pos = sys_handle
            .get_ball_snapshot(skyhook_id)
            .await
            .ok()
            .flatten()
            .map(|(_, b)| b.position)
            .unwrap_or_else(|| DVec3::from_array(pos_arr));
        let ship_opt = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten();
        let cs = sys_handle
            .get_combat_state(ship_id)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();

        let broken = match &ship_opt {
            None => true,
            Some((_, sb)) => {
                cs.is_cloaked
                    || (cs.max_structure_hp > 0.0 && cs.structure_hp <= 0.0)
                    || (sb.position.distance(skyhook_pos) - sb.radius - 500.0).max(0.0)
                        > SKYHOOK_SILO_LINK_RANGE_M
            }
        };
        if broken {
            ctx.db.with_mut_snapshot(|snap| {
                snap.sov_runtime.skyhook_silo_links.remove(&skyhook_id);
            });
            if let Some((_, mut sb)) = ship_opt {
                if orig_vel > 0.0 {
                    sb.max_velocity = orig_vel;
                    let _ = sys_handle.spawn_ball(sb).await;
                }
            }
            continue;
        }

        if now_unix < completes_at {
            if let Some((_, mut sb)) = ship_opt {
                if sb.max_velocity > SKYHOOK_SILO_LINK_MAX_SPEED_MS {
                    sb.max_velocity = SKYHOOK_SILO_LINK_MAX_SPEED_MS;
                    let _ = sys_handle.spawn_ball(sb).await;
                }
            }
            continue;
        }

        // 10-minute link completed: drop 100% of insecure surplus bay into an in-space Cargo Container ball,
        // leaving secure bay untouched, and restore ship max velocity.
        let drop_res = ctx.db.with_mut_snapshot(|snap| {
            snap.sov_runtime.skyhook_silo_links.remove(&skyhook_id);
            let _ = advance_skyhook_reagents_in_snap(snap, skyhook_id, now_unix);
            let entry = snap.sov_runtime.skyhook_reagent_bays.get_mut(&skyhook_id)?;
            if entry.2 == 0 {
                return None;
            }
            let r_type = entry.0;
            let stolen = entry.2;
            entry.2 = 0;
            entry.3 = now_unix;
            let final_sec = entry.1;
            snap.sov_runtime.skyhook_maturity_rem.insert(skyhook_id, (0, 0));
            snap.sov_runtime
                .skyhook_reagents
                .insert(skyhook_id, (r_type, final_sec));

            snap.next_entity_id += 1;
            let container_id = snap.next_entity_id;
            let c_pos = [pos_arr[0] + 500.0, pos_arr[1], pos_arr[2]];
            snap.items.insert(
                container_id,
                InventoryItemRecord {
                    item_id: container_id,
                    type_id: 23,
                    owner_id: hacker_cid,
                    location_id: i64::from(sys_id),
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.item_positions.insert(container_id, c_pos);
            snap.next_entity_id += 1;
            let loot_id = snap.next_entity_id;
            snap.items.insert(
                loot_id,
                InventoryItemRecord {
                    item_id: loot_id,
                    type_id: r_type,
                    owner_id: hacker_cid,
                    location_id: container_id,
                    flag: InventoryFlag::Cargo,
                    quantity: stolen as i32,
                    ..Default::default()
                },
            );
            Some((container_id, c_pos, r_type, final_sec))
        });

        if let Some((_, mut sb)) = ship_opt {
            if orig_vel > 0.0 {
                sb.max_velocity = orig_vel;
                let _ = sys_handle.spawn_ball(sb).await;
            }
        }
        if let Some((container_id, c_pos, r_type, final_sec)) = drop_res {
            let _ = sys_handle
                .spawn_ball(Ball {
                    id: container_id,
                    type_id: 23,
                    position: DVec3::from_array(c_pos),
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 0.0,
                    mass: 1_000.0,
                    radius: 10.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Rigid,
                })
                .await;
            let mut n = Vec::new();
            let mut id_b = Vec::new();
            encode_varint_field(1, skyhook_id as u64, &mut id_b);
            encode_bytes_field(1, &id_b, &mut n);
            let mut sim = Vec::new();
            encode_varint_field(1, final_sec, &mut sim);
            encode_varint_field(2, 0, &mut sim);
            let mut r = Vec::new();
            let mut rt_b = Vec::new();
            encode_varint_field(1, u64::from(r_type), &mut rt_b);
            encode_bytes_field(1, &rt_b, &mut r);
            encode_bytes_field(2, &sim, &mut r);
            encode_bytes_field(2, &r, &mut n);
            let _ = ctx.quasar_gateway.publish_notice(
                NoticeScope::SolarSystem(sys_id),
                GoogleAny {
                    type_url: format!(
                        "{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.skyhook.api.SkyhookReagentsSimulatedNotice"
                    ),
                    value: n,
                },
                now_unix,
            );
        }
    }
}

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
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;

/// Skyhook reagent maturity cycle (24 hours = 86,400 seconds).
pub const SKYHOOK_MATURITY_CYCLE_SECS: u64 = 24 * 3_600;
/// Fallback hourly planetary reagent yield constant (non-reagent or unlinked planets harvest `0`).
pub const SKYHOOK_HOURLY_REAGENT_YIELD: u64 = 50;
/// Secured and unsecured bay capacities on an Orbital Skyhook (`orbitalSkyhook/const.pyj` / Quasar `Definition`).
pub const SKYHOOK_REAGENT_BAY_CAPACITY: u64 = 25_000;
/// Secured harvest split percentage (`0..=100`): `50%` matures immediately to Secure, `50%` enters Surplus.
pub const REAGENT_SECURED_PERCENTAGE: u64 = 50;
/// Orbital Skyhook Reagent Silo ship-link duration (10 minutes = 600 seconds, `Orbital_Skyhook.md`).
pub const SKYHOOK_SILO_LINK_DURATION_SECS: u64 = 600;
/// Maximum ship-to-Skyhook range to initiate and maintain a Reagent Silo link (`15,000 m`).
pub const SKYHOOK_SILO_LINK_RANGE_M: f64 = 15_000.0;
/// Maximum ship speed while linked to an Orbital Skyhook Reagent Silo (`1,000 m/s`).
pub const SKYHOOK_SILO_LINK_MAX_SPEED_MS: f64 = 1_000.0;
/// Base shield HP assigned to an anchored Orbital Skyhook (`10%` threshold = `1,000.0` HP).
pub const SKYHOOK_BASE_SHIELD_HP: f64 = 10_000.0;
/// Base armor HP assigned to an anchored Orbital Skyhook.
pub const SKYHOOK_BASE_ARMOR_HP: f64 = SKYHOOK_BASE_SHIELD_HP;
/// Base hull HP assigned to an anchored Orbital Skyhook.
pub const SKYHOOK_BASE_HULL_HP: f64 = SKYHOOK_BASE_SHIELD_HP;

/// Maps SDE planet type to extracted Colony Reagent (`orbitalSkyhook/const.pyj`):
/// ONLY Ice (`12`) -> Superionic Ice (`81144`) and Lava (`2015`) -> Magmatic Gas (`81143`).
/// Oceanic (`2014`), Temperate (`11`), and Barren (`2016`) planets yield Workforce only.
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

/// Computes the next 2-hour Skyhook reagent theft vulnerability window `(start, start + 7_200)`
/// every 3–4 days (`72h..=96h`) centered around the defender corporation's configured `reinforce_hour`
/// (`Orbital_Skyhook.md`).
pub fn next_skyhook_theft_window(
    skyhook_id: i64,
    prev_start: u64,
    reinforce_hour: Option<u8>,
    now_unix: u64,
) -> (u64, u64) {
    let mut start = prev_start;
    let mut cycle = 0u64;
    while start.saturating_add(7_200) <= now_unix || cycle == 0 {
        let hash = (skyhook_id.unsigned_abs())
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(cycle.saturating_add(1).wrapping_mul(1_442_695_040_888_963_407));
        let days = if (hash >> 8) % 2 == 0 { 3 } else { 4 };
        let mut next = start.saturating_add(days * 86_400);
        if let Some(rh) = reinforce_hour {
            // Align to UTC day boundary + defender reinforce_hour ± 1h bell jitter, clamped inside 3–4d cadence.
            let day_base = (next / 86_400) * 86_400;
            let jitter_secs = match hash % 3 {
                0 => -3_600i64,
                1 => 0i64,
                _ => 3_600i64,
            };
            let target = (day_base as i64)
                .saturating_add(i64::from(rh % 24) * 3_600)
                .saturating_add(jitter_secs)
                .max(0) as u64;
            if target >= start.saturating_add(259_200) && target <= start.saturating_add(345_600) {
                next = target;
            }
        }
        start = next;
        cycle = cycle.saturating_add(1);
        if cycle > 10_000 {
            let interval = 302_400u64;
            start = prev_start + ((now_unix.saturating_sub(prev_start) / interval) + 1) * interval;
            break;
        }
    }
    (start, start.saturating_add(7_200))
}

fn has_skyhook_runtime_state(snap: &WorldStateSnapshot, skyhook_id: i64) -> bool {
    let rt = &snap.sov_runtime;
    rt.skyhook_planets.contains_key(&skyhook_id)
        || rt.skyhook_active.contains_key(&skyhook_id)
        || rt.skyhook_reagent_bays.contains_key(&skyhook_id)
        || rt.skyhook_reagents.contains_key(&skyhook_id)
        || rt.skyhook_maturity_rem.contains_key(&skyhook_id)
        || rt.skyhook_last_harvest_unix.contains_key(&skyhook_id)
        || rt.skyhook_theft_windows.contains_key(&skyhook_id)
        || rt.skyhook_hackable.contains_key(&skyhook_id)
        || rt.skyhook_silo_links.contains_key(&skyhook_id)
}

/// Clears all per-skyhook planet/reagent/theft/link runtime state when a Skyhook is destroyed.
pub(crate) fn cleanup_destroyed_skyhook_in_snap(snap: &mut WorldStateSnapshot, skyhook_id: i64) {
    snap.sov_runtime.skyhook_planets.remove(&skyhook_id);
    snap.sov_runtime.skyhook_active.remove(&skyhook_id);
    snap.sov_runtime.skyhook_reagent_bays.remove(&skyhook_id);
    snap.sov_runtime.skyhook_reagents.remove(&skyhook_id);
    snap.sov_runtime.skyhook_maturity_rem.remove(&skyhook_id);
    snap.sov_runtime.skyhook_last_harvest_unix.remove(&skyhook_id);
    snap.sov_runtime.skyhook_theft_windows.remove(&skyhook_id);
    snap.sov_runtime.skyhook_hackable.remove(&skyhook_id);
    snap.sov_runtime.skyhook_silo_links.remove(&skyhook_id);
}

/// Rolls an expired 2-hour Skyhook theft window forward to its next 3–4 day window and clears stale hackable state.
pub fn advance_skyhook_theft_window_in_snap(
    snap: &mut WorldStateSnapshot,
    skyhook_id: i64,
    now_unix: u64,
) -> Option<(u64, u64)> {
    let reinforce_hour = snap
        .structures
        .get(&skyhook_id)
        .and_then(|st| snap.corp_governance.get(&st.owner_corp_id))
        .map(|g| g.reinforce_hour);
    let sov_rt = &mut *snap.sov_runtime;
    let window = sov_rt.skyhook_theft_windows.get_mut(&skyhook_id)?;
    if now_unix >= window.1 {
        *window = next_skyhook_theft_window(skyhook_id, window.0, reinforce_hour, now_unix);
        if now_unix < window.0 {
            sov_rt.skyhook_hackable.insert(skyhook_id, false);
        }
    }
    Some(*window)
}

/// Advances hourly planetary reagent harvesting into `insecure_qty` and linear 24h maturity from
/// `insecure_qty` into `secure_qty` using persisted `(batch_qty, rem)` remainder state so 1 Hz
/// ticks never integer-truncate maturity progress, and advances expired 3–4 day theft windows.
pub fn advance_skyhook_reagents_in_snap(
    snap: &mut WorldStateSnapshot,
    sde: &eve_sde::SdeRegistry,
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
            .is_none_or(|st| !st.state.eq_ignore_ascii_case("destroyed"));

    let mut harvested_now = 0u64;
    if is_active {
        if let Some(&(bay_reagent_type, _, _, bay_ts)) =
            snap.sov_runtime.skyhook_reagent_bays.get(&skyhook_id)
        {
            let planet_id = snap.sov_runtime.skyhook_planets.get(&skyhook_id).copied();
            let sys_id = snap.structures.get(&skyhook_id).map(|s| s.solar_system_id);
            let hourly_yield = crate::services::quasar_sov_skyhook::planet_hourly_reagent_yield(
                sde,
                planet_id,
                sys_id,
                bay_reagent_type,
            );
            let last_h = snap
                .sov_runtime
                .skyhook_last_harvest_unix
                .entry(skyhook_id)
                .or_insert(if bay_ts > 0 { bay_ts } else { now_unix });
            if now_unix >= last_h.saturating_add(3_600) {
                let hours = (now_unix - *last_h) / 3_600;
                *last_h = last_h.saturating_add(hours * 3_600);
                harvested_now = hours.saturating_mul(hourly_yield);
            }
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
        let cap_matured = (numer / cycle)
            .min(u128::from(insecure_qty))
            .min(u128::from(SKYHOOK_REAGENT_BAY_CAPACITY.saturating_sub(secure_qty))) as u64;
        insecure_qty -= cap_matured;
        secure_qty = secure_qty.saturating_add(cap_matured);
        if insecure_qty == 0 {
            batch_qty = 0;
            rem = 0;
        } else {
            rem = numer % cycle;
        }
    }
    if harvested_now > 0 {
        let to_secure = (harvested_now.saturating_mul(REAGENT_SECURED_PERCENTAGE)) / 100;
        let to_surplus = harvested_now - to_secure;
        let sec_room = SKYHOOK_REAGENT_BAY_CAPACITY.saturating_sub(secure_qty);
        let sec_add = to_secure.min(sec_room);
        let sec_spill = to_secure - sec_add;
        secure_qty = secure_qty.saturating_add(sec_add);

        let desired_insec = to_surplus.saturating_add(sec_spill);
        let insec_room = SKYHOOK_REAGENT_BAY_CAPACITY.saturating_sub(insecure_qty);
        let insec_add = desired_insec.min(insec_room);
        let insec_spill = desired_insec - insec_add;
        insecure_qty = insecure_qty.saturating_add(insec_add);

        if insec_spill > 0 {
            let extra_sec_room = SKYHOOK_REAGENT_BAY_CAPACITY.saturating_sub(secure_qty);
            secure_qty = secure_qty.saturating_add(insec_spill.min(extra_sec_room));
        }
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
    let _ = advance_skyhook_theft_window_in_snap(snap, skyhook_id, now_unix);
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
            client_error: None,
        });
    }
    let corp_id = session.corporation_id.unwrap_or(0);
    let sys_id = session.solar_system_id.ok_or_else(|| BridgeRpcError {
        code: "NoSolarSystem".into(),
        message: "No active solar system".into(),
        client_error: None,
    })?;
    let ship_id = session.ship_id.ok_or_else(|| BridgeRpcError {
        code: "ShipNotInSpace".into(),
        message: "Active ship required".into(),
        client_error: None,
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
            client_error: None,
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
            client_error: None,
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
            client_error: None,
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
            client_error: None,
        });
    }

    let first_arg = flat_args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let now = ctx.now_unix();
    let skyhook_id = ctx.db.try_with_mut_snapshot(|snap| {
        let sov_alliance = snap
            .system_states
            .get(&sys_id)
            .and_then(|s| s.sov_alliance_id)
            .unwrap_or(0);
        if !crate::services::sov_gameplay::is_claimable_nullsec_system(&ctx.sde, sys_id)
            || sov_alliance == 0
        {
            return Err(BridgeRpcError {
                code: "CannotAnchorSkyhookInEmpireSpace".into(),
                message: format!(
                    "Orbital Skyhooks can only be anchored in sovereign null-sec solar systems (system {sys_id}, security {sys_sec:.2})"
                ),
                client_error: None,
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
                client_error: None,
            });
        }
        if snap
            .sov_runtime
            .skyhook_planets
            .iter()
            .any(|(&sh_id, &pid)| {
                pid == planet_id
                    && snap
                        .structures
                        .get(&sh_id)
                        .is_some_and(|s| !s.state.eq_ignore_ascii_case("destroyed"))
            })
        {
            return Err(BridgeRpcError {
                code: "SkyhookAlreadyExistsAtPlanet".into(),
                message: format!("Planet {planet_id} already has an anchored Orbital Skyhook"),
                client_error: None,
            });
        }
        let item_id = if first_arg > 0
            && first_arg != i64::from(TYPE_ORBITAL_SKYHOOK)
            && first_arg != planet_id
        {
            let it = snap
                .items
                .get(&first_arg)
                .filter(|i| i.type_id == TYPE_ORBITAL_SKYHOOK && i.quantity >= 1)
                .ok_or_else(|| BridgeRpcError {
                    code: "MissingSkyhookItem".into(),
                    message: format!("Orbital Skyhook item {first_arg} not found"),
                    client_error: None,
                })?;
            if it.owner_id != char_id && it.owner_id != corp_id {
                return Err(BridgeRpcError {
                    code: "ItemOwnershipViolation".into(),
                    message: format!("Character {char_id} does not own Orbital Skyhook item {first_arg}"),
                    client_error: None,
                });
            }
            if it.location_id != ship_id || it.flag != InventoryFlag::Cargo {
                return Err(BridgeRpcError {
                    code: "ItemNotInCargo".into(),
                    message: format!("Orbital Skyhook item {first_arg} must be in active ship cargo"),
                    client_error: None,
                });
            }
            if snap.is_item_or_ancestor_locked_or_impounded(first_arg) {
                return Err(BridgeRpcError {
                    code: "ItemLockedInContractOrJob".into(),
                    message: format!("Orbital Skyhook item {first_arg} is locked in an open contract or industry job"),
                    client_error: None,
                });
            }
            first_arg
        } else {
            snap.items
                .values()
                .filter(|i| {
                    (i.owner_id == char_id || i.owner_id == corp_id)
                        && i.location_id == ship_id
                        && i.flag == InventoryFlag::Cargo
                        && i.type_id == TYPE_ORBITAL_SKYHOOK
                        && i.quantity >= 1
                        && !snap.is_item_or_ancestor_locked_or_impounded(i.item_id)
                })
                .map(|i| i.item_id)
                .min()
                .ok_or_else(|| BridgeRpcError {
                    code: "MissingSkyhookItem".into(),
                    message: format!(
                        "Anchoring requires an unlocked Orbital Skyhook item (type {TYPE_ORBITAL_SKYHOOK}) in ship cargo"
                    ),
                    client_error: None,
                })?
        };
        let item_owner = snap.items.get(&item_id).map(|it| it.owner_id).unwrap_or(char_id);
        snap.consume_exact_item_quantity(
            item_id,
            1,
            eve_db::ConsumeExactOptions::for_owner_exact(Some(char_id), item_owner),
        )
        .map_err(|e| BridgeRpcError {
            code: "InventoryError".into(),
            message: e.to_string(),
            client_error: None,
        })?;
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
                armor_hp: SKYHOOK_BASE_ARMOR_HP,
                hull_hp: SKYHOOK_BASE_HULL_HP,
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
    crate::services::sov_services::emit_sov_structures_updated(ctx, sys_id, &[skyhook_id]);
    ctx.emit_typed(
        NotificationTarget::SolarSystem(sys_id),
        crate::router::notify_registry::OnSkyhookFirstActivation {
            skyhook_id,
            planet_id,
        },
    );
    Ok(PyRep::keyval([
        ("structureID", PyRep::Integer(skyhook_id)),
        ("typeID", PyRep::Integer(TYPE_ORBITAL_SKYHOOK as i64)),
    ]))
}

fn skyhook_layer_resonances(sde: &eve_sde::SdeRegistry, attr_ids: [u16; 4]) -> [f64; 4] {
    let attrs = sde
        .types
        .get(&TYPE_ORBITAL_SKYHOOK)
        .map(|t| &t.default_attributes);
    let get = |id: u16| -> f64 {
        attrs
            .and_then(|a| a.get(&id).copied())
            .unwrap_or(0.8)
            .clamp(0.0, 1.0)
    };
    [
        get(attr_ids[0]),
        get(attr_ids[1]),
        get(attr_ids[2]),
        get(attr_ids[3]),
    ]
}

fn effective_layer_damage(res: [f64; 4], em: f64, th: f64, ki: f64, ex: f64) -> f64 {
    em.max(0.0) * res[0]
        + th.max(0.0) * res[1]
        + ki.max(0.0) * res[2]
        + ex.max(0.0) * res[3]
}

/// Returns `[em, thermal, kinetic, explosive]` shield damage resonances (`0.0..=1.0`) for
/// `TYPE_ORBITAL_SKYHOOK` (`81080`) from SDE attributes `271, 274, 273, 272` (default `0.8`).
pub fn skyhook_shield_resonances(sde: &eve_sde::SdeRegistry) -> [f64; 4] {
    skyhook_layer_resonances(
        sde,
        [
            eve_sde::attr_ids::SHIELD_EM_DAMAGE_RESONANCE,
            eve_sde::attr_ids::SHIELD_THERMAL_DAMAGE_RESONANCE,
            eve_sde::attr_ids::SHIELD_KINETIC_DAMAGE_RESONANCE,
            eve_sde::attr_ids::SHIELD_EXPLOSIVE_DAMAGE_RESONANCE,
        ],
    )
}

/// Returns `[em, thermal, kinetic, explosive]` armor damage resonances (`0.0..=1.0`) for
/// `TYPE_ORBITAL_SKYHOOK` (`81080`) from SDE attributes `267, 270, 269, 268` (default `0.8`).
pub fn skyhook_armor_resonances(sde: &eve_sde::SdeRegistry) -> [f64; 4] {
    skyhook_layer_resonances(
        sde,
        [
            eve_sde::attr_ids::ARMOR_EM_DAMAGE_RESONANCE,
            eve_sde::attr_ids::ARMOR_THERMAL_DAMAGE_RESONANCE,
            eve_sde::attr_ids::ARMOR_KINETIC_DAMAGE_RESONANCE,
            eve_sde::attr_ids::ARMOR_EXPLOSIVE_DAMAGE_RESONANCE,
        ],
    )
}

/// Returns `[em, thermal, kinetic, explosive]` hull damage resonances (`0.0..=1.0`) for
/// `TYPE_ORBITAL_SKYHOOK` (`81080`) from SDE attributes `113, 110, 109, 111` (default `0.8`).
pub fn skyhook_hull_resonances(sde: &eve_sde::SdeRegistry) -> [f64; 4] {
    skyhook_layer_resonances(
        sde,
        [
            eve_sde::attr_ids::EM_DAMAGE_RESONANCE,
            eve_sde::attr_ids::THERMAL_DAMAGE_RESONANCE,
            eve_sde::attr_ids::KINETIC_DAMAGE_RESONANCE,
            eve_sde::attr_ids::EXPLOSIVE_DAMAGE_RESONANCE,
        ],
    )
}

/// Applies omni weapon damage (`damage` split evenly across `[EM, Thermal, Kinetic, Explosive]`)
/// against an Orbital Skyhook using SDE layer damage resonances.
pub fn apply_skyhook_weapon_damage(
    ctx: &ServerContext,
    skyhook_id: i64,
    damage: f64,
) -> Result<PyRep, BridgeRpcError> {
    if !damage.is_finite() {
        return Err(BridgeRpcError {
            code: "InvalidDamage".into(),
            message: "Weapon damage must be a finite number".into(),
            client_error: None,
        });
    }
    let q = damage.max(0.0) * 0.25;
    apply_skyhook_weapon_damage_typed(ctx, skyhook_id, q, q, q, q)
}

/// Applies typed live weapon damage (`[em, thermal, kinetic, explosive]`) to an Orbital Skyhook's
/// active defense layer (`ShieldVulnerable` -> `ArmorReinforced` -> `ArmorVulnerable` ->
/// `HullVulnerable` -> `Destroyed`) via SDE layer resonances.
pub fn apply_skyhook_weapon_damage_typed(
    ctx: &ServerContext,
    skyhook_id: i64,
    em: f64,
    thermal: f64,
    kinetic: f64,
    explosive: f64,
) -> Result<PyRep, BridgeRpcError> {
    apply_skyhook_weapon_damage_with_attacker(ctx, skyhook_id, em, thermal, kinetic, explosive, 0)
}

/// Applies typed live weapon damage (`[em, thermal, kinetic, explosive]`) with `attacker_char_id`
/// attribution across the Skyhook lifecycle (`ShieldVulnerable` -> `ArmorReinforced` ->
/// `ArmorVulnerable` -> `HullVulnerable` -> `Destroyed`), destroying the structure with wreck/killmail
/// generation and Sov Hub network reconciliation when hull reaches `0.0`.
pub(crate) fn apply_skyhook_weapon_damage_with_attacker(
    ctx: &ServerContext,
    skyhook_id: i64,
    em: f64,
    thermal: f64,
    kinetic: f64,
    explosive: f64,
    attacker_char_id: i64,
) -> Result<PyRep, BridgeRpcError> {
    if !em.is_finite() || !thermal.is_finite() || !kinetic.is_finite() || !explosive.is_finite() {
        return Err(BridgeRpcError {
            code: "InvalidDamage".into(),
            message: "Weapon damage components must be finite numbers".into(),
            client_error: None,
        });
    }
    let eff_shield = effective_layer_damage(
        skyhook_shield_resonances(&ctx.sde),
        em,
        thermal,
        kinetic,
        explosive,
    );
    let eff_armor = effective_layer_damage(
        skyhook_armor_resonances(&ctx.sde),
        em,
        thermal,
        kinetic,
        explosive,
    );
    let eff_hull = effective_layer_damage(
        skyhook_hull_resonances(&ctx.sde),
        em,
        thermal,
        kinetic,
        explosive,
    );
    let now = ctx.now_unix();
    let pending: crate::services::quasar_sov_hub::PendingNotices =
        std::sync::Arc::new(std::sync::RwLock::new(Vec::new()));
    let (
        sys_id,
        shield_hp,
        armor_hp,
        hull_hp,
        shield_pct,
        armor_pct,
        hull_pct,
        applied_dmg,
        hackable_now,
        newly_hackable,
        state_str,
        state_changed,
        destroyed_now,
        pending_sig,
    ) = ctx.db.try_with_mut_snapshot(|snap| {
        let window = snap.sov_runtime.skyhook_theft_windows.get(&skyhook_id).copied();
        let in_window = window.is_some_and(|(s, e)| s <= now && now < e);
        let (
            sys_id,
            prev_state,
            mut state_changed,
            destroyed_now,
            applied,
            s_hp,
            a_hp,
            h_hp,
            hackable,
            newly_hackable,
        ) = {
            let st = snap.structures.get_mut(&skyhook_id).ok_or_else(|| BridgeRpcError {
                code: "StructureNotFound".into(),
                message: format!("Orbital Skyhook {skyhook_id} not found"),
                client_error: None,
            })?;
            if st.state.eq_ignore_ascii_case("destroyed") {
                return Err(BridgeRpcError {
                    code: "StructureDestroyed".into(),
                    message: format!("Orbital Skyhook {skyhook_id} is destroyed"),
                    client_error: None,
                });
            }
            if st.state == "ArmorReinforced" && now < st.reinforced_until_unix {
                return Err(BridgeRpcError {
                    code: "StructureInvulnerable".into(),
                    message: format!("Orbital Skyhook {skyhook_id} is reinforced"),
                    client_error: None,
                });
            }
            let prev_state = st.state.clone();
            let mut state_changed = false;
            if st.state == "ArmorReinforced"
                && st.reinforced_until_unix > 0
                && st.shield_hp <= 0.0
            {
                st.state = "ArmorVulnerable".into();
                st.reinforced_until_unix = 0;
                state_changed = true;
            }
            st.last_damage_unix = now;
            let mut destroyed_now = false;
            let applied = match st.state.as_str() {
                "HullVulnerable" => {
                    let prev_hp = st.hull_hp;
                    st.hull_hp = (st.hull_hp - eff_hull).max(0.0);
                    destroyed_now = st.hull_hp <= 0.0;
                    (prev_hp - st.hull_hp).max(0.0)
                }
                "ArmorVulnerable" => {
                    let prev_hp = st.armor_hp;
                    st.armor_hp = (st.armor_hp - eff_armor).max(0.0);
                    if st.armor_hp <= 0.0 {
                        st.state = "HullVulnerable".into();
                        state_changed = true;
                    }
                    (prev_hp - st.armor_hp).max(0.0)
                }
                _ => {
                    let prev_hp = st.shield_hp;
                    if st.shield_hp > 0.0 {
                        st.shield_hp = (st.shield_hp - eff_shield).max(0.0);
                    }
                    // Evaluate ArmorReinforced outside `st.shield_hp > 0.0`: a Skyhook dropped to
                    // 0.0 shield during a theft window stays ShieldVulnerable while open, so any
                    // positive post-window hit must still trigger the 24h ArmorReinforced timer.
                    if st.shield_hp <= 0.0 && !in_window && eff_shield > 0.0 {
                        st.state = "ArmorReinforced".into();
                        st.reinforced_until_unix = now + 86_400;
                        state_changed = true;
                    }
                    (prev_hp - st.shield_hp).max(0.0)
                }
            };
            let s_hp = st.shield_hp;
            let a_hp = st.armor_hp;
            let h_hp = st.hull_hp;
            let sys_id = st.solar_system_id;
            let hackable = in_window
                && !destroyed_now
                && !st.state.eq_ignore_ascii_case("destroyed")
                && (s_hp / SKYHOOK_BASE_SHIELD_HP).clamp(0.0, 1.0) <= 0.10 + 1e-9;
            let was_hackable = snap
                .sov_runtime
                .skyhook_hackable
                .insert(skyhook_id, hackable)
                .unwrap_or(false);
            if st.state != prev_state {
                state_changed = true;
            }
            (
                sys_id,
                prev_state,
                state_changed,
                destroyed_now,
                applied,
                s_hp,
                a_hp,
                h_hp,
                hackable,
                hackable && !was_hackable,
            )
        };

        if attacker_char_id > 0 && (applied > 0.0 || state_changed) {
            let mut ds =
                crate::services::structure_acl_defense_rules::load_defense_settings(snap, skyhook_id);
            ds.record_attacker(attacker_char_id, now);
            crate::services::structure_acl_defense_rules::save_defense_settings(snap, &ds);
        }

        let mut pending_sig = Vec::new();
        let mut final_s_hp = s_hp;
        let mut final_a_hp = a_hp;
        let mut final_h_hp = h_hp;
        let mut final_state = snap
            .structures
            .get(&skyhook_id)
            .map(|s| s.state.clone())
            .unwrap_or(prev_state);
        if destroyed_now {
            let mut ext =
                crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
            let outcome = crate::services::station_structure::structure_lifecycle::destroy_structure(
                ctx,
                snap,
                &mut ext,
                skyhook_id,
                attacker_char_id,
                now,
            );
            crate::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
            pending_sig = outcome.pending_signal_tracker_notifs;
            pending
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .extend(outcome.pending_quasar_notices);
            final_s_hp = 0.0;
            final_a_hp = 0.0;
            final_h_hp = 0.0;
            final_state =
                crate::services::station_structure::structure_lifecycle::STATE_DESTROYED.into();
            state_changed = true;
        }

        let s_pct = (final_s_hp / SKYHOOK_BASE_SHIELD_HP).clamp(0.0, 1.0);
        let a_pct = (final_a_hp / SKYHOOK_BASE_ARMOR_HP).clamp(0.0, 1.0);
        let h_pct = (final_h_hp / SKYHOOK_BASE_HULL_HP).clamp(0.0, 1.0);
        Ok((
            sys_id,
            final_s_hp,
            final_a_hp,
            final_h_hp,
            s_pct,
            a_pct,
            h_pct,
            applied,
            hackable,
            newly_hackable,
            final_state,
            state_changed,
            destroyed_now,
            pending_sig,
        ))
    })?;

    if !pending_sig.is_empty() {
        crate::services::fleet_social::fleet_social_scanning::dispatch_signal_tracker_notifications(
            ctx,
            pending_sig,
        );
    }
    crate::services::sov_gameplay::flush_pending_quasar(ctx, &pending, now);
    ctx.emit_typed(
        NotificationTarget::SolarSystem(sys_id),
        crate::router::notify_registry::OnSkyhookSiloDamageUpdated {
            shield_percentage: shield_pct,
        },
    );
    if state_changed {
        crate::services::sov_services::emit_sov_structures_updated(ctx, sys_id, &[skyhook_id]);
    }
    if newly_hackable || hackable_now {
        crate::services::sov_services::emit_skyhook_can_be_hacked(ctx, sys_id, skyhook_id);
    }
    Ok(PyRep::keyval([
        ("structureID", PyRep::Integer(skyhook_id)),
        ("state", PyRep::String(state_str)),
        ("shieldHP", PyRep::Real(shield_hp)),
        ("armorHP", PyRep::Real(armor_hp)),
        ("hullHP", PyRep::Real(hull_hp)),
        ("shieldPct", PyRep::Real(shield_pct)),
        ("armorPct", PyRep::Real(armor_pct)),
        ("hullPct", PyRep::Real(hull_pct)),
        ("appliedDamage", PyRep::Real(applied_dmg)),
        ("hackable", PyRep::Bool(hackable_now)),
        ("destroyed", PyRep::Bool(destroyed_now)),
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
    let (_, ship_ball) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .ok()
        .flatten()
        .ok_or_else(|| BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: format!("Ship {ship_id} is not present in space in system {sys_id}"),
            client_error: None,
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
            client_error: None,
        });
    }

    let skyhook_st = ctx
        .db
        .with_snapshot(|snap| snap.structures.get(&skyhook_id).cloned())
        .filter(|st| st.type_id == TYPE_ORBITAL_SKYHOOK && st.state != "Destroyed")
        .ok_or_else(|| BridgeRpcError {
            code: "SkyhookNotFound".into(),
            message: format!("Orbital Skyhook {skyhook_id} not found"),
            client_error: None,
        })?;
    if skyhook_st.solar_system_id != sys_id {
        return Err(BridgeRpcError {
            code: "TargetNotInSameSystem".into(),
            message: format!(
                "Orbital Skyhook {skyhook_id} is in system {}, caller is in {sys_id}",
                skyhook_st.solar_system_id
            ),
            client_error: None,
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
            client_error: None,
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
                    client_error: None,
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
                    client_error: None,
                });
            }
            let completes_at = now + SKYHOOK_SILO_LINK_DURATION_SECS;
            let orig_max_vel = ship_ball.max_velocity;
            ctx.db.try_with_mut_snapshot(|snap| {
                let _ = advance_skyhook_reagents_in_snap(snap, &ctx.sde, skyhook_id, now);
                let window_open = match snap
                    .sov_runtime
                    .skyhook_theft_windows
                    .get(&skyhook_id)
                    .copied()
                {
                    Some((start, end)) => start <= now && now <= end,
                    None => snap
                        .sov_runtime
                        .skyhook_hackable
                        .get(&skyhook_id)
                        .copied()
                        .unwrap_or(false),
                };
                if !window_open {
                    return Err(BridgeRpcError {
                        code: "SkyhookNotVulnerableToTheft".into(),
                        message: format!(
                            "Skyhook {skyhook_id} is outside its reagent theft vulnerability window"
                        ),
                        client_error: None,
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
                            client_error: None,
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
                        client_error: None,
                    });
                }
                snap.sov_runtime.skyhook_silo_links.insert(
                    skyhook_id,
                    (char_id, ship_id, completes_at, orig_max_vel),
                );
                Ok(())
            })?;

            let _ = sys_handle
                .set_ball_max_velocity_cap(ship_id, Some(SKYHOOK_SILO_LINK_MAX_SPEED_MS))
                .await;
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
                    client_error: None,
                });
            }
            let req_qty_opt = args.get(4).and_then(|a| a.as_i64()).filter(|&q| q > 0).map(|q| q as u64);
            let (reagent_tid, extracted, rem_sec, rem_insec, cycle_ts, planet_id) =
                ctx.db.try_with_mut_snapshot(|snap| {
                    let free_m3 = super::ship_free_cargo_m3(ctx, snap, ship_id)?;
                    let _ = advance_skyhook_reagents_in_snap(snap, &ctx.sde, skyhook_id, now);
                    let pid = snap.sov_runtime.skyhook_planets.get(&skyhook_id).copied();
                    let entry = snap
                        .sov_runtime
                        .skyhook_reagent_bays
                        .get_mut(&skyhook_id)
                        .ok_or_else(|| BridgeRpcError {
                            code: "SkyhookReagentBayNotFound".into(),
                            message: format!("Skyhook {skyhook_id} has no reagent bay"),
                            client_error: None,
                        })?;
                    if entry.1 == 0 {
                        return Err(BridgeRpcError {
                            code: "NoSecuredReagentsAvailable".into(),
                            message: format!("Skyhook {skyhook_id} has 0 secured reagents available"),
                            client_error: None,
                        });
                    }
                    let unit_vol = ctx
                        .sde
                        .types
                        .get(&entry.0)
                        .map(|t| t.volume)
                        .unwrap_or(0.01)
                        .max(1e-6);
                    let max_units = ((free_m3 + 1e-9) / unit_vol).floor() as u64;
                    if max_units == 0 {
                        let req_vol = (req_qty_opt.unwrap_or(1) as f64) * unit_vol;
                        return Err(BridgeRpcError::with_client_error(
                            "NotEnoughCargoSpace",
                            format!(
                                "Active ship {ship_id} does not have enough free cargo capacity to extract Skyhook reagents"
                            ),
                            eve_proto::ClientError::hold_capacity_error(
                                eve_proto::UserErrorKey::NotEnoughCargoSpace,
                                req_vol,
                                free_m3.max(0.0),
                            ),
                        ));
                    }
                    let take = req_qty_opt.unwrap_or(entry.1).min(entry.1).min(max_units);
                    entry.1 -= take;
                    let r_tid = entry.0;
                    let rem_s = entry.1;
                    let insec = entry.2;
                    let cycle_ts = entry.3;
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
                    Ok((r_tid, take, rem_s, insec, cycle_ts, pid))
                })?;
            if let Some(notice) =
                crate::services::quasar_sov_resources::build_reagent_simulations_notice(
                    "sov_gameplay_skyhook.extract_reagents",
                    skyhook_id,
                    planet_id,
                    reagent_tid,
                    rem_sec,
                    rem_insec,
                    cycle_ts,
                )
            {
                let _ = ctx.quasar_gateway.publish_notice(
                    NoticeScope::SolarSystem(skyhook_st.solar_system_id),
                    notice,
                    now,
                );
            }
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
            client_error: None,
        }),
    }
}

/// Advances all Orbital Skyhook reagent bays and resolves active 10-minute Reagent Silo links.
pub async fn tick_skyhooks(ctx: &ServerContext, now_unix: u64) {
    let (skyhook_ids, links): (Vec<i64>, Vec<(i64, (i64, i64, u64, f64))>) =
        ctx.db.with_snapshot(|snap| {
            let mut ids: Vec<i64> = snap.sov_runtime.skyhook_reagent_bays.keys().copied().collect();
            for &id in snap.sov_runtime.skyhook_active.keys() {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
            for (&id, st) in &snap.structures {
                if st.type_id == crate::services::sov_services::TYPE_ORBITAL_SKYHOOK
                    && (st.state == "ArmorReinforced"
                        || st.state.eq_ignore_ascii_case("destroyed")
                        || st.last_damage_unix > 0)
                    && !ids.contains(&id)
                {
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

    if skyhook_ids.is_empty() && links.is_empty() {
        return;
    }

    let pending: crate::services::quasar_sov_hub::PendingNotices =
        std::sync::Arc::new(std::sync::RwLock::new(Vec::new()));
    let (newly_hackable, reinf_expired, repaired): (Vec<(u32, i64)>, Vec<(u32, i64)>, Vec<(u32, i64)>) = ctx.db.with_mut_snapshot(|snap| {
        let mut opened = Vec::new();
        let mut expired_reinf = Vec::new();
        let mut repaired = Vec::new();
        for id in skyhook_ids {
            if let Some(sys_id) = snap
                .structures
                .get(&id)
                .filter(|s| s.state.eq_ignore_ascii_case("destroyed"))
                .map(|s| s.solar_system_id)
            {
                if has_skyhook_runtime_state(snap, id) {
                    let _ = crate::services::quasar_sov_workforce::with_system_hub_network_reconcile(
                        snap,
                        &ctx.sde,
                        &pending,
                        sys_id,
                        now_unix,
                        |snap| {
                            cleanup_destroyed_skyhook_in_snap(snap, id);
                            Ok::<_, ()>(())
                        },
                    );
                }
                continue;
            }
            if let Some(st) = snap.structures.get_mut(&id) {
                if st.type_id == crate::services::sov_services::TYPE_ORBITAL_SKYHOOK
                    && st.state == "ArmorReinforced"
                    && st.reinforced_until_unix > 0
                    && now_unix >= st.reinforced_until_unix
                {
                    let exit_unix = st.reinforced_until_unix;
                    st.state = "ArmorVulnerable".into();
                    st.reinforced_until_unix = 0;
                    if st.last_damage_unix > 0 {
                        // Start the unattacked armor repair timer from reinforcement exit
                        // rather than the prior shield-drop hit; fixtures with `last_damage_unix == 0`
                        // keep 0 so pre-seeded damaged states are preserved.
                        st.last_damage_unix = exit_unix;
                    }
                    expired_reinf.push((st.solar_system_id, id));
                }
            }
            let _ = advance_skyhook_reagents_in_snap(snap, &ctx.sde, id, now_unix);
            if !snap.sov_runtime.skyhook_theft_windows.contains_key(&id) {
                let reinforce_hour = snap
                    .structures
                    .get(&id)
                    .and_then(|st| snap.corp_governance.get(&st.owner_corp_id))
                    .map(|g| g.reinforce_hour);
                let initial_window =
                    next_skyhook_theft_window(id, now_unix, reinforce_hour, now_unix);
                snap.sov_runtime
                    .skyhook_theft_windows
                    .insert(id, initial_window);
            }
            let (start, end) = advance_skyhook_theft_window_in_snap(snap, id, now_unix)
                .unwrap_or_else(|| next_skyhook_theft_window(id, now_unix, None, now_unix));
            let open_now = start <= now_unix && now_unix < end;
            let has_active_link = snap.sov_runtime.skyhook_silo_links.contains_key(&id);
            if !open_now && !has_active_link {
                if let Some(st) = snap.structures.get_mut(&id) {
                    let repair_delay = if st.state == "HullVulnerable" {
                        crate::services::station_structure::structure_lifecycle::REPAIR_HULL_SECS
                    } else {
                        crate::services::station_structure::structure_lifecycle::REPAIR_SHIELD_ARMOR_SECS
                    };
                    let damaged = st.shield_hp < SKYHOOK_BASE_SHIELD_HP
                        || st.armor_hp < SKYHOOK_BASE_ARMOR_HP
                        || st.hull_hp < SKYHOOK_BASE_HULL_HP;
                    if st.type_id == crate::services::sov_services::TYPE_ORBITAL_SKYHOOK
                        && matches!(
                            st.state.as_str(),
                            "ShieldVulnerable" | "ArmorVulnerable" | "HullVulnerable" | "Online"
                        )
                        && st.last_damage_unix > 0
                        && damaged
                        && now_unix >= st.last_damage_unix.saturating_add(repair_delay)
                    {
                        st.shield_hp = SKYHOOK_BASE_SHIELD_HP;
                        st.armor_hp = SKYHOOK_BASE_ARMOR_HP;
                        st.hull_hp = SKYHOOK_BASE_HULL_HP;
                        if matches!(st.state.as_str(), "ArmorVulnerable" | "HullVulnerable") {
                            st.state = "ShieldVulnerable".into();
                        }
                        st.reinforced_until_unix = 0;
                        st.last_damage_unix = 0;
                        repaired.push((st.solar_system_id, id));
                    }
                }
            }
            let shield_low = snap.structures.get(&id).is_some_and(|st| {
                st.state != "Destroyed" && st.shield_hp <= SKYHOOK_BASE_SHIELD_HP * 0.10 + 1e-9
            });
            let hackable_now = open_now && shield_low;
            let was_open = snap.sov_runtime.skyhook_hackable.get(&id).copied().unwrap_or(false);
            if hackable_now != was_open {
                snap.sov_runtime.skyhook_hackable.insert(id, hackable_now);
                if hackable_now {
                    if let Some(st) = snap.structures.get(&id) {
                        opened.push((st.solar_system_id, id));
                    }
                }
            }
        }
        (opened, expired_reinf, repaired)
    });
    crate::services::sov_gameplay::flush_pending_quasar(ctx, &pending, now_unix);
    let mut updated_structs = reinf_expired;
    for item in &repaired {
        if !updated_structs.contains(item) {
            updated_structs.push(*item);
        }
    }
    for (sys_id, sh_id) in updated_structs {
        crate::services::sov_services::emit_sov_structures_updated(ctx, sys_id, &[sh_id]);
    }
    for (sys_id, sh_id) in repaired {
        if let Some(sys_h) = ctx.systems.get_handle(sys_id) {
            let _ = sys_h
                .modify_combat_state(sh_id, |cs| {
                    cs.shield_hp = SKYHOOK_BASE_SHIELD_HP;
                    cs.max_shield_hp = SKYHOOK_BASE_SHIELD_HP;
                    cs.armor_hp = SKYHOOK_BASE_ARMOR_HP;
                    cs.max_armor_hp = SKYHOOK_BASE_ARMOR_HP;
                    cs.structure_hp = SKYHOOK_BASE_HULL_HP;
                    cs.max_structure_hp = SKYHOOK_BASE_HULL_HP;
                })
                .await;
        }
    }
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
            if ship_opt.is_some() && orig_vel > 0.0 {
                let _ = sys_handle.set_ball_max_velocity_cap(ship_id, None).await;
            }
            continue;
        }

        if now_unix < completes_at {
            if let Some((_, sb)) = ship_opt {
                if sb.max_velocity > SKYHOOK_SILO_LINK_MAX_SPEED_MS {
                    let _ = sys_handle
                        .set_ball_max_velocity_cap(ship_id, Some(SKYHOOK_SILO_LINK_MAX_SPEED_MS))
                        .await;
                }
            }
            continue;
        }

        // 10-minute link completed: drop 100% of insecure surplus bay into an in-space Cargo Container ball,
        // leaving secure bay untouched, and restore ship max velocity.
        let drop_res = ctx.db.with_mut_snapshot(|snap| {
            snap.sov_runtime.skyhook_silo_links.remove(&skyhook_id);
            let _ = advance_skyhook_reagents_in_snap(snap, &ctx.sde, skyhook_id, now_unix);
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
            let planet_id = snap.sov_runtime.skyhook_planets.get(&skyhook_id).copied();
            Some((container_id, c_pos, r_type, final_sec, planet_id))
        });

        if ship_opt.is_some() && orig_vel > 0.0 {
            let _ = sys_handle.set_ball_max_velocity_cap(ship_id, None).await;
        }
        if let Some((container_id, c_pos, r_type, final_sec, planet_id)) = drop_res {
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
            if let Some(notice) =
                crate::services::quasar_sov_resources::build_reagent_simulations_notice(
                    "sov_gameplay_skyhook.tick_theft",
                    skyhook_id,
                    planet_id,
                    r_type,
                    final_sec,
                    0,
                    now_unix,
                )
            {
                let _ = ctx.quasar_gateway.publish_notice(
                    NoticeScope::SolarSystem(sys_id),
                    notice,
                    now_unix,
                );
            }
        }
    }
}

#[cfg(test)] #[path = "sov_gameplay_skyhook_tests.rs"]
mod sov_gameplay_skyhook_tests;

#[cfg(test)] #[path = "sov_gameplay_skyhook_edges2_tests.rs"]
mod sov_gameplay_skyhook_edges2_tests;

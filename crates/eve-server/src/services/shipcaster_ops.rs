//! `shipcaster_ops` — Faction Warfare Interstellar Shipcasters, Dynamic FW Shipcaster Beacon
//! Construction/Linking/Cycling/Disruption, and Rookie Starter Shipcasters.
//!
//! Implements:
//! - `shipCasterLandingPadMgr.GetAllLandingPads`
//! - `shipCasterLandingPadMgr.GetFactionLandingPads`
//! - `shipCasterLauncherMgr.GetFactionsWithShipcaster`
//! - `starterShipcasterTravelMgr.CanCharacterJump`
//! - `starterShipcasterTravelMgr.CmdJumpThroughStarterShipcaster`
//! - `shipcasterTravelMgr.CmdJumpThroughShipcaster`
//! - Dynamic FW Shipcaster Beacon Constructor deployment (`75932..=75935`), SDE `underConstruction`
//!   material contribution + militia LP payout, completion to `builtLandingPad` (`75928..=75931`),
//!   0..=3 simultaneous linked beacons per empire faction, 6-minute (`360`s) destination cycling,
//!   and opposing-militia disruption/unlinking with `OnShipcasterTargetStateChanged(itemID)`.

#![forbid(unsafe_code)]

use eve_db::InventoryFlag;

use crate::router::{NotificationTarget, ServerContext};
use crate::services::cluster_macho_misc_services::{
    LandingPadRecord, LauncherRecord, persist_misc_state, with_misc_state,
};
use eve_destiny::{Ball, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub const SHIPCASTER_MAX_RANGE_M: f64 = 5_000.0;
pub const MAX_LINKED_LANDING_PADS_PER_FACTION: usize = 3;
pub const SHIPCASTER_TARGET_CYCLE_SECS: u64 = 360;
pub const STARTER_SHIPCASTER_MAX_ROOKIE_AGE_SECS: u64 = 30 * 86_400;

fn rpc_err(code: impl Into<String>, msg: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: msg.into(),
    }
}

fn is_capital_or_freighter_group(group_id: u32) -> bool {
    matches!(
        group_id,
        30 | 485 | 513 | 547 | 659 | 883 | 902 | 1538 | 4594
    )
}

pub(crate) use shipcaster_ops_starter::emit_shipcaster_target_state_changed_core;

fn emit_shipcaster_target_state_changed(ctx: &ServerContext, solar_system_id: u32, item_id: i64) {
    // parity: notify:OnShipcasterTargetStateChanged
    emit_shipcaster_target_state_changed_core(
        &ctx.db,
        Some(&ctx.pending_notifications),
        &ctx.notify_tx,
        solar_system_id,
        item_id,
    );
}

fn build_beacon_slim_keyval(ctx: &ServerContext, pad: &LandingPadRecord) -> PyRep {
    let (group_id, category_id) = ctx.sde.types.get(&pad.type_id).map(|t| (i64::from(t.group_id), i64::from(t.category_id))).unwrap_or((4547, 2));
    let progress: Vec<(PyRep, PyRep)> = pad.contributed_materials.iter().map(|(&tid, &q)| (PyRep::Integer(i64::from(tid)), PyRep::Integer(i64::from(q)))).collect();
    PyRep::keyval([
        ("itemID", PyRep::Integer(pad.item_id)), ("typeID", PyRep::Integer(i64::from(pad.type_id))),
        ("groupID", PyRep::Integer(group_id)), ("categoryID", PyRep::Integer(category_id)),
        ("ownerID", PyRep::Integer(i64::from(pad.faction_id))), ("charID", PyRep::None),
        ("corpID", PyRep::Integer(0)), ("allianceID", PyRep::None),
        ("component_underConstruction_progress", PyRep::Dict(progress)),
        ("component_underConstruction_completed", PyRep::Bool(pad.is_built)),
    ])
}

async fn emit_beacon_slim_item_change(ctx: &ServerContext, old_pad: &LandingPadRecord, new_pad: &LandingPadRecord) {
    ctx.notify(
        NotificationTarget::SolarSystem(new_pad.solar_system_id),
        "OnSlimItemChange",
        "solarsystemid",
        vec![build_beacon_slim_keyval(ctx, old_pad), build_beacon_slim_keyval(ctx, new_pad)],
    );
    let (group_id, category_id) = ctx.sde.types.get(&new_pad.type_id).map(|t| (t.group_id, t.category_id)).unwrap_or((4547, 2));
    let sys = ctx.systems.get_or_spawn(new_pad.solar_system_id);
    let _ = sys.change_slim_item(new_pad.item_id, SlimItemMeta {
        group_id, category_id, owner_id: i64::from(new_pad.faction_id), char_id: None,
        corp_id: 0, alliance_id: -1, security_status: 0.0, is_cloaked: false, ..Default::default()
    }).await;
}

async fn spawn_or_replace_pad_ball(ctx: &ServerContext, pad: &LandingPadRecord) {
    let sys = ctx.systems.get_or_spawn(pad.solar_system_id);
    let _ = sys.remove_ball(pad.item_id).await;
    let (group_id, category_id) = ctx
        .sde
        .types
        .get(&pad.type_id)
        .map(|t| (t.group_id, t.category_id))
        .unwrap_or((4547, 2));
    let _ = sys
        .spawn_ball_with_meta(
            Ball {
                id: pad.item_id,
                type_id: pad.type_id,
                position: pad.position,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 1.0e10,
                radius: 20_000.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            },
            ctx.build_ship_combat_state(0, pad.item_id, pad.type_id),
            SlimItemMeta {
                group_id,
                category_id,
                owner_id: pad.faction_id as i64,
                char_id: None,
                corp_id: 0,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
                ..Default::default()
            },
        )
        .await;
}

/// Returns the currently active destination landing pad for `launcher` during its 6-minute
/// (`360`s) rotation cycle across `1..=3` built, linked, non-disrupted landing pads (`Shipcaster.md:15`).
pub fn active_linked_pad_for_launcher(
    launcher: &LauncherRecord,
    pads: &[LandingPadRecord],
    now_unix: u64,
) -> Option<LandingPadRecord> {
    let mut active: Vec<LandingPadRecord> = launcher
        .linked_pad_ids
        .iter()
        .filter_map(|id| {
            pads.iter()
                .find(|p| p.item_id == *id && p.is_built && !p.is_disrupted && p.link_timestamp > 0)
                .cloned()
        })
        .collect();
    if active.is_empty() {
        return None;
    }
    if active.len() == 1 {
        return active.pop();
    }
    let idx = ((now_unix / SHIPCASTER_TARGET_CYCLE_SECS) as usize) % active.len();
    Some(active.swap_remove(idx))
}

/// Returns the active target landing pad for a given empire faction (`500001..=500004`) at `ctx.now_unix()`.
pub fn active_target_pad_for_faction(ctx: &ServerContext, faction_id: i32) -> Option<LandingPadRecord> {
    let now = ctx.now_unix();
    with_misc_state(ctx, |st| {
        let launcher = st.launchers.iter().find(|l| l.faction_id == faction_id)?;
        active_linked_pad_for_launcher(launcher, &st.landing_pads, now)
    })
}

/// Deploys an FW Shipcaster Beacon Constructor (`75932..=75935`) from the pilot's ship hold into
/// space in a lowsec FW warzone solar system, initializing SDE `underConstruction` requirements.
pub async fn deploy_fw_beacon_constructor(
    ctx: &ServerContext,
    session: &SessionState,
    constructor_item_id: i64,
    position: DVec3,
) -> Result<LandingPadRecord, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(rpc_err("CantDoThatWhileDocked", "Cannot deploy beacon constructor while docked"));
    }
    let Some(ship_id) = session.ship_id else {
        return Err(rpc_err("CustomError", "Deploying a beacon constructor requires an active ship"));
    };
    let Some(sys_id) = session.solar_system_id else {
        return Err(rpc_err("CustomError", "Deploying a beacon constructor requires an active solar system"));
    };
    let sys_info = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .ok_or_else(|| rpc_err("InvalidSolarSystem", "Unknown solar system"))?;
    let rounded_sec = (sys_info.security_status * 10.0).round() / 10.0;
    if sys_id >= 31_000_000 || sys_id == 30_100_000 || rounded_sec <= 0.0 || rounded_sec >= 0.45 {
        return Err(rpc_err(
            "InvalidBeaconDeploymentSystem",
            "Shipcaster Beacon Constructors can only be deployed in low-security Faction Warfare systems",
        ));
    }

    let comps = ctx
        .sde
        .shipcaster_sde_components()
        .map_err(|e| rpc_err("SdeError", e.to_string()))?;

    let pad = ctx.db.try_with_mut_snapshot(|snap| {
        if !snap.fw_system_occupations.contains_key(&sys_id) {
            return Err(rpc_err(
                "InvalidBeaconDeploymentSystem",
                "Shipcaster Beacon Constructors can only be deployed in low-security Faction Warfare systems",
            ));
        }
        let char_war_faction = snap
            .characters
            .get(&char_id)
            .and_then(|c| c.war_faction_id)
            .ok_or_else(|| {
                rpc_err(
                    "FactionShipcasterAccessDenied",
                    "Only enlisted Faction Warfare militia pilots may deploy a Shipcaster Beacon Constructor",
                )
            })?;
        let item = snap
            .items
            .get(&constructor_item_id)
            .ok_or_else(|| rpc_err("ItemNotFound", "Beacon constructor item not found"))?;
        if item.location_id != ship_id
            || item.owner_id != char_id
            || item.flag != InventoryFlag::Cargo
            || snap.is_item_locked_in_contract_or_job(item.item_id)
            || item.quantity == 0
        {
            return Err(rpc_err(
                "ItemNotInCargo",
                "Beacon constructor must be an unlocked item in the active ship's cargo hold",
            ));
        }
        let spec = comps.under_construction.get(&item.type_id).ok_or_else(|| {
            rpc_err(
                "InvalidBeaconConstructorType",
                format!("Item type {} is not an underConstruction beacon constructor", item.type_id),
            )
        })?;
        let pad_faction = comps
            .landing_pad_factions
            .get(&spec.built_item_type_id)
            .copied()
            .unwrap_or(0);
        if i64::from(pad_faction) != char_war_faction {
            return Err(rpc_err(
                "FactionShipcasterAccessDenied",
                "Pilot militia faction does not match this Shipcaster Beacon Constructor",
            ));
        }
        let c_tid = item.type_id;
        let required_materials: BTreeMap<u32, u32> = spec
            .input_items
            .iter()
            .map(|(&tid, entry)| (tid, entry.required_qty))
            .collect();
        let alloc_id = snap.next_entity_id + 1;
        let new_pad = with_misc_state(ctx, |st| {
            if st
                .landing_pads
                .iter()
                .any(|p| p.solar_system_id == sys_id && matches!(p.faction_id, 500_001..=500_004) && !p.is_disrupted)
            {
                return Err(rpc_err(
                    "BeaconAlreadyExistsInSystem",
                    "A Shipcaster Beacon or Constructor is already deployed in this solar system",
                ));
            }
            let record = LandingPadRecord {
                item_id: alloc_id,
                type_id: c_tid,
                built_type_id: spec.built_item_type_id,
                solar_system_id: sys_id,
                faction_id: pad_faction as i32,
                is_built: false,
                link_timestamp: 0,
                is_disrupted: false,
                position,
                required_materials,
                contributed_materials: BTreeMap::new(),
            };
            st.landing_pads.push(record.clone());
            Ok(record)
        })?;
        snap.next_entity_id = alloc_id;
        if let Some(con_owner) = snap.items.get(&constructor_item_id).map(|i| i.owner_id) {
            snap.consume_exact_item_quantity(
                constructor_item_id,
                1,
                eve_db::ConsumeExactOptions::for_owner_exact(Some(char_id), con_owner),
            )
            .map_err(|e| BridgeRpcError {
                code: "ConstructorLocked".into(),
                message: e.to_string(),
            })?;
        }
        Ok::<LandingPadRecord, BridgeRpcError>(new_pad)
    })?;

    persist_misc_state(ctx);
    spawn_or_replace_pad_ball(ctx, &pad).await;
    Ok(pad)
}

/// Contributes construction materials from the pilot's active ship hold into an unbuilt FW
/// Shipcaster Beacon Constructor (`75932..=75935`), awarding SDE militia LP and automatically
/// transitioning to `builtLandingPad` (`75928..=75931`) + linking (if `< 3` pads linked) on completion.
pub async fn contribute_fw_beacon_material(
    ctx: &ServerContext,
    session: &SessionState,
    pad_item_id: i64,
    cargo_item_id: i64,
    quantity: u32,
) -> Result<LandingPadRecord, BridgeRpcError> {
    contribute_fw_beacon_material_inner(ctx, session, pad_item_id, cargo_item_id, quantity)
        .await
        .map(|(pad, _)| pad)
}

async fn contribute_fw_beacon_material_inner(
    ctx: &ServerContext,
    session: &SessionState,
    pad_item_id: i64,
    cargo_item_id: i64,
    quantity: u32,
) -> Result<(LandingPadRecord, u32), BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if quantity == 0 {
        return Err(rpc_err("InvalidArguments", "Contribution quantity must be positive"));
    }
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(rpc_err("CantDoThatWhileDocked", "Cannot contribute materials while docked"));
    }
    let Some(ship_id) = session.ship_id else {
        return Err(rpc_err("CustomError", "Contributing materials requires an active ship"));
    };
    let Some(sys_id) = session.solar_system_id else {
        return Err(rpc_err("CustomError", "Contributing materials requires an active solar system"));
    };

    let comps = ctx
        .sde
        .shipcaster_sde_components()
        .map_err(|e| rpc_err("SdeError", e.to_string()))?;

    let interaction_range_m = with_misc_state(ctx, |st| {
        let pad = st
            .landing_pads
            .iter()
            .find(|p| p.item_id == pad_item_id)
            .ok_or_else(|| rpc_err("LandingPadNotFound", format!("Beacon constructor {pad_item_id} not found")))?;
        if pad.solar_system_id != sys_id {
            return Err(rpc_err("InvalidSolarSystem", "Pilot is not in the beacon constructor's solar system"));
        }
        if pad.is_built {
            return Err(rpc_err("BeaconAlreadyBuilt", "Shipcaster Beacon construction is already complete"));
        }
        let spec = comps.under_construction.get(&pad.type_id).ok_or_else(|| {
            rpc_err("InvalidBeaconConstructorType", "Pad does not carry an SDE underConstruction component")
        })?;
        Ok::<f64, BridgeRpcError>(spec.interaction_range_m)
    })?;

    let sys = ctx.systems.get_or_spawn(sys_id);
    check_proximity(&sys, ship_id, pad_item_id, interaction_range_m, "beacon constructor").await?;

    let (old_pad, mut updated_pad, taken_qty, newly_built) = ctx.db.try_with_mut_snapshot(|snap| {
        let char_war_faction = snap
            .characters
            .get(&char_id)
            .and_then(|c| c.war_faction_id)
            .unwrap_or(0);
        let item = snap
            .items
            .get(&cargo_item_id)
            .ok_or_else(|| rpc_err("ItemNotFound", "Material stack not found in inventory"))?;
        if item.location_id != ship_id
            || item.owner_id != char_id
            || item.flag != InventoryFlag::Cargo
            || snap.is_item_locked_in_contract_or_job(item.item_id)
            || item.quantity == 0
        {
            return Err(rpc_err("ItemNotInCargo", "Material stack must be an unlocked item in the active ship's cargo hold"));
        }
        let mat_tid = item.type_id;
        let avail = u32::try_from(item.quantity.max(0)).unwrap_or(0);

        let (old_pad, updated_pad, take, newly_built, lp_reward_opt) = with_misc_state(ctx, |st| {
            let pad_mut = st
                .landing_pads
                .iter_mut()
                .find(|p| p.item_id == pad_item_id)
                .ok_or_else(|| rpc_err("LandingPadNotFound", format!("Beacon constructor {pad_item_id} not found")))?;
            if pad_mut.solar_system_id != sys_id {
                return Err(rpc_err("InvalidSolarSystem", "Pilot is not in the beacon constructor's solar system"));
            }
            if pad_mut.is_built {
                return Err(rpc_err("BeaconAlreadyBuilt", "Shipcaster Beacon construction is already complete"));
            }
            if char_war_faction != i64::from(pad_mut.faction_id) {
                return Err(rpc_err(
                    "FactionShipcasterAccessDenied",
                    "Only pilots enlisted in the owning militia may contribute construction materials",
                ));
            }
            let spec = comps.under_construction.get(&pad_mut.type_id).ok_or_else(|| {
                rpc_err("InvalidBeaconConstructorType", "Pad does not carry an SDE underConstruction component")
            })?;
            let Some(&req_qty) = pad_mut.required_materials.get(&mat_tid) else {
                return Err(rpc_err(
                    "InvalidConstructionMaterial",
                    format!("Type {mat_tid} is not required by constructor {}", pad_mut.type_id),
                ));
            };
            let already = pad_mut.contributed_materials.get(&mat_tid).copied().unwrap_or(0);
            let deficit = req_qty.saturating_sub(already);
            if deficit == 0 {
                return Err(rpc_err("MaterialRequirementAlreadyMet", "Material requirement is already satisfied"));
            }
            let take = quantity.min(deficit).min(avail);
            if take == 0 {
                return Err(rpc_err("InsufficientMaterialQuantity", "No usable units available"));
            }
            let old_pad = pad_mut.clone();
            let cur = pad_mut.contributed_materials.entry(mat_tid).or_insert(0);
            *cur = cur.saturating_add(take);
            let all_done = pad_mut
                .required_materials
                .iter()
                .all(|(tid, &req)| pad_mut.contributed_materials.get(tid).copied().unwrap_or(0) >= req);
            let newly_built = if all_done && !pad_mut.is_built {
                pad_mut.is_built = true;
                if pad_mut.built_type_id > 0 {
                    pad_mut.type_id = pad_mut.built_type_id;
                }
                true
            } else {
                false
            };
            let lp_reward_opt = spec
                .input_items
                .get(&mat_tid)
                .and_then(|in_spec| in_spec.lp_corp_id.filter(|_| in_spec.lp_per_unit > 0).map(|corp| (corp, in_spec.lp_per_unit)));
            Ok((old_pad, pad_mut.clone(), take, newly_built, lp_reward_opt))
        })?;

        let take_i32 = i32::try_from(take).unwrap_or(i32::MAX);
        if let Some(cargo_owner) = snap.items.get(&cargo_item_id).map(|i| i.owner_id) {
            let mut opts = eve_db::ConsumeExactOptions::for_character_stack(char_id);
            opts.expected_owner_id = cargo_owner;
            snap.consume_exact_item_quantity(cargo_item_id, take_i32, opts)
                .map_err(|e| BridgeRpcError {
                    code: "MaterialLocked".into(),
                    message: e.to_string(),
                })?;
        }
        if let Some((lp_corp, lp_per_unit)) = lp_reward_opt {
            let lp_gain = i64::from(lp_per_unit) * i64::from(take);
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                let entry = ch.loyalty_points.entry(lp_corp).or_insert(0);
                *entry = entry.saturating_add(lp_gain);
            }
        }
        Ok::<(LandingPadRecord, LandingPadRecord, u32, bool), BridgeRpcError>((
            old_pad,
            updated_pad,
            take,
            newly_built,
        ))
    })?;

    if newly_built {
        if let Ok(linked_pad) = link_fw_landing_pad(ctx, pad_item_id) {
            updated_pad = linked_pad;
        }
    }

    persist_misc_state(ctx);
    if updated_pad.is_built {
        spawn_or_replace_pad_ball(ctx, &updated_pad).await;
        if newly_built {
            emit_shipcaster_target_state_changed(ctx, updated_pad.solar_system_id, updated_pad.item_id);
        }
    }
    emit_beacon_slim_item_change(ctx, &old_pad, &updated_pad).await;
    Ok((updated_pad, taken_qty))
}

/// V24 client `beyonce.CallComponentFromClient(itemID, "underConstruction", "DepositItemsFromShip", qtyByTypeID)`
/// handler: deposits `{typeID: qty}` stacks from the pilot's active ship hold into the target
/// `underConstruction` beacon constructor and returns `(successfulDeposit, failedToMove)`.
pub async fn deposit_items_from_ship(
    ctx: &ServerContext,
    session: &SessionState,
    pad_item_id: i64,
    type_and_qty_arg: Option<&PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let ship_id = session
        .ship_id
        .ok_or_else(|| rpc_err("NoActiveShip", "DepositItemsFromShip requires an active ship"))?;
    let Some(PyRep::Dict(pairs)) = type_and_qty_arg else {
        return Err(rpc_err(
            "InvalidArguments",
            "DepositItemsFromShip requires a {typeID: qty} dict",
        ));
    };
    let mut any_deposited = false;
    let mut failed_to_move = Vec::new();
    for (k, v) in pairs {
        let Some(tid) = k.as_i64().and_then(|x| u32::try_from(x).ok()) else {
            continue;
        };
        let mut rem = v.as_i64().and_then(|x| u32::try_from(x).ok()).unwrap_or(0);
        if rem == 0 {
            continue;
        }
        let stack_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
            snap.items
                .values()
                .filter(|it| {
                    it.location_id == ship_id
                        && it.owner_id == char_id
                        && it.flag == InventoryFlag::Cargo
                        && !snap.is_item_locked_in_contract_or_job(it.item_id)
                        && it.type_id == tid
                        && it.quantity > 0
                })
                .map(|it| it.item_id)
                .collect()
        });
        for sid in stack_ids {
            if rem == 0 {
                break;
            }
            match contribute_fw_beacon_material_inner(ctx, session, pad_item_id, sid, rem).await {
                Ok((_, taken)) if taken > 0 => {
                    any_deposited = true;
                    rem = rem.saturating_sub(taken);
                }
                _ => break,
            }
        }
        if rem > 0 {
            failed_to_move.push((PyRep::Integer(tid as i64), PyRep::Integer(rem as i64)));
        }
    }
    Ok(PyRep::Tuple(vec![
        PyRep::Bool(any_deposited),
        PyRep::Dict(failed_to_move),
    ]))
}

/// Explicitly links a built, non-disrupted FW Shipcaster Beacon to its faction's War HQ Shipcaster,
/// enforcing EVE's maximum of 3 simultaneously linked beacons per empire faction.
pub fn link_fw_landing_pad(
    ctx: &ServerContext,
    pad_item_id: i64,
) -> Result<LandingPadRecord, BridgeRpcError> {
    let now_unix = ctx.now_unix();
    let (pad, war_hq_launcher) = with_misc_state(ctx, |st| {
        let Some(pad_idx) = st.landing_pads.iter().position(|p| p.item_id == pad_item_id) else {
            return Err(rpc_err("LandingPadNotFound", format!("Landing pad {pad_item_id} not found")));
        };
        if !st.landing_pads[pad_idx].is_built || st.landing_pads[pad_idx].is_disrupted {
            return Err(rpc_err(
                "LandingPadUnavailable",
                "Landing pad must be completed and undisrupted before linking",
            ));
        }
        let faction_id = st.landing_pads[pad_idx].faction_id;
        let Some(launcher_idx) = st.launchers.iter().position(|l| l.faction_id == faction_id) else {
            return Err(rpc_err("LauncherNotFound", "No shipcaster launcher registered for faction"));
        };
        if !st.launchers[launcher_idx].linked_pad_ids.contains(&pad_item_id)
            && st.launchers[launcher_idx].linked_pad_ids.len() >= MAX_LINKED_LANDING_PADS_PER_FACTION
        {
            return Err(rpc_err(
                "MaxLinkedLandingPadsReached",
                "An Empire Shipcaster may link at most 3 simultaneous landing pads",
            ));
        }
        st.landing_pads[pad_idx].link_timestamp = now_unix.max(1) as i64;
        if !st.launchers[launcher_idx].linked_pad_ids.contains(&pad_item_id) {
            st.launchers[launcher_idx].linked_pad_ids.push(pad_item_id);
        }
        Ok((
            st.landing_pads[pad_idx].clone(),
            (
                st.launchers[launcher_idx].solar_system_id,
                st.launchers[launcher_idx].item_id,
            ),
        ))
    })?;
    persist_misc_state(ctx);
    emit_shipcaster_target_state_changed(ctx, pad.solar_system_id, pad.item_id);
    emit_shipcaster_target_state_changed(ctx, war_hq_launcher.0, war_hq_launcher.1);
    Ok(pad)
}

/// Disrupts or destroys an FW Shipcaster Beacon (e.g. from opposing militia attack), severing its
/// link to the Empire War HQ Shipcaster and emitting `OnShipcasterTargetStateChanged(itemID)`.
pub async fn disrupt_or_destroy_fw_landing_pad(
    ctx: &ServerContext,
    pad_item_id: i64,
    destroy: bool,
) -> Result<LandingPadRecord, BridgeRpcError> {
    let (pad, launcher_opt) = with_misc_state(ctx, |st| {
        let Some(pad_idx) = st.landing_pads.iter().position(|p| p.item_id == pad_item_id) else {
            return Err(rpc_err("LandingPadNotFound", format!("Landing pad {pad_item_id} not found")));
        };
        st.landing_pads[pad_idx].is_disrupted = true;
        st.landing_pads[pad_idx].link_timestamp = 0;
        let faction_id = st.landing_pads[pad_idx].faction_id;
        let mut launcher_info = None;
        if let Some(launcher) = st.launchers.iter_mut().find(|l| l.faction_id == faction_id) {
            launcher.linked_pad_ids.retain(|&id| id != pad_item_id);
            launcher_info = Some((launcher.solar_system_id, launcher.item_id));
        }
        let record = if destroy {
            st.landing_pads.remove(pad_idx)
        } else {
            st.landing_pads[pad_idx].clone()
        };
        Ok((record, launcher_info))
    })?;
    persist_misc_state(ctx);
    if destroy {
        let sys = ctx.systems.get_or_spawn(pad.solar_system_id);
        let _ = sys.remove_ball(pad.item_id).await;
    }
    emit_shipcaster_target_state_changed(ctx, pad.solar_system_id, pad.item_id);
    if let Some((war_hq_sys, launcher_id)) = launcher_opt {
        emit_shipcaster_target_state_changed(ctx, war_hq_sys, launcher_id);
    }
    Ok(pad)
}

#[path = "shipcaster_ops_starter.rs"]
mod shipcaster_ops_starter;
pub use shipcaster_ops_starter::{
    apply_shipcaster_arrival_fx, apply_shipcaster_beacon_alignment_and_spool,
    calculate_shipcaster_arrival_position, calculate_shipcaster_launch_vector,
    check_proximity, check_starter_shipcaster_eligibility, execute_shipcaster_jump,
    on_landing_pad_ball_destroyed, on_landing_pad_ball_destroyed_core,
};

/// Dispatches Shipcaster, Landing Pad Construction, and Starter Shipcaster RPC calls.
pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // parity: rpc:shipCasterLandingPadMgr.GetAllLandingPads
        ("shipCasterLandingPadMgr", "GetAllLandingPads") => {
            handle_get_all_landing_pads(ctx, session)
        }

        // parity: rpc:shipCasterLandingPadMgr.GetFactionLandingPads
        ("shipCasterLandingPadMgr", "GetFactionLandingPads") => {
            handle_get_faction_landing_pads(ctx, session, args)
        }

        // parity: rpc:shipCasterLauncherMgr.GetFactionsWithShipcaster
        ("shipCasterLauncherMgr", "GetFactionsWithShipcaster") => {
            handle_get_factions_with_shipcaster(ctx, session)
        }

        // parity: rpc:starterShipcasterTravelMgr.CanCharacterJump
        ("starterShipcasterTravelMgr", "CanCharacterJump") => {
            handle_can_character_jump(ctx, session)
        }

        // parity: rpc:starterShipcasterTravelMgr.CmdJumpThroughStarterShipcaster
        ("starterShipcasterTravelMgr", "CmdJumpThroughStarterShipcaster") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(rpc_err(
                    "CantDoThatWhileDocked",
                    "Cannot jump shipcaster while docked",
                )));
            }
            let Some(ship_id) = session.ship_id else {
                return Some(Err(rpc_err(
                    "CustomError",
                    "CmdJumpThroughStarterShipcaster requires an active ship",
                )));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(rpc_err(
                    "CustomError",
                    "CmdJumpThroughStarterShipcaster requires an active solar system",
                )));
            };

            let target_arg = match args.first().and_then(|a| a.as_i64()) {
                Some(id) if id > 0 => id,
                Some(id) => {
                    return Some(Err(rpc_err("TargetNotFound", format!("Invalid caster ID {id}"))))
                }
                None => {
                    return Some(Err(rpc_err(
                        "InvalidArguments",
                        "CmdJumpThroughStarterShipcaster requires shipcasterID",
                    )))
                }
            };

            let (starter_launcher, dest_sys) = match with_misc_state(ctx, |st| {
                if let Some(l) = st
                    .starter_launchers
                    .iter()
                    .find(|l| l.item_id == target_arg)
                    .cloned()
                {
                    let dest = l.staging_system_id;
                    Some(Ok((l, dest)))
                } else if let Some(l) = st
                    .starter_launchers
                    .iter()
                    .find(|l| l.solar_system_id == sys_id)
                    .cloned()
                {
                    if (30_000_000..40_000_000).contains(&target_arg)
                        && target_arg as u32 != l.staging_system_id
                    {
                        return Some(Err(rpc_err(
                            "InvalidDestinationSystem",
                            "Destination system does not match starter shipcaster War HQ system",
                        )));
                    }
                    let dest = l.staging_system_id;
                    Some(Ok((l, dest)))
                } else {
                    None
                }
            }) {
                Some(Ok(pair)) => pair,
                Some(Err(e)) => return Some(Err(e)),
                None => {
                    return Some(Err(rpc_err(
                        "LauncherNotFound",
                        format!("Starter shipcaster {target_arg} not registered"),
                    )))
                }
            };

            if starter_launcher.solar_system_id != sys_id {
                return Some(Err(rpc_err(
                    "InvalidSolarSystem",
                    format!(
                        "Starter shipcaster {} is located in system {}, not current system {sys_id}",
                        starter_launcher.item_id, starter_launcher.solar_system_id
                    ),
                )));
            }

            if let Err(e) = check_starter_shipcaster_eligibility(ctx, session, char_id) {
                return Some(Err(e));
            }

            let sys = ctx.systems.get_or_spawn(sys_id);
            if let Err(e) = check_proximity(
                &sys,
                ship_id,
                starter_launcher.item_id,
                SHIPCASTER_MAX_RANGE_M,
                "starter shipcaster",
            )
            .await
            {
                return Some(Err(e));
            }

            let starter_station_pos = ctx
                .sde
                .stations
                .get(&starter_launcher.staging_station_id)
                .map(|st| st.position)
                .unwrap_or_else(|| DVec3::ZERO);
            let launch_vector = calculate_shipcaster_launch_vector(
                &ctx.sde,
                sys_id,
                starter_launcher.position,
                dest_sys,
                starter_station_pos,
            );
            let arrival_pos =
                calculate_shipcaster_arrival_position(starter_station_pos, launch_vector, 2_500.0);

            if let Err(e) = shipcaster_ops_starter::prevalidate_shipcaster_jump(
                ctx, char_id, ship_id, sys_id,
            )
            .await
            {
                return Some(Err(e));
            }

            let _ = apply_shipcaster_beacon_alignment_and_spool(
                &sys,
                ship_id,
                starter_launcher.item_id,
                launch_vector,
            )
            .await;

            if let Err(e) = execute_shipcaster_jump(
                ctx,
                session,
                char_id,
                ship_id,
                sys_id,
                dest_sys,
                arrival_pos,
                launch_vector,
            )
            .await
            {
                return Some(Err(e));
            }

            with_misc_state(ctx, |st| {
                st.used_starter_shipcaster_chars.insert(char_id);
            });
            persist_misc_state(ctx);

            Some(Ok(PyRep::None))
        }

        // parity: rpc:shipcasterTravelMgr.CmdJumpThroughShipcaster
        ("shipcasterTravelMgr", "CmdJumpThroughShipcaster") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(rpc_err(
                    "CantDoThatWhileDocked",
                    "Cannot jump shipcaster while docked",
                )));
            }
            let Some(ship_id) = session.ship_id else {
                return Some(Err(rpc_err(
                    "CustomError",
                    "CmdJumpThroughShipcaster requires an active ship",
                )));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(rpc_err(
                    "CustomError",
                    "CmdJumpThroughShipcaster requires an active solar system",
                )));
            };

            let shipcaster_id = match args.first().and_then(|a| a.as_i64()) {
                Some(id) if id > 0 => id,
                Some(id) => {
                    return Some(Err(rpc_err("TargetNotFound", format!("Invalid shipcasterID {id}"))))
                }
                None => {
                    return Some(Err(rpc_err(
                        "InvalidArguments",
                        "CmdJumpThroughShipcaster requires (shipcasterID, toSolarsystemID, toLandingPadID)",
                    )))
                }
            };
            let to_system_arg = match args.get(1).and_then(|a| a.as_i64()) {
                Some(id) if id > 0 => id,
                Some(id) => {
                    return Some(Err(rpc_err(
                        "TargetNotFound",
                        format!("Invalid toSolarsystemID {id}"),
                    )))
                }
                None => {
                    return Some(Err(rpc_err(
                        "InvalidArguments",
                        "CmdJumpThroughShipcaster requires toSolarsystemID",
                    )))
                }
            };
            let to_pad_id = match args.get(2).and_then(|a| a.as_i64()) {
                Some(id) if id > 0 => id,
                Some(id) => {
                    return Some(Err(rpc_err(
                        "TargetNotFound",
                        format!("Invalid toLandingPadID {id}"),
                    )))
                }
                None => {
                    return Some(Err(rpc_err(
                        "InvalidArguments",
                        "CmdJumpThroughShipcaster requires toLandingPadID",
                    )))
                }
            };

            let (launchers, pads) =
                with_misc_state(ctx, |st| (st.launchers.clone(), st.landing_pads.clone()));

            let Some(launcher) = launchers.iter().find(|l| l.item_id == shipcaster_id).cloned() else {
                return Some(Err(rpc_err(
                    "LauncherNotFound",
                    format!("Shipcaster launcher {shipcaster_id} not registered"),
                )));
            };
            let to_system_id = u32::try_from(to_system_arg).unwrap_or(0);
            let Some(pad) = pads.iter().find(|p| p.item_id == to_pad_id).cloned() else {
                return Some(Err(rpc_err(
                    "LandingPadNotFound",
                    format!("Landing pad {to_pad_id} not registered"),
                )));
            };

            let ship_type_id = ctx
                .db
                .with_snapshot(|s| s.items.get(&ship_id).map(|i| i.type_id))
                .unwrap_or(0);
            let ship_group_id = ctx.sde.types.get(&ship_type_id).map(|t| t.group_id).unwrap_or(0);
            if is_capital_or_freighter_group(ship_group_id) {
                return Some(Err(rpc_err(
                    "CapitalShipCannotJumpShipcaster",
                    "Capital ships and freighters cannot jump through an interstellar shipcaster",
                )));
            }

            let char_war_faction = ctx
                .db
                .with_snapshot(|s| s.characters.get(&char_id).and_then(|c| c.war_faction_id));
            if launcher.solar_system_id == 30_100_000 {
                if !matches!(char_war_faction, Some(500010 | 500011)) {
                    return Some(Err(rpc_err(
                        "NonPirateRefused",
                        "The Deathless Shipcaster in Zarzakh refuses non-pirate characters",
                    )));
                }
                if pad.faction_id > 0 && char_war_faction != Some(pad.faction_id as i64) {
                    return Some(Err(rpc_err(
                        "FobFactionMismatch",
                        "Cannot jump to another pirate faction's Forward Operating Base",
                    )));
                }
            } else if to_system_id == 30_100_000 && launcher.faction_id == 0 {
                // Deathless Shipcaster beacons into Zarzakh refuse Empire militia characters (`feat:navigation.deathless-shipcaster-zarzakh`)
                if matches!(char_war_faction, Some(500_001..=500_004)) {
                    return Some(Err(rpc_err(
                        "EmpireMilitiaRefused",
                        "The Deathless Shipcaster refuses characters enlisted in an Empire militia",
                    )));
                }
            } else if launcher.faction_id > 0 && char_war_faction != Some(launcher.faction_id as i64) {
                return Some(Err(rpc_err(
                    "FactionShipcasterAccessDenied",
                    "Character militia faction does not match shipcaster faction",
                )));
            }

            if (launcher.solar_system_id == 30_100_000 || to_system_id == 30_100_000)
                && crate::services::cluster_macho_misc_services::is_zarzakh_horizon_siege_active(ctx)
            {
                return Some(Err(rpc_err(
                    "HorizonSiegeActive",
                    "Shipcaster travel to/from Zarzakh is disabled during active Horizon Siege",
                )));
            }

            // Per-target charge, cooldown, and SDE stargate insurgency warzone connectivity (`feat:navigation.deathless-shipcaster-fob`)
            if matches!(pad.faction_id, 500010 | 500011) {
                let now_u = ctx.now_unix();
                let campaign_systems: Vec<u32> = crate::services::insurgency_services::read_insurgency_runtime(ctx, |rt| {
                    rt.campaigns
                        .values()
                        .filter(|c| c.pirate_faction_id == i64::from(pad.faction_id) && c.winddown_end_unix > now_u)
                        .flat_map(|c| c.systems.iter().copied())
                        .collect()
                });
                if !campaign_systems.is_empty() {
                    let is_stargate_connected = campaign_systems.contains(&to_system_id)
                        || ctx.sde.jumps.values().any(|j| {
                            j.from_solar_system_id == to_system_id && campaign_systems.contains(&j.to_solar_system_id)
                        });
                    if !is_stargate_connected {
                        return Some(Err(rpc_err(
                            "InsurgencyWarzoneNotConnected",
                            format!("Destination system {to_system_id} is not connected via SDE stargate topology to active pirate insurgency systems"),
                        )));
                    }
                }
                let (on_cooldown, out_of_charges) = with_misc_state(ctx, |st| {
                    let now = ctx.now_unix();
                    let cd_exp = st.target_cooldowns.get(&pad.item_id).copied().unwrap_or(0);
                    let cd_active = cd_exp > now;
                    let charges = if !cd_active && cd_exp > 0 {
                        1
                    } else {
                        st.target_charges.get(&pad.item_id).copied().unwrap_or(1)
                    };
                    (cd_active, charges == 0)
                });
                if on_cooldown {
                    return Some(Err(rpc_err(
                        "ShipcasterTargetOnCooldown",
                        format!("Shipcaster target {} is currently on cooldown", pad.item_id),
                    )));
                }
                if out_of_charges {
                    return Some(Err(rpc_err(
                        "ShipcasterTargetNoCharges",
                        format!("Shipcaster target {} has no remaining charges", pad.item_id),
                    )));
                }
            }

            if launcher.solar_system_id != sys_id {
                return Some(Err(rpc_err(
                    "InvalidSolarSystem",
                    format!(
                        "Shipcaster launcher {} is located in War HQ system {}, not current system {sys_id}",
                        launcher.item_id, launcher.solar_system_id
                    ),
                )));
            }
            if !pad.is_built || pad.is_disrupted {
                return Some(Err(rpc_err(
                    "LandingPadUnavailable",
                    format!("Landing pad {} is not built or is disrupted", pad.item_id),
                )));
            }
            if !launcher.linked_pad_ids.contains(&pad.item_id) || pad.link_timestamp <= 0 {
                return Some(Err(rpc_err(
                    "LandingPadNotLinked",
                    format!(
                        "Landing pad {} is not currently linked to launcher {}",
                        pad.item_id, launcher.item_id
                    ),
                )));
            }
            if pad.solar_system_id != to_system_id {
                return Some(Err(rpc_err(
                    "InvalidDestinationSystem",
                    format!(
                        "Landing pad {} is located in system {}, not specified destination {to_system_id}",
                        pad.item_id, pad.solar_system_id
                    ),
                )));
            }

            let now = ctx.now_unix();
            if matches!(launcher.faction_id, 500001..=500004) {
                let Some(active_pad) = active_linked_pad_for_launcher(&launcher, &pads, now) else {
                    return Some(Err(rpc_err(
                        "LandingPadNotLinked",
                        "No active linked landing pad available on shipcaster",
                    )));
                };
                if active_pad.item_id != pad.item_id {
                    return Some(Err(rpc_err(
                        "ShipcasterTargetNotActiveInCycle",
                        format!(
                            "Shipcaster is currently locked onto beacon {} during this 6-minute cycle",
                            active_pad.item_id
                        ),
                    )));
                }
            }

            {
                let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                if cw.get_or_create(char_id).is_weapon_timer_active(now) {
                    return Some(Err(rpc_err(
                        "WeaponsTimerActive",
                        "Cannot jump through shipcaster while weapons timer is active",
                    )));
                }
            }

            let sys = ctx.systems.get_or_spawn(sys_id);
            if let Err(e) = check_proximity(
                &sys,
                ship_id,
                launcher.item_id,
                SHIPCASTER_MAX_RANGE_M,
                "shipcaster launcher",
            )
            .await
            {
                return Some(Err(e));
            }

            let launch_vector = calculate_shipcaster_launch_vector(
                &ctx.sde,
                sys_id,
                launcher.position,
                to_system_id,
                pad.position,
            );
            let arrival_pos =
                calculate_shipcaster_arrival_position(pad.position, launch_vector, 2_500.0);

            if let Err(e) = shipcaster_ops_starter::prevalidate_shipcaster_jump(
                ctx, char_id, ship_id, sys_id,
            )
            .await
            {
                return Some(Err(e));
            }

            let _ = apply_shipcaster_beacon_alignment_and_spool(
                &sys,
                ship_id,
                launcher.item_id,
                launch_vector,
            )
            .await;

            if let Err(e) = execute_shipcaster_jump(
                ctx,
                session,
                char_id,
                ship_id,
                sys_id,
                to_system_id,
                arrival_pos,
                launch_vector,
            )
            .await
            {
                return Some(Err(e));
            }

            if matches!(pad.faction_id, 500010 | 500011) {
                let _ =
                    crate::services::cluster_macho_misc_services::check_and_apply_shipcaster_target_cooldown(
                        ctx,
                        pad.item_id,
                        120,
                    );
            }

            Some(Ok(PyRep::None))
        }

        _ => None,
    }
}

fn handle_get_all_landing_pads(
    ctx: &ServerContext,
    session: &SessionState,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if let Err(e) = ServerContext::require_auth(session) {
        return Some(Err(e));
    }
    let pads = with_misc_state(ctx, |st| {
        st.landing_pads
            .iter()
            .filter(|p| matches!(p.faction_id, 500001..=500004))
            .map(|p| p.to_keyval())
            .collect::<Vec<_>>()
    });
    Some(Ok(PyRep::List(pads)))
}

fn handle_get_faction_landing_pads(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    if let Err(e) = ServerContext::require_auth(session) {
        return Some(Err(e));
    }
    let faction_id = match args.first().and_then(|a| a.as_i64()) {
        Some(fid) if fid > 0 => match i32::try_from(fid) {
            Ok(v) => v,
            Err(_) => {
                return Some(Err(rpc_err(
                    "InvalidArguments",
                    "GetFactionLandingPads factionID out of range",
                )))
            }
        },
        Some(_) => {
            return Some(Err(rpc_err(
                "InvalidArguments",
                "GetFactionLandingPads requires positive factionID",
            )))
        }
        None => {
            return Some(Err(rpc_err(
                "InvalidArguments",
                "GetFactionLandingPads requires factionID",
            )))
        }
    };
    let pads = with_misc_state(ctx, |st| {
        st.landing_pads
            .iter()
            .filter(|p| p.faction_id == faction_id)
            .map(|p| p.to_keyval())
            .collect::<Vec<_>>()
    });
    Some(Ok(PyRep::List(pads)))
}

fn handle_get_factions_with_shipcaster(
    ctx: &ServerContext,
    session: &SessionState,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if let Err(e) = ServerContext::require_auth(session) {
        return Some(Err(e));
    }
    let factions = with_misc_state(ctx, |st| {
        let mut set = BTreeSet::new();
        for l in &st.launchers {
            if matches!(l.faction_id, 500001..=500004) {
                set.insert(l.faction_id);
            }
        }
        set.into_iter()
            .map(|f| PyRep::Integer(f as i64))
            .collect::<Vec<_>>()
    });
    Some(Ok(PyRep::List(factions)))
}

fn handle_can_character_jump(
    ctx: &ServerContext,
    session: &SessionState,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let eligible = check_starter_shipcaster_eligibility(ctx, session, char_id).is_ok();
    Some(Ok(PyRep::Bool(eligible)))
}

#[cfg(test)] #[path = "shipcaster_ops_tests.rs"] mod shipcaster_ops_tests;

#[cfg(test)]
#[path = "shipcaster_ops_edges_tests.rs"]
mod shipcaster_ops_edges_tests;


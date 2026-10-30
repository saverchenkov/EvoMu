//! `pve_crab_beacons` — CONCORD Rogue Analysis Beacon (CRAB) Deployment, Capital Ship Link,
//! Rogue Drone Waves, Deployer-Restricted Loot Window & Self-Destruct (`feat:pve.crab-link-activation`,
//! `feat:pve.crab-ceq-signal-interference`).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use crate::services::pve_special_sites::{
    advance_roaming_storms, ensure_special_sites_initialized, next_spec_seq, spec_now_sec,
    sync_metaliminal_storm_effects_and_sites, with_pve_special_reg, with_pve_special_reg_db,
    SystemInterferenceRecord,
};
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{SdeRegistry, SolarSystemId, TypeId};
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub const GROUP_CRAB_BEACON: u32 = 4137;
pub const TYPE_CRAB_BEACON: TypeId = 60_244;
pub const TYPE_CARRIER_CRAB_BEACON: TypeId = 92_183;

/// Capital ship link duration: 4 minutes (`240s`).
pub const CRAB_LINK_DURATION_SECS: u64 = 240;
/// Active scan duration after link completes: 10 minutes (`600s`).
pub const CRAB_SCAN_DURATION_SECS: u64 = 600;
/// Deployer-exclusive loot window after scan completes: 2 minutes (`120s`), followed by self-destruct.
pub const CRAB_LOOT_EXCLUSIVE_SECS: u64 = 120;
/// Idle unlinked beacon decay lifetime (`3,600s`).
pub const CRAB_BEACON_IDLE_LIFETIME_SECS: u64 = 3_600;
/// Maximum range to initiate a capital ship link (`15 km`).
pub const CRAB_LINK_RANGE_M: f64 = 15_000.0;
/// CEQ cost per CRAB activation (`30` out of `100` cap -> `3` activations).
pub const CRAB_CEQ_COST: i64 = 30;
/// Per-CRAB Signal Interference increase (`1/12` -> refused at 12 CRABs).
pub const CRAB_INTERFERENCE_DELTA: f64 = 1.0 / 12.0;

// V24 Rogue Drone CRAB wave NPCs (`group_id = 4037`)
const WAVE_1_DRONES: &[TypeId] = &[60_255, 60_316, 60_317];
const WAVE_2_DRONES: &[TypeId] = &[60_318, 60_331, 60_332, 60_505];
const WAVE_3_DRONES: &[TypeId] = &[60_333, 60_506, 60_517];

// V24 CRAB special loot items
const TYPE_ROGUE_DRONE_INFESTATION_DATA: TypeId = 60_459;
const TYPE_RADICAL_DRONE_DDA_MUTA: TypeId = 60_476;
const TYPE_RADICAL_FIGHTER_FSU_MUTA: TypeId = 60_477;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CrabBeaconPhase {
    Idle,
    Linking,
    Scanning,
    LootRestricted,
    SelfDestructed,
}

impl CrabBeaconPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Linking => "linking",
            Self::Scanning => "scanning",
            Self::LootRestricted => "loot_restricted",
            Self::SelfDestructed => "self_destructed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrabBeaconRecord {
    pub beacon_id: i64,
    pub type_id: TypeId,
    pub deployer_char_id: i64,
    pub solar_system_id: SolarSystemId,
    pub position: [f64; 3],
    pub deployed_at_sec: u64,
    pub phase: CrabBeaconPhase,
    pub linked_char_id: Option<i64>,
    pub linked_ship_id: Option<i64>,
    pub link_started_at_sec: u64,
    pub link_completes_at_sec: u64,
    pub scan_completes_at_sec: u64,
    pub self_destruct_at_sec: u64,
    pub waves_spawned: u8,
    pub wave_npc_ball_ids: Vec<i64>,
    pub orig_resonances: Option<([f64; 4], [f64; 4], [f64; 4])>,
    pub open_container_id: Option<i64>,
}

pub fn is_crab_beacon_type(sde: &SdeRegistry, type_id: TypeId) -> bool {
    matches!(type_id, TYPE_CRAB_BEACON | TYPE_CARRIER_CRAB_BEACON)
        || sde
            .types
            .get(&type_id)
            .is_some_and(|t| t.group_id == GROUP_CRAB_BEACON)
}

pub fn is_crab_beacon_item(ctx: &ServerContext, item_id: i64) -> bool {
    let in_snap = ctx.db.with_snapshot(|snap| {
        snap.items
            .get(&item_id)
            .is_some_and(|it| is_crab_beacon_type(&ctx.sde, it.type_id))
    });
    if in_snap {
        return true;
    }
    with_pve_special_reg(ctx, |reg| reg.crab_beacons.contains_key(&item_id))
}

/// Validates that `sys_id` is a known-space Low-Sec (`0.0 < sec < 0.45`) or Null-Sec (`sec <= 0.0`) system.
pub fn validate_crab_deployment_system(
    sde: &SdeRegistry,
    sys_id: SolarSystemId,
) -> Result<(), BridgeRpcError> {
    let Some(sys) = sde.solar_systems.get(&sys_id) else {
        return Err(BridgeRpcError::user_error("CantDeployInSystem"));
    };
    if sys_id >= 30_100_000
        || !matches!(sys.wormhole_class_id, None | Some(7..=9))
        || matches!(sys.region_id, 10_000_004 | 10_000_017 | 10_000_019)
    {
        return Err(BridgeRpcError::user_error("CantDeployInWormhole"));
    }
    if sys.security_status >= 0.45 {
        return Err(BridgeRpcError::user_error("CantDeployInHighSec"));
    }
    Ok(())
}

/// Checks if `ship_type_id` is an eligible capital hull for `beacon_type_id` per SDE type lists `300` / `946`.
pub fn is_eligible_crab_ship(sde: &SdeRegistry, beacon_type_id: TypeId, ship_type_id: TypeId) -> bool {
    let Some(ship_t) = sde.types.get(&ship_type_id) else {
        return false;
    };
    if ship_t.category_id != 6 {
        return false;
    }
    if beacon_type_id == TYPE_CARRIER_CRAB_BEACON {
        if let Some(tl) = sde.type_lists.get(&946) {
            return tl.included_group_ids.contains(&ship_t.group_id)
                || tl.included_type_ids.contains(&ship_type_id);
        }
        return matches!(ship_t.group_id, 547 | 5120);
    }
    if let Some(tl) = sde.type_lists.get(&300) {
        if tl.included_group_ids.contains(&ship_t.group_id)
            || tl.included_type_ids.contains(&ship_type_id)
        {
            return true;
        }
    }
    matches!(
        ship_t.group_id,
        30 | 485 | 547 | 659 | 883 | 4594 | 5120
    ) || ship_type_id == 11_019
}

/// Registers a newly deployed CRAB beacon in `PveSpecialSitesRegistry`.
pub fn register_crab_beacon(
    ctx: &ServerContext,
    beacon_id: i64,
    type_id: TypeId,
    deployer_char_id: i64,
    solar_system_id: SolarSystemId,
    position: DVec3,
) {
    let now = ctx.now_unix();
    with_pve_special_reg(ctx, |reg| {
        reg.crab_beacons.insert(
            beacon_id,
            CrabBeaconRecord {
                beacon_id,
                type_id,
                deployer_char_id,
                solar_system_id,
                position: [position.x, position.y, position.z],
                deployed_at_sec: now,
                phase: CrabBeaconPhase::Idle,
                linked_char_id: None,
                linked_ship_id: None,
                link_started_at_sec: 0,
                link_completes_at_sec: 0,
                scan_completes_at_sec: 0,
                self_destruct_at_sec: 0,
                waves_spawned: 0,
                wave_npc_ball_ids: Vec::new(),
                orig_resonances: None,
                open_container_id: None,
            },
        );
    });
}

/// Returns `true` while `ship_id` has an active 4-minute CRAB beacon link (disallowing warp, cloak, and tether).
pub fn is_ship_crab_linked(ctx: &ServerContext, ship_id: i64, now_unix: u64) -> bool {
    let effective_now = now_unix.max(ctx.now_unix());
    with_pve_special_reg(ctx, |reg| {
        let now = effective_now.max(spec_now_sec(reg));
        reg.crab_beacons.values().any(|b| {
            b.phase == CrabBeaconPhase::Linking
                && b.linked_ship_id == Some(ship_id)
                && now < b.link_completes_at_sec
        })
    })
}

/// Checks whether `char_id` may view or take loot from `container_id` if it is a CRAB beacon.
/// Returns `Ok(None)` if `container_id` is not a CRAB beacon, `Ok(Some(true))` if allowed,
/// or `Err(...)` if the scan is still in progress or the 2-minute deployer restriction is active.
pub fn check_crab_loot_access(
    ctx: &ServerContext,
    container_id: i64,
    char_id: i64,
    is_gm: bool,
) -> Result<Option<bool>, BridgeRpcError> {
    if !is_crab_beacon_item(ctx, container_id) {
        return Ok(None);
    }
    tick_all_crab_beacons(ctx);
    let now = ctx.now_unix();
    let rec_opt = with_pve_special_reg(ctx, |reg| reg.crab_beacons.get(&container_id).cloned());
    let Some(rec) = rec_opt else {
        return Err(BridgeRpcError::user_error("CrabScanInProgress"));
    };
    match rec.phase {
        CrabBeaconPhase::Idle | CrabBeaconPhase::Linking | CrabBeaconPhase::Scanning => {
            Err(BridgeRpcError::user_error("CrabScanInProgress"))
        }
        CrabBeaconPhase::LootRestricted if now < rec.self_destruct_at_sec => {
            if is_gm || char_id == rec.deployer_char_id {
                Ok(Some(true))
            } else {
                Err(BridgeRpcError::user_error("LootOwnershipRestricted"))
            }
        }
        CrabBeaconPhase::LootRestricted | CrabBeaconPhase::SelfDestructed => {
            Err(BridgeRpcError::user_error("ItemNotFound"))
        }
    }
}

fn apply_crab_link_ship_effects(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    beacon_id: i64,
    char_id: i64,
    ship_id: i64,
    expires_at_unix: u64,
) -> Option<([f64; 4], [f64; 4], [f64; 4])> {
    {
        let mut st = crate::services::inventory_fitting::fitting_scanners_superweapons::scanners_superweapons_state()
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let list = st.ship_dbuffs.entry((ctx.instance_id, ship_id)).or_default();
        list.retain(|e| e.source_module_id != beacon_id);
        for &(dbuff_id, val) in &[(4u32, 1.0f64), (5, 1.0), (57, 1.0), (2141, -10.0)] {
            list.push(crate::services::inventory_fitting::fitting_scanners_superweapons::ActiveDbuffEntry {
                dbuff_id,
                value: val,
                expires_at_unix,
                source_module_id: beacon_id,
            });
        }
    }
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut orig_res = None;
    sys_handle.try_update_combat_state(ship_id, |cs| {
        orig_res = Some((cs.shield_resonance, cs.armor_resonance, cs.structure_resonance));
        for i in 0..4 {
            cs.shield_resonance[i] *= 0.90;
            cs.armor_resonance[i] *= 0.90;
            cs.structure_resonance[i] *= 0.90;
        }
        cs.is_tethered = false;
    });
    ctx.invalidate_ship_fitting(ship_id);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnDbuffUpdated",
        "charid",
        vec![PyRep::Integer(ship_id)],
    );
    orig_res
}

fn clear_crab_link_ship_effects(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    beacon_id: i64,
    char_id_opt: Option<i64>,
    ship_id_opt: Option<i64>,
    orig_resonances: Option<([f64; 4], [f64; 4], [f64; 4])>,
) {
    let Some(ship_id) = ship_id_opt else { return };
    {
        let mut st = crate::services::inventory_fitting::fitting_scanners_superweapons::scanners_superweapons_state()
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(list) = st.ship_dbuffs.get_mut(&(ctx.instance_id, ship_id)) {
            list.retain(|e| e.source_module_id != beacon_id);
        }
    }
    if let Some((sr, ar, hr)) = orig_resonances {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        sys_handle.try_update_combat_state(ship_id, |cs| {
            cs.shield_resonance = sr;
            cs.armor_resonance = ar;
            cs.structure_resonance = hr;
        });
    }
    ctx.invalidate_ship_fitting(ship_id);
    if let Some(cid) = char_id_opt {
        ctx.notify(
            NotificationTarget::Character(cid),
            "OnDbuffUpdated",
            "charid",
            vec![PyRep::Integer(ship_id)],
        );
    }
}

fn build_rogue_drone_combat(sde: &SdeRegistry, type_id: TypeId) -> CombatState {
    if let Some(t) = sde.types.get(&type_id) {
        let a = &t.default_attributes;
        let sh = a.get(&263).copied().unwrap_or(1_200.0).max(300.0);
        let ar = a.get(&265).copied().unwrap_or(1_200.0).max(300.0);
        let st = a.get(&9).copied().unwrap_or(1_000.0).max(300.0);
        return CombatState {
            shield_hp: sh,
            max_shield_hp: sh,
            armor_hp: ar,
            max_armor_hp: ar,
            structure_hp: st,
            max_structure_hp: st,
            shield_resonance: [
                a.get(&271).copied().unwrap_or(1.0).max(0.25),
                a.get(&274).copied().unwrap_or(1.0).max(0.25),
                a.get(&273).copied().unwrap_or(1.0).max(0.25),
                a.get(&272).copied().unwrap_or(1.0).max(0.25),
            ],
            armor_resonance: [
                a.get(&267).copied().unwrap_or(1.0).max(0.25),
                a.get(&270).copied().unwrap_or(1.0).max(0.25),
                a.get(&269).copied().unwrap_or(1.0).max(0.25),
                a.get(&268).copied().unwrap_or(1.0).max(0.25),
            ],
            structure_resonance: [
                a.get(&109).copied().unwrap_or(1.0).max(0.25),
                a.get(&113).copied().unwrap_or(1.0).max(0.25),
                a.get(&111).copied().unwrap_or(1.0).max(0.25),
                a.get(&110).copied().unwrap_or(1.0).max(0.25),
            ],
            ..CombatState::default()
        };
    }
    CombatState {
        shield_hp: 1_500.0,
        max_shield_hp: 1_500.0,
        armor_hp: 1_500.0,
        max_armor_hp: 1_500.0,
        structure_hp: 1_200.0,
        max_structure_hp: 1_200.0,
        ..CombatState::default()
    }
}

fn spawn_crab_rogue_drone_wave(ctx: &ServerContext, beacon_id: i64, wave_num: u8) {
    let spawned_info = with_pve_special_reg(ctx, |reg| {
        let drone_types = match wave_num {
            1 => WAVE_1_DRONES,
            2 => WAVE_2_DRONES,
            _ => WAVE_3_DRONES,
        };
        let mut allocated = Vec::with_capacity(drone_types.len());
        for &tid in drone_types {
            let seq = next_spec_seq(reg);
            allocated.push((1_897_000_000 + seq, tid));
        }
        let Some(rec) = reg.crab_beacons.get_mut(&beacon_id) else {
            return None;
        };
        if rec.phase != CrabBeaconPhase::Scanning || rec.waves_spawned >= wave_num {
            return None;
        }
        rec.waves_spawned = wave_num;
        for &(bid, _) in &allocated {
            rec.wave_npc_ball_ids.push(bid);
        }
        Some((rec.solar_system_id, DVec3::from_array(rec.position), allocated))
    });

    let Some((sys_id, beacon_pos, allocated)) = spawned_info else {
        return;
    };
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (idx, (bid, tid)) in allocated.into_iter().enumerate() {
        ctx.systems.claim_special_site_ball(sys_id, bid);
        let angle = (idx as f64) * 1.4 + f64::from(wave_num);
        let offset = DVec3::new(angle.cos() * 4_500.0, 250.0 * f64::from(wave_num), angle.sin() * 4_500.0);
        let radius = ctx.sde.types.get(&tid).map(|t| t.radius.max(35.0)).unwrap_or(45.0);
        let max_vel = ctx
            .sde
            .types
            .get(&tid)
            .and_then(|t| t.default_attributes.get(&37).copied())
            .unwrap_or(280.0)
            .max(120.0);
        let cs = build_rogue_drone_combat(&ctx.sde, tid);
        sys_handle.try_spawn_ball_with_combat(
            Ball {
                id: bid,
                type_id: tid,
                position: beacon_pos + offset,
                velocity: DVec3::ZERO,
                max_velocity: max_vel,
                inertia: 1.0,
                mass: 15_000.0,
                radius,
                speed_fraction: 0.0,
                mode: MovementMode::Orbit {
                    target_id: beacon_id,
                    range: 4_500.0,
                },
            },
            cs,
        );
    }
}

fn finalize_crab_scan_success(ctx: &ServerContext, beacon_id: i64) {
    let info = with_pve_special_reg(ctx, |reg| {
        let mut loot_ids = Vec::with_capacity(3);
        for _ in 0..3 {
            let seq = next_spec_seq(reg);
            loot_ids.push(1_897_500_000 + seq);
        }
        let rec = reg.crab_beacons.get_mut(&beacon_id)?;
        if rec.phase != CrabBeaconPhase::Scanning {
            return None;
        }
        rec.phase = CrabBeaconPhase::LootRestricted;
        let drones_to_clear = std::mem::take(&mut rec.wave_npc_ball_ids);
        let data_qty = if rec.type_id == TYPE_CARRIER_CRAB_BEACON { 10 } else { 15 };
        Some((
            rec.solar_system_id,
            rec.deployer_char_id,
            rec.self_destruct_at_sec,
            drones_to_clear,
            loot_ids,
            data_qty,
        ))
    });

    let Some((sys_id, deployer_char_id, self_destruct_at_sec, drones_to_clear, loot_ids, data_qty)) =
        info
    else {
        return;
    };

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for drone_bid in drones_to_clear {
        sys_handle.try_remove_ball(drone_bid);
    }

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(beacon_item) = snap.items.get_mut(&beacon_id) {
            beacon_item.flag = InventoryFlag::None;
            beacon_item.version += 1;
        }
        let rewards = [
            (loot_ids[0], TYPE_ROGUE_DRONE_INFESTATION_DATA, data_qty),
            (loot_ids[1], TYPE_RADICAL_DRONE_DDA_MUTA, 1),
            (loot_ids[2], TYPE_RADICAL_FIGHTER_FSU_MUTA, 1),
        ];
        for (iid, tid, qty) in rewards {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: deployer_char_id,
                    location_id: beacon_id,
                    flag: InventoryFlag::Cargo,
                    quantity: qty,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnSlimItemChange",
        "solarsystemid",
        vec![
            PyRep::Integer(beacon_id),
            PyRep::keyval([
                ("crabPhase", PyRep::String("LootRestricted".into())),
                ("crabSelfDestructAt", PyRep::Integer(self_destruct_at_sec as i64)),
            ]),
        ],
    );
}

fn self_destruct_crab_beacon(ctx: &ServerContext, beacon_id: i64) {
    let now = ctx.now_unix();
    let info = with_pve_special_reg(ctx, |reg| {
        let rec = reg.crab_beacons.get_mut(&beacon_id)?;
        if rec.phase == CrabBeaconPhase::SelfDestructed {
            return None;
        }
        let prev_phase = rec.phase;
        rec.phase = CrabBeaconPhase::SelfDestructed;
        let drones_to_clear = std::mem::take(&mut rec.wave_npc_ball_ids);
        Some((
            prev_phase,
            rec.solar_system_id,
            rec.deployer_char_id,
            rec.linked_char_id,
            rec.linked_ship_id,
            rec.orig_resonances,
            rec.position,
            drones_to_clear,
        ))
    });

    let Some((
        prev_phase,
        sys_id,
        _deployer_char_id,
        linked_char_id,
        linked_ship_id,
        orig_resonances,
        pos_arr,
        drones_to_clear,
    )) = info
    else {
        return;
    };

    if prev_phase == CrabBeaconPhase::Linking {
        clear_crab_link_ship_effects(
            ctx,
            sys_id,
            beacon_id,
            linked_char_id,
            linked_ship_id,
            orig_resonances,
        );
    }

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for drone_bid in drones_to_clear {
        sys_handle.try_remove_ball(drone_bid);
    }
    sys_handle.try_remove_ball(beacon_id);

    let spawned_can_id = ctx.db.with_mut_snapshot(|snap| {
        let remaining_loot: Vec<i64> = snap
            .items
            .values()
            .filter(|i| i.location_id == beacon_id && i.quantity > 0)
            .map(|i| i.item_id)
            .collect();
        let can_id_opt = if !remaining_loot.is_empty() {
            snap.next_entity_id += 1;
            let can_id = snap.next_entity_id;
            snap.items.insert(
                can_id,
                InventoryItemRecord {
                    item_id: can_id,
                    type_id: 23, // Open-to-all Cargo Container
                    owner_id: 0,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.item_positions.insert(can_id, pos_arr);
            crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(snap, can_id, now, true);
            for iid in remaining_loot {
                if let Some(it) = snap.items.get_mut(&iid) {
                    it.location_id = can_id;
                    it.flag = InventoryFlag::Cargo;
                    it.owner_id = 0;
                    it.version += 1;
                }
            }
            Some(can_id)
        } else {
            None
        };
        snap.items.remove(&beacon_id);
        snap.item_positions.remove(&beacon_id);
        snap.sp6_lifecycle.space_object_meta.remove(&beacon_id);
        crate::services::mobile_deployables::write_deployable_state_snap(snap, |st| {
            st.deployables.remove(&beacon_id);
        });
        can_id_opt
    });

    if let Some(can_id) = spawned_can_id {
        with_pve_special_reg(ctx, |reg| {
            if let Some(rec) = reg.crab_beacons.get_mut(&beacon_id) {
                rec.open_container_id = Some(can_id);
            }
        });
        let can_cs = CombatState {
            shield_hp: 0.0,
            max_shield_hp: 0.0,
            armor_hp: 0.0,
            max_armor_hp: 0.0,
            structure_hp: 500.0,
            max_structure_hp: 500.0,
            ..CombatState::default()
        };
        sys_handle.try_spawn_ball_with_combat(
            Ball {
                id: can_id,
                type_id: 23,
                position: DVec3::from_array(pos_arr),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000.0,
                radius: 25.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            can_cs,
        );
    }

    ctx.notify(
        NotificationTarget::SolarSystem(sys_id),
        "OnSlimItemChange",
        "solarsystemid",
        vec![
            PyRep::Integer(beacon_id),
            PyRep::keyval([
                ("crabPhase", PyRep::String("SelfDestructed".into())),
                ("openContainerID", PyRep::Integer(spawned_can_id.unwrap_or(0))),
            ]),
        ],
    );
}

fn advance_single_crab_beacon(ctx: &ServerContext, beacon_id: i64, now: u64) {
    let snap_rec = with_pve_special_reg(ctx, |reg| reg.crab_beacons.get(&beacon_id).cloned());
    let Some(rec) = snap_rec else { return };
    if rec.phase == CrabBeaconPhase::SelfDestructed {
        return;
    }

    if rec.phase == CrabBeaconPhase::Idle
        && rec.deployed_at_sec > 0
        && now >= rec.deployed_at_sec.saturating_add(CRAB_BEACON_IDLE_LIFETIME_SECS)
    {
        self_destruct_crab_beacon(ctx, beacon_id);
        return;
    }

    if rec.phase == CrabBeaconPhase::Linking && now >= rec.link_completes_at_sec {
        clear_crab_link_ship_effects(
            ctx,
            rec.solar_system_id,
            beacon_id,
            rec.linked_char_id,
            rec.linked_ship_id,
            rec.orig_resonances,
        );
        with_pve_special_reg(ctx, |reg| {
            if let Some(b) = reg.crab_beacons.get_mut(&beacon_id) {
                b.phase = CrabBeaconPhase::Scanning;
                b.orig_resonances = None;
            }
        });
        spawn_crab_rogue_drone_wave(ctx, beacon_id, 1);
    }

    let cur = with_pve_special_reg(ctx, |reg| reg.crab_beacons.get(&beacon_id).cloned());
    let Some(cur) = cur else { return };
    if cur.phase == CrabBeaconPhase::Scanning {
        let wave2_at = cur.link_completes_at_sec.saturating_add(200);
        let wave3_at = cur.link_completes_at_sec.saturating_add(400);
        if cur.waves_spawned < 2 && now >= wave2_at {
            spawn_crab_rogue_drone_wave(ctx, beacon_id, 2);
        }
        if cur.waves_spawned < 3 && now >= wave3_at {
            spawn_crab_rogue_drone_wave(ctx, beacon_id, 3);
        }
        if now >= cur.scan_completes_at_sec {
            finalize_crab_scan_success(ctx, beacon_id);
        }
    }

    let after_scan = with_pve_special_reg(ctx, |reg| reg.crab_beacons.get(&beacon_id).cloned());
    if let Some(b) = after_scan {
        if b.phase == CrabBeaconPhase::LootRestricted && now >= b.self_destruct_at_sec {
            self_destruct_crab_beacon(ctx, beacon_id);
        }
    }
}

fn has_any_crab_beacons(ctx: &ServerContext) -> bool {
    ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs
            .get("incursions_state")
            .is_some_and(|b| !b.contains("\"crab_beacons\":{}"))
    })
}

fn has_special_sites_state(ctx: &ServerContext) -> bool {
    ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs.contains_key("incursions_state")
            || snap.owner_notes.contains_key(&-8_300_000_000_000)
    })
}

/// Advances all active CRAB beacons across the cluster up to `ctx.now_unix()`.
pub fn tick_all_crab_beacons(ctx: &ServerContext) {
    if !has_any_crab_beacons(ctx) {
        return;
    }
    let now = ctx.now_unix();
    let ids: Vec<i64> = with_pve_special_reg(ctx, |reg| {
        reg.crab_beacons
            .values()
            .filter(|b| b.phase != CrabBeaconPhase::SelfDestructed)
            .map(|b| b.beacon_id)
            .collect()
    });
    for bid in ids {
        advance_single_crab_beacon(ctx, bid, now);
    }
}

/// Advances active CRAB beacons in `sys_id` on the 1 Hz solar system actor tick.
pub async fn tick_crab_beacons_in_system(ctx: &ServerContext, sys_id: SolarSystemId) {
    if !has_any_crab_beacons(ctx) {
        return;
    }
    let now = ctx.now_unix();
    let ids: Vec<i64> = with_pve_special_reg(ctx, |reg| {
        reg.crab_beacons
            .values()
            .filter(|b| {
                b.solar_system_id == sys_id && b.phase != CrabBeaconPhase::SelfDestructed
            })
            .map(|b| b.beacon_id)
            .collect()
    });
    for bid in ids {
        advance_single_crab_beacon(ctx, bid, now);
    }
}

/// Called when an NPC ball is destroyed in `solar_system_id`; advances CRAB Rogue Drone waves if cleared early.
pub async fn on_crab_wave_npc_destroyed(
    _systems: &Arc<crate::system_actor::SystemRegistry>,
    db: &Arc<WorldRepository>,
    sde: &Arc<eve_sde::SdeRegistry>,
    solar_system_id: SolarSystemId,
    npc_ball_id: i64,
) {
    let now_sec = db.clock().now_unix_secs();
    let next_wave_info = with_pve_special_reg_db(db, now_sec, |reg| {
        let mut to_spawn = None;
        for b in reg.crab_beacons.values_mut() {
            if b.solar_system_id == solar_system_id && b.wave_npc_ball_ids.contains(&npc_ball_id) {
                b.wave_npc_ball_ids.retain(|&id| id != npc_ball_id);
                if b.phase == CrabBeaconPhase::Scanning
                    && b.wave_npc_ball_ids.is_empty()
                    && b.waves_spawned < 3
                {
                    let next_w = b.waves_spawned + 1;
                    b.waves_spawned = next_w;
                    let drone_types = match next_w {
                        2 => WAVE_2_DRONES,
                        _ => WAVE_3_DRONES,
                    };
                    let mut allocated = Vec::with_capacity(drone_types.len());
                    for &tid in drone_types {
                        reg.next_seq += 1;
                        let bid = 1_897_000_000 + reg.next_seq;
                        b.wave_npc_ball_ids.push(bid);
                        allocated.push((bid, tid));
                    }
                    to_spawn = Some((b.beacon_id, next_w, DVec3::from_array(b.position), allocated));
                }
                break;
            }
        }
        to_spawn
    });

    if let Some((beacon_id, next_w, beacon_pos, allocated)) = next_wave_info {
        let sys_handle = _systems.get_or_spawn(solar_system_id);
        for (idx, (bid, tid)) in allocated.into_iter().enumerate() {
            _systems.claim_special_site_ball(solar_system_id, bid);
            let angle = (idx as f64) * 1.4 + f64::from(next_w);
            let offset = DVec3::new(angle.cos() * 4_500.0, 250.0 * f64::from(next_w), angle.sin() * 4_500.0);
            let cs = build_rogue_drone_combat(sde, tid);
            let _ = sys_handle
                .spawn_ball_with_combat(
                    Ball {
                        id: bid,
                        type_id: tid,
                        position: beacon_pos + offset,
                        velocity: DVec3::ZERO,
                        max_velocity: 280.0,
                        inertia: 1.0,
                        mass: 15_000.0,
                        radius: 45.0,
                        speed_fraction: 0.0,
                        mode: MovementMode::Orbit {
                            target_id: beacon_id,
                            range: 4_500.0,
                        },
                    },
                    cs,
                )
                .await;
        }
    }
}

fn ensure_beacon_registered_from_item(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    beacon_id: i64,
) -> Option<CrabBeaconRecord> {
    if let Some(rec) = with_pve_special_reg(ctx, |reg| reg.crab_beacons.get(&beacon_id).cloned()) {
        return Some(rec);
    }
    let item_info = ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&beacon_id)?;
        if !is_crab_beacon_type(&ctx.sde, it.type_id) {
            return None;
        }
        if it.location_id != sys_id as i64 {
            return None;
        }
        let pos = snap.item_positions.get(&beacon_id).copied().unwrap_or([0.0; 3]);
        Some((it.type_id, it.owner_id, DVec3::from_array(pos)))
    });
    let (type_id, owner_id, pos) = item_info?;
    register_crab_beacon(ctx, beacon_id, type_id, owner_id, sys_id, pos);
    with_pve_special_reg(ctx, |reg| reg.crab_beacons.get(&beacon_id).cloned())
}

fn beacon_state_keyval(rec: &CrabBeaconRecord, now: u64) -> PyRep {
    let remaining_link = if rec.phase == CrabBeaconPhase::Linking {
        rec.link_completes_at_sec.saturating_sub(now) as i64
    } else {
        0
    };
    let remaining_scan = if rec.phase == CrabBeaconPhase::Scanning {
        rec.scan_completes_at_sec.saturating_sub(now) as i64
    } else {
        0
    };
    let remaining_loot_lock = if rec.phase == CrabBeaconPhase::LootRestricted {
        rec.self_destruct_at_sec.saturating_sub(now) as i64
    } else {
        0
    };
    PyRep::keyval([
        ("beaconID", PyRep::Integer(rec.beacon_id)),
        ("typeID", PyRep::Integer(rec.type_id as i64)),
        ("solarSystemID", PyRep::Integer(rec.solar_system_id as i64)),
        ("deployerCharID", PyRep::Integer(rec.deployer_char_id)),
        ("phase", PyRep::String(rec.phase.as_str().into())),
        ("linked", PyRep::Bool(rec.phase == CrabBeaconPhase::Linking)),
        ("linkedCharID", rec.linked_char_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
        ("linkedShipID", rec.linked_ship_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
        ("linkDurationSeconds", PyRep::Integer(CRAB_LINK_DURATION_SECS as i64)),
        ("linkCompletesAtUnix", PyRep::Integer(rec.link_completes_at_sec as i64)),
        ("linkRemainingSeconds", PyRep::Integer(remaining_link)),
        ("scanDurationSeconds", PyRep::Integer(CRAB_SCAN_DURATION_SECS as i64)),
        ("scanCompletesAtUnix", PyRep::Integer(rec.scan_completes_at_sec as i64)),
        ("scanRemainingSeconds", PyRep::Integer(remaining_scan)),
        ("selfDestructAtUnix", PyRep::Integer(rec.self_destruct_at_sec as i64)),
        ("lootRestrictedRemainingSeconds", PyRep::Integer(remaining_loot_lock)),
        ("wavesSpawned", PyRep::Integer(rec.waves_spawned as i64)),
        ("activeWaveDrones", PyRep::Integer(rec.wave_npc_ball_ids.len() as i64)),
        ("openContainerID", rec.open_container_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
    ])
}

/// Handles V24 `beyonce.CallComponentFromClient` calls targeting a CRAB beacon (`linkWithShip` & `crabBeacon`).
#[allow(clippy::too_many_arguments)]
pub async fn handle_crab_component_call(
    ctx: &ServerContext,
    _session: &SessionState,
    sys_id: SolarSystemId,
    char_id: i64,
    ship_id: i64,
    beacon_id: i64,
    comp_name: &str,
    comp_method: &str,
    _args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let Some(initial_rec) = ensure_beacon_registered_from_item(ctx, sys_id, beacon_id) else {
        return Err(BridgeRpcError::user_error("CrabBeaconNotFound"));
    };
    if initial_rec.solar_system_id != sys_id {
        return Err(BridgeRpcError::user_error("TargetNotInSameSystem"));
    }

    advance_single_crab_beacon(ctx, beacon_id, ctx.now_unix());
    let now = ctx.now_unix();

    match (comp_name, comp_method) {
        ("linkWithShip" | "crabBeacon", "GetLinkState" | "GetState") => {
            let rec = with_pve_special_reg(ctx, |reg| reg.crab_beacons.get(&beacon_id).cloned())
                .unwrap_or(initial_rec);
            Ok(beacon_state_keyval(&rec, now))
        }
        ("linkWithShip", "InitiateLink" | "InitiateLinkQuick") => {
            validate_crab_deployment_system(&ctx.sde, sys_id)?;

            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let (_, ship_ball) = sys_handle
                .get_ball_snapshot(ship_id)
                .await
                .ok()
                .flatten()
                .ok_or_else(|| BridgeRpcError::user_error("ShipNotInSpace"))?;
            let cs = sys_handle
                .get_combat_state(ship_id)
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            if cs.is_cloaked {
                return Err(BridgeRpcError::user_error("CannotLinkWhileCloaked"));
            }

            let beacon_pos = if let Some((_, b)) = sys_handle.get_ball_snapshot(beacon_id).await.ok().flatten() {
                b.position
            } else {
                DVec3::from_array(initial_rec.position)
            };
            let dist = (ship_ball.position.distance(beacon_pos) - ship_ball.radius - 400.0).max(0.0);
            if dist > CRAB_LINK_RANGE_M {
                return Err(BridgeRpcError::user_error("OutOfCrabLinkRange"));
            }

            if !is_eligible_crab_ship(&ctx.sde, initial_rec.type_id, ship_ball.type_id) {
                return Err(BridgeRpcError::user_error("IneligibleShipForCrabLink"));
            }

            // Verify beacon is idle and system signal interference has capacity for another CRAB
            let current_level = crate::services::pve_special_sites::get_system_interference_level_decayed(
                ctx, sys_id, now,
            );
            if current_level + CRAB_INTERFERENCE_DELTA > 1.0 + 1e-6 {
                return Err(BridgeRpcError::user_error("ExcessInterference"));
            }

            let already_active = with_pve_special_reg(ctx, |reg| {
                reg.crab_beacons
                    .get(&beacon_id)
                    .is_none_or(|b| b.phase != CrabBeaconPhase::Idle)
            });
            if already_active {
                return Err(BridgeRpcError::user_error("CrabBeaconAlreadyActive"));
            }

            // Deduct 30 CEQ (100 cap -> 3 activations; 4th refused with InsufficientEnergy)
            let updated_energy = crate::services::character_energy::consume_character_energy(
                ctx,
                char_id,
                CRAB_CEQ_COST,
            )?;

            // Increase system signal interference by 1/12 (refused at 12 CRABs)
            record_system_interference_activity_with_char(
                ctx,
                char_id,
                sys_id,
                CRAB_INTERFERENCE_DELTA,
            );
            let new_interference = crate::services::pve_special_sites::get_system_interference_level_decayed(
                ctx, sys_id, now,
            );

            let link_completes = now + CRAB_LINK_DURATION_SECS;
            let scan_completes = link_completes + CRAB_SCAN_DURATION_SECS;
            let self_destruct_at = scan_completes + CRAB_LOOT_EXCLUSIVE_SECS;

            let orig_res = apply_crab_link_ship_effects(
                ctx,
                sys_id,
                beacon_id,
                char_id,
                ship_id,
                link_completes,
            );

            with_pve_special_reg(ctx, |reg| {
                if let Some(b) = reg.crab_beacons.get_mut(&beacon_id) {
                    b.phase = CrabBeaconPhase::Linking;
                    b.linked_char_id = Some(char_id);
                    b.linked_ship_id = Some(ship_id);
                    b.link_started_at_sec = now;
                    b.link_completes_at_sec = link_completes;
                    b.scan_completes_at_sec = scan_completes;
                    b.self_destruct_at_sec = self_destruct_at;
                    b.orig_resonances = orig_res;
                }
            });

            ctx.notify(
                NotificationTarget::SolarSystem(sys_id),
                "OnSlimItemChange",
                "solarsystemid",
                vec![
                    PyRep::Integer(beacon_id),
                    PyRep::keyval([
                        ("crabPhase", PyRep::String("Linking".into())),
                        ("crabLinkedShipID", PyRep::Integer(ship_id)),
                        ("crabLinkCompletesAt", PyRep::Integer(link_completes as i64)),
                    ]),
                ],
            );

            Ok(PyRep::keyval([
                ("beaconID", PyRep::Integer(beacon_id)),
                ("shipID", PyRep::Integer(ship_id)),
                ("linked", PyRep::Bool(true)),
                ("phase", PyRep::String("linking".into())),
                ("linkDurationSeconds", PyRep::Integer(CRAB_LINK_DURATION_SECS as i64)),
                ("linkCompletesAtUnix", PyRep::Integer(link_completes as i64)),
                ("scanDurationSeconds", PyRep::Integer(CRAB_SCAN_DURATION_SECS as i64)),
                ("scanCompletesAtUnix", PyRep::Integer(scan_completes as i64)),
                ("selfDestructAtUnix", PyRep::Integer(self_destruct_at as i64)),
                ("resistanceBonusPct", PyRep::Real(10.0)),
                ("ceqRemaining", PyRep::Integer(updated_energy.current_energy)),
                ("systemInterference", PyRep::Real(new_interference)),
            ]))
        }
        _ => Err(BridgeRpcError {
            code: "UnsupportedComponentCall".into(),
            message: format!("Unsupported CRAB component call {comp_name}.{comp_method}"),
        }),
    }
}

pub const INTERFERENCE_DECAY_WINDOW_SECS: f64 = 25.0 * 3_600.0; // 25 hours = 90,000s

pub fn on_server_clock_advanced(ctx: &ServerContext, delta_sec: u64) {
    if !has_special_sites_state(ctx) {
        return;
    }
    let now_u = ctx.now_unix();
    let should_move_storms = with_pve_special_reg(ctx, |reg| {
        let any_due = reg
            .roaming_storms
            .iter()
            .any(|s| s.next_move_at_sec > 0 && now_u >= s.next_move_at_sec);
        if any_due {
            if reg.sim_clock_sec < now_u {
                reg.sim_clock_sec = now_u;
            }
            advance_roaming_storms(ctx, reg, delta_sec);
        }
        any_due
    });
    tick_all_crab_beacons(ctx);
    if delta_sec > 0 {
        crate::services::pve_incursion_lifecycle::advance_incursion_lifecycles(ctx);
    }
    if should_move_storms {
        sync_metaliminal_storm_effects_and_sites(ctx);
    }
}

pub fn get_system_interference_level_decayed(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
    now_sec: u64,
) -> f64 {
    with_pve_special_reg(ctx, |reg| {
        let Some(entry) = reg.interference_by_system.get_mut(&solar_system_id) else {
            return 0.0;
        };
        if entry.last_updated_sec > 0 && now_sec > entry.last_updated_sec {
            let elapsed = now_sec - entry.last_updated_sec;
            let decay = (elapsed as f64) / INTERFERENCE_DECAY_WINDOW_SECS;
            entry.interference_level = (entry.interference_level - decay).max(0.0);
            entry.band = if entry.interference_level >= 0.70 {
                3
            } else if entry.interference_level >= 0.35 {
                2
            } else {
                1
            };
            entry.last_updated_sec = now_sec;
        }
        entry.interference_level
    })
}

pub fn record_system_interference_activity_with_char(
    ctx: &ServerContext,
    char_id: i64,
    solar_system_id: SolarSystemId,
    activity_delta: f64,
) {
    ensure_special_sites_initialized(ctx);
    let delta = activity_delta.clamp(0.0, 1.0);
    let now_sec = ctx.now_unix();
    let (level, old_band, new_band) = with_pve_special_reg(ctx, |reg| {
        let entry = reg
            .interference_by_system
            .entry(solar_system_id)
            .or_insert(SystemInterferenceRecord {
                solar_system_id,
                interference_level: 0.0,
                band: 1,
                last_updated_sec: now_sec,
            });
        if entry.last_updated_sec > 0 && now_sec > entry.last_updated_sec {
            let elapsed = now_sec - entry.last_updated_sec;
            let decay = (elapsed as f64) / INTERFERENCE_DECAY_WINDOW_SECS;
            entry.interference_level = (entry.interference_level - decay).max(0.0);
        }
        let prev = entry.band;
        if delta > 0.0 {
            entry.interference_level = (entry.interference_level + delta).clamp(0.0, 1.0);
        }
        entry.band = if entry.interference_level >= 0.70 {
            3
        } else if entry.interference_level >= 0.35 {
            2
        } else {
            1
        };
        entry.last_updated_sec = now_sec;
        (entry.interference_level, prev, entry.band)
    });
    if new_band != old_band {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnSolarsystemInterferenceChanged",
            "charid",
            vec![
                PyRep::Integer(solar_system_id as i64),
                PyRep::Real(level),
                PyRep::Integer(new_band as i64),
            ],
        );
        ctx.notify(
            NotificationTarget::SolarSystem(solar_system_id),
            "OnSolarsystemInterferenceChanged",
            "solarsystemid",
            vec![
                PyRep::Integer(solar_system_id as i64),
                PyRep::Real(level),
                PyRep::Integer(new_band as i64),
            ],
        );
    }
}

pub fn record_system_interference_activity(
    ctx: &ServerContext,
    session: &mut SessionState,
    solar_system_id: SolarSystemId,
    activity_delta: f64,
) {
    ensure_special_sites_initialized(ctx);
    let delta = activity_delta.clamp(0.0, 1.0);
    let now_sec = ctx.now_unix();
    let (level, old_band, new_band) = with_pve_special_reg(ctx, |reg| {
        let entry = reg
            .interference_by_system
            .entry(solar_system_id)
            .or_insert(SystemInterferenceRecord {
                solar_system_id,
                interference_level: 0.0,
                band: 1,
                last_updated_sec: now_sec,
            });
        if entry.last_updated_sec > 0 && now_sec > entry.last_updated_sec {
            let elapsed = now_sec - entry.last_updated_sec;
            let decay = (elapsed as f64) / INTERFERENCE_DECAY_WINDOW_SECS;
            entry.interference_level = (entry.interference_level - decay).max(0.0);
        }
        let prev = entry.band;
        if delta > 0.0 {
            entry.interference_level = (entry.interference_level + delta).clamp(0.0, 1.0);
        }
        entry.band = if entry.interference_level >= 0.70 {
            3
        } else if entry.interference_level >= 0.35 {
            2
        } else {
            1
        };
        entry.last_updated_sec = now_sec;
        (entry.interference_level, prev, entry.band)
    });
    if new_band != old_band {
        session.push_notification(
            "OnSolarsystemInterferenceChanged",
            vec![
                PyRep::Integer(solar_system_id as i64),
                PyRep::Real(level),
                PyRep::Integer(new_band as i64),
            ],
        );
        ctx.notify(
            NotificationTarget::SolarSystem(solar_system_id),
            "OnSolarsystemInterferenceChanged",
            "solarsystemid",
            vec![
                PyRep::Integer(solar_system_id as i64),
                PyRep::Real(level),
                PyRep::Integer(new_band as i64),
            ],
        );
    }
}

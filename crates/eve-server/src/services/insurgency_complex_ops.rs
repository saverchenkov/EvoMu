//! `insurgency_complex_ops` — Live Pirate Insurgency Laundering, Mining Op Ambush, and Corporate Outpost Raid complexes.
//!
//! All mechanics execute through live V24 client RPCs (`beyonce.CallComponentFromClient` -> `itemTrader.ProcessTrade`,
//! `keeper`/`beyonce.ActivateAccelerationGate`, `beyonce.CmdWarpToStuff`, `dogmaIM.AddTarget`/`Activate`, `invbroker`)
//! and cluster/system world ticks (`tick_insurgency_campaigns`, `tick_fw_gameplay`, `populate_npc_wreck_on_turret_kill`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_entity;
use crate::services::combat_crimewatch::with_mut_combat_runtime;
use crate::services::fw_gameplay::{militia_lp_corp_id, record_insurgency_contribution};
use crate::services::fw_services::{
    HQ_SYSTEM_AMO, HQ_SYSTEM_INTAKI, HQ_SYSTEM_MEHATOOR, HQ_SYSTEM_ONNAMON,
};
use crate::services::insurgency_services::{
    add_system_corruption, add_system_suppression, get_campaign_phase_and_timer,
    FACTION_ANGEL_CARTEL, FACTION_GURISTAS, PHASE_ACTIVE,
};
use crate::services::security_mgr::insurgency_suppression_empire_lp_multiplier;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, WorldStateSnapshot};
use eve_destiny::{Ball, MovementMode};
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const INSURGENCY_OPS_BLOB_KEY: &str = "insurgency_complex_ops";

pub const TYPE_GURISTAS_LAUNDERING_FACILITY: u32 = 78_868;
pub const TYPE_ANGEL_LAUNDERING_FACILITY: u32 = 78_869;
pub const TYPE_FEDERATION_CI_OUTPOST_HUB: u32 = 78_870;
pub const TYPE_IMPERIAL_CI_OUTPOST_HUB: u32 = 78_871;
pub const TYPE_STATE_CI_OUTPOST_HUB: u32 = 79_115;
pub const TYPE_REPUBLIC_CI_OUTPOST_HUB: u32 = 79_116;

pub const TYPE_UPWELL_CONSORTIUM_DATA_CACHE: u32 = 78_848;
pub const TYPE_GURISTAS_INSURGENCY_DATA_SEQ: u32 = 78_857;
pub const TYPE_ANGEL_INSURGENCY_DATA_SEQ: u32 = 78_862;

pub const TYPE_MINING_AMBUSH_BEACON: u32 = 78_994;
pub const TYPE_ORE_RORQUAL: u32 = 79_589;
pub const TYPE_ORE_RORQUAL_WRECK: u32 = 79_586;

pub const TYPE_OUTPOST_BEACON_SMALL: u32 = 78_799;
pub const TYPE_OUTPOST_BEACON_MEDIUM: u32 = 78_805;
pub const TYPE_OUTPOST_BEACON_MODERATE: u32 = 97_658;
pub const TYPE_OUTPOST_BEACON_LARGE_ADV: u32 = 78_814;
pub const TYPE_OUTPOST_RAID_GATE: u32 = 93_650;

/// DERIVED (synthetic): Havoc Mining Op Ambush ransom negotiation timer (`300s`), site cooldown (`900s`), and base LP (`30,000`).
pub const MINING_AMBUSH_RANSOM_SECS: u64 = 300;
pub const MINING_AMBUSH_RESPAWN_SECS: u64 = 900;
pub const MINING_AMBUSH_BASE_LP: i64 = 30_000;
pub const MINING_AMBUSH_CORRUPTION_PTS: i64 = 50;
pub const MINING_AMBUSH_SUPPRESSION_PTS: i64 = 50;

pub const LARGE_ADV_OUTPOST_RESPAWN_SECS: u64 = 40 * 60;
pub const STANDARD_OUTPOST_RESPAWN_SECS: u64 = 900;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OutpostRaidSize {
    Small,
    Medium,
    Moderate,
    LargeAdv,
}

impl OutpostRaidSize {
    pub const ALL: [Self; 4] = [Self::Small, Self::Medium, Self::Moderate, Self::LargeAdv];

    pub fn slot_index(self) -> i64 {
        match self {
            Self::Small => 1,
            Self::Medium => 2,
            Self::Moderate => 3,
            Self::LargeAdv => 4,
        }
    }

    pub fn beacon_type_id(self) -> u32 {
        match self {
            Self::Small => TYPE_OUTPOST_BEACON_SMALL,
            Self::Medium => TYPE_OUTPOST_BEACON_MEDIUM,
            Self::Moderate => TYPE_OUTPOST_BEACON_MODERATE,
            Self::LargeAdv => TYPE_OUTPOST_BEACON_LARGE_ADV,
        }
    }

    pub fn base_capture_duration_secs(self) -> u64 {
        match self {
            Self::Small => 600,
            Self::Medium | Self::Moderate | Self::LargeAdv => 900,
        }
    }

    pub fn contribution_percent(self) -> f64 {
        match self {
            Self::Small => 0.25,
            Self::Medium => 0.5,
            Self::Moderate => 1.5,
            Self::LargeAdv => 1.0,
        }
    }

    /// Returns hundredths of an insurgency point (`denominator = 1000` = `100%`, so `1%` = `10` pts = `1,000` subpoints).
    pub fn contribution_subpoints(self) -> i64 {
        match self {
            Self::Small => 250,
            Self::Medium => 500,
            Self::Moderate => 1_500,
            Self::LargeAdv => 1_000,
        }
    }

    pub fn base_lp_reward(self) -> i64 {
        match self {
            Self::Small => 15_000,
            Self::Medium => 22_500,
            Self::Moderate => 27_000,
            Self::LargeAdv => 30_000,
        }
    }

    pub fn respawn_delay_secs(self) -> u64 {
        if self == Self::LargeAdv {
            LARGE_ADV_OUTPOST_RESPAWN_SECS
        } else {
            STANDARD_OUTPOST_RESPAWN_SECS
        }
    }

    pub fn allows_ship_group(self, group_id: u32) -> bool {
        let frig_dessie = matches!(
            group_id,
            25 | 324 | 830 | 831 | 834 | 893 | 1283 | 1527 | 420 | 541 | 1305 | 1534
        );
        let cruiser = matches!(group_id, 26 | 358 | 832 | 833 | 894 | 906 | 963 | 1972);
        let bc = matches!(group_id, 419 | 540 | 1201);
        let bs = matches!(group_id, 27 | 898 | 900);
        match self {
            Self::Small => frig_dessie,
            Self::Medium => frig_dessie || cruiser,
            Self::Moderate => frig_dessie || cruiser || bc,
            Self::LargeAdv => frig_dessie || cruiser || bc || bs,
        }
    }

    pub fn gate_position(self) -> DVec3 {
        DVec3::new(1_400_000.0 + (self.slot_index() as f64) * 100_000.0, 0.0, 0.0)
    }

    pub fn pocket_position(self) -> DVec3 {
        DVec3::new(self.gate_position().x, 0.0, 250_000.0)
    }

    pub fn mordus_defender_type_id(self) -> u32 {
        match self {
            Self::Small => 79_545,
            Self::Medium => 79_549,
            Self::Moderate => 79_553,
            Self::LargeAdv => 79_557,
        }
    }

    pub fn pirate_defender_type_id(self, pirate_faction_id: i64) -> u32 {
        if pirate_faction_id == FACTION_ANGEL_CARTEL {
            match self {
                Self::Small => 79_515,
                Self::Medium => 79_520,
                Self::Moderate => 79_523,
                Self::LargeAdv => 79_526,
            }
        } else {
            match self {
                Self::Small => 79_529,
                Self::Medium => 79_533,
                Self::Moderate => 79_537,
                Self::LargeAdv => 79_541,
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutpostRaidInstance {
    pub gate_id: i64,
    pub beacon_id: i64,
    pub campaign_id: i64,
    pub solar_system_id: u32,
    pub pirate_faction_id: i64,
    pub target_faction_id: i64,
    pub size: OutpostRaidSize,
    pub is_static: bool,
    pub occupants: Vec<(i64, i64)>,
    pub mordus_defender_ball_id: Option<i64>,
    pub pirate_defender_ball_id: Option<i64>,
    pub mordus_defender_cleared: bool,
    pub pirate_defender_cleared: bool,
    pub weighted_elapsed_secs: f64,
    pub last_step_unix: u64,
    pub completed_on_cooldown: bool,
    pub respawn_at_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiningAmbushInstance {
    pub campaign_id: i64,
    pub solar_system_id: u32,
    pub pirate_faction_id: i64,
    pub target_faction_id: i64,
    pub beacon_id: i64,
    pub rorqual_ball_id: i64,
    pub pirate_tackler_ids: Vec<i64>,
    pub ore_defender_ids: Vec<i64>,
    pub ransom_elapsed_secs: u64,
    pub last_step_unix: u64,
    pub resolved: bool,
    pub rorqual_warping_off_until: Option<u64>,
    pub cooldown_until_unix: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InsurgencyComplexOpsState {
    pub outpost_raids: HashMap<i64, OutpostRaidInstance>,
    pub mining_ambushes: HashMap<u32, MiningAmbushInstance>,
    pub subpoint_remainders: HashMap<String, i64>,
}

pub fn load_ops_state(snap: &WorldStateSnapshot) -> InsurgencyComplexOpsState {
    snap.service_state_blobs
        .get(INSURGENCY_OPS_BLOB_KEY)
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default()
}

pub fn save_ops_state(snap: &mut WorldStateSnapshot, state: &InsurgencyComplexOpsState) {
    if let Ok(json) = serde_json::to_string(state) {
        snap.service_state_blobs.insert(INSURGENCY_OPS_BLOB_KEY.into(), json);
    }
}

pub fn is_battlecruiser_group(group_id: u32) -> bool {
    matches!(group_id, 419 | 540 | 1201)
}

pub fn share_site_lp_up_to_5(base_lp_per_pilot: i64, pilot_count: usize) -> i64 {
    if pilot_count == 0 || base_lp_per_pilot <= 0 {
        0
    } else if pilot_count <= 5 {
        base_lp_per_pilot
    } else {
        (base_lp_per_pilot * 5) / (pilot_count as i64)
    }
}

fn credit_militia_lp(ctx: &ServerContext, char_id: i64, faction_id: i64, lp: i64) {
    if char_id <= 0 || lp <= 0 {
        return;
    }
    let lp_corp = militia_lp_corp_id(&ctx.sde, faction_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            let entry = ch.loyalty_points.entry(lp_corp).or_insert(0);
            *entry = entry.saturating_add(lp);
        }
    });
}

fn add_percentage_campaign_points(
    ctx: &ServerContext,
    campaign_id: i64,
    sys_id: u32,
    is_corruption: bool,
    subpoints: i64,
) -> (i64, u8) {
    let pts_to_add = ctx.db.with_mut_snapshot(|snap| {
        let mut st = load_ops_state(snap);
        let key = format!("{campaign_id}:{sys_id}:{}", u8::from(is_corruption));
        let cur = st.subpoint_remainders.get(&key).copied().unwrap_or(0) + subpoints;
        let whole = cur / 100;
        st.subpoint_remainders.insert(key, cur % 100);
        save_ops_state(snap, &st);
        whole
    });
    if is_corruption {
        add_system_corruption(ctx, campaign_id, sys_id, pts_to_add)
    } else {
        add_system_suppression(ctx, campaign_id, sys_id, pts_to_add)
    }
}

pub fn is_named_pirate_commander_roamer(type_id: u32) -> bool {
    matches!(type_id, 79_329 | 79_330 | 79_332 | 79_334 | 79_358 | 79_360 | 79_362 | 79_364)
}

pub fn select_named_or_regular_roamer_type(
    ctx: &ServerContext,
    group_id: u32,
    pirate_fac: i64,
    c_st: u8,
    s_st: u8,
) -> Option<u32> {
    if group_id == 4637 {
        if c_st < 2 {
            return None;
        }
        if c_st >= 3 {
            let named: &[u32] = if pirate_fac == FACTION_ANGEL_CARTEL {
                &[79_329, 79_330, 79_332, 79_334]
            } else {
                &[79_358, 79_360, 79_362, 79_364]
            };
            let idx = usize::from(c_st - 3).min(named.len() - 1);
            return named.get(idx).copied().filter(|tid| ctx.sde.types.contains_key(tid));
        }
        let mut reg: Vec<u32> = ctx
            .sde
            .types
            .values()
            .filter(|t| {
                t.group_id == 4637
                    && t.faction_id == Some(pirate_fac as u32)
                    && !is_named_pirate_commander_roamer(t.type_id)
            })
            .map(|t| t.type_id)
            .collect();
        reg.sort_unstable();
        return reg.first().copied();
    }
    if group_id == 4638 && s_st >= 3 {
        let named = [79_389_u32, 79_394, 79_395, 79_396];
        let idx = usize::from(s_st - 3).min(named.len() - 1);
        return named.get(idx).copied().filter(|tid| ctx.sde.types.contains_key(tid));
    }
    None
}

pub fn named_insurgency_roamer_loot_drop(npc_type_id: u32) -> Option<(u32, i64)> {
    match npc_type_id {
        79_329..=79_334 => Some((TYPE_ANGEL_INSURGENCY_DATA_SEQ, 1)),
        79_358..=79_364 => Some((TYPE_GURISTAS_INSURGENCY_DATA_SEQ, 1)),
        79_389 | 79_394 | 79_395 | 79_396 | 79_508 => Some((TYPE_UPWELL_CONSORTIUM_DATA_CACHE, 1)),
        TYPE_ORE_RORQUAL => Some((TYPE_UPWELL_CONSORTIUM_DATA_CACHE, 3)),
        _ => None,
    }
}

pub fn append_insurgency_npc_wreck_loot(
    npc_type_id: u32,
    loot_to_insert: &mut Vec<(u32, i32, bool)>,
) {
    if let Some((drop_tid, qty)) = named_insurgency_roamer_loot_drop(npc_type_id) {
        if !loot_to_insert.iter().any(|&(tid, _, _)| tid == drop_tid) {
            loot_to_insert.push((drop_tid, qty as i32, false));
        }
    }
    if npc_type_id == TYPE_ORE_RORQUAL && !loot_to_insert.iter().any(|&(tid, _, _)| tid == 17_867) {
        loot_to_insert.push((17_867, 5, false));
    }
}

pub fn check_insurgency_laundering_access(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: u32,
    trader_type_id: u32,
) -> Result<(), BridgeRpcError> {
    if !matches!(
        trader_type_id,
        TYPE_GURISTAS_LAUNDERING_FACILITY
            | TYPE_ANGEL_LAUNDERING_FACILITY
            | TYPE_FEDERATION_CI_OUTPOST_HUB
            | TYPE_IMPERIAL_CI_OUTPOST_HUB
            | TYPE_STATE_CI_OUTPOST_HUB
            | TYPE_REPUBLIC_CI_OUTPOST_HUB
    ) {
        return Ok(());
    }
    let now = ctx.now_unix();
    let war_fac = ctx
        .db
        .with_snapshot(|s| s.characters.get(&char_id).and_then(|c| c.war_faction_id));

    if matches!(
        trader_type_id,
        TYPE_GURISTAS_LAUNDERING_FACILITY | TYPE_ANGEL_LAUNDERING_FACILITY
    ) {
        let req_fac = if trader_type_id == TYPE_ANGEL_LAUNDERING_FACILITY {
            FACTION_ANGEL_CARTEL
        } else {
            FACTION_GURISTAS
        };
        if war_fac != Some(req_fac) {
            return Err(BridgeRpcError {
                code: "FactionNotAligned".into(),
                message: "Only aligned Pirate Militia pilots may use a Pirate Laundering Facility".into(),
            });
        }
        let valid_fob = ctx.db.with_snapshot(|snap| {
            snap.insurgency_runtime.campaigns.values().any(|c| {
                get_campaign_phase_and_timer(c, now).0 == PHASE_ACTIVE
                    && c.pirate_faction_id == req_fac
                    && c.fob_solar_system_id == sys_id
            })
        });
        if !valid_fob {
            return Err(BridgeRpcError {
                code: "InvalidLaunderingLocation".into(),
                message: "Pirate Laundering hand-ins require an active Pirate FOB system".into(),
            });
        }
        return Ok(());
    }

    let (req_hq_sys, req_emp_fac) = match trader_type_id {
        TYPE_FEDERATION_CI_OUTPOST_HUB => (HQ_SYSTEM_INTAKI, 500_004_i64),
        TYPE_IMPERIAL_CI_OUTPOST_HUB => (HQ_SYSTEM_MEHATOOR, 500_003_i64),
        TYPE_STATE_CI_OUTPOST_HUB => (HQ_SYSTEM_ONNAMON, 500_001_i64),
        _ => (HQ_SYSTEM_AMO, 500_002_i64),
    };
    if sys_id != req_hq_sys {
        return Err(BridgeRpcError {
            code: "InvalidWarHQLocation".into(),
            message: "Counter-Insurgency Outpost hand-ins must occur in the Empire War HQ system".into(),
        });
    }
    if war_fac != Some(req_emp_fac) {
        return Err(BridgeRpcError {
            code: "FactionNotAligned".into(),
            message: "Only pilots enlisted in this War HQ's Empire Militia may hand in Insurgency Data Sequences".into(),
        });
    }
    Ok(())
}

pub fn on_insurgency_laundering_trade_completed(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: u32,
    trader_type_id: u32,
    multiplier: i32,
) {
    if !matches!(
        trader_type_id,
        TYPE_GURISTAS_LAUNDERING_FACILITY
            | TYPE_ANGEL_LAUNDERING_FACILITY
            | TYPE_FEDERATION_CI_OUTPOST_HUB
            | TYPE_IMPERIAL_CI_OUTPOST_HUB
            | TYPE_STATE_CI_OUTPOST_HUB
            | TYPE_REPUBLIC_CI_OUTPOST_HUB
    ) {
        return;
    }
    let mult = i64::from(multiplier.max(1));
    let now = ctx.now_unix();
    let Some(war_fac) = ctx
        .db
        .with_snapshot(|s| s.characters.get(&char_id).and_then(|c| c.war_faction_id))
    else {
        return;
    };
    credit_militia_lp(ctx, char_id, war_fac, 2_500 * mult);

    if matches!(
        trader_type_id,
        TYPE_GURISTAS_LAUNDERING_FACILITY | TYPE_ANGEL_LAUNDERING_FACILITY
    ) {
        let camp_opt = ctx.db.with_snapshot(|snap| {
            snap.insurgency_runtime
                .campaigns
                .values()
                .find(|c| {
                    get_campaign_phase_and_timer(c, now).0 == PHASE_ACTIVE
                        && c.pirate_faction_id == war_fac
                        && c.fob_solar_system_id == sys_id
                })
                .map(|c| c.campaign_id)
        });
        if let Some(cid) = camp_opt {
            let _ = add_system_corruption(ctx, cid, sys_id, 15 * mult);
            record_insurgency_contribution(ctx, cid, char_id, war_fac, 15 * mult);
        }
    } else {
        let target_pirate = if matches!(war_fac, 500_002 | 500_003) {
            FACTION_ANGEL_CARTEL
        } else {
            FACTION_GURISTAS
        };
        let camp_opt = ctx.db.with_snapshot(|snap| {
            snap.insurgency_runtime
                .campaigns
                .values()
                .find(|c| {
                    get_campaign_phase_and_timer(c, now).0 == PHASE_ACTIVE
                        && c.pirate_faction_id == target_pirate
                })
                .map(|c| (c.campaign_id, c.fob_solar_system_id))
        });
        if let Some((cid, fob_sys)) = camp_opt {
            let _ = add_system_suppression(ctx, cid, fob_sys, 15 * mult);
            record_insurgency_contribution(ctx, cid, char_id, war_fac, 15 * mult);
        }
    }
}

async fn spawn_static_space_object(
    ctx: &ServerContext,
    sys_id: u32,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    pos: DVec3,
) {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let radius = ctx.sde.types.get(&type_id).map(|t| t.radius.max(50.0)).unwrap_or(250.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.entry(item_id).or_insert_with(|| InventoryItemRecord {
            item_id,
            type_id,
            owner_id,
            location_id: i64::from(sys_id),
            flag: InventoryFlag::None,
            quantity: 1,
            singleton: true,
            version: 1,
            ..Default::default()
        });
        snap.item_positions.insert(item_id, [pos.x, pos.y, pos.z]);
    });
    match sys_handle.get_ball_snapshot(item_id).await.ok().flatten() {
        None => {
            let _ = sys_handle
                .spawn_ball(Ball {
                    id: item_id,
                    type_id,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 0.0,
                    mass: 1.0e8,
                    radius,
                    speed_fraction: 0.0,
                    mode: MovementMode::Rigid,
                })
                .await;
        }
        Some((_, b)) if !matches!(b.mode, MovementMode::Rigid) => {
            let mut bp = sys_handle.ballpark.write().unwrap_or_else(|e| e.into_inner());
            if let Some(ball) = bp.balls.get_mut(&item_id) {
                ball.mode = MovementMode::Rigid;
                ball.position = pos;
            }
        }
        _ => {}
    }
}

async fn remove_space_object(ctx: &ServerContext, sys_id: u32, item_id: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&item_id);
        snap.item_positions.remove(&item_id);
    });
    let _ = ctx.systems.get_or_spawn(sys_id).remove_ball(item_id).await;
}

pub fn is_outpost_raid_gate(snap: &WorldStateSnapshot, gate_id: i64) -> bool {
    load_ops_state(snap).outpost_raids.contains_key(&gate_id)
}

pub async fn activate_outpost_raid_gate_in_space(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    gate_id: i64,
) -> Result<PyRep, BridgeRpcError> {
    let now = ctx.now_unix();
    let (raid, war_fac, ship_type_id, has_wcs) = ctx.db.with_snapshot(|snap| {
        let ops = load_ops_state(snap);
        let r = ops.outpost_raids.get(&gate_id).cloned();
        let wf = snap.characters.get(&char_id).and_then(|c| c.war_faction_id);
        let stid = snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(0);
        let wcs = snap.items.values().any(|it| {
            it.location_id == ship_id
                && (11..=19).contains(&(it.flag as u16))
                && ctx.sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 315)
        });
        (r, wf, stid, wcs)
    });
    let Some(raid) = raid.filter(|r| r.solar_system_id == sys_id && !r.completed_on_cooldown) else {
        return Err(BridgeRpcError {
            code: "GateNotFound".into(),
            message: format!("Outpost raid gate {gate_id} is not active in system {sys_id}"),
        });
    };
    let Some(fac) = war_fac.filter(|&f| matches!(f, 500_001..=500_004) || f == raid.pirate_faction_id) else {
        return Err(BridgeRpcError {
            code: "NotEnlistedInMilitia".into(),
            message: "Only Pirate or Empire Militia pilots can enter Corporate Outpost Raid complexes".into(),
        });
    };
    if has_wcs {
        return Err(BridgeRpcError {
            code: "WarpCoreStabilizerRestricted".into(),
            message: "Ships fitted with Warp Core Stabilizers cannot enter Insurgency Outpost Raid complexes".into(),
        });
    }
    let ship_grp = ctx.sde.types.get(&ship_type_id).map(|t| t.group_id).unwrap_or(0);
    if ship_type_id == 670 || !raid.size.allows_ship_group(ship_grp) {
        return Err(BridgeRpcError {
            code: "ShipRestrictedForComplex".into(),
            message: format!("Ship type {ship_type_id} is restricted from {:?} Corporate Outpost Raid", raid.size),
        });
    }

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let Some((_, sb)) = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten() else {
        return Err(BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: "Active ship ball not found in space".into(),
        });
    };
    let gate_pos = sys_handle
        .get_ball_snapshot(gate_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| (b.position, b.radius))
        .unwrap_or_else(|| (raid.size.gate_position(), 250.0));
    let surface_dist = (sb.position.distance(gate_pos.0) - sb.radius - gate_pos.1).max(0.0);
    if surface_dist > 2_500.0 {
        return Err(BridgeRpcError {
            code: "TooFarFromGate".into(),
            message: format!("Ship is {surface_dist:.0} m from acceleration gate (max 2500 m)"),
        });
    }

    let pocket = raid.size.pocket_position();
    let _ = sys_handle.set_ball_position(ship_id, pocket).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(ship_id, [pocket.x, pocket.y, pocket.z]);
    });

    let is_pirate_raider = matches!(fac, FACTION_GURISTAS | FACTION_ANGEL_CARTEL);
    let need_spawn = if is_pirate_raider {
        !raid.mordus_defender_cleared
            && match raid.mordus_defender_ball_id {
                Some(bid) => sys_handle.get_ball_snapshot(bid).await.ok().flatten().is_none(),
                None => true,
            }
    } else {
        !raid.pirate_defender_cleared
            && match raid.pirate_defender_ball_id {
                Some(bid) => sys_handle.get_ball_snapshot(bid).await.ok().flatten().is_none(),
                None => true,
            }
    };

    let spawned_def_id = if need_spawn {
        let (def_tid, def_fac) = if is_pirate_raider {
            (raid.size.mordus_defender_type_id(), 500_018_u32)
        } else {
            (raid.size.pirate_defender_type_id(raid.pirate_faction_id), raid.pirate_faction_id as u32)
        };
        Some(
            spawn_specialized_npc_entity(
                ctx,
                sys_id,
                def_tid,
                "InsurgencyOutpostDefender",
                def_fac,
                pocket + DVec3::new(3_500.0, 0.0, 0.0),
            )
            .await,
        )
    } else {
        None
    };

    ctx.db.with_mut_snapshot(|snap| {
        let mut ops = load_ops_state(snap);
        if let Some(r) = ops.outpost_raids.get_mut(&gate_id) {
            let was_empty = r.occupants.is_empty();
            if let Some(entry) = r.occupants.iter_mut().find(|(c, _)| *c == char_id) {
                entry.1 = ship_id;
            } else {
                r.occupants.push((char_id, ship_id));
            }
            if was_empty || r.last_step_unix == 0 {
                r.last_step_unix = now;
            }
            if let Some(bid) = spawned_def_id {
                if is_pirate_raider {
                    r.mordus_defender_ball_id = Some(bid);
                } else {
                    r.pirate_defender_ball_id = Some(bid);
                }
            }
        }
        save_ops_state(snap, &ops);
    });

    Ok(PyRep::keyval([
        ("gateID", PyRep::Integer(gate_id)),
        ("pocketPosition", PyRep::List(vec![PyRep::Real(pocket.x), PyRep::Real(pocket.y), PyRep::Real(pocket.z)])),
        ("activated", PyRep::Bool(true)),
    ]))
}

pub async fn sync_insurgency_complex_sites_and_facilities(ctx: &ServerContext, now_unix: u64) {
    let active_campaigns: Vec<(i64, i64, i64, u32, Vec<(u32, u8)>)> = ctx.db.with_snapshot(|snap| {
        snap.insurgency_runtime
            .campaigns
            .values()
            .filter(|c| get_campaign_phase_and_timer(c, now_unix).0 == PHASE_ACTIVE)
            .map(|c| {
                let sys_list = c
                    .systems
                    .iter()
                    .map(|&s| {
                        let c_st = snap.system_states.get(&s).map_or(0, |st| st.insurgency_corruption_stage);
                        (s, c_st)
                    })
                    .collect();
                (c.campaign_id, c.pirate_faction_id, c.target_faction_id, c.fob_solar_system_id, sys_list)
            })
            .collect()
    });

    for (camp_id, p_fac, t_fac, fob_sys, sys_list) in &active_campaigns {
        let lf_id = 1_891_100_000 + *camp_id;
        let lf_tid = if *p_fac == FACTION_ANGEL_CARTEL {
            TYPE_ANGEL_LAUNDERING_FACILITY
        } else {
            TYPE_GURISTAS_LAUNDERING_FACILITY
        };
        spawn_static_space_object(ctx, *fob_sys, lf_id, lf_tid, *p_fac, DVec3::new(1_150_000.0, 0.0, 45_000.0)).await;

        let hq_hubs: &[(u32, i64, u32, i64)] = if *p_fac == FACTION_ANGEL_CARTEL {
            &[
                (HQ_SYSTEM_MEHATOOR, 1_891_200_002, TYPE_IMPERIAL_CI_OUTPOST_HUB, 500_003),
                (HQ_SYSTEM_AMO, 1_891_200_004, TYPE_REPUBLIC_CI_OUTPOST_HUB, 500_002),
            ]
        } else {
            &[
                (HQ_SYSTEM_INTAKI, 1_891_200_001, TYPE_FEDERATION_CI_OUTPOST_HUB, 500_004),
                (HQ_SYSTEM_ONNAMON, 1_891_200_003, TYPE_STATE_CI_OUTPOST_HUB, 500_001),
            ]
        };
        for &(hq_sys, hub_id, hub_tid, emp_fac) in hq_hubs {
            spawn_static_space_object(ctx, hq_sys, hub_id, hub_tid, emp_fac, DVec3::new(1_150_000.0, 0.0, 35_000.0)).await;
        }

        for &(sys_id, c_st) in sys_list {
            for size in OutpostRaidSize::ALL {
                let gate_id = 1_893_100_000 + i64::from(sys_id) * 10 + size.slot_index();
                let beacon_id = gate_id + 1_000_000;
                let should_spawn = ctx.db.with_mut_snapshot(|snap| {
                    let mut ops = load_ops_state(snap);
                    let entry = ops.outpost_raids.entry(gate_id).or_insert_with(|| OutpostRaidInstance {
                        gate_id,
                        beacon_id,
                        campaign_id: *camp_id,
                        solar_system_id: sys_id,
                        pirate_faction_id: *p_fac,
                        target_faction_id: *t_fac,
                        size,
                        is_static: size == OutpostRaidSize::LargeAdv,
                        occupants: Vec::new(),
                        mordus_defender_ball_id: None,
                        pirate_defender_ball_id: None,
                        mordus_defender_cleared: false,
                        pirate_defender_cleared: false,
                        weighted_elapsed_secs: 0.0,
                        last_step_unix: now_unix,
                        completed_on_cooldown: false,
                        respawn_at_unix: 0,
                    });
                    if entry.completed_on_cooldown && now_unix >= entry.respawn_at_unix {
                        entry.completed_on_cooldown = false;
                        entry.respawn_at_unix = 0;
                        entry.weighted_elapsed_secs = 0.0;
                        entry.last_step_unix = now_unix;
                        entry.occupants.clear();
                        entry.mordus_defender_ball_id = None;
                        entry.pirate_defender_ball_id = None;
                        entry.mordus_defender_cleared = false;
                        entry.pirate_defender_cleared = false;
                    }
                    let active_now = !entry.completed_on_cooldown;
                    save_ops_state(snap, &ops);
                    active_now
                });
                if should_spawn {
                    let gpos = size.gate_position();
                    spawn_static_space_object(ctx, sys_id, gate_id, TYPE_OUTPOST_RAID_GATE, *t_fac, gpos).await;
                    spawn_static_space_object(ctx, sys_id, beacon_id, size.beacon_type_id(), *t_fac, gpos + DVec3::new(0.0, 250.0, 0.0)).await;
                }
            }
            if c_st >= 1 {
                ensure_mining_ambush_in_system(ctx, *camp_id, *p_fac, *t_fac, sys_id, now_unix).await;
            } else {
                despawn_mining_ambush_in_system(ctx, sys_id).await;
            }
        }
    }
}

async fn ensure_mining_ambush_in_system(
    ctx: &ServerContext,
    camp_id: i64,
    p_fac: i64,
    t_fac: i64,
    sys_id: u32,
    now_unix: u64,
) {
    let existing = ctx.db.with_snapshot(|snap| load_ops_state(snap).mining_ambushes.get(&sys_id).cloned());
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    if let Some(amb) = existing {
        if let Some(warp_until) = amb.rorqual_warping_off_until {
            if now_unix >= warp_until {
                let _ = sys_handle.remove_ball(amb.rorqual_ball_id).await;
                with_mut_combat_runtime(ctx, |rt| {
                    rt.npc_entities.remove(&amb.rorqual_ball_id);
                });
                ctx.db.with_mut_snapshot(|snap| {
                    let mut ops = load_ops_state(snap);
                    if let Some(a) = ops.mining_ambushes.get_mut(&sys_id) {
                        a.rorqual_warping_off_until = None;
                    }
                    save_ops_state(snap, &ops);
                });
            }
        }
        if amb.resolved && now_unix < amb.cooldown_until_unix {
            return;
        }
        if !amb.resolved && sys_handle.get_ball_snapshot(amb.rorqual_ball_id).await.ok().flatten().is_some() {
            return;
        }
    }

    let grid = DVec3::new(1_350_000.0, 0.0, -150_000.0);
    let beacon_id = 1_892_100_000 + i64::from(sys_id);
    spawn_static_space_object(ctx, sys_id, beacon_id, TYPE_MINING_AMBUSH_BEACON, 1_000_129, grid).await;

    let rorq_id = spawn_specialized_npc_entity(
        ctx,
        sys_id,
        TYPE_ORE_RORQUAL,
        "InsurgencyOreRorqual",
        500_014,
        grid + DVec3::new(500.0, 0.0, 0.0),
    )
    .await;
    with_mut_combat_runtime(ctx, |rt| {
        if let Some(npc) = rt.npc_entities.get_mut(&rorq_id) {
            npc.custom_cargo_loot = named_insurgency_roamer_loot_drop(TYPE_ORE_RORQUAL);
        }
    });
    if let Ok(Some(mut rcs)) = sys_handle.get_combat_state(rorq_id).await {
        rcs.warp_scramble_strength = 3;
        let _ = sys_handle.set_combat_state(rorq_id, rcs).await;
    }
    sys_handle.scrambled_balls.write().unwrap_or_else(|e| e.into_inner()).insert(rorq_id);

    let pirate_types: [u32; 3] = if p_fac == FACTION_ANGEL_CARTEL {
        [79_596, 79_598, 79_599]
    } else {
        [79_597, 79_600, 79_601]
    };
    let mut pirate_ids = Vec::with_capacity(3);
    for (idx, &ptid) in pirate_types.iter().enumerate() {
        pirate_ids.push(
            spawn_specialized_npc_entity(
                ctx,
                sys_id,
                ptid,
                "InsurgencyAmbushTackler",
                p_fac as u32,
                grid + DVec3::new(1_800.0 + (idx as f64) * 800.0, 0.0, 0.0),
            )
            .await,
        );
    }

    let mut ore_def_ids = Vec::with_capacity(2);
    for (idx, &(dtid, dfac)) in [(79_592_u32, 500_014_u32), (79_594_u32, 500_018_u32)].iter().enumerate() {
        ore_def_ids.push(
            spawn_specialized_npc_entity(
                ctx,
                sys_id,
                dtid,
                "InsurgencyAmbushOreDefender",
                dfac,
                grid + DVec3::new(-1_800.0, 0.0, (idx as f64) * 1_200.0),
            )
            .await,
        );
    }

    ctx.db.with_mut_snapshot(|snap| {
        let mut ops = load_ops_state(snap);
        ops.mining_ambushes.insert(
            sys_id,
            MiningAmbushInstance {
                campaign_id: camp_id,
                solar_system_id: sys_id,
                pirate_faction_id: p_fac,
                target_faction_id: t_fac,
                beacon_id,
                rorqual_ball_id: rorq_id,
                pirate_tackler_ids: pirate_ids,
                ore_defender_ids: ore_def_ids,
                ransom_elapsed_secs: 0,
                last_step_unix: now_unix,
                resolved: false,
                rorqual_warping_off_until: None,
                cooldown_until_unix: 0,
            },
        );
        save_ops_state(snap, &ops);
    });
}

pub async fn despawn_mining_ambush_in_system(ctx: &ServerContext, sys_id: u32) {
    let removed = ctx.db.with_mut_snapshot(|snap| {
        let mut ops = load_ops_state(snap);
        let amb = ops.mining_ambushes.remove(&sys_id);
        save_ops_state(snap, &ops);
        amb
    });
    if let Some(amb) = removed {
        remove_space_object(ctx, sys_id, amb.beacon_id).await;
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let mut all_npcs = vec![amb.rorqual_ball_id];
        all_npcs.extend(amb.pirate_tackler_ids);
        all_npcs.extend(amb.ore_defender_ids);
        with_mut_combat_runtime(ctx, |rt| {
            for bid in &all_npcs {
                rt.npc_entities.remove(bid);
            }
        });
        for bid in all_npcs {
            let _ = sys_handle.remove_ball(bid).await;
        }
    }
}

pub async fn cleanup_orphan_insurgency_complex_ops(ctx: &ServerContext, orphan_systems: &[u32]) {
    for &sys_id in orphan_systems {
        despawn_mining_ambush_in_system(ctx, sys_id).await;
        let raid_gates: Vec<(i64, i64, Option<i64>, Option<i64>)> = ctx.db.with_mut_snapshot(|snap| {
            let mut ops = load_ops_state(snap);
            let keys: Vec<i64> = ops
                .outpost_raids
                .iter()
                .filter(|(_, r)| r.solar_system_id == sys_id)
                .map(|(&gid, _)| gid)
                .collect();
            let mut out = Vec::new();
            for gid in keys {
                if let Some(r) = ops.outpost_raids.remove(&gid) {
                    out.push((r.gate_id, r.beacon_id, r.mordus_defender_ball_id, r.pirate_defender_ball_id));
                }
            }
            save_ops_state(snap, &ops);
            out
        });
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        for (gid, bid, md, pd) in raid_gates {
            remove_space_object(ctx, sys_id, gid).await;
            remove_space_object(ctx, sys_id, bid).await;
            for def in [md, pd].into_iter().flatten() {
                with_mut_combat_runtime(ctx, |rt| {
                    rt.npc_entities.remove(&def);
                });
                let _ = sys_handle.remove_ball(def).await;
            }
        }
    }
}

async fn collect_aligned_pilots_on_grid(
    ctx: &ServerContext,
    sys_id: u32,
    anchor: DVec3,
    range_m: f64,
    allow_pirate_fac: Option<i64>,
    allow_empire: bool,
) -> Vec<(i64, i64, u32)> {
    let candidates: Vec<(i64, i64, i64, u32)> = ctx.db.with_snapshot(|snap| {
        snap.characters
            .values()
            .filter_map(|c| {
                if c.solar_system_id != sys_id || c.station_id.is_some() || c.structure_id.is_some() || c.active_ship_id <= 0 {
                    return None;
                }
                let wf = c.war_faction_id?;
                if !((allow_empire && matches!(wf, 500_001..=500_004)) || allow_pirate_fac == Some(wf)) {
                    return None;
                }
                let stid = snap.items.get(&c.active_ship_id).map(|i| i.type_id).unwrap_or(0);
                if stid == 0 || stid == 670 {
                    return None;
                }
                let grp = ctx.sde.types.get(&stid).map(|t| t.group_id).unwrap_or(0);
                Some((c.character_id, c.active_ship_id, wf, grp))
            })
            .collect()
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut out = Vec::new();
    for (cid, shid, wf, grp) in candidates {
        if sys_handle.get_combat_state(shid).await.ok().flatten().is_some_and(|cs| cs.is_cloaked) {
            continue;
        }
        if let Ok(Some((_, b))) = sys_handle.get_ball_snapshot(shid).await {
            if b.position.distance(anchor) <= range_m {
                out.push((cid, wf, grp));
            }
        }
    }
    out.sort_by_key(|&(cid, _, _)| cid);
    out
}

async fn release_and_warp_off_rorqual(ctx: &ServerContext, sys_id: u32, rorqual_ball_id: i64) {
    with_mut_combat_runtime(ctx, |rt| {
        rt.npc_entities.remove(&rorqual_ball_id);
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.set_warp_scramble_strength(rorqual_ball_id, 0).await;
    sys_handle.scrambled_balls.write().unwrap_or_else(|e| e.into_inner()).remove(&rorqual_ball_id);
    let mut bp = sys_handle.ballpark.write().unwrap_or_else(|e| e.into_inner());
    if let Some(cs) = bp.combat_states.get_mut(&rorqual_ball_id) {
        cs.warp_scramble_strength = 0;
    }
    if let Some(pos) = bp.balls.get(&rorqual_ball_id).map(|b| b.position) {
        let _ = bp.set_mode(rorqual_ball_id, MovementMode::new_warp(pos, pos + DVec3::new(0.0, 0.0, 5_000_000_000.0), 3.0));
    }
    crate::system_actor::sync_ball_positions(&bp, &sys_handle.ball_positions, &sys_handle.scrambled_balls);
}

pub async fn on_insurgency_npc_destroyed(
    ctx: &ServerContext,
    sys_id: u32,
    npc_ball_id: i64,
    npc_type_id: u32,
    wreck_id: i64,
    killer_char_id: i64,
) {
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let mut ops = load_ops_state(snap);
        let mut changed = false;
        for r in ops.outpost_raids.values_mut() {
            if r.mordus_defender_ball_id == Some(npc_ball_id) {
                r.mordus_defender_ball_id = None;
                r.mordus_defender_cleared = true;
                r.last_step_unix = now;
                changed = true;
            }
            if r.pirate_defender_ball_id == Some(npc_ball_id) {
                r.pirate_defender_ball_id = None;
                r.pirate_defender_cleared = true;
                r.last_step_unix = now;
                changed = true;
            }
        }
        if changed {
            save_ops_state(snap, &ops);
        }
    });

    let amb_opt = ctx.db.with_snapshot(|snap| load_ops_state(snap).mining_ambushes.get(&sys_id).cloned());
    let Some(mut amb) = amb_opt.filter(|a| !a.resolved) else {
        return;
    };

    if npc_ball_id == amb.rorqual_ball_id || npc_type_id == TYPE_ORE_RORQUAL {
        if wreck_id > 0 {
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(w) = snap.items.get_mut(&wreck_id) {
                    w.type_id = TYPE_ORE_RORQUAL_WRECK;
                }
            });
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let mut bp = sys_handle.ballpark.write().unwrap_or_else(|e| e.into_inner());
            if let Some(wb) = bp.balls.get_mut(&wreck_id) {
                wb.type_id = TYPE_ORE_RORQUAL_WRECK;
            }
        }
        let _ = add_system_corruption(ctx, amb.campaign_id, sys_id, MINING_AMBUSH_CORRUPTION_PTS);
        amb.resolved = true;
        amb.cooldown_until_unix = now + MINING_AMBUSH_RESPAWN_SECS;
        let rem_npcs: Vec<i64> = amb.pirate_tackler_ids.drain(..).chain(amb.ore_defender_ids.drain(..)).collect();
        let beacon_id = amb.beacon_id;
        ctx.db.with_mut_snapshot(|snap| {
            let mut ops = load_ops_state(snap);
            ops.mining_ambushes.insert(sys_id, amb);
            save_ops_state(snap, &ops);
        });
        remove_space_object(ctx, sys_id, beacon_id).await;
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        with_mut_combat_runtime(ctx, |rt| {
            for bid in &rem_npcs {
                rt.npc_entities.remove(bid);
            }
        });
        for bid in rem_npcs {
            let _ = sys_handle.remove_ball(bid).await;
        }
        return;
    }

    let had_ore_def = !amb.ore_defender_ids.is_empty();
    amb.ore_defender_ids.retain(|&id| id != npc_ball_id);
    if had_ore_def && amb.ore_defender_ids.is_empty() {
        amb.last_step_unix = now;
    }
    let was_pirate_tackler = amb.pirate_tackler_ids.contains(&npc_ball_id);
    amb.pirate_tackler_ids.retain(|&id| id != npc_ball_id);

    if was_pirate_tackler && amb.pirate_tackler_ids.is_empty() {
        let grid = DVec3::new(1_350_000.0, 0.0, -150_000.0);
        let mut emp_pilots = collect_aligned_pilots_on_grid(ctx, sys_id, grid, 75_000.0, None, true).await;
        if killer_char_id > 0 && !emp_pilots.iter().any(|&(c, _, _)| c == killer_char_id) {
            if let Some(wf) = ctx
                .db
                .with_snapshot(|s| s.characters.get(&killer_char_id).and_then(|c| c.war_faction_id))
                .filter(|&f| matches!(f, 500_001..=500_004))
            {
                emp_pilots.push((killer_char_id, wf, 0));
            }
        }
        release_and_warp_off_rorqual(ctx, sys_id, amb.rorqual_ball_id).await;
        let _ = add_system_suppression(ctx, amb.campaign_id, sys_id, MINING_AMBUSH_SUPPRESSION_PTS);
        let per_pilot_base = share_site_lp_up_to_5(MINING_AMBUSH_BASE_LP, emp_pilots.len());
        let contrib_each = (MINING_AMBUSH_SUPPRESSION_PTS / (emp_pilots.len().max(1) as i64)).max(1);
        for (cid, wf, _) in emp_pilots {
            let mult = insurgency_suppression_empire_lp_multiplier(ctx, sys_id, wf);
            let lp = ((per_pilot_base as f64) * mult).round() as i64;
            credit_militia_lp(ctx, cid, wf, lp);
            record_insurgency_contribution(ctx, amb.campaign_id, cid, wf, contrib_each);
        }
        amb.resolved = true;
        amb.rorqual_warping_off_until = Some(now + 15);
        amb.cooldown_until_unix = now + MINING_AMBUSH_RESPAWN_SECS;
        let rem_defs: Vec<i64> = amb.ore_defender_ids.drain(..).collect();
        let beacon_id = amb.beacon_id;
        ctx.db.with_mut_snapshot(|snap| {
            let mut ops = load_ops_state(snap);
            ops.mining_ambushes.insert(sys_id, amb);
            save_ops_state(snap, &ops);
        });
        remove_space_object(ctx, sys_id, beacon_id).await;
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        with_mut_combat_runtime(ctx, |rt| {
            for bid in &rem_defs {
                rt.npc_entities.remove(bid);
            }
        });
        for bid in rem_defs {
            let _ = sys_handle.remove_ball(bid).await;
        }
        return;
    }

    ctx.db.with_mut_snapshot(|snap| {
        let mut ops = load_ops_state(snap);
        ops.mining_ambushes.insert(sys_id, amb);
        save_ops_state(snap, &ops);
    });
}

pub async fn tick_insurgency_complex_ops(ctx: &ServerContext, now_unix: u64) {
    sync_insurgency_complex_sites_and_facilities(ctx, now_unix).await;

    let ambushes: Vec<MiningAmbushInstance> = ctx.db.with_snapshot(|snap| {
        load_ops_state(snap).mining_ambushes.values().filter(|a| !a.resolved).cloned().collect()
    });
    for mut amb in ambushes {
        let sys_id = amb.solar_system_id;
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        amb.ore_defender_ids.retain(|&id| sys_handle.get_ball_position_sync(id).is_some());
        amb.pirate_tackler_ids.retain(|&id| sys_handle.get_ball_position_sync(id).is_some());
        let dt = now_unix.saturating_sub(amb.last_step_unix).max(1);
        amb.last_step_unix = now_unix;

        if !amb.pirate_tackler_ids.is_empty() {
            if let Ok(Some(mut rcs)) = sys_handle.get_combat_state(amb.rorqual_ball_id).await {
                if rcs.warp_scramble_strength < 1 {
                    rcs.warp_scramble_strength = 3;
                    let _ = sys_handle.set_combat_state(amb.rorqual_ball_id, rcs).await;
                }
            }
            sys_handle.scrambled_balls.write().unwrap_or_else(|e| e.into_inner()).insert(amb.rorqual_ball_id);
        }

        let grid = DVec3::new(1_350_000.0, 0.0, -150_000.0);
        let pir_pilots = collect_aligned_pilots_on_grid(ctx, sys_id, grid, 75_000.0, Some(amb.pirate_faction_id), false).await;
        let emp_pilots = collect_aligned_pilots_on_grid(ctx, sys_id, grid, 75_000.0, None, true).await;

        if amb.ore_defender_ids.is_empty() && !amb.pirate_tackler_ids.is_empty() && !pir_pilots.is_empty() && emp_pilots.is_empty() {
            amb.ransom_elapsed_secs = amb.ransom_elapsed_secs.saturating_add(dt);
            if amb.ransom_elapsed_secs >= MINING_AMBUSH_RANSOM_SECS {
                let lp_each = share_site_lp_up_to_5(MINING_AMBUSH_BASE_LP, pir_pilots.len());
                let isk_each = IskCents::from_f64_isk(5_000_000.0 / (pir_pilots.len() as f64));
                let contrib_each = (MINING_AMBUSH_CORRUPTION_PTS / (pir_pilots.len() as i64)).max(1);
                for &(cid, wf, _) in &pir_pilots {
                    credit_militia_lp(ctx, cid, wf, lp_each);
                    record_insurgency_contribution(ctx, amb.campaign_id, cid, wf, contrib_each);
                    ctx.db.with_mut_snapshot(|snap| {
                        if let Some(ch) = snap.characters.get_mut(&cid) {
                            ch.balance = IskCents(ch.balance.0.saturating_add(isk_each.0));
                            let bal = ch.balance;
                            snap.append_journal(cid, "MiningAmbushRansom", isk_each, bal, "ORE Rorqual ransom payout (Mining Op Ambush)");
                        }
                    });
                }
                let _ = add_system_corruption(ctx, amb.campaign_id, sys_id, MINING_AMBUSH_CORRUPTION_PTS);
                release_and_warp_off_rorqual(ctx, sys_id, amb.rorqual_ball_id).await;
                amb.resolved = true;
                amb.rorqual_warping_off_until = Some(now_unix + 15);
                amb.cooldown_until_unix = now_unix + MINING_AMBUSH_RESPAWN_SECS;
                let rem_pirs: Vec<i64> = amb.pirate_tackler_ids.drain(..).collect();
                let beacon_id = amb.beacon_id;
                ctx.db.with_mut_snapshot(|snap| {
                    let mut ops = load_ops_state(snap);
                    ops.mining_ambushes.insert(sys_id, amb);
                    save_ops_state(snap, &ops);
                });
                remove_space_object(ctx, sys_id, beacon_id).await;
                with_mut_combat_runtime(ctx, |rt| {
                    for bid in &rem_pirs {
                        rt.npc_entities.remove(bid);
                    }
                });
                for bid in rem_pirs {
                    let _ = sys_handle.remove_ball(bid).await;
                }
                continue;
            }
        }
        ctx.db.with_mut_snapshot(|snap| {
            let mut ops = load_ops_state(snap);
            ops.mining_ambushes.insert(sys_id, amb);
            save_ops_state(snap, &ops);
        });
    }

    let raids: Vec<OutpostRaidInstance> = ctx.db.with_snapshot(|snap| {
        load_ops_state(snap).outpost_raids.values().filter(|r| !r.completed_on_cooldown).cloned().collect()
    });
    for mut raid in raids {
        let sys_id = raid.solar_system_id;
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let pocket = raid.size.pocket_position();
        let dt = now_unix.saturating_sub(raid.last_step_unix).max(1);
        raid.last_step_unix = now_unix;

        if let Some(md) = raid.mordus_defender_ball_id {
            if sys_handle.get_ball_snapshot(md).await.ok().flatten().is_none() {
                raid.mordus_defender_ball_id = None;
                raid.mordus_defender_cleared = true;
            }
        }
        if let Some(pd) = raid.pirate_defender_ball_id {
            if sys_handle.get_ball_snapshot(pd).await.ok().flatten().is_none() {
                raid.pirate_defender_ball_id = None;
                raid.pirate_defender_cleared = true;
            }
        }

        let pir_pilots = collect_aligned_pilots_on_grid(ctx, sys_id, pocket, 30_000.0, Some(raid.pirate_faction_id), false).await;
        let emp_pilots = collect_aligned_pilots_on_grid(ctx, sys_id, pocket, 30_000.0, None, true).await;

        if (pir_pilots.is_empty() && emp_pilots.is_empty()) || (!pir_pilots.is_empty() && !emp_pilots.is_empty()) {
            ctx.db.with_mut_snapshot(|snap| {
                let mut ops = load_ops_state(snap);
                ops.outpost_raids.insert(raid.gate_id, raid);
                save_ops_state(snap, &ops);
            });
            continue;
        }

        let capturing_pirates = !pir_pilots.is_empty();
        if capturing_pirates && !raid.mordus_defender_cleared {
            if raid.mordus_defender_ball_id.is_none() {
                raid.mordus_defender_ball_id = Some(
                    spawn_specialized_npc_entity(ctx, sys_id, raid.size.mordus_defender_type_id(), "InsurgencyOutpostDefender", 500_018, pocket + DVec3::new(3_500.0, 0.0, 0.0)).await,
                );
            }
            ctx.db.with_mut_snapshot(|snap| {
                let mut ops = load_ops_state(snap);
                ops.outpost_raids.insert(raid.gate_id, raid);
                save_ops_state(snap, &ops);
            });
            continue;
        }
        if !capturing_pirates && !raid.pirate_defender_cleared {
            if raid.pirate_defender_ball_id.is_none() {
                raid.pirate_defender_ball_id = Some(
                    spawn_specialized_npc_entity(ctx, sys_id, raid.size.pirate_defender_type_id(raid.pirate_faction_id), "InsurgencyOutpostDefender", raid.pirate_faction_id as u32, pocket + DVec3::new(3_500.0, 0.0, 0.0)).await,
                );
            }
            ctx.db.with_mut_snapshot(|snap| {
                let mut ops = load_ops_state(snap);
                ops.outpost_raids.insert(raid.gate_id, raid);
                save_ops_state(snap, &ops);
            });
            continue;
        }

        let active_pilots = if capturing_pirates { &pir_pilots } else { &emp_pilots };
        let any_bc = raid.size == OutpostRaidSize::Moderate && active_pilots.iter().any(|&(_, _, grp)| is_battlecruiser_group(grp));
        raid.weighted_elapsed_secs += (dt as f64) * if any_bc { 1.25 } else { 1.0 };

        if raid.weighted_elapsed_secs >= raid.size.base_capture_duration_secs() as f64 {
            let subpts = raid.size.contribution_subpoints();
            let _ = add_percentage_campaign_points(ctx, raid.campaign_id, sys_id, capturing_pirates, subpts);
            let shared_lp = share_site_lp_up_to_5(raid.size.base_lp_reward(), active_pilots.len());
            let contrib_pts = ((subpts / 100).max(1) / (active_pilots.len() as i64)).max(1);
            for &(cid, wf, grp) in active_pilots {
                let bc_mult = if raid.size == OutpostRaidSize::Moderate && is_battlecruiser_group(grp) { 1.25 } else { 1.0 };
                let supp_mult = if capturing_pirates { 1.0 } else { insurgency_suppression_empire_lp_multiplier(ctx, sys_id, wf) };
                let lp = ((shared_lp as f64) * bc_mult * supp_mult).round() as i64;
                credit_militia_lp(ctx, cid, wf, lp);
                record_insurgency_contribution(ctx, raid.campaign_id, cid, wf, contrib_pts);
            }

            let gid = raid.gate_id;
            let bid = raid.beacon_id;
            let def_balls = [raid.mordus_defender_ball_id, raid.pirate_defender_ball_id];
            raid.completed_on_cooldown = true;
            raid.respawn_at_unix = now_unix + raid.size.respawn_delay_secs();
            raid.occupants.clear();
            raid.mordus_defender_ball_id = None;
            raid.pirate_defender_ball_id = None;

            ctx.db.with_mut_snapshot(|snap| {
                let mut ops = load_ops_state(snap);
                ops.outpost_raids.insert(gid, raid);
                save_ops_state(snap, &ops);
            });
            remove_space_object(ctx, sys_id, gid).await;
            remove_space_object(ctx, sys_id, bid).await;
            for def in def_balls.into_iter().flatten() {
                with_mut_combat_runtime(ctx, |rt| {
                    rt.npc_entities.remove(&def);
                });
                let _ = sys_handle.remove_ball(def).await;
            }
        } else {
            ctx.db.with_mut_snapshot(|snap| {
                let mut ops = load_ops_state(snap);
                ops.outpost_raids.insert(raid.gate_id, raid);
                save_ops_state(snap, &ops);
            });
        }
    }
}

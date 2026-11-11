//! Authentic RPC + GM-slash edge-case coverage across PvE (Abyssal, Void,
//! Dungeons/Escalations, ESS Reserve Banks, Hacking), Agents/Missions/Epic Arcs/LP Store,
//! Combat (Crimewatch/Safety/Duels/KillRights, Targeting/Weapons/Ammo/Overload),
//! and Space Exploration (Probe Scanning, Wormholes, Random Filaments, Cyno Beacons).
//!
//! All tests drive real RPCs and GM slash commands deterministically with zero direct
//! snapshot mutations (`with_mut_snapshot`) in test code.

use crate::common::{gm_pilot_in_space, login_pilot, pin_clock, slash_cmd, LivePilot};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const SYS_JITA: u32 = 30_000_142;
const STA_JITA: i64 = 60_003_760;
const SYS_ZARZAKH: u32 = 30_100_000;
const REGION_POCHVEN: u32 = 10_000_070;

// Common ship & module types verified against SDE dynamically where needed.
const TYPE_TRISTAN: u32 = 593;
const TYPE_CARACAL: u32 = 621;
const TYPE_ROKH: u32 = 24_688; // Battleship (disallowed in abyss / gated DED)
const TYPE_CAPSULE: u32 = 670;
const TYPE_CORE_PROBE_LAUNCHER: u32 = 17_938;
const TYPE_CORE_SCAN_PROBE: u32 = 30_013;
const TYPE_COMBAT_SCAN_PROBE: u32 = 30_028;
const TYPE_ANTIMATTER_S: u32 = 222;
const TYPE_CRUISE_MISSILE: u32 = 205; // Incompatible ammo for hybrid turret
const TYPE_CYNO_GENERATOR: u32 = 21_096;

fn find_small_blaster_type(ctx: &ServerContext) -> u32 {
    ctx.sde
        .types
        .values()
        .filter(|t| t.name == "Light Ion Blaster I")
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .expect("Light Ion Blaster I in SDE")
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

async fn rpc_bound(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    bound_id: u64,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, Some(bound_id), method, args, &HashMap::new(), None)
        .await
}

async fn create_item(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, qty: i32) -> i64 {
    let rep = slash_cmd(ctx, sess, &format!("/createitem {type_id} {qty}")).await;
    rep.dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("/createitem returns itemID")
}

async fn fit_module(ctx: &ServerContext, sess: &mut SessionState, type_id: u32) -> i64 {
    if let Some(t) = ctx.sde.types.get(&type_id) {
        let req_skills: Vec<u32> = [182u16, 183, 184, 1285, 1289, 1290]
            .into_iter()
            .filter_map(|aid| t.default_attributes.get(&aid).map(|&v| v as u32))
            .filter(|&sid| sid > 0)
            .collect();
        for sid in req_skills {
            let _ = slash_cmd(ctx, sess, &format!("/giveskill {sid} 5")).await;
        }
    }
    let rep = slash_cmd(ctx, sess, &format!("/fit me {type_id}")).await;
    rep.dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("/fit returns itemID")
}

async fn dock_at_station(ctx: &ServerContext, p: &mut LivePilot, sys_id: u32, station_id: i64) {
    if p.sess.station_id == Some(station_id) && p.sess.solar_system_id == Some(sys_id) {
        return;
    }
    if p.sess.station_id.is_some() || p.sess.structure_id.is_some() {
        rpc(ctx, &mut p.sess, "stationSvc", "Undock", &[])
            .await
            .expect("Undock");
    }
    if p.sess.solar_system_id != Some(sys_id) {
        slash_cmd(ctx, &mut p.sess, &format!("/tr {sys_id}")).await;
    }
    let st_pos = ctx
        .sde
        .stations
        .get(&station_id)
        .map(|s| s.position)
        .unwrap_or(DVec3::ZERO);
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .set_ball_position(p.ship_id, st_pos + DVec3::new(300.0, 0.0, 0.0))
        .await;
    sys_h.step_manual(0.1).await.unwrap();
    ctx.advance_secs(12);
    rpc(ctx, &mut p.sess, "stationSvc", "Dock", &[PyRep::Integer(station_id)])
        .await
        .expect("Dock at station");
}

async fn lock_target(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32, ship_id: i64, target_id: i64) {
    rpc(ctx, sess, "dogmaIM", "AddTarget", &[PyRep::Integer(target_id)])
        .await
        .expect("AddTarget");
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for _ in 0..40 {
        sys_h.step_manual(1.0).await.unwrap();
        ctx.advance_secs(1);
        if sys_h
            .get_target_locks(ship_id)
            .await
            .unwrap()
            .iter()
            .any(|l| l.target_id == target_id && l.locked)
        {
            return;
        }
    }
    panic!("target lock did not complete for ship {ship_id} -> {target_id}");
}

fn find_nullsec_system(ctx: &ServerContext) -> u32 {
    ctx.db
        .with_snapshot(|snap| {
            snap.system_states
                .keys()
                .copied()
                .filter(|&id| {
                    id < 31_000_000
                        && ctx
                            .sde
                            .solar_systems
                            .get(&id)
                            .is_some_and(|s| s.security_status <= 0.0)
                })
                .min()
        })
        .unwrap_or_else(|| {
            ctx.sde
                .solar_systems
                .values()
                .filter(|s| s.solar_system_id < 31_000_000 && s.security_status <= 0.0)
                .min_by_key(|s| s.solar_system_id)
                .expect("nullsec solar system")
                .solar_system_id
        })
}

fn find_lowsec_system(ctx: &ServerContext) -> u32 {
    ctx.sde
        .solar_systems
        .values()
        .filter(|s| {
            s.solar_system_id < 31_000_000
                && s.security_status > 0.0
                && s.security_status < 0.45
                && s.region_id != REGION_POCHVEN
        })
        .min_by_key(|s| s.solar_system_id)
        .expect("lowsec solar system")
        .solar_system_id
}

fn find_pochven_system(ctx: &ServerContext) -> u32 {
    ctx.sde
        .solar_systems
        .values()
        .filter(|s| s.region_id == REGION_POCHVEN)
        .min_by_key(|s| s.solar_system_id)
        .expect("pochven solar system")
        .solar_system_id
}

fn find_wormhole_system(ctx: &ServerContext) -> u32 {
    ctx.sde
        .solar_systems
        .values()
        .filter(|s| (31_000_000..32_000_000).contains(&s.solar_system_id))
        .min_by_key(|s| s.solar_system_id)
        .expect("wormhole solar system")
        .solar_system_id
}

fn find_filament_type(ctx: &ServerContext, prefix: &str) -> u32 {
    ctx.sde
        .types
        .values()
        .filter(|t| t.name.starts_with(prefix) && t.group_id == 1979)
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .unwrap_or_else(|| panic!("filament starting with {prefix} not found"))
}

// ============================================================================
// GROUP 1: Abyssal Deadspace, Void Gates, ESS Reserve Banks, & Expeditions
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edge_abyssal_deployment_gates_and_in_pocket_restrictions() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);

    let t0_calm = find_filament_type(&ctx, "Tranquil ");
    let t1_calm = find_filament_type(&ctx, "Calm ");
    let t2_fierce = find_filament_type(&ctx, "Agitated ");

    // Docked deployment -> ShipDocked
    let mut gm = gm_pilot_in_space(&ctx, 5501, 95_500_001, TYPE_CARACAL, SYS_JITA).await;
    let docked_fil = create_item(&ctx, &mut gm.sess, t0_calm, 3).await;
    dock_at_station(&ctx, &mut gm, SYS_JITA, STA_JITA).await;
    let e_dock = rpc(
        &ctx,
        &mut gm.sess,
        "abyssalMgr",
        "AbyssalEntranceDeployment",
        &[PyRep::Integer(docked_fil)],
    )
    .await
    .unwrap_err();
    assert_eq!(e_dock.code, "ShipDocked");

    // Undock to Jita space
    rpc(&ctx, &mut gm.sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock");
    // Session change timer active immediately after undock -> SessionChangeInProgress
    assert_eq!(
        rpc(
            &ctx,
            &mut gm.sess,
            "abyssalMgr",
            "AbyssalEntranceDeployment",
            &[PyRep::Integer(docked_fil)],
        )
        .await
        .unwrap_err()
        .code,
        "SessionChangeInProgress"
    );
    ctx.advance_secs(12);

    // Outside abyssal space gate/ready checks -> NotInAbyssalSpace
    assert_eq!(
        rpc(&ctx, &mut gm.sess, "abyssalMgr", "ClientIsReady", &[])
            .await
            .unwrap_err()
            .code,
        "NotInAbyssalSpace"
    );
    assert_eq!(
        rpc(&ctx, &mut gm.sess, "abyssalMgr", "AbyssalGateActivation", &[])
            .await
            .unwrap_err()
            .code,
        "NotInAbyssalSpace"
    );
    assert_eq!(
        rpc(&ctx, &mut gm.sess, "abyssalMgr", "AbyssalEndGateActivation", &[])
            .await
            .unwrap_err()
            .code,
        "NotInAbyssalSpace"
    );
    assert_eq!(
        rpc(&ctx, &mut gm.sess, "voidSpaceMgr", "VoidSpaceEndGateActivation", &[])
            .await
            .unwrap_err()
            .code,
        "NotInVoidSpace"
    );

    // Move well clear (> 1000 km) of Jita station before testing filament deployment validation
    let jita_h = ctx.systems.get_or_spawn(SYS_JITA);
    jita_h
        .set_ball_position(gm.ship_id, DVec3::new(1.0e11, 0.0, 0.0))
        .await;
    jita_h.step_manual(0.1).await.unwrap();

    // Non-existent & non-filament items -> ItemNotFound / InvalidAbyssalFilament
    assert_eq!(
        rpc(
            &ctx,
            &mut gm.sess,
            "abyssalMgr",
            "AbyssalEntranceDeployment",
            &[PyRep::Integer(999_888_777)],
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotFound"
    );
    let non_fil = create_item(&ctx, &mut gm.sess, TYPE_ANTIMATTER_S, 5).await;
    assert_eq!(
        rpc(
            &ctx,
            &mut gm.sess,
            "abyssalMgr",
            "AbyssalEntranceDeployment",
            &[PyRep::Integer(non_fil)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidAbyssalFilament"
    );

    // T2 filament in highsec -> safety green / highsec tier policy
    let agi_fil = create_item(&ctx, &mut gm.sess, t2_fierce, 1).await;
    rpc(
        &ctx,
        &mut gm.sess,
        "crimewatch",
        "SetSafetyLevel",
        &[PyRep::Integer(2)],
    )
    .await
    .expect("SetSafetyLevel green");
    let e_safety_or_hs = rpc(
        &ctx,
        &mut gm.sess,
        "abyssalMgr",
        "AbyssalEntranceDeployment",
        &[PyRep::Integer(agi_fil)],
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            e_safety_or_hs.code.as_str(),
            "CantDoInCurrentSafetyLevel" | "FilamentDisallowedInHighSec"
        ),
        "unexpected error code: {}",
        e_safety_or_hs.code
    );

    // Disallowed hull (Battleship) in space -> DisallowedShipType
    let mut bs_gm = gm_pilot_in_space(&ctx, 5502, 95_500_002, TYPE_ROKH, SYS_JITA).await;
    let bs_fil = create_item(&ctx, &mut bs_gm.sess, t1_calm, 1).await;
    assert_eq!(
        rpc(
            &ctx,
            &mut bs_gm.sess,
            "abyssalMgr",
            "AbyssalEntranceDeployment",
            &[PyRep::Integer(bs_fil)],
        )
        .await
        .unwrap_err()
        .code,
        "DisallowedShipType"
    );

    // Valid T0 deployment in cruiser -> trace spawned -> proximity blocks second deployment
    let space_fil = create_item(&ctx, &mut gm.sess, t0_calm, 3).await;
    let dep = rpc(
        &ctx,
        &mut gm.sess,
        "abyssalMgr",
        "AbyssalEntranceDeployment",
        &[PyRep::Integer(space_fil)],
    )
    .await
    .expect("AbyssalEntranceDeployment T0");
    let trace_id = dep
        .dict_get("traceBallID")
        .and_then(|v| v.as_i64())
        .expect("traceBallID");

    let e_prox = rpc(
        &ctx,
        &mut gm.sess,
        "abyssalMgr",
        "AbyssalEntranceDeployment",
        &[PyRep::Integer(space_fil)],
    )
    .await
    .unwrap_err();
    assert_eq!(e_prox.code, "ProximityRestriction");

    // Enter abyssal pocket via entrance gate
    let enter_rep = rpc(
        &ctx,
        &mut gm.sess,
        "abyssalMgr",
        "AbyssalEntranceGateActivation",
        &[PyRep::Integer(trace_id)],
    )
    .await
    .expect("Enter abyssal pocket");
    let pocket_id = enter_rep
        .dict_get("pocketID")
        .and_then(|v| v.as_i64())
        .expect("pocketID");
    let r1_gate_id = 1_895_000_000 + pocket_id * 10 + 1;

    // In-pocket restrictions: Local chat, D-scan blocked, EndGate before final room
    assert_eq!(
        rpc(&ctx, &mut gm.sess, "scanMgr", "ConeScan", &[])
            .await
            .unwrap_err()
            .code,
        "CannotDscanInThisSystem"
    );
    assert_eq!(
        rpc(&ctx, &mut gm.sess, "abyssalMgr", "AbyssalEndGateActivation", &[])
            .await
            .unwrap_err()
            .code,
        "NotInFinalRoom"
    );
    // Out-of-range / locked / invalid gate checks inside pocket
    assert_eq!(
        rpc(
            &ctx,
            &mut gm.sess,
            "abyssalMgr",
            "AbyssalGateActivation",
            &[PyRep::Integer(999_999_111)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidGate"
    );
    let e_locked = rpc(
        &ctx,
        &mut gm.sess,
        "abyssalMgr",
        "AbyssalGateActivation",
        &[PyRep::Integer(r1_gate_id)],
    )
    .await
    .unwrap_err();
    assert!(
        matches!(e_locked.code.as_str(), "GateLocked" | "GateOutOfRange"),
        "unexpected gate activation error: {}",
        e_locked.code
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edge_ess_main_and_reserve_bank_validation_and_gates() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);

    let null_sys = find_nullsec_system(&ctx);
    let mut gm = gm_pilot_in_space(&ctx, 5503, 95_500_003, TYPE_CARACAL, SYS_JITA).await;

    // In highsec: EssOnlyInNullSec & NotLinkedToEss
    assert_eq!(
        rpc(
            &ctx,
            &mut gm.sess,
            "essMgr",
            "AttemptLinkToMainBank",
            &[],
        )
        .await
        .unwrap_err()
        .code,
        "EssOnlyInNullSec"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut gm.sess,
            "essMgr",
            "AttemptLinkToReserveBank",
            &[],
        )
        .await
        .unwrap_err()
        .code,
        "EssOnlyInNullSec"
    );
    assert_eq!(
        rpc(&ctx, &mut gm.sess, "essMgr", "RequestMainBankUnlink", &[])
            .await
            .expect("RequestMainBankUnlink when unlinked"),
        PyRep::Bool(false)
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut gm.sess,
            "essMgr",
            "RequestUnlockReserveBank",
            &[PyRep::Integer(0)],
        )
        .await
        .unwrap_err()
        .code,
        "EssOnlyInNullSec"
    );

    // Move to nullsec & trigger ESS beacon seeding via GetDataForClientSolarSystem
    slash_cmd(&ctx, &mut gm.sess, &format!("/tr {null_sys}")).await;
    let ess_data = rpc(
        &ctx,
        &mut gm.sess,
        "essMgr",
        "GetDataForClientSolarSystem",
        &[],
    )
    .await
    .expect("GetDataForClientSolarSystem");
    assert!(matches!(ess_data, PyRep::Dict(_) | PyRep::None));

    // Find ESS beacon ball in null_sys read-only
    let sys_h = ctx.systems.get_or_spawn(null_sys);
    let balls = sys_h.list_balls().await.unwrap();
    let (ess_id, ess_pos, gate_opt) = {
        let beacon = balls
            .iter()
            .filter(|b| {
                b.id == 820_000_000 + i64::from(null_sys)
                    || matches!(b.type_id, 32882 | 55914 | 56002)
            })
            .min_by_key(|b| b.id)
            .expect("ESS beacon ball seeded in nullsec");
        let gate = balls
            .iter()
            .filter(|b| {
                b.id == 821_000_000 + i64::from(null_sys)
                    || ctx
                        .sde
                        .types
                        .get(&b.type_id)
                        .is_some_and(|t| t.name.contains("Acceleration Gate"))
            })
            .min_by_key(|b| b.id)
            .map(|b| (b.id, b.position));
        (beacon.id, beacon.position, gate)
    };

    // Safety green blocks ESS link
    rpc(
        &ctx,
        &mut gm.sess,
        "crimewatch",
        "SetSafetyLevel",
        &[PyRep::Integer(2)],
    )
    .await
    .expect("SetSafetyLevel green");
    assert_eq!(
        rpc(
            &ctx,
            &mut gm.sess,
            "essMgr",
            "AttemptLinkToMainBank",
            &[PyRep::Integer(ess_id)],
        )
        .await
        .unwrap_err()
        .code,
        "CantDoInCurrentSafetyLevel"
    );

    // Safety red but far from ESS -> OutsideEssLinkRange
    rpc(
        &ctx,
        &mut gm.sess,
        "crimewatch",
        "SetSafetyLevel",
        &[PyRep::Integer(0)],
    )
    .await
    .expect("SetSafetyLevel red");
    sys_h
        .set_ball_position(gm.ship_id, ess_pos + DVec3::new(100_000.0, 0.0, 0.0))
        .await;
    sys_h.step_manual(0.1).await.unwrap();
    assert_eq!(
        rpc(
            &ctx,
            &mut gm.sess,
            "essMgr",
            "AttemptLinkToMainBank",
            &[PyRep::Integer(ess_id)],
        )
        .await
        .unwrap_err()
        .code,
        "OutsideEssLinkRange"
    );

    // Acceleration gate out-of-range check if present
    if let Some((gate_id, gate_pos)) = gate_opt {
        sys_h
            .set_ball_position(gm.ship_id, gate_pos + DVec3::new(50_000.0, 0.0, 0.0))
            .await;
        sys_h.step_manual(0.1).await.unwrap();
        let e_gate = rpc(
            &ctx,
            &mut gm.sess,
            "beyonce",
            "ActivateAccelerationGate",
            &[PyRep::Integer(gate_id)],
        )
        .await
        .unwrap_err();
        assert_eq!(e_gate.code, "OutsideGateActivationRange");
    }

    // Move within ESS link range (1 km <= 2.5 km)
    sys_h
        .set_ball_position(gm.ship_id, ess_pos + DVec3::new(1_000.0, 0.0, 0.0))
        .await;
    sys_h.step_manual(0.1).await.unwrap();

    // Missing Reserve Key -> MissingEssReserveSecurityKey
    assert_eq!(
        rpc(
            &ctx,
            &mut gm.sess,
            "essMgr",
            "AttemptLinkToReserveBank",
            &[PyRep::Integer(ess_id)],
        )
        .await
        .unwrap_err()
        .code,
        "MissingEssReserveSecurityKey"
    );

    // Main bank when empty -> EssBankEmpty
    assert_eq!(
        rpc(
            &ctx,
            &mut gm.sess,
            "essMgr",
            "AttemptLinkToMainBank",
            &[PyRep::Integer(ess_id)],
        )
        .await
        .unwrap_err()
        .code,
        "EssBankEmpty"
    );

    // Wrong-quadrant Reserve Bank Key -> EssReserveKeyWrongQuadrant
    let wrong_key = ctx
        .sde
        .types
        .values()
        .filter(|t| t.name.contains("Key") && t.name.contains("Reserve"))
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id);
    if let Some(key_tid) = wrong_key {
        let _key_item = create_item(&ctx, &mut gm.sess, key_tid, 1).await;
        let res = rpc(
            &ctx,
            &mut gm.sess,
            "essMgr",
            "RequestUnlockReserveBank",
            &[PyRep::Integer(ess_id)],
        )
        .await;
        if let Err(err) = res {
            assert!(
                matches!(
                    err.code.as_str(),
                    "EssReserveKeyWrongQuadrant" | "MissingEssReserveSecurityKey" | "EssBankEmpty"
                ),
                "unexpected reserve unlock error: {}",
                err.code
            );
        }
    }

    // Ship docked in nullsec station (if station exists) or capsule blocks ESS
    let mut pod_gm = gm_pilot_in_space(&ctx, 5504, 95_500_004, TYPE_CAPSULE, null_sys).await;
    let pod_sys = ctx.systems.get_or_spawn(null_sys);
    pod_sys
        .set_ball_position(pod_gm.ship_id, ess_pos + DVec3::new(2_000.0, 0.0, 0.0))
        .await;
    pod_sys.step_manual(0.1).await.unwrap();
    rpc(
        &ctx,
        &mut pod_gm.sess,
        "crimewatch",
        "SetSafetyLevel",
        &[PyRep::Integer(0)],
    )
    .await
    .expect("safety red");
    assert_eq!(
        rpc(
            &ctx,
            &mut pod_gm.sess,
            "essMgr",
            "AttemptLinkToMainBank",
            &[PyRep::Integer(ess_id)],
        )
        .await
        .unwrap_err()
        .code,
        "CantDoThatInPod"
    );

    // Unknown dungeon room / anomaly gate advance -> NotInSpace when docked
    dock_at_station(&ctx, &mut gm, SYS_JITA, STA_JITA).await;
    let e_anom = rpc(
        &ctx,
        &mut gm.sess,
        "dungeon",
        "EnterCombatAnomaly",
        &[PyRep::String("NON-EXISTENT".into()), PyRep::Bool(true)],
    )
    .await;
    assert!(e_anom.is_err());
}

// ============================================================================
// GROUP 2: Agents, Missions, R&D, Locator Agents, Epic Arcs & LP Store
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edge_agent_missions_colocation_standing_alpha_clone_and_actions() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);

    let mut gm = gm_pilot_in_space(&ctx, 5505, 95_500_005, TYPE_TRISTAN, SYS_JITA).await;

    // Pick deterministic L1 mission agent, L4 mission agent, Storyline agent, FW agent
    let l1_agent = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.level == 1 && a.agent_type_id == 2 && a.station_id.is_some() && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id)
        .expect("L1 agent")
        .clone();
    let l4_agent = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.level == 4 && a.agent_type_id == 2 && a.station_id.is_some() && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id)
        .expect("L4 agent")
        .clone();

    // UnknownAgent
    assert_eq!(
        rpc(
            &ctx,
            &mut gm.sess,
            "agentMgr",
            "DoAction",
            &[PyRep::Integer(1), PyRep::Integer(999_999_991)],
        )
        .await
        .unwrap_err()
        .code,
        "UnknownAgent"
    );

    // AgentNotCoLocated while in space / wrong station
    dock_at_station(&ctx, &mut gm, SYS_JITA, STA_JITA).await;
    if l1_agent.station_id != Some(STA_JITA) {
        assert_eq!(
            rpc(
                &ctx,
                &mut gm.sess,
                "agentMgr",
                "DoAction",
                &[PyRep::Integer(1), PyRep::Integer(l1_agent.agent_id)],
            )
            .await
            .unwrap_err()
            .code,
            "AgentNotCoLocated"
        );
    }

    // StorylineAgentRequiresReferral (if any storyline agent exists)
    if let Some(story_agent) = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.agent_type_id == 6 && a.station_id.is_some() && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id)
    {
        dock_at_station(
            &ctx,
            &mut gm,
            story_agent.solar_system_id.unwrap(),
            story_agent.station_id.unwrap(),
        )
        .await;
        assert_eq!(
            rpc(
                &ctx,
                &mut gm.sess,
                "agentMgr",
                "DoAction",
                &[PyRep::Integer(1), PyRep::Integer(story_agent.agent_id)],
            )
            .await
            .unwrap_err()
            .code,
            "StorylineAgentRequiresReferral"
        );
    }

    // Co-locate with L1 agent: MissionNotActive, Incomplete, Quit, Decline
    dock_at_station(
        &ctx,
        &mut gm,
        l1_agent.solar_system_id.unwrap(),
        l1_agent.station_id.unwrap(),
    )
    .await;
    assert_eq!(
        rpc(
            &ctx,
            &mut gm.sess,
            "agentMgr",
            "DoAction",
            &[PyRep::Integer(6), PyRep::Integer(l1_agent.agent_id)],
        )
        .await
        .unwrap_err()
        .code,
        "MissionNotActive"
    );
    // Request & accept mission -> try Complete immediately -> MissionIncomplete
    let _req = rpc(
        &ctx,
        &mut gm.sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(l1_agent.agent_id)],
    )
    .await
    .expect("Request L1 mission");
    let _acc = rpc(
        &ctx,
        &mut gm.sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(l1_agent.agent_id)],
    )
    .await
    .expect("Accept L1 mission");
    let e_inc = rpc(
        &ctx,
        &mut gm.sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(l1_agent.agent_id)],
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            e_inc.code.as_str(),
            "MissionIncomplete" | "CourierNotAtDestinationStation" | "MissingCourierCargo"
        ),
        "unexpected incomplete mission error: {}",
        e_inc.code
    );

    // Quit mission -> then verify standing/Alpha checks on L4
    let _quit = rpc(
        &ctx,
        &mut gm.sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(11), PyRep::Integer(l1_agent.agent_id)],
    )
    .await
    .expect("Quit L1 mission");

    // Co-locate with L4 agent -> CantDoMissionAlphaClone / InsufficientAgentStanding
    dock_at_station(
        &ctx,
        &mut gm,
        l4_agent.solar_system_id.unwrap(),
        l4_agent.station_id.unwrap(),
    )
    .await;
    // Set omega temporarily so standing check triggers first, then Alpha blocks
    let uid = gm.sess.user_id.expect("gm user_id");
    slash_cmd(&ctx, &mut gm.sess, &format!("/setomega {uid} 1999999999")).await;
    assert_eq!(
        rpc(
            &ctx,
            &mut gm.sess,
            "agentMgr",
            "DoAction",
            &[PyRep::Integer(1), PyRep::Integer(l4_agent.agent_id)],
        )
        .await
        .unwrap_err()
        .code,
        "InsufficientAgentStanding"
    );

    // Grant standing via /standing <corp_id> 6.0, then downgrade GM account to Alpha via /setomega <uid> 0
    slash_cmd(
        &ctx,
        &mut gm.sess,
        &format!("/standing {} 6.0", l4_agent.corporation_id),
    )
    .await;
    slash_cmd(&ctx, &mut gm.sess, &format!("/setomega {uid} 0")).await;
    assert_eq!(
        rpc(
            &ctx,
            &mut gm.sess,
            "agentMgr",
            "DoAction",
            &[PyRep::Integer(1), PyRep::Integer(l4_agent.agent_id)],
        )
        .await
        .unwrap_err()
        .code,
        "CantDoMissionAlphaClone"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edge_research_locator_epic_arc_and_lp_store_errors() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);

    let mut gm = gm_pilot_in_space(&ctx, 5506, 95_500_006, TYPE_TRISTAN, SYS_JITA).await;
    let mut target_wh =
        gm_pilot_in_space(&ctx, 5507, 95_500_007, TYPE_TRISTAN, find_wormhole_system(&ctx)).await;

    // 1. R&D agent errors
    if let Some(rd_agent) = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.agent_type_id == 4 && a.station_id.is_some() && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id)
    {
        dock_at_station(
            &ctx,
            &mut gm,
            rd_agent.solar_system_id.unwrap(),
            rd_agent.station_id.unwrap(),
        )
        .await;
        // CancelResearch / BuyDatacores before starting -> ResearchNotActive
        assert_eq!(
            rpc(
                &ctx,
                &mut gm.sess,
                "agentMgr",
                "DoAction",
                &[PyRep::Integer(13), PyRep::Integer(rd_agent.agent_id)],
            )
            .await
            .unwrap_err()
            .code,
            "ResearchNotActive"
        );
        assert_eq!(
            rpc(
                &ctx,
                &mut gm.sess,
                "agentMgr",
                "DoAction",
                &[PyRep::Integer(14), PyRep::Integer(rd_agent.agent_id)],
            )
            .await
            .unwrap_err()
            .code,
            "ResearchNotActive"
        );
        // StartResearch without skill/standing -> fails with standing or skill error
        let e_rd = rpc(
            &ctx,
            &mut gm.sess,
            "agentMgr",
            "DoAction",
            &[PyRep::Integer(12), PyRep::Integer(rd_agent.agent_id)],
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                e_rd.code.as_str(),
                "InsufficientAgentStanding"
                    | "MissingRequiredSkill"
                    | "InsufficientSkills"
                    | "ErrorNoRelevantResearchSkills"
            ),
            "unexpected R&D start error: {}",
            e_rd.code
        );
    }

    // 2. Non-locator agent with action 15 -> AgentHasNoLocatorService
    let non_loc = ctx
        .sde
        .agents
        .values()
        .filter(|a| !a.is_locator && a.station_id.is_some() && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id)
        .expect("non-locator agent");
    dock_at_station(
        &ctx,
        &mut gm,
        non_loc.solar_system_id.unwrap(),
        non_loc.station_id.unwrap(),
    )
    .await;
    assert_eq!(
        rpc(
            &ctx,
            &mut gm.sess,
            "agentMgr",
            "DoAction",
            &[PyRep::Integer(15), PyRep::Integer(non_loc.agent_id)],
        )
        .await
        .unwrap_err()
        .code,
        "AgentHasNoLocatorService"
    );

    // 3. Co-located Locator agent error cases
    if let Some(loc_agent) = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.is_locator && a.level == 1 && a.station_id.is_some() && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id)
    {
        dock_at_station(
            &ctx,
            &mut gm,
            loc_agent.solar_system_id.unwrap(),
            loc_agent.station_id.unwrap(),
        )
        .await;
        // Unknown target -> TargetCharacterNotFound
        assert_eq!(
            rpc(
                &ctx,
                &mut gm.sess,
                "agentMgr",
                "DoAction",
                &[
                    PyRep::Integer(15),
                    PyRep::Integer(loc_agent.agent_id),
                    PyRep::Integer(888_999_777),
                ],
            )
            .await
            .unwrap_err()
            .code,
            "TargetCharacterNotFound"
        );
        // Target in wormhole space -> LocatorTargetInUnknownSpace
        assert_eq!(
            rpc(
                &ctx,
                &mut gm.sess,
                "agentMgr",
                "DoAction",
                &[
                    PyRep::Integer(15),
                    PyRep::Integer(loc_agent.agent_id),
                    PyRep::Integer(target_wh.char_id),
                ],
            )
            .await
            .unwrap_err()
            .code,
            "LocatorTargetInUnknownSpace"
        );
        // Target in another constellation (Jita) for L1 locator -> LocatorTargetOutOfRange (unless L1 locator IS in Jita's constellation)
        let loc_sys = loc_agent.solar_system_id.unwrap();
        let diff_sys = ctx
            .sde
            .solar_systems
            .values()
            .filter(|s| {
                s.solar_system_id < 31_000_000
                    && s.constellation_id
                        != ctx.sde.solar_systems.get(&loc_sys).map(|ls| ls.constellation_id).unwrap_or(0)
            })
            .min_by_key(|s| s.solar_system_id)
            .unwrap()
            .solar_system_id;
        slash_cmd(&ctx, &mut target_wh.sess, &format!("/tr {diff_sys}")).await;
        assert_eq!(
            rpc(
                &ctx,
                &mut gm.sess,
                "agentMgr",
                "DoAction",
                &[
                    PyRep::Integer(15),
                    PyRep::Integer(loc_agent.agent_id),
                    PyRep::Integer(target_wh.char_id),
                ],
            )
            .await
            .unwrap_err()
            .code,
            "LocatorTargetOutOfRange"
        );

        // Move target into same system as L1 locator -> drain wallet via GiveCash -> InsufficientFunds, then /giveisk -> locates -> immediate retry hits LocatorAgentOnCooldown
        slash_cmd(&ctx, &mut target_wh.sess, &format!("/tr {loc_sys}")).await;
        let bal_isk = ctx.db.with_snapshot(|s| {
            s.characters
                .get(&gm.char_id)
                .map(|c| c.balance.to_wire_f64())
                .unwrap_or(0.0)
        });
        if bal_isk > 100.0 {
            rpc(
                &ctx,
                &mut gm.sess,
                "account",
                "GiveCash",
                &[
                    PyRep::Integer(target_wh.char_id),
                    PyRep::Real(bal_isk),
                    PyRep::String("drain for fee test".into()),
                ],
            )
            .await
            .expect("GiveCash drain");
        }
        assert_eq!(
            rpc(
                &ctx,
                &mut gm.sess,
                "agentMgr",
                "DoAction",
                &[
                    PyRep::Integer(15),
                    PyRep::Integer(loc_agent.agent_id),
                    PyRep::Integer(target_wh.char_id),
                ],
            )
            .await
            .unwrap_err()
            .code,
            "InsufficientFunds"
        );
        slash_cmd(&ctx, &mut gm.sess, "/giveisk 500000").await;
        rpc(
            &ctx,
            &mut gm.sess,
            "agentMgr",
            "DoAction",
            &[
                PyRep::Integer(15),
                PyRep::Integer(loc_agent.agent_id),
                PyRep::Integer(target_wh.char_id),
            ],
        )
        .await
        .expect("Locator success");
        assert_eq!(
            rpc(
                &ctx,
                &mut gm.sess,
                "agentMgr",
                "DoAction",
                &[
                    PyRep::Integer(15),
                    PyRep::Integer(loc_agent.agent_id),
                    PyRep::Integer(target_wh.char_id),
                ],
            )
            .await
            .unwrap_err()
            .code,
            "LocatorAgentOnCooldown"
        );
    }

    // 4. LPStoreMgr validation errors: undocked, invalid qty, unknown offer, insufficient LP
    rpc(&ctx, &mut gm.sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock for LP test");
    assert_eq!(
        rpc(
            &ctx,
            &mut gm.sess,
            "LPStoreMgr",
            "TakeOfferForCharacter",
            &[PyRep::Integer(1_000_035), PyRep::Integer(1), PyRep::Integer(0)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidQuantity"
    );
    let e_undocked_lp = rpc(
        &ctx,
        &mut gm.sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(1_000_035), PyRep::Integer(1), PyRep::Integer(1)],
    )
    .await
    .unwrap_err();
    assert!(
        matches!(e_undocked_lp.code.as_str(), "MustBeDocked" | "UserError"),
        "unexpected undocked LP error: {} ({})",
        e_undocked_lp.code,
        e_undocked_lp.message
    );

    // Dock in Jita (Caldari Navy 1_000_035)
    dock_at_station(&ctx, &mut gm, SYS_JITA, STA_JITA).await;
    assert_eq!(
        rpc(
            &ctx,
            &mut gm.sess,
            "LPStoreMgr",
            "TakeOfferForCharacter",
            &[
                PyRep::Integer(1_000_035),
                PyRep::Integer(999_999_888),
                PyRep::Integer(1),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "OfferNotFound"
    );
    if let Ok(PyRep::List(offers)) = rpc(
        &ctx,
        &mut gm.sess,
        "LPStoreMgr",
        "GetAvailableOffersFromCorp",
        &[PyRep::Integer(1_000_035)],
    )
    .await
    {
        if let Some(offer_id) = offers.first().and_then(|o| o.dict_get("offerID")).and_then(|v| v.as_i64()) {
            let e_lp = rpc(
                &ctx,
                &mut gm.sess,
                "LPStoreMgr",
                "TakeOfferForCharacter",
                &[
                    PyRep::Integer(1_000_035),
                    PyRep::Integer(offer_id),
                    PyRep::Integer(1),
                ],
            )
            .await
            .unwrap_err();
            assert!(
                matches!(
                    e_lp.code.as_str(),
                    "InsufficientLoyaltyPoints" | "MissingRequiredItems" | "InsufficientFunds"
                ),
                "unexpected LP error: {}",
                e_lp.code
            );
        }
    }
}

// ============================================================================
// GROUP 3: Crimewatch, Safety Locks, Duels, KillRights, Targeting & Weapons
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edge_crimewatch_safety_duel_and_killright_validation() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);

    let mut p1 = gm_pilot_in_space(&ctx, 5508, 95_500_008, TYPE_TRISTAN, SYS_JITA).await;
    let mut p2 = gm_pilot_in_space(&ctx, 5509, 95_500_009, TYPE_TRISTAN, SYS_JITA).await;

    // Non-standard safety level defaults to Green (2)
    let safe_rep = rpc(
        &ctx,
        &mut p1.sess,
        "crimewatch",
        "SetSafetyLevel",
        &[PyRep::Integer(42)],
    )
    .await
    .expect("SetSafetyLevel defaults unknown to Green");
    assert_eq!(safe_rep.dict_get("safetyLevel"), Some(&PyRep::Integer(2)));

    // Set safety green on p1 and check high-sec weapon lock on neutral p2
    rpc(
        &ctx,
        &mut p1.sess,
        "crimewatch",
        "SetSafetyLevel",
        &[PyRep::Integer(2)],
    )
    .await
    .expect("Safety green");
    let mod1 = fit_module(&ctx, &mut p1.sess, find_small_blaster_type(&ctx)).await;
    let ammo1 = create_item(&ctx, &mut p1.sess, TYPE_ANTIMATTER_S, 200).await;
    rpc(
        &ctx,
        &mut p1.sess,
        "dogmaIM",
        "LoadAmmo",
        &[
            PyRep::Integer(p1.ship_id),
            PyRep::Integer(mod1),
            PyRep::Integer(ammo1),
        ],
    )
    .await
    .expect("LoadAmmo");
    let sys_h = ctx.systems.get_or_spawn(SYS_JITA);
    sys_h
        .set_ball_position(p2.ship_id, DVec3::new(10_500.0, 0.0, 0.0))
        .await;
    sys_h.set_ball_position(p1.ship_id, DVec3::new(10_000.0, 0.0, 0.0)).await;
    sys_h.step_manual(0.1).await.unwrap();
    lock_target(&ctx, &mut p1.sess, SYS_JITA, p1.ship_id, p2.ship_id).await;
    // Immediately after LoadAmmo reload timer is active -> ModuleIsReloading; after 12s -> CantDoInCurrentSafetyLevel
    assert_eq!(
        rpc(
            &ctx,
            &mut p1.sess,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(mod1), PyRep::Integer(p2.ship_id)],
        )
        .await
        .unwrap_err()
        .code,
        "ModuleIsReloading"
    );
    ctx.advance_secs(12);
    sys_h.step_manual(12.0).await.unwrap();
    assert_eq!(
        rpc(
            &ctx,
            &mut p1.sess,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(mod1), PyRep::Integer(p2.ship_id)],
        )
        .await
        .unwrap_err()
        .code,
        "CantDoInCurrentSafetyLevel"
    );

    // Duel challenges: self, unknown character, no pending challenge, valid decline
    assert!(
        rpc(
            &ctx,
            &mut p1.sess,
            "crimewatch",
            "StartDuelChallenge",
            &[PyRep::Integer(p1.char_id)],
        )
        .await
        .is_err()
    );
    assert!(
        rpc(
            &ctx,
            &mut p1.sess,
            "crimewatch",
            "StartDuelChallenge",
            &[PyRep::Integer(999_777_666)],
        )
        .await
        .is_err()
    );
    assert!(
        rpc(
            &ctx,
            &mut p2.sess,
            "crimewatch",
            "RespondToDuelChallenge",
            &[PyRep::Integer(p1.char_id), PyRep::Bool(true)],
        )
        .await
        .is_err()
    );
    rpc(
        &ctx,
        &mut p1.sess,
        "crimewatch",
        "StartDuelChallenge",
        &[PyRep::Integer(p2.char_id)],
    )
    .await
    .expect("StartDuelChallenge");
    rpc(
        &ctx,
        &mut p2.sess,
        "crimewatch",
        "RespondToDuelChallenge",
        &[PyRep::Integer(p1.char_id), PyRep::Bool(false)],
    )
    .await
    .expect("Decline duel");
    // Second respond fails (no longer pending)
    assert!(
        rpc(
            &ctx,
            &mut p2.sess,
            "crimewatch",
            "RespondToDuelChallenge",
            &[PyRep::Integer(p1.char_id), PyRep::Bool(true)],
        )
        .await
        .is_err()
    );

    // KillRight errors
    assert_eq!(
        rpc(
            &ctx,
            &mut p1.sess,
            "killRightMgr",
            "ActivateKillRight",
            &[PyRep::Integer(777_888_999)],
        )
        .await
        .unwrap_err()
        .code,
        "KillRightNotFound"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut p1.sess,
            "killRightMgr",
            "BuyKillRight",
            &[PyRep::Integer(777_888_999), PyRep::Real(1000.0)],
        )
        .await
        .unwrap_err()
        .code,
        "KillRightNotFound"
    );
    assert!(
        rpc(
            &ctx,
            &mut p1.sess,
            "killRightMgr",
            "SellKillRight",
            &[PyRep::Integer(777_888_999), PyRep::Real(1000.0)],
        )
        .await
        .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edge_targeting_ammo_loading_and_weapon_overload_errors() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);

    let mut p1 = gm_pilot_in_space(&ctx, 5510, 95_500_010, TYPE_TRISTAN, SYS_JITA).await;
    let low_sys = find_lowsec_system(&ctx);
    let p2 = gm_pilot_in_space(&ctx, 5511, 95_500_011, TYPE_TRISTAN, low_sys).await;
    slash_cmd(&ctx, &mut p1.sess, &format!("/tr {low_sys}")).await;
    rpc(
        &ctx,
        &mut p1.sess,
        "crimewatch",
        "SetSafetyLevel",
        &[PyRep::Integer(0)],
    )
    .await
    .expect("Safety red");

    // AddTarget errors: invalid/nonexistent ball
    assert!(
        rpc(
            &ctx,
            &mut p1.sess,
            "dogmaIM",
            "AddTarget",
            &[PyRep::Integer(999_888_777)],
        )
        .await
        .is_err()
    );

    // Weapon module not found -> ModuleNotFound
    assert_eq!(
        rpc(
            &ctx,
            &mut p1.sess,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(999_888_666)],
        )
        .await
        .unwrap_err()
        .code,
        "ModuleNotFound"
    );

    // Fit blaster, test LoadAmmo IncompatibleAmmo & ItemNotFound
    let gun_id = fit_module(&ctx, &mut p1.sess, find_small_blaster_type(&ctx)).await;
    let bad_ammo = create_item(&ctx, &mut p1.sess, TYPE_CRUISE_MISSILE, 10).await;
    assert_eq!(
        rpc(
            &ctx,
            &mut p1.sess,
            "dogmaIM",
            "LoadAmmo",
            &[
                PyRep::Integer(p1.ship_id),
                PyRep::Integer(gun_id),
                PyRep::Integer(bad_ammo),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "IncompatibleAmmo"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut p1.sess,
            "dogmaIM",
            "LoadAmmo",
            &[
                PyRep::Integer(p1.ship_id),
                PyRep::Integer(gun_id),
                PyRep::Integer(999_555_444),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotFound"
    );

    // Combat weapon validation through Vorton Projector (`group_id == 4060`) + turret reload/overload/offline
    let skybreaker_type = ctx
        .sde
        .types
        .values()
        .filter(|t| t.name == "Skybreaker")
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .unwrap_or(54_731);
    let vorton_type = ctx
        .sde
        .types
        .values()
        .filter(|t| t.name == "Small Vorton Projector I")
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .unwrap_or(54_739);
    let vorton_ammo_type = ctx
        .sde
        .types
        .values()
        .filter(|t| t.name == "GalvaSurge Condenser Pack S")
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .unwrap_or(54_769);
    let mut p3 = gm_pilot_in_space(&ctx, 5516, 95_500_016, skybreaker_type, low_sys).await;
    let vorton_id = fit_module(&ctx, &mut p3.sess, vorton_type).await;

    let sys_h = ctx.systems.get_or_spawn(low_sys);
    sys_h.set_ball_position(p1.ship_id, DVec3::ZERO).await;
    sys_h.set_ball_position(p3.ship_id, DVec3::ZERO).await;
    sys_h
        .set_ball_position(p2.ship_id, DVec3::new(15_000.0, 0.0, 0.0))
        .await;
    sys_h.step_manual(0.1).await.unwrap();

    // Activate unloaded Vorton against unlocked/locked target -> NoAmmoLoaded
    lock_target(&ctx, &mut p3.sess, low_sys, p3.ship_id, p2.ship_id).await;
    assert_eq!(
        rpc(
            &ctx,
            &mut p3.sess,
            "dogmaIM",
            "Activate",
            &[
                PyRep::Integer(vorton_id),
                PyRep::String("targetAttack".into()),
                PyRep::Integer(p2.ship_id),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "NoAmmoLoaded"
    );
    rpc(&ctx, &mut p3.sess, "dogmaIM", "RemoveTarget", &[PyRep::Integer(p2.ship_id)])
        .await
        .expect("Unlock p2 on p3");

    // Load Vorton ammo & verify ModuleIsReloading immediately after LoadAmmo
    let vorton_ammo = create_item(&ctx, &mut p3.sess, vorton_ammo_type, 200).await;
    rpc(
        &ctx,
        &mut p3.sess,
        "dogmaIM",
        "LoadAmmo",
        &[
            PyRep::Integer(p3.ship_id),
            PyRep::Integer(vorton_id),
            PyRep::Integer(vorton_ammo),
        ],
    )
    .await
    .expect("LoadAmmo Vorton");
    assert_eq!(
        rpc(
            &ctx,
            &mut p3.sess,
            "dogmaIM",
            "Activate",
            &[
                PyRep::Integer(vorton_id),
                PyRep::String("targetAttack".into()),
                PyRep::Integer(p2.ship_id),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "ModuleIsReloading"
    );
    ctx.advance_secs(12);
    sys_h.step_manual(12.0).await.unwrap();

    // Activate loaded Vorton without target -> TargetRequired
    assert_eq!(
        rpc(&ctx, &mut p3.sess, "dogmaIM", "Activate", &[PyRep::Integer(vorton_id)])
            .await
            .unwrap_err()
            .code,
        "TargetRequired"
    );

    // Activate on unlocked target -> TargetNotLocked
    assert_eq!(
        rpc(
            &ctx,
            &mut p3.sess,
            "dogmaIM",
            "Activate",
            &[
                PyRep::Integer(vorton_id),
                PyRep::String("targetAttack".into()),
                PyRep::Integer(p2.ship_id),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "TargetNotLocked"
    );

    // Move p2 to 35 km (lockable by Skybreaker, beyond Small Vorton Projector optimal+3*falloff) -> TargetOutOfRange
    sys_h
        .set_ball_position(p2.ship_id, DVec3::new(35_000.0, 0.0, 0.0))
        .await;
    sys_h.step_manual(0.1).await.unwrap();
    lock_target(&ctx, &mut p3.sess, low_sys, p3.ship_id, p2.ship_id).await;
    assert_eq!(
        rpc(
            &ctx,
            &mut p3.sess,
            "dogmaIM",
            "Activate",
            &[
                PyRep::Integer(vorton_id),
                PyRep::String("targetAttack".into()),
                PyRep::Integer(p2.ship_id),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "TargetOutOfRange"
    );

    // Move p2 within Vorton range (2 km), activate once -> ModuleAlreadyActive on second call
    sys_h
        .set_ball_position(p2.ship_id, DVec3::new(2_000.0, 0.0, 0.0))
        .await;
    sys_h.step_manual(0.1).await.unwrap();
    rpc(
        &ctx,
        &mut p3.sess,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(vorton_id),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(p2.ship_id),
        ],
    )
    .await
    .expect("Activate Vorton in range");
    assert_eq!(
        rpc(
            &ctx,
            &mut p3.sess,
            "dogmaIM",
            "Activate",
            &[
                PyRep::Integer(vorton_id),
                PyRep::String("targetAttack".into()),
                PyRep::Integer(p2.ship_id),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "ModuleAlreadyActive"
    );

    // Load valid ammo on p1 blaster, test TargetNotLocked, Overload, & ModuleOffline
    let good_ammo = create_item(&ctx, &mut p1.sess, TYPE_ANTIMATTER_S, 200).await;
    rpc(
        &ctx,
        &mut p1.sess,
        "dogmaIM",
        "LoadAmmo",
        &[
            PyRep::Integer(p1.ship_id),
            PyRep::Integer(gun_id),
            PyRep::Integer(good_ammo),
        ],
    )
    .await
    .expect("LoadAmmo valid on blaster");
    ctx.advance_secs(12);
    sys_h.step_manual(12.0).await.unwrap();
    assert_eq!(
        rpc(
            &ctx,
            &mut p1.sess,
            "dogmaIM",
            "Activate",
            &[
                PyRep::Integer(gun_id),
                PyRep::String("targetAttack".into()),
                PyRep::Integer(p2.ship_id),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "TargetNotLocked"
    );

    // Overload / StopOverload toggles module overload state cleanly
    let ov_rep = rpc(
        &ctx,
        &mut p1.sess,
        "dogmaIM",
        "Overload",
        &[PyRep::Integer(gun_id)],
    )
    .await
    .expect("Overload blaster");
    assert_eq!(ov_rep.dict_get("overloaded"), Some(&PyRep::Bool(true)));
    let stop_ov_rep = rpc(
        &ctx,
        &mut p1.sess,
        "dogmaIM",
        "StopOverload",
        &[PyRep::Integer(gun_id)],
    )
    .await
    .expect("StopOverload blaster");
    assert_eq!(stop_ov_rep.dict_get("overloaded"), Some(&PyRep::Bool(false)));

    // Fit blaster via invbroker.Add while docked, TakeModuleOffline, undock -> Activate fails with ModuleOffline
    dock_at_station(&ctx, &mut p1, SYS_JITA, STA_JITA).await;
    let blaster_type = find_small_blaster_type(&ctx);
    let req_skills: Vec<u32> = ctx
        .sde
        .types
        .get(&blaster_type)
        .map(|t| {
            [182_u16, 183, 184, 1285, 1289, 1290]
                .iter()
                .filter_map(|a| t.default_attributes.get(a))
                .map(|v| v.round() as u32)
                .filter(|&s| s > 0)
                .collect()
        })
        .unwrap_or_default();
    for sk in req_skills {
        slash_cmd(&ctx, &mut p1.sess, &format!("/giveskill {sk} 5")).await;
    }
    let blaster_mod = create_item(&ctx, &mut p1.sess, blaster_type, 1).await;
    rpc(
        &ctx,
        &mut p1.sess,
        "invbroker",
        "Add",
        &[
            PyRep::Integer(blaster_mod),
            PyRep::Integer(p1.ship_id),
            PyRep::Integer(28),
        ],
    )
    .await
    .expect("Fit blaster via invbroker.Add");
    rpc(
        &ctx,
        &mut p1.sess,
        "dogmaIM",
        "TakeModuleOffline",
        &[PyRep::Integer(p1.ship_id), PyRep::Integer(blaster_mod)],
    )
    .await
    .expect("Offline module");
    rpc(&ctx, &mut p1.sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock p1 after offlining");
    ctx.advance_secs(12);
    slash_cmd(&ctx, &mut p1.sess, &format!("/tr {low_sys}")).await;
    assert_eq!(
        rpc(
            &ctx,
            &mut p1.sess,
            "dogmaIM",
            "Activate",
            &[
                PyRep::Integer(blaster_mod),
                PyRep::String("targetAttack".into()),
                PyRep::Integer(p2.ship_id),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "ModuleOffline"
    );
}

// ============================================================================
// GROUP 4: Exploration Scanning, Hacking Minigame, Wormholes, Random Filaments, Cyno
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edge_probe_scanning_and_dscan_validation_and_rbac() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);

    // Non-GM player (demo char 90_000_001) cannot query remote system scanMgr
    let mut player_sess = SessionState::new(5512);
    login_pilot(&ctx, &mut player_sess, "demo", "demo-secret").await;
    rpc(
        &ctx,
        &mut player_sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
    )
    .await
    .expect("Select demo char");

    let remote_sys = find_lowsec_system(&ctx);
    assert_eq!(
        rpc(
            &ctx,
            &mut player_sess,
            "scanMgr",
            "GetSystemScanMgr",
            &[PyRep::Integer(i64::from(remote_sys))],
        )
        .await
        .unwrap_err()
        .code,
        "AccessDenied"
    );

    // Bound GetSystemScanMgr to own system + ConeScan while docked -> CannotDirectionalScanWhileDocked
    let own_sys = player_sess.solar_system_id.unwrap_or(SYS_JITA);
    let scan_rep = rpc(
        &ctx,
        &mut player_sess,
        "scanMgr",
        "GetSystemScanMgr",
        &[PyRep::Integer(i64::from(own_sys))],
    )
    .await
    .expect("GetSystemScanMgr own sys");
    let scan_oid = scan_rep.dict_get("boundID").and_then(|v| v.as_i64()).unwrap_or(0) as u64;
    if player_sess.station_id.is_some() {
        assert_eq!(
            rpc(&ctx, &mut player_sess, "scanMgr", "ConeScan", &[])
                .await
                .unwrap_err()
                .code,
            "CannotDirectionalScanWhileDocked"
        );
    }

    // RequestScans with no probes -> NoActiveProbes
    assert_eq!(
        rpc_bound(&ctx, &mut player_sess, "scanMgr", scan_oid, "RequestScans", &[PyRep::Dict(Vec::new())])
            .await
            .unwrap_err()
            .code,
        "NoActiveProbes"
    );
    // InvalidProbeState
    assert_eq!(
        rpc_bound(&ctx, &mut player_sess, "scanMgr", scan_oid, "SetActivityState", &[PyRep::Integer(99)])
            .await
            .unwrap_err()
            .code,
        "InvalidProbeState"
    );

    // GM pilot tests Core Probe Launcher NoProbesLoaded, InvalidProbeForLauncher, InvalidProbeCoordinates
    let mut gm = gm_pilot_in_space(&ctx, 5513, 95_500_013, TYPE_TRISTAN, SYS_JITA).await;
    let launcher = fit_module(&ctx, &mut gm.sess, TYPE_CORE_PROBE_LAUNCHER).await;
    assert_eq!(
        rpc(&ctx, &mut gm.sess, "dogmaIM", "Activate", &[PyRep::Integer(launcher)])
            .await
            .unwrap_err()
            .code,
        "NoProbesLoaded"
    );
    let combat_probes = create_item(&ctx, &mut gm.sess, TYPE_COMBAT_SCAN_PROBE, 8).await;
    rpc(
        &ctx,
        &mut gm.sess,
        "dogmaIM",
        "LoadAmmo",
        &[
            PyRep::Integer(gm.ship_id),
            PyRep::Integer(launcher),
            PyRep::Integer(combat_probes),
        ],
    )
    .await
    .expect("Load combat probes into launcher");
    assert_eq!(
        rpc(&ctx, &mut gm.sess, "dogmaIM", "Activate", &[PyRep::Integer(launcher)])
            .await
            .unwrap_err()
            .code,
        "InvalidProbeForLauncher"
    );
    let core_probes = create_item(&ctx, &mut gm.sess, TYPE_CORE_SCAN_PROBE, 8).await;
    rpc(
        &ctx,
        &mut gm.sess,
        "dogmaIM",
        "LoadAmmo",
        &[
            PyRep::Integer(gm.ship_id),
            PyRep::Integer(launcher),
            PyRep::Integer(core_probes),
        ],
    )
    .await
    .expect("Load Core Probes");
    let launch_rep = rpc(&ctx, &mut gm.sess, "dogmaIM", "Activate", &[PyRep::Integer(launcher)])
        .await
        .expect("Launch core probes");
    let first_probe_id = match launch_rep.dict_get("probeIDs") {
        Some(PyRep::List(ids)) => ids.first().and_then(|v| v.as_i64()).unwrap_or(1),
        _ => 1,
    };

    let scan_gm = rpc(&ctx, &mut gm.sess, "scanMgr", "GetSystemScanMgr", &[])
        .await
        .expect("GM GetSystemScanMgr");
    let gm_scan_oid = scan_gm.dict_get("boundID").and_then(|v| v.as_i64()).unwrap_or(0) as u64;
    let bad_coords = PyRep::Dict(vec![(
        PyRep::Integer(first_probe_id),
        PyRep::keyval([(
            "pos",
            PyRep::Tuple(vec![PyRep::Real(f64::NAN), PyRep::Real(0.0), PyRep::Real(0.0)]),
        )]),
    )]);
    assert_eq!(
        rpc_bound(&ctx, &mut gm.sess, "scanMgr", gm_scan_oid, "RequestScans", &[bad_coords])
            .await
            .unwrap_err()
            .code,
        "InvalidProbeCoordinates"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edge_hacking_minigame_validation_and_wormhole_cyno_filament_errors() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);

    let mut gm = gm_pilot_in_space(&ctx, 5514, 95_500_014, TYPE_TRISTAN, SYS_JITA).await;

    // 1. Hacking minigame errors
    assert_eq!(
        rpc(
            &ctx,
            &mut gm.sess,
            "hackingMgr",
            "ClickedOnTile",
            &[PyRep::Integer(0)],
        )
        .await
        .unwrap_err()
        .code,
        "NoActiveHackingGame"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut gm.sess,
            "hackingMgr",
            "StartNewGameInstance",
            &[PyRep::Integer(999_888_777)],
        )
        .await
        .unwrap_err()
        .code,
        "TargetNotFound"
    );
    dock_at_station(&ctx, &mut gm, SYS_JITA, STA_JITA).await;
    assert_eq!(
        rpc(
            &ctx,
            &mut gm.sess,
            "hackingMgr",
            "StartNewGameInstance",
            &[],
        )
        .await
        .unwrap_err()
        .code,
        "ShipDocked"
    );
    // While docked: WormholeJump & RandomJumpFilament also fail with ShipDocked
    assert_eq!(
        rpc(
            &ctx,
            &mut gm.sess,
            "wormholeMgr",
            "WormholeJump",
            &[PyRep::Integer(1)],
        )
        .await
        .unwrap_err()
        .code,
        "ShipDocked"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut gm.sess,
            "randomJumpMgr",
            "ActivateRandomJumpFilament",
            &[PyRep::Integer(1)],
        )
        .await
        .unwrap_err()
        .code,
        "ShipDocked"
    );

    // Undock & verify WormholeJump missing/nonexistent errors
    rpc(&ctx, &mut gm.sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock");
    ctx.advance_secs(12);
    assert_eq!(
        rpc(&ctx, &mut gm.sess, "wormholeMgr", "WormholeJump", &[])
            .await
            .unwrap_err()
            .code,
        "MissingWormholeBallId"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut gm.sess,
            "wormholeMgr",
            "WormholeJump",
            &[PyRep::Integer(999_777_888)],
        )
        .await
        .unwrap_err()
        .code,
        "WormholeNotPresent"
    );
    assert_eq!(
        rpc(&ctx, &mut gm.sess, "randomJumpMgr", "ActivateRandomJumpFilament", &[])
            .await
            .unwrap_err()
            .code,
        "NotInFleet"
    );

    // 2. Cyno generator activation errors: Tristan disallowed hull, then Force Recon across Highsec, Zarzakh, Pochven, & Lowsec without fleet
    assert_eq!(
        rpc(
            &ctx,
            &mut gm.sess,
            "slash",
            "SlashCmd",
            &[PyRep::String(format!("/fit me {TYPE_CYNO_GENERATOR}"))],
        )
        .await
        .unwrap_err()
        .code,
        "FittingValidationFailed"
    );
    let recon_type = ctx
        .sde
        .types
        .values()
        .filter(|t| t.group_id == 833)
        .min_by_key(|t| t.type_id)
        .expect("Force Recon hull in SDE")
        .type_id;
    let poch_sys = find_pochven_system(&ctx);
    let mut recon = gm_pilot_in_space(&ctx, 5515, 95_500_015, recon_type, SYS_JITA).await;
    let recon_cyno = fit_module(&ctx, &mut recon.sess, TYPE_CYNO_GENERATOR).await;
    assert_eq!(
        rpc(&ctx, &mut recon.sess, "dogmaIM", "Activate", &[PyRep::Integer(recon_cyno)])
            .await
            .unwrap_err()
            .code,
        "CannotLightCynoInSystem"
    );
    slash_cmd(&ctx, &mut recon.sess, &format!("/tr {SYS_ZARZAKH}")).await;
    assert_eq!(
        rpc(&ctx, &mut recon.sess, "dogmaIM", "Activate", &[PyRep::Integer(recon_cyno)])
            .await
            .unwrap_err()
            .code,
        "CynoDisallowedInZarzakh"
    );
    slash_cmd(&ctx, &mut recon.sess, &format!("/tr {poch_sys}")).await;
    assert_eq!(
        rpc(&ctx, &mut recon.sess, "dogmaIM", "Activate", &[PyRep::Integer(recon_cyno)])
            .await
            .unwrap_err()
            .code,
        "CynoJammedInPochven"
    );
    let low_sys = find_lowsec_system(&ctx);
    slash_cmd(&ctx, &mut recon.sess, &format!("/tr {low_sys}")).await;
    assert_eq!(
        rpc(&ctx, &mut recon.sess, "dogmaIM", "Activate", &[PyRep::Integer(recon_cyno)])
            .await
            .unwrap_err()
            .code,
        "MustBeInFleet"
    );

    // 3. Structure Cyno Beacon jump errors
    let e_bcn = rpc(
        &ctx,
        &mut gm.sess,
        "structureCynoBeaconMgr",
        "CmdJumpToStructureBeacon",
        &[PyRep::Integer(999_888_777)],
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            e_bcn.code.as_str(),
            "BeaconNotFound" | "NoJumpDrive" | "SkillMissing"
        ),
        "unexpected beacon jump error: {}",
        e_bcn.code
    );

    let wh_sys = find_wormhole_system(&ctx);
    slash_cmd(&ctx, &mut gm.sess, &format!("/tr {wh_sys}")).await;
    let e_wh_jump = rpc(
        &ctx,
        &mut gm.sess,
        "structureCynoBeaconMgr",
        "CmdJumpToStructureBeacon",
        &[PyRep::Integer(1_000_000_000_001)],
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            e_wh_jump.code.as_str(),
            "JumpDriveForbiddenInWormholeSpace" | "BeaconNotFound" | "NoJumpDrive"
        ),
        "unexpected WH beacon jump error: {}",
        e_wh_jump.code
    );
}

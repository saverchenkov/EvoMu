//! Unit tests for `space_cyno_filaments` (wormholes, random-jump filaments, structure cyno beacons, fleet beacon jumps, and directional cone scans).

#![forbid(unsafe_code)]

use super::*;
use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::mobile_deployables::{
    write_deployable_state_snap, MobileDeployableRecord,
};
use eve_db::types::{FleetRecord, UpwellStructureRecord};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn put_item(
    snap: &mut eve_db::WorldStateSnapshot,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> i64 {
    snap.items.insert(
        item_id,
        InventoryItemRecord {
            item_id,
            type_id,
            owner_id,
            location_id,
            flag,
            quantity,
            singleton,
            version: 1,
            ..Default::default()
        },
    );
    item_id
}

fn clone_type_with(
    sde: &mut Arc<SdeRegistry>,
    base_tid: u32,
    new_tid: u32,
    group_id: u32,
    category_id: u32,
    volume: f64,
    capacity: f64,
    attrs: &[(u16, f64)],
) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg.types.values().next().cloned().expect("base sde type");
    if let Some(b) = reg.types.get(&base_tid) {
        rec = b.clone();
    }
    rec.type_id = new_tid;
    rec.group_id = group_id;
    rec.category_id = category_id;
    rec.name = format!("TestType_{new_tid}");
    rec.volume = volume;
    rec.capacity = capacity;
    rec.default_attributes.clear();
    for &(k, v) in attrs {
        rec.default_attributes.insert(k, v);
    }
    reg.types.insert(new_tid, rec);
}

fn make_session(
    char_id: i64,
    corp_id: i64,
    sys_id: Option<u32>,
    ship_id: Option<i64>,
) -> SessionState {
    let mut s = SessionState::new(720);
    s.user_id = Some(30);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = sys_id;
    s.ship_id = ship_id;
    s
}

async fn spawn_ship(
    sys_h: &crate::system_actor::SystemHandle,
    id: i64,
    type_id: u32,
    group_id: u32,
    category_id: u32,
    position: DVec3,
    velocity: DVec3,
    is_cloaked: bool,
) {
    let ball = Ball {
        id,
        type_id,
        position,
        velocity,
        max_velocity: 350.0,
        inertia: 2.0,
        mass: 1_500_000.0,
        radius: 50.0,
        speed_fraction: 0.5,
        mode: MovementMode::Stop,
    };
    let mut cs = CombatState::default();
    cs.is_cloaked = is_cloaked;
    let meta = SlimItemMeta {
        owner_id: 9001,
        char_id: Some(9001),
        corp_id: 98000001,
        alliance_id: -1,
        group_id,
        category_id,
        security_status: 0.0,
        is_cloaked,
        ..Default::default()
    };
    let _ = sys_h
        .spawn_ball_with_meta(ball, cs, meta)
        .await;
}

fn online_pharolux(struct_id: i64, sys_id: u32, corp_id: i64) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id: struct_id,
        type_id: 35840,
        owner_corp_id: corp_id,
        solar_system_id: sys_id,
        name: format!("Pharolux-{struct_id}"),
        state: "ShieldVulnerable".to_string(),
        quantum_core_installed: true,
        fuel_blocks: 500,
        position: [1_000_000.0, 2_000_000.0, 3_000_000.0],
        shield_hp: 500_000.0,
        armor_hp: 500_000.0,
        hull_hp: 500_000.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0,
        self_destruct_at_unix: None,
    }
}

#[tokio::test]
async fn validate_and_get_structure_cyno_beacons_filters_invalid_states() {
    let ctx = test_ctx();
    let corp_id = 98_111_001;
    let char_id = 95_101;
    let valid_id = 1_040_000_001;
    let wrong_type_id = 1_040_000_002;
    let no_fuel_id = 1_040_000_003;
    let unanchored_id = 1_040_000_004;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.corporation_id = corp_id;
        snap.characters.insert(char_id, ch);
        snap.structures.insert(valid_id, online_pharolux(valid_id, 30000142, corp_id));

        let mut wt = online_pharolux(wrong_type_id, 30000142, corp_id);
        wt.type_id = 35832; // Astrahus, not Pharolux
        snap.structures.insert(wrong_type_id, wt);

        let mut nf = online_pharolux(no_fuel_id, 30000142, corp_id);
        nf.fuel_blocks = 0;
        snap.structures.insert(no_fuel_id, nf);

        let mut ua = online_pharolux(unanchored_id, 30000142, corp_id);
        ua.state = "Unanchored".to_string();
        snap.structures.insert(unanchored_id, ua);
    });

    let mut sess = make_session(char_id, corp_id, Some(30000142), Some(77001));
    let kw = HashMap::new();
    let listed = handle(
        &ctx,
        &mut sess,
        None,
        "structureCynoBeaconMgr",
        "GetMyAccessibleOnlineCynoBeaconStructures",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("ok");

    let PyRep::List(items) = listed else {
        panic!("expected list of beacons");
    };
    let ids: Vec<i64> = items
        .iter()
        .filter_map(|r| match r {
            PyRep::KeyVal(kv) => kv.get("structureID").and_then(PyRep::as_i64),
            PyRep::Dict(d) => d
                .iter()
                .find(|(k, _)| k.as_str() == Some("structureID"))
                .and_then(|(_, v)| v.as_i64()),
            _ => None,
        })
        .collect();
    assert!(ids.contains(&valid_id));
    assert!(!ids.contains(&wrong_type_id));
    assert!(!ids.contains(&no_fuel_id));
    assert!(!ids.contains(&unanchored_id));
}

#[tokio::test]
async fn jump_bridge_and_group_jump_to_structure_beacon_execute() {
    let mut ctx = test_ctx();
    let corp_id = 98_222_001;
    let char_id = 95_201;
    let wingman_id = 95_202;
    let origin_ship = 77_201;
    let wingman_ship = 77_202;

    let mut low_systems: Vec<u32> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.security_status < 0.45 && s.solar_system_id < 31_000_000 && s.region_id != 10_000_070)
        .map(|s| s.solar_system_id)
        .collect();
    low_systems.sort_unstable();
    let cur_sys = low_systems[0];
    let dst_sys = low_systems
        .iter()
        .copied()
        .find(|&sid| {
            sid != cur_sys && super::super::space_nav_travel::distance_ly(&ctx, cur_sys, sid) <= 5.0
        })
        .expect("nearby low/null system within 5 LY");

    let jump_ship_tid = 99_610;
    let iso_tid = 17887;
    let can_jump = super::super::space_nav_travel::attr_id_by_name(&ctx, "canJump").unwrap_or(866);
    let jd_range = super::super::space_nav_travel::attr_id_by_name(&ctx, "jumpDriveRange").unwrap_or(868);
    let jd_amount = super::super::space_nav_travel::attr_id_by_name(&ctx, "jumpDriveConsumptionAmount").unwrap_or(869);
    let jd_type = super::super::space_nav_travel::attr_id_by_name(&ctx, "jumpDriveConsumptionType").unwrap_or(867);
    clone_type_with(
        &mut ctx.sde,
        22474,
        jump_ship_tid,
        898,
        6,
        15_000.0,
        500.0,
        &[
            (can_jump, 1.0),
            (jd_range, 8.0),
            (jd_amount, 10.0),
            (jd_type, f64::from(iso_tid)),
        ],
    );
    clone_type_with(&mut ctx.sde, 34, iso_tid, 423, 4, 0.05, 0.0, &[]);

    let beacon_id = 1_040_222_001;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.corporation_id = corp_id;
        ch.solar_system_id = cur_sys;
        ch.active_ship_id = origin_ship;
        ch.station_id = None;
        ch.structure_id = None;
        snap.characters.insert(char_id, ch);
        put_item(snap, origin_ship, jump_ship_tid, char_id, i64::from(cur_sys), InventoryFlag::Hangar, 1, true);
        put_item(snap, wingman_ship, 588, wingman_id, i64::from(cur_sys), InventoryFlag::Hangar, 1, true);
        put_item(
            snap,
            77_203,
            iso_tid,
            char_id,
            origin_ship,
            InventoryFlag::SpecializedFuelBay,
            500,
            false,
        );
        snap.structures.insert(beacon_id, online_pharolux(beacon_id, dst_sys, corp_id));
        snap.fleets.insert(
            7001,
            FleetRecord {
                fleet_id: 7001,
                boss_id: char_id,
                members: vec![char_id, wingman_id],
                wings: vec![],
                squads: vec![],
                free_move: true,
                is_registered: false,
                ..Default::default()
            },
        );
    });

    let cur_h = ctx.systems.get_or_spawn(cur_sys);
    spawn_ship(&cur_h, origin_ship, jump_ship_tid, 898, 6, DVec3::ZERO, DVec3::ZERO, false).await;
    spawn_ship(
        &cur_h,
        wingman_ship,
        588,
        25,
        6,
        DVec3::new(1500.0, 0.0, 0.0),
        DVec3::ZERO,
        false,
    )
    .await;

    let mut sess = make_session(char_id, corp_id, Some(cur_sys), Some(origin_ship));
    let kw = HashMap::new();

    // 1. Bridge wingman to beacon
    let bridge_res = handle(
        &ctx,
        &mut sess,
        None,
        "structureCynoBeaconMgr",
        "CmdBridgeToStructureBeacon",
        &[PyRep::Integer(beacon_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("bridge ok");
    assert!(matches!(bridge_res, PyRep::KeyVal(_)));

    // 2. Group jump
    let grp_res = handle(
        &ctx,
        &mut sess,
        None,
        "structureCynoBeaconMgr",
        "CmdGroupJumpToStructureBeacon",
        &[PyRep::Integer(beacon_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("group jump ok");
    assert!(matches!(grp_res, PyRep::KeyVal(_)));

    let jump_res = handle(
        &ctx,
        &mut sess,
        None,
        "structureCynoBeaconMgr",
        "CmdJumpToStructureBeacon",
        &[PyRep::Integer(beacon_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("jump to beacon ok");
    let PyRep::KeyVal(kv) = jump_res else {
        panic!("expected KeyVal");
    };
    assert_eq!(kv.get("solarSystemID").and_then(|v| v.as_i64()), Some(dst_sys as i64));
    assert_eq!(sess.solar_system_id, Some(dst_sys));
}

#[tokio::test]
async fn random_jump_filament_teleports_fleet_away_from_stargates() {
    let mut ctx = test_ctx();
    let cur_sys = 30000142;
    let char_id = 95_301;
    let ship_id = 77_301;
    let filament_tid = 99_620;
    // Attribute 3025 = filamentSystemDistributionID: 4041 = Noise/Needlejack Null-Sec
    clone_type_with(
        &mut ctx.sde,
        58958,
        filament_tid,
        4041,
        30,
        0.1,
        0.0,
        &[(3025, 4041.0)],
    );

    let fil_item_id = ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.solar_system_id = cur_sys;
        ch.active_ship_id = ship_id;
        ch.station_id = None;
        ch.structure_id = None;
        snap.characters.insert(char_id, ch);
        snap.fleets.insert(
            7301,
            FleetRecord {
                fleet_id: 7301,
                boss_id: char_id,
                members: vec![char_id],
                wings: vec![],
                squads: vec![],
                free_move: true,
                is_registered: false,
                ..Default::default()
            },
        );
        put_item(snap, ship_id, 588, char_id, i64::from(cur_sys), InventoryFlag::Hangar, 1, true);
        put_item(
            snap,
            77_302,
            filament_tid,
            char_id,
            ship_id,
            InventoryFlag::Cargo,
            2,
            false,
        )
    });
    let sys_h = ctx.systems.get_or_spawn(cur_sys);
    // Safe deep-space coordinate > 1,000,000 m from all gates/stations
    spawn_ship(
        &sys_h,
        ship_id,
        588,
        25,
        6,
        DVec3::new(1.5e13, -2.1e13, 0.9e13),
        DVec3::ZERO,
        false,
    )
    .await;

    let mut sess = make_session(char_id, 98000001, Some(cur_sys), Some(ship_id));
    let kw = HashMap::new();
    let res = handle(
        &ctx,
        &mut sess,
        None,
        "randomJumpMgr",
        "ActivateRandomJumpFilament",
        &[PyRep::Integer(fil_item_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("filament ok");

    let PyRep::KeyVal(kv) = res else {
        panic!("expected KeyVal");
    };
    let new_sys = kv.get("solarSystemID").and_then(|v| v.as_i64()).expect("destination system id") as u32;
    assert_ne!(new_sys, cur_sys);
    assert_eq!(sess.solar_system_id, Some(new_sys));
    let rem_qty = ctx.db.get_item(fil_item_id).map(|i| i.quantity).unwrap_or(0);
    assert_eq!(rem_qty, 1);
}

#[tokio::test]
async fn cone_scan_filters_angles_inhibitors_cloaks_and_docks() {
    let ctx = test_ctx();
    let sys_id = 30000142;
    let char_id = 95_401;
    let caller_ship = 77_401;
    let in_cone_ship = 77_402;
    let out_cone_ship = 77_403;
    let cloaked_ship = 77_404;
    let inhibited_ship = 77_405;
    let inhibitor_id = 77_406;

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    // Caller moving +X, visible +X target, behind -X target, cloaked +X target, inhibitor + hidden ship
    spawn_ship(&sys_h, caller_ship, 588, 25, 6, DVec3::ZERO, DVec3::new(200.0, 0.0, 0.0), false).await;
    spawn_ship(&sys_h, in_cone_ship, 588, 25, 6, DVec3::new(1_000_000.0, 0.0, 0.0), DVec3::ZERO, false).await;
    spawn_ship(&sys_h, out_cone_ship, 588, 25, 6, DVec3::new(-1_000_000.0, 0.0, 0.0), DVec3::ZERO, false).await;
    spawn_ship(&sys_h, cloaked_ship, 588, 25, 6, DVec3::new(1_200_000.0, 0.0, 0.0), DVec3::ZERO, true).await;
    spawn_ship(
        &sys_h,
        inhibitor_id,
        crate::services::mobile_deployables_ext::TYPE_MOBILE_SCAN_INHIBITOR,
        crate::services::mobile_deployables_ext::GROUP_MOBILE_SCAN_INHIBITOR,
        22,
        DVec3::new(5_000_000.0, 0.0, 0.0),
        DVec3::ZERO,
        false,
    )
    .await;
    spawn_ship(&sys_h, inhibited_ship, 588, 25, 6, DVec3::new(5_005_000.0, 0.0, 0.0), DVec3::ZERO, false).await;

    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        write_deployable_state_snap(snap, |st| {
            st.deployables.insert(
                inhibitor_id,
                MobileDeployableRecord {
                    item_id: inhibitor_id,
                    type_id: crate::services::mobile_deployables_ext::TYPE_MOBILE_SCAN_INHIBITOR,
                    group_id: crate::services::mobile_deployables_ext::GROUP_MOBILE_SCAN_INHIBITOR,
                    owner_id: 9002,
                    solar_system_id: sys_id,
                    anchor_start_unix: now_u.saturating_sub(120),
                    anchor_duration_secs: 60,
                    anchor_completed: true,
                    is_unanchored: false,
                    expires_at_unix: now_u + 3600,
                    last_interaction_unix: now_u,
                    reinforced_until_unix: None,
                    last_ping_unix: None,
                    position: [5_000_000.0, 0.0, 0.0],
                    deployed_fleet_id: None,
                },
            );
        });
    });
    let balls = sys_h.list_balls().await.unwrap_or_default();
    assert_eq!(active_scan_inhibitor_positions(&ctx, sys_id, &balls).len(), 1);

    let mut sess = make_session(char_id, 98000001, Some(sys_id), Some(caller_ship));
    let bound = BoundObjectEntry {
        bound_id: 1,
        session_id: sess.session_id,
        user_id: 30,
        character_id: char_id,
        service: "scanMgr".into(),
        object_id: sys_id as i64,
        oid_str: format!("N=1:{sys_id}"),
        ref_id: 1,
    };
    let kw = HashMap::new();

    // Pass 0.5236 radians (~30 deg)
    let res = handle(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "ConeScan",
        &[PyRep::Real(0.5236), PyRep::Real(14.3 * 149_597_870_700.0)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("ConeScan ok");

    let PyRep::List(list) = res else {
        panic!("expected List");
    };
    let hit_ids: Vec<i64> = list
        .iter()
        .filter_map(|r| match r {
            PyRep::KeyVal(kv) => kv.get("id").and_then(PyRep::as_i64),
            _ => None,
        })
        .collect();
    assert!(hit_ids.contains(&in_cone_ship));
    assert!(hit_ids.contains(&inhibitor_id));
    assert!(!hit_ids.contains(&out_cone_ship));
    assert!(!hit_ids.contains(&cloaked_ship));
    assert!(!hit_ids.contains(&inhibited_ship));

    // Docked session cannot d-scan
    let mut docked = sess;
    docked.station_id = Some(60003760);
    let err_res = handle(
        &ctx,
        &mut docked,
        Some(&bound),
        "scanMgr",
        "ConeScan",
        &[PyRep::Real(360.0), PyRep::Real(1.0e12)],
        &kw,
    )
    .await
    .expect("handled");
    assert!(err_res.is_err());
}

#[tokio::test]
async fn fleet_beacon_jump_rejects_highsec_and_non_fleet_targets() {
    let ctx = test_ctx();
    let sys_id = 30000142;
    let boss_id = 95_501;
    let member_id = 95_502;
    let boss_ship = 77_501;
    let member_ship = 77_502;

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    spawn_ship(&sys_h, boss_ship, 588, 25, 6, DVec3::new(9_000_000.0, 4_000_000.0, 0.0), DVec3::ZERO, false).await;
    spawn_ship(&sys_h, member_ship, 588, 25, 6, DVec3::ZERO, DVec3::ZERO, false).await;

    ctx.db.with_mut_snapshot(|snap| {
        put_item(snap, boss_ship, 588, boss_id, i64::from(sys_id), InventoryFlag::Hangar, 1, true);
        put_item(snap, member_ship, 588, member_id, i64::from(sys_id), InventoryFlag::Hangar, 1, true);
    });

    let mut member_sess = make_session(member_id, 98000001, Some(sys_id), Some(member_ship));
    let kw = HashMap::new();
    // rpc-lint: allow live crucible:src/eve-server/ship/BeyonceService.h CmdBeaconJumpFleet
    let non_fleet_err = handle(&ctx, &mut member_sess, None, "beyonce", "CmdBeaconJumpFleet", &[PyRep::Integer(boss_id), PyRep::Integer(boss_ship), PyRep::Integer(i64::from(sys_id))], &kw)
        .await
        .expect("handled")
        .unwrap_err();
    assert_eq!(non_fleet_err.code, "FleetNotInFleet");

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            7501,
            FleetRecord {
                fleet_id: 7501,
                boss_id,
                members: vec![boss_id, member_id],
                wings: vec![],
                squads: vec![],
                free_move: true,
                is_registered: false,
                ..Default::default()
            },
        );
    });
    // rpc-lint: allow live crucible:src/eve-server/ship/BeyonceService.h CmdBeaconJumpFleet
    let hs_err = handle(&ctx, &mut member_sess, None, "beyonce", "CmdBeaconJumpFleet", &[PyRep::Integer(boss_id), PyRep::Integer(boss_ship), PyRep::Integer(i64::from(sys_id))], &kw)
        .await
        .expect("handled")
        .unwrap_err();
    assert_eq!(hs_err.code, "CannotJumpToHighSecBeacon");
}

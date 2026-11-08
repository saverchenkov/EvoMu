//! Unit tests for `space_cyno_filaments` (wormholes, random-jump filaments, structure cyno beacons, fleet beacon jumps, and directional cone scans).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::types::{FleetRecord, UpwellStructureRecord};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "space_filaments_and_dscan_tests.rs"]
mod space_filaments_and_dscan_tests;

pub(super) static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

pub(super) fn test_ctx() -> ServerContext {
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

pub(super) fn put_item(
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

pub(super) fn clone_type_with(
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

pub(super) fn make_session(
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

pub(super) async fn spawn_ship(
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

pub(super) fn online_pharolux(struct_id: i64, sys_id: u32, corp_id: i64) -> UpwellStructureRecord {
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
async fn criminal_wormhole_jump_from_lowsec_into_highsec_schedules_concord() {
    let ctx = test_ctx();
    let old_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < 0.0)
        .map(|s| s.solar_system_id)
        .expect("lowsec/nullsec system in SDE");
    let dest_hs_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status >= 0.45 && s.solar_system_id != old_sys)
        .map(|s| s.solar_system_id)
        .expect("highsec system in SDE");
    let now_u = ctx.now_unix();
    let char_id = 96_305_i64;
    let ship_id = 96_399_i64;
    let wh_ball_id = 750_000_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("char");
        ch.character_id = char_id;
        ch.corporation_id = 98_000_001;
        ch.solar_system_id = old_sys;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        put_item(snap, ship_id, 588, char_id, old_sys as i64, InventoryFlag::None, 1, true);
        let st_old = snap.system_states.entry(old_sys).or_default();
        st_old.wormhole_destination_system_id = Some(dest_hs_sys);
        st_old.wormhole_remaining_mass_kg = 1_000_000_000.0;
        let st_dest = snap.system_states.entry(dest_hs_sys).or_default();
        st_dest.wormhole_remaining_mass_kg = 1_000_000_000.0;
    });
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(char_id).criminal_flag_until_unix = now_u + 900;
    }

    let old_h = ctx.systems.get_or_spawn(old_sys);
    spawn_ship(&old_h, ship_id, 588, 25, 6, DVec3::ZERO, DVec3::ZERO, false).await;
    spawn_ship(&old_h, wh_ball_id, 26272, 988, 2, DVec3::new(500.0, 0.0, 0.0), DVec3::ZERO, false).await;

    let mut sess = make_session(char_id, 98_000_001, Some(old_sys), Some(ship_id));
    let _ = handle(
        &ctx,
        &mut sess,
        None,
        "wormholeMgr",
        "WormholeJump",
        &[PyRep::Integer(wh_ball_id)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("wormhole jump ok");

    assert_eq!(sess.solar_system_id, Some(dest_hs_sys));
    let pending = ctx.crimewatch.read().unwrap().states[&char_id].pending_concord_arrival;
    assert_eq!(pending.map(|p| (p.0, p.2)), Some((dest_hs_sys, ship_id)));
}

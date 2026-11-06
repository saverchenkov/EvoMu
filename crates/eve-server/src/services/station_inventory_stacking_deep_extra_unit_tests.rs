#![forbid(unsafe_code)]

use crate::router::{context::BoundObjectEntry, ServerContext};
use crate::services::combat_crimewatch::{self, SafetyLevel};
use crate::services::inventory_fitting::fitting_containers_ships::{
    read_container_ship_state, save_container_ship_state,
};
use crate::services::{inventory_stacking, station_structure};
use eve_db::{
    InventoryFlag, InventoryItemRecord, UpwellStructureRecord, VirtualServerClock, WorldRepository,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn test_ctx(now_unix: u64) -> ServerContext {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    ctx
}

fn ensure_char(
    ctx: &ServerContext,
    char_id: i64,
    corp_id: i64,
    station_id: Option<i64>,
    sys_id: u32,
    ship_id: i64,
) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = format!("Pilot {char_id}");
        ch.corporation_id = corp_id;
        ch.station_id = station_id;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        ch.security_status = 5.0;
        ch.clone_grade = 2;
        ch.fleet_id = None;
        snap.characters.insert(char_id, ch);
        snap.accounts.insert(
            format!("user_{char_id}"),
            eve_db::AccountRecord {
                user_id: char_id as u64,
                username: format!("user_{char_id}"),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
    });
    let _ = ctx
        .db
        .acquire_online_lock(char_id as u64, char_id as u64, Some(char_id), true, ctx.now_unix_secs());
}

fn make_session(
    char_id: i64,
    corp_id: i64,
    station_id: Option<i64>,
    sys_id: Option<u32>,
    ship_id: Option<i64>,
) -> SessionState {
    SessionState {
        session_id: char_id as u64,
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        station_id,
        structure_id: None,
        solar_system_id: sys_id,
        ship_id,
        ..Default::default()
    }
}

fn make_bound(object_id: i64, char_id: i64) -> BoundObjectEntry {
    BoundObjectEntry {
        bound_id: 20_001,
        session_id: char_id as u64,
        user_id: char_id as u64,
        character_id: char_id,
        service: "invbroker".into(),
        object_id,
        oid_str: format!("N=20001:{object_id}"),
        ref_id: 1,
    }
}

fn insert_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) {
    ctx.db.with_mut_snapshot(|snap| {
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
                ..Default::default()
            },
        );
    });
}

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v),
        PyRep::KeyVal(map) => map.get(key),
        _ => None,
    }
}

async fn call_station(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    station_structure::handle(ctx, sess, svc, method, args, &HashMap::new())
        .await
        .expect("unhandled station_structure route")
}

#[tokio::test]
async fn station_metadata_guests_standing_and_service_check() {
    let ctx = test_ctx(1_800_000_000);
    let station_id = 60_003_760_i64; // Jita IV-4
    let sys_id = 30_000_142_u32;
    let alliance_id = 99_000_111_i64;
    let corp_id = 98_000_111_i64;
    let char_id = 91_000_111_i64;
    let ship_id = 1_000_900_001_i64;

    ensure_char(&ctx, char_id, corp_id, Some(station_id), sys_id, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.values_mut().next() {
            let mut mc = corp.clone();
            mc.corporation_id = corp_id;
            mc.alliance_id = Some(alliance_id);
            snap.corporations.insert(corp_id, mc);
        }
        snap.structures.insert(
            1_000_000_010_001,
            UpwellStructureRecord {
                structure_id: 1_000_000_010_001,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Test Astrahus".into(),
                position: [1_000_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });

    let mut sess = make_session(char_id, corp_id, Some(station_id), Some(sys_id), Some(ship_id));
    sess.alliance_id = Some(alliance_id);

    // GetAllianceSystems + GetStationsForOwner
    let all_sys = call_station(&ctx, &mut sess, "stationSvc", "GetAllianceSystems", &[]).await.unwrap();
    assert!(matches!(&all_sys, PyRep::List(v) if v.contains(&PyRep::Integer(sys_id as i64))));
    let owned_st = call_station(&ctx, &mut sess, "stationSvc", "GetStationsForOwner", &[PyRep::Integer(corp_id)]).await.unwrap();
    assert!(matches!(&owned_st, PyRep::List(v) if v.len() == 1));

    // GetSolarSystem: station ID -> integer solarSystemID, missing station -> err, sys ID -> KeyVal
    let sol_from_st = call_station(&ctx, &mut sess, "stationSvc", "GetSolarSystem", &[PyRep::Integer(station_id)]).await.unwrap();
    assert_eq!(sol_from_st, PyRep::Integer(sys_id as i64));
    assert_eq!(
        call_station(&ctx, &mut sess, "stationSvc", "GetSolarSystem", &[PyRep::Integer(69_999_999)])
            .await
            .unwrap_err()
            .code,
        "StationNotFound"
    );
    let sys_kv = call_station(&ctx, &mut sess, "stationSvc", "GetSolarSystem", &[PyRep::Integer(sys_id as i64)]).await.unwrap();
    assert_eq!(dict_get(&sys_kv, "solarSystemID"), Some(&PyRep::Integer(sys_id as i64)));

    // GetStation + GetStationItemBits
    let st_kv = call_station(&ctx, &mut sess, "stationSvc", "GetStation", &[PyRep::Integer(station_id)]).await.unwrap();
    assert_eq!(dict_get(&st_kv, "stationID"), Some(&PyRep::Integer(station_id)));
    let bits = call_station(&ctx, &mut sess, "stationSvc", "GetStationItemBits", &[PyRep::Integer(station_id)]).await.unwrap();
    assert!(matches!(&bits, PyRep::Tuple(t) if t.len() == 4 && t[1] == PyRep::Integer(station_id)));

    // GetGuests: docked vs undocked error
    let guests = call_station(&ctx, &mut sess, "station", "GetGuests", &[]).await.unwrap();
    assert!(matches!(&guests, PyRep::List(g) if g.len() == 1));
    let mut undocked = make_session(char_id, corp_id, None, Some(sys_id), Some(ship_id));
    assert_eq!(
        call_station(&ctx, &mut undocked, "station", "GetGuests", &[]).await.unwrap_err().code,
        "NotDockedInStation"
    );

    // DoStandingCheckForStationService & npc_station_offers_service
    assert_eq!(
        call_station(&ctx, &mut sess, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(0)])
            .await
            .unwrap_err()
            .code,
        "InvalidStationService"
    );
    assert!(call_station(&ctx, &mut sess, "corpStationMgr", "DoStandingCheckForStationService", &[PyRep::Integer(1)])
        .await
        .is_ok());
    assert!(!station_structure::npc_station_offers_service(&ctx.sde, station_id, -1));
    assert!(!station_structure::npc_station_offers_service(&ctx.sde, 69_999_999, 1));
}

#[tokio::test]
async fn station_dock_undock_and_anchor_structure_gates() {
    let ctx = test_ctx(1_800_000_000);
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let corp_id = 98_000_222_i64;
    let char_id = 91_000_222_i64;
    let ship_id = 1_000_900_002_i64;

    ensure_char(&ctx, char_id, corp_id, Some(station_id), sys_id, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);

    // Dock validation errors
    let mut docked_sess = make_session(char_id, corp_id, Some(station_id), Some(sys_id), Some(ship_id));
    assert_eq!(
        call_station(&ctx, &mut docked_sess, "stationSvc", "Dock", &[PyRep::Integer(station_id)])
            .await
            .unwrap_err()
            .code,
        "AlreadyDocked"
    );

    let mut no_ship_sess = make_session(char_id, corp_id, None, Some(sys_id), None);
    assert_eq!(
        call_station(&ctx, &mut no_ship_sess, "stationSvc", "Dock", &[PyRep::Integer(station_id)])
            .await
            .unwrap_err()
            .code,
        "NoActiveShip"
    );

    let mut space_sess = make_session(char_id, corp_id, None, Some(sys_id), Some(ship_id));
    assert_eq!(
        call_station(&ctx, &mut space_sess, "stationSvc", "Dock", &[]).await.unwrap_err().code,
        "MissingLocationId"
    );
    assert_eq!(
        call_station(
            &ctx,
            &mut space_sess,
            "stationSvc",
            "Dock",
            &[PyRep::Integer(station_id), PyRep::Integer(ship_id + 1)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidShipId"
    );
    assert_eq!(
        call_station(&ctx, &mut space_sess, "stationSvc", "Dock", &[PyRep::Integer(station_id)])
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    // Spawn ship outside docking perimeter then inside docking perimeter
    let st_pos = ctx.sde.stations.get(&station_id).unwrap().position;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let make_ball = |pos: DVec3| Ball {
        id: ship_id,
        type_id: 587,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 0.5,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle
        .spawn_ball_with_meta(
            make_ball(st_pos + DVec3::new(900_000.0, 0.0, 0.0)),
            ctx.build_ship_combat_state(char_id, ship_id, 587),
            ctx.build_ship_slim_meta(char_id, 587),
        )
        .await;
    assert_eq!(
        call_station(&ctx, &mut space_sess, "stationSvc", "Dock", &[PyRep::Integer(station_id)])
            .await
            .unwrap_err()
            .code,
        "OutsideDockingPerimeter"
    );

    // Move ship within docking perimeter + test successful dock
    let _ = sys_handle.remove_ball(ship_id).await;
    let _ = sys_handle
        .spawn_ball_with_meta(
            make_ball(st_pos + DVec3::new(100.0, 0.0, 0.0)),
            ctx.build_ship_combat_state(char_id, ship_id, 587),
            ctx.build_ship_slim_meta(char_id, 587),
        )
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = sys_id as i64;
        }
    });
    assert!(call_station(&ctx, &mut space_sess, "stationSvc", "Dock", &[PyRep::Integer(station_id)])
        .await
        .is_ok());
    assert_eq!(space_sess.station_id, Some(station_id));

    // posMgr.AnchorStructure validation paths
    let depot_id = 1_000_900_010_i64;
    let tritanium_id = 1_000_900_011_i64;
    let other_char_depot = 1_000_900_012_i64;
    insert_item(&ctx, tritanium_id, 34, char_id, ship_id, InventoryFlag::Cargo, 10, false);
    insert_item(&ctx, other_char_depot, 33474, char_id + 1, ship_id, InventoryFlag::Cargo, 1, false);
    insert_item(&ctx, depot_id, 33474, char_id, station_id, InventoryFlag::Hangar, 1, false);

    assert_eq!(
        call_station(&ctx, &mut space_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(tritanium_id)])
            .await
            .unwrap_err()
            .code,
        "ItemNotAnchorable"
    );
    assert_eq!(
        call_station(&ctx, &mut space_sess, "posMgr", "AnchorStructure", &[PyRep::Integer(other_char_depot)])
            .await
            .unwrap_err()
            .code,
        "NotInSpace"
    );

    let mut in_space = make_session(char_id, corp_id, None, Some(sys_id), Some(ship_id));
    assert_eq!(
        call_station(&ctx, &mut in_space, "posMgr", "AnchorStructure", &[PyRep::Integer(other_char_depot)])
            .await
            .unwrap_err()
            .code,
        "NotItemOwner"
    );
    assert_eq!(
        call_station(&ctx, &mut in_space, "posMgr", "AnchorStructure", &[PyRep::Integer(depot_id)])
            .await
            .unwrap_err()
            .code,
        "InvalidItemLocation"
    );
}

#[tokio::test]
async fn inventory_stacking_split_merge_stackall_and_loot_rules() {
    let ctx = test_ctx(1_800_000_000);
    let char_id = 91_000_333_i64;
    let corp_id = 98_000_333_i64;
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 1_000_900_020_i64;
    let st_a = 1_000_900_021_i64;
    let st_b = 1_000_900_022_i64;
    let st_c = 1_000_900_023_i64;

    ensure_char(&ctx, char_id, corp_id, Some(station_id), sys_id, ship_id);
    insert_item(&ctx, ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
    insert_item(&ctx, st_a, 34, char_id, station_id, InventoryFlag::Hangar, 100, false);
    insert_item(&ctx, st_b, 34, char_id, station_id, InventoryFlag::Hangar, 40, false);
    insert_item(&ctx, st_c, 34, char_id + 1, station_id, InventoryFlag::Hangar, 20, false);

    let mut sess = make_session(char_id, corp_id, Some(station_id), Some(sys_id), Some(ship_id));
    let bound = make_bound(station_id, char_id);
    let kw = HashMap::new();

    // SplitStack: 3-arg + SplitFailed error
    let split = inventory_stacking::handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&bound),
        "invbroker",
        "SplitStack",
        &[PyRep::Integer(station_id), PyRep::Integer(st_a), PyRep::Integer(25)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_get(&split, "originalQuantity"), Some(&PyRep::Integer(75)));
    assert_eq!(dict_get(&split, "newQuantity"), Some(&PyRep::Integer(25)));
    assert_eq!(
        inventory_stacking::handle_stack_ops(
            &ctx,
            &mut sess,
            Some(&bound),
            "invbroker",
            "SplitStack",
            &[PyRep::Integer(st_a), PyRep::Integer(0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "SplitFailed"
    );

    // MultiMerge: ALSC lock check + owner mismatch + valid merge
    let mut alsc = read_container_ship_state(&ctx);
    alsc.alsc_locked_items.insert(st_b);
    save_container_ship_state(&ctx, &alsc);
    assert_eq!(
        inventory_stacking::handle_stack_ops(
            &ctx,
            &mut sess,
            Some(&bound),
            "invbroker",
            "MultiMerge",
            &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(st_b), PyRep::Integer(st_a), PyRep::Integer(10)])])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ALSCItemLocked"
    );
    alsc.alsc_locked_items.remove(&st_b);
    save_container_ship_state(&ctx, &alsc);

    assert!(inventory_stacking::handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&bound),
        "invbroker",
        "MultiMerge",
        &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(st_c), PyRep::Integer(st_a), PyRep::Integer(5)])])],
        &kw,
    )
    .unwrap()
    .is_err());

    // StackAll combines all owned unlocked Tritanium stacks in station hangar (75 + 25 + 40 = 140)
    let stacked = inventory_stacking::handle_stack_ops(
        &ctx,
        &mut sess,
        Some(&bound),
        "invbroker",
        "StackAll",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dict_get(&stacked, "mergedCount"), Some(&PyRep::Integer(2)));

    // handle_multi_add: CannotMoveActiveShip + CorpRoleDenied + valid move into ship cargo
    let err_ship = inventory_stacking::handle_multi_add(
        &ctx,
        &mut sess,
        Some(&bound),
        &[PyRep::List(vec![PyRep::Integer(ship_id)]), PyRep::Integer(station_id)],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err_ship.code, "CannotMoveActiveShip");

    let corp_div_item = 1_000_900_030_i64;
    insert_item(
        &ctx,
        corp_div_item,
        34,
        corp_id,
        station_id,
        InventoryFlag::from_u16(116),
        10,
        false,
    );
    let err_corp = inventory_stacking::handle_multi_add(
        &ctx,
        &mut sess,
        Some(&bound),
        &[PyRep::List(vec![PyRep::Integer(corp_div_item)]), PyRep::Integer(station_id)],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err_corp.code, "CorpRoleDenied");

    // Wreck / Jetcan CrimeWatch loot legality + ALSC password in handle_list + jetcan despawn
    let jetcan_id = 1_000_900_040_i64;
    let loot_item_id = 1_000_900_041_i64;
    let thief_id = 91_000_444_i64;
    ensure_char(&ctx, thief_id, 98_000_444, None, sys_id, 1_000_900_050);
    insert_item(&ctx, jetcan_id, 23, char_id, sys_id as i64, InventoryFlag::None, 1, true);
    insert_item(&ctx, loot_item_id, 34, char_id, jetcan_id, InventoryFlag::Cargo, 5, false);
    let loot_rec = ctx.db.with_snapshot(|s| s.items.get(&loot_item_id).cloned()).unwrap();

    combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(thief_id, SafetyLevel::Green);
    });
    assert_eq!(
        inventory_stacking::evaluate_space_wreck_or_jetcan_loot(&ctx, thief_id, &loot_rec)
            .unwrap_err()
            .code,
        "CantDoInCurrentSafetyLevel"
    );

    combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(thief_id, SafetyLevel::Yellow);
    });
    assert_eq!(
        inventory_stacking::evaluate_space_wreck_or_jetcan_loot(&ctx, thief_id, &loot_rec).unwrap(),
        (true, true)
    );
    let thief_sess = make_session(thief_id, 98_000_444, None, Some(sys_id), Some(1_000_900_050));
    inventory_stacking::apply_space_loot_suspect_flag(&ctx, &thief_sess, thief_id);
    assert!(ctx
        .crimewatch
        .read()
        .unwrap()
        .states
        .get(&thief_id)
        .is_some_and(|st| st.suspect_flag_until_unix > 0));

    // ALSC password check & empty jetcan despawn
    let mut pw_state = read_container_ship_state(&ctx);
    pw_state.container_passwords.insert(jetcan_id, "secret".into());
    save_container_ship_state(&ctx, &pw_state);
    let jet_bound = make_bound(jetcan_id, char_id);
    let space_owner = make_session(char_id, corp_id, None, Some(sys_id), Some(ship_id));
    assert_eq!(
        inventory_stacking::handle_list(&ctx, &space_owner, Some(&jet_bound), &[], &kw)
            .unwrap_err()
            .code,
        "ALSCPasswordRequired"
    );
    assert!(inventory_stacking::handle_list(
        &ctx,
        &space_owner,
        Some(&jet_bound),
        &[PyRep::String("secret".into())],
        &kw,
    )
    .is_ok());

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&loot_item_id);
    });
    inventory_stacking::despawn_empty_jetcan_if_needed(&ctx, Some(sys_id), jetcan_id).await;
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&jetcan_id)));
}

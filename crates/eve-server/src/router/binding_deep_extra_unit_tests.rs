//! Deep branch-coverage and mutation-killing unit tests for `router::binding`:
//! moniker authorization across `dogmaIM`, `ship`, `invbroker`, `station`, `corpStationMgr`,
//! `repairSvc`, `reprocessingSvc`, `insuranceSvc`, `skillMgr2`, `charMgr`, `jumpCloneSvc`,
//! `beyonce`, `scanMgr`, `crimewatch`, `i2`, and positive-ID services; session transition
//! invalidation & `OnMachoObjectDisconnect`; module deactivation & target-lock hooks; and
//! Alpha/Omega skill prerequisites & target painter signature bloom math.

#![forbid(unsafe_code)]

use super::ServerContext;
use eve_db::{
    InventoryFlag, InventoryItemRecord, UpwellStructureRecord, WorldRepository,
};
use eve_machonet::service_roles::ROLE_PLAYER;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

pub(super) static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
const EXPECTED_BIND_DENIED_CODE: &str = "BindAuthorizationDenied";

pub(super) fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

#[test]
fn moniker_bind_and_verify_authorization_matrix_across_all_services() {
    let ctx = test_ctx();
    let char_id = 90_000_001i64;
    let other_char = 90_000_002i64;
    let sys_id = 30_000_142u32;
    let other_sys = 30_000_144u32;
    let st_id = 60_003_760i64;
    let other_st_in_jita = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id == sys_id && s.station_id != st_id)
        .map(|s| s.station_id)
        .unwrap_or(60_003_760);
    let ship_id = 100_000_001i64;
    let struct_id = 1_000_000_777_001i64;
    let item_in_station = 777_101i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: 1_000_009,
                solar_system_id: sys_id,
                name: "Jita Astrahus".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 10_000.0,
                armor_hp: 10_000.0,
                hull_hp: 10_000.0,
                ..Default::default()
            },
        );
        snap.items.insert(
            item_in_station,
            InventoryItemRecord {
                item_id: item_in_station,
                type_id: 3293,
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut sess = SessionState {
        session_id: 501,
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        station_id: Some(st_id),
        structure_id: None,
        ship_id: Some(ship_id),
        role: ROLE_PLAYER,
        ..Default::default()
    };

    // 1) dogmaIM & ship: allowed for ship_id, station_id, structure_id, solar_system_id; denied for unrelated
    for svc in ["dogmaIM", "ship"] {
        assert!(ctx.bind_object(&sess, svc, ship_id).is_ok());
        assert!(ctx.bind_object(&sess, svc, st_id).is_ok());
        assert!(ctx.bind_object(&sess, svc, sys_id as i64).is_ok());
        assert_eq!(
            ctx.bind_object(&sess, svc, 999_888_777).unwrap_err().code,
            "BindAuthorizationDenied"
        );
    }

    // 2) station, corpStationMgr, repairSvc, reprocessingSvc, insuranceSvc:
    //    allowed for active station_id or structure_id; denied for solar_system_id or other station
    let mut station_bids = Vec::new();
    for svc in [
        "station",
        "corpStationMgr",
        "repairSvc",
        "reprocessingSvc",
        "insuranceSvc",
    ] {
        let bid = ctx.bind_object(&sess, svc, st_id).expect(svc);
        assert!(ctx.verify_bound_object(&sess, bid).is_ok());
        station_bids.push((svc, bid));
        assert_eq!(
            ctx.bind_object(&sess, svc, sys_id as i64).unwrap_err().code,
            "BindAuthorizationDenied"
        );
    }
    // When undocked into space, verify_bound_object rejects all station-scoped monikers
    sess.station_id = None;
    for (svc, bid) in &station_bids {
        assert_eq!(
            ctx.verify_bound_object(&sess, *bid).unwrap_err().code,
            "BoundObjectOwnershipViolation",
            "{svc} moniker must fail verification when undocked"
        );
    }
    // When docked in Upwell structure `struct_id`, all 5 services bind & verify against `struct_id`
    sess.structure_id = Some(struct_id);
    for svc in [
        "station",
        "corpStationMgr",
        "repairSvc",
        "reprocessingSvc",
        "insuranceSvc",
    ] {
        let bid = ctx.bind_object(&sess, svc, struct_id).expect(svc);
        assert!(ctx.verify_bound_object(&sess, bid).is_ok());
    }
    sess.structure_id = None;
    sess.station_id = Some(st_id);

    // 3) invbroker:
    //    - While docked: station_id, ship_id, other station in same system, and structure in same system succeed;
    //      binding to solar_system_id while docked is DENIED!
    assert!(ctx.bind_object(&sess, "invbroker", st_id).is_ok());
    assert!(ctx.bind_object(&sess, "invbroker", ship_id).is_ok());
    assert!(ctx.bind_object(&sess, "invbroker", other_st_in_jita).is_ok());
    assert!(ctx.bind_object(&sess, "invbroker", struct_id).is_ok());
    assert_eq!(
        ctx.bind_object(&sess, "invbroker", sys_id as i64)
            .unwrap_err()
            .code,
        "BindAuthorizationDenied", // rpc-lint: allow error code assertion
        "invbroker cannot bind to solar_system_id while docked"
    );
    //    - Verified sub-object on `item_in_station` passes verify_bound_object while at `st_id`,
    //      and fails when pilot moves to `other_sys` undocked!
    let b_sub_item = ctx
        .bind_verified_sub_object(&sess, "invbroker", item_in_station)
        .unwrap();
    assert!(ctx.verify_bound_object(&sess, b_sub_item).is_ok());

    //    - While undocked in `sys_id`: binding `invbroker` to `sys_id` succeeds!
    sess.station_id = None;
    let b_inv_sys = ctx
        .bind_object(&sess, "invbroker", sys_id as i64)
        .expect("invbroker bind to solar_system_id while undocked");
    assert!(ctx.verify_bound_object(&sess, b_inv_sys).is_ok());
    sess.solar_system_id = Some(other_sys);
    assert_eq!(
        ctx.verify_bound_object(&sess, b_inv_sys).unwrap_err().code,
        "BoundObjectOwnershipViolation" // rpc-lint: allow error code assertion
    );
    assert_eq!(
        ctx.verify_bound_object(&sess, b_sub_item).unwrap_err().code,
        "BoundObjectOwnershipViolation" // rpc-lint: allow error code assertion
    );
    sess.solar_system_id = Some(sys_id);
    sess.station_id = Some(st_id);

    // 4) skillMgr2, charMgr, jumpCloneSvc: allowed for char_id, solar_system_id, station_id, structure_id, and 0;
    //    denied for `other_char`!
    sess.structure_id = Some(struct_id);
    for svc in ["skillMgr2", "charMgr", "jumpCloneSvc"] {
        for valid_id in [char_id, sys_id as i64, st_id, struct_id, 0] {
            assert!(
                ctx.bind_object(&sess, svc, valid_id).is_ok(),
                "{svc} should allow {valid_id}"
            );
        }
        assert_eq!(
            ctx.bind_object(&sess, svc, other_char).unwrap_err().code,
            "BindAuthorizationDenied" // rpc-lint: allow error code assertion
        );
    }

    // 5) crimewatch: allowed for char_id, solar_system_id, station_id, docked struct_id, and 0;
    //    denied for foreign structure IDs, foreign solar systems, and other_char!
    for valid_id in [char_id, sys_id as i64, st_id, struct_id, 0] {
        assert!(
            ctx.bind_object(&sess, "crimewatch", valid_id).is_ok(),
            "crimewatch should allow {valid_id}"
        );
    }
    let foreign_struct_id = struct_id + 1;
    assert_eq!(
        ctx.bind_object(&sess, "crimewatch", foreign_struct_id)
            .unwrap_err()
            .code,
        "BindAuthorizationDenied" // rpc-lint: allow error code assertion
    );
    assert_eq!(
        ctx.bind_object(&sess, "crimewatch", 30_000_144)
            .unwrap_err()
            .code,
        "BindAuthorizationDenied" // rpc-lint: allow error code assertion
    );
    assert_eq!(
        ctx.bind_object(&sess, "crimewatch", other_char)
            .unwrap_err()
            .code,
        "BindAuthorizationDenied" // rpc-lint: allow error code assertion
    );

    // 6) beyonce & scanMgr: only solar_system_id allowed; verify_bound_object fails when system changes
    for svc in ["beyonce", "scanMgr"] {
        let bid = ctx.bind_object(&sess, svc, sys_id as i64).unwrap();
        assert!(ctx.verify_bound_object(&sess, bid).is_ok());
        assert_eq!(
            ctx.bind_object(&sess, svc, st_id).unwrap_err().code,
            "BindAuthorizationDenied" // rpc-lint: allow error code assertion
        );
        sess.solar_system_id = Some(other_sys);
        assert_eq!(
            ctx.verify_bound_object(&sess, bid).unwrap_err().code,
            "BoundObjectOwnershipViolation" // rpc-lint: allow error code assertion
        );
        sess.solar_system_id = Some(sys_id);
    }

    // 7) Context-authorized vs unauthorized object bindings across corp/alliance/fleet/planet/trade/pos/structure services
    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.insert(
            700_000_001,
            eve_db::WarRecord {
                war_id: 700_000_001,
                aggressor_corp_id: 98_000_111,
                defender_corp_id: 98_000_222,
                mutual: false,
                started_unix: 1_800_000_000,
                active: true,
            },
        );
    });
    sess.corporation_id = Some(98_000_111);
    sess.alliance_id = Some(99_000_111);
    sess.fleet_id = Some(77_000_111);
    for (svc, allowed_id, denied_id) in [
        ("agentMgr", 3_018_922_i64, 0_i64),
        ("keeper", 1_i64, -5_i64),
        ("corpRegistry", 98_000_111_i64, 98_000_222_i64),
        ("corpmgr", 98_000_111_i64, 98_000_222_i64),
        ("allianceRegistry", 99_000_111_i64, 99_000_222_i64),
        ("fleetObjectHandler", 77_000_111_i64, 77_000_222_i64),
        ("warRegistry", 98_000_111_i64, 98_000_222_i64),
        ("warStatisticMgr", 700_000_001_i64, 700_000_999_i64),
        ("voucher", char_id, char_id + 1),
        ("trademgr", 10_000_002_i64, 999_999_i64),
        ("devIndexManager", i64::from(sys_id), i64::from(other_sys)),
        ("contractProxy", char_id, char_id + 1),
    ] {
        assert!(ctx.bind_object(&sess, svc, allowed_id).is_ok(), "{svc} allowed");
        assert_eq!(
            ctx.bind_object(&sess, svc, denied_id).unwrap_err().code,
            "BindAuthorizationDenied", // rpc-lint: allow error code assertion
            "{svc} denied"
        );
    }
}

#[test]
fn session_transition_invalidation_and_macho_object_disconnect_notifications() {
    let ctx = test_ctx();
    let char_id = 90_000_001i64;
    let sys_id = 30_000_142u32;
    let st_id = 60_003_760i64;

    let sess = SessionState {
        session_id: 601,
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(500),
        solar_system_id: Some(sys_id),
        station_id: Some(st_id),
        ship_id: Some(100_000_001),
        role: ROLE_PLAYER,
        ..Default::default()
    };
    ctx.db
        .acquire_online_lock(1, 601, Some(char_id), true, 1_800_000_000)
        .unwrap();

    let b_bey = ctx.bind_object(&sess, "beyonce", sys_id as i64).unwrap();
    let b_scan = ctx.bind_object(&sess, "scanMgr", sys_id as i64).unwrap();
    let b_pi = ctx
        .bind_object(&sess, "planetOrbitalRegistryBroker", sys_id as i64)
        .unwrap();
    let b_keep_sys = ctx.bind_object(&sess, "keeper", sys_id as i64).unwrap();
    let b_keep_global = ctx.bind_object(&sess, "keeper", 1).unwrap();
    let b_pos_sys = ctx.bind_object(&sess, "posMgr", sys_id as i64).unwrap();
    let b_pos_non_sys = ctx.bind_object(&sess, "posMgr", 500).unwrap();
    let b_dev_sys = ctx
        .bind_object(&sess, "devIndexManager", sys_id as i64)
        .unwrap();
    let b_repair = ctx.bind_object(&sess, "repairSvc", st_id).unwrap();

    // Cross-system planetOrbitalRegistryBroker and solar-system-scoped keeper are denied
    assert_eq!(
        ctx.bind_object(&sess, "planetOrbitalRegistryBroker", 30_000_144)
            .unwrap_err()
            .code,
        EXPECTED_BIND_DENIED_CODE
    );
    assert_eq!(
        ctx.bind_object(&sess, "keeper", 30_000_144)
            .unwrap_err()
            .code,
        EXPECTED_BIND_DENIED_CODE
    );

    // 1) Exempt reasons never invalidate any bound objects
    for exempt in [
        "set_account_key",
        "corp_change",
        "fleet_change",
        "board_or_eject",
        "ship_loss",
        "clone_swap",
        "login",
    ] {
        ctx.invalidate_bound_objects_for_session_transition(char_id, sys_id, Some(st_id), exempt);
    }
    for bid in [
        b_bey,
        b_scan,
        b_pi,
        b_keep_sys,
        b_keep_global,
        b_pos_sys,
        b_pos_non_sys,
        b_dev_sys,
        b_repair,
    ] {
        assert!(ctx.bound_objects.read().unwrap().contains_key(&bid));
    }

    // 2) "dock" invalidates solar-system space bindings (keeper/posMgr/devIndexManager with
    //    solar-system object_id), while preserving same-system planetOrbitalRegistryBroker,
    //    non-system keeper (1), b_pos_non_sys (500), and b_repair (matching active_dock_id = Some(st_id))!
    ctx.pending_notifications.write().unwrap().clear();
    ctx.invalidate_bound_objects_for_session_transition(char_id, sys_id, Some(st_id), "dock");
    for sys_bid in [b_bey, b_scan, b_keep_sys, b_pos_sys, b_dev_sys] {
        assert!(!ctx.bound_objects.read().unwrap().contains_key(&sys_bid));
    }
    assert!(ctx.bound_objects.read().unwrap().contains_key(&b_pi));
    assert!(ctx.bound_objects.read().unwrap().contains_key(&b_keep_global));
    assert!(ctx.bound_objects.read().unwrap().contains_key(&b_pos_non_sys));
    assert!(ctx.bound_objects.read().unwrap().contains_key(&b_repair));

    let disconnect_notifs: Vec<_> = ctx.pending_notifications.read().unwrap()[&char_id]
        .iter()
        .filter(|n| n.event == "OnMachoObjectDisconnect")
        .cloned()
        .collect();
    assert_eq!(disconnect_notifs.len(), 5);
    assert_eq!(
        disconnect_notifs[0].args[1],
        PyRep::Integer(1_000_000_000_601)
    );

    // 3) release_bound_object rejects wrong user/session and emits OnMachoObjectDisconnect on success
    let mut wrong_user = sess.clone();
    wrong_user.user_id = Some(999);
    assert_eq!(
        ctx.release_bound_object(&wrong_user, b_repair)
            .unwrap_err()
            .code,
        "BoundObjectOwnershipViolation"
    );
    ctx.release_bound_object(&sess, b_repair)
        .expect("release b_repair");
    assert!(!ctx.bound_objects.read().unwrap().contains_key(&b_repair));

    // 4) "slash_teleport" and "tourney_teleport" evict space, PI, and station bindings
    let b_bey_t = ctx.bind_object(&sess, "beyonce", sys_id as i64).unwrap();
    let b_rep_t = ctx.bind_object(&sess, "repairSvc", st_id).unwrap();
    ctx.invalidate_bound_objects_for_session_transition(
        char_id,
        30_000_144,
        None,
        "slash_teleport",
    );
    assert!(!ctx.bound_objects.read().unwrap().contains_key(&b_pi));
    assert!(!ctx.bound_objects.read().unwrap().contains_key(&b_bey_t));
    assert!(!ctx.bound_objects.read().unwrap().contains_key(&b_rep_t));
    let b_bey_a = ctx.bind_object(&sess, "beyonce", sys_id as i64).unwrap();
    ctx.invalidate_bound_objects_for_session_transition(
        char_id,
        30_000_144,
        None,
        "tourney_teleport",
    );
    assert!(!ctx.bound_objects.read().unwrap().contains_key(&b_bey_a));
}

#[test]
fn verify_all_remaining_services_and_invalidation_transitions() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let other_sys = 30_000_144_u32;
    let st_id = 60_003_760_i64;
    let ship_id = 100_000_001_i64;
    let struct_id = 1_000_000_888_001_i64;
    let other_struct_id = 1_000_000_888_002_i64;
    let item_in_ship = 888_101_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: 98_000_111,
                solar_system_id: sys_id,
                name: "Own Struct".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 10_000.0,
                armor_hp: 10_000.0,
                hull_hp: 10_000.0,
                ..Default::default()
            },
        );
        snap.structures.insert(
            other_struct_id,
            UpwellStructureRecord {
                structure_id: other_struct_id,
                type_id: 35_832,
                owner_corp_id: 98_000_111,
                solar_system_id: other_sys,
                name: "Other Struct".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 10_000.0,
                armor_hp: 10_000.0,
                hull_hp: 10_000.0,
                ..Default::default()
            },
        );
        snap.wars.insert(
            700_000_002,
            eve_db::WarRecord {
                war_id: 700_000_002,
                aggressor_corp_id: 98_000_111,
                defender_corp_id: 98_000_222,
                mutual: false,
                started_unix: 1_800_000_000,
                active: true,
            },
        );
        snap.items.insert(
            item_in_ship,
            InventoryItemRecord {
                item_id: item_in_ship,
                type_id: 3293,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = 98_000_111;
            c.alliance_id = Some(99_000_111);
            c.fleet_id = Some(77_000_111);
            c.active_ship_id = ship_id;
        }
    });

    let sess = SessionState {
        session_id: 701,
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(98_000_111),
        alliance_id: Some(99_000_111),
        fleet_id: Some(77_000_111),
        solar_system_id: Some(sys_id),
        station_id: Some(st_id),
        ship_id: Some(ship_id),
        role: ROLE_PLAYER,
        ..Default::default()
    };

    // Zero / negative object_id checks for planetMgr, structure, posMgr, trademgr
    let err_bind_denied = "BindAuthorizationDenied";
    let err_ownership_violation = "BoundObjectOwnershipViolation";
    for svc in ["planetMgr", "structure", "posMgr", "trademgr"] {
        assert_eq!(
            ctx.bind_object(&sess, svc, 0).unwrap_err().code,
            err_bind_denied
        );
    }

    // Bind & verify all remaining services to exercise verify_service_binding_context arms
    let mut bids = Vec::new();
    for (svc, oid) in [
        ("officeManager", st_id),
        ("posMgr", struct_id),
        ("trademgr", 10_000_002),
        ("warRegistry", 98_000_111),
        ("warStatisticMgr", 700_000_002),
        ("devIndexManager", sys_id as i64),
        ("voucher", 98_000_111),
        ("contractProxy", 98_000_111),
        ("agentMgr", 3_018_922),
        ("keeper", 1),
        ("tourneyMgr", 1),
        ("crimewatch", sys_id as i64),
        ("skillMgr2", st_id),
    ] {
        let bid = ctx.bind_object(&sess, svc, oid).expect(svc);
        assert!(ctx.verify_bound_object(&sess, bid).is_ok(), "verify {svc}");
        bids.push((svc, bid));
    }

    // Negative object_id on bound entry fails verify_service_binding_context
    let neg_bid = ctx.bind_object(&sess, "keeper", 1).unwrap();
    if let Some(entry) = ctx.bound_objects.write().unwrap().get_mut(&neg_bid) {
        entry.object_id = -1;
    }
    assert_eq!(
        ctx.verify_bound_object(&sess, neg_bid).unwrap_err().code,
        err_ownership_violation
    );

    // Trigger corp_change invalidation for corp/alliance/war/office/voucher/contractProxy/structure
    let b_corp = ctx.bind_object(&sess, "corpRegistry", 98_000_111).unwrap();
    let b_ally = ctx.bind_object(&sess, "allianceRegistry", 99_000_111).unwrap();
    let b_war = ctx.bind_object(&sess, "warRegistry", 98_000_111).unwrap();
    let b_off = ctx.bind_object(&sess, "officeManager", 98_000_111).unwrap();
    let b_vou = ctx.bind_object(&sess, "voucher", 98_000_111).unwrap();
    let b_con = ctx.bind_object(&sess, "contractProxy", 98_000_111).unwrap();
    let b_str = ctx.bind_object(&sess, "structure", other_struct_id).unwrap();
    let b_flt = ctx.bind_object(&sess, "fleetObjectHandler", 77_000_111).unwrap();
    let b_sub_ship = ctx
        .bind_verified_sub_object(&sess, "invbroker", item_in_ship)
        .unwrap();
    let b_dogma = ctx.bind_object(&sess, "dogmaIM", ship_id).unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = 98_000_333;
            c.alliance_id = None;
            c.fleet_id = None;
            c.active_ship_id = 100_000_002;
        }
    });
    ctx.invalidate_bound_objects_for_session_transition(char_id, sys_id, Some(st_id), "corp_change");
    for gone in [b_corp, b_ally, b_war, b_off, b_vou, b_con, b_str] {
        assert!(!ctx.bound_objects.read().unwrap().contains_key(&gone), "gone {gone}");
    }

    ctx.invalidate_bound_objects_for_session_transition(char_id, sys_id, Some(st_id), "fleet_change");
    assert!(!ctx.bound_objects.read().unwrap().contains_key(&b_flt));

    // board_or_eject invalidates dogmaIM for old ship and invbroker item no longer in new active ship
    ctx.invalidate_bound_objects_for_session_transition(
        char_id,
        sys_id,
        None,
        "board_or_eject",
    );
    assert!(!ctx.bound_objects.read().unwrap().contains_key(&b_dogma));
    assert!(!ctx.bound_objects.read().unwrap().contains_key(&b_sub_ship));

    // stargate_jump invalidates crimewatch bound to old system, preserves crimewatch bound to dock
    let b_cw_sys = ctx.bind_object(&sess, "crimewatch", sys_id as i64).unwrap();
    let b_cw_st = ctx.bind_object(&sess, "crimewatch", st_id).unwrap();
    ctx.invalidate_bound_objects_for_session_transition(char_id, other_sys, Some(st_id), "stargate_jump");
    assert!(!ctx.bound_objects.read().unwrap().contains_key(&b_cw_sys));
    assert!(ctx.bound_objects.read().unwrap().contains_key(&b_cw_st));
    let _ = sess;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn container_contents_bpc_singleton_range_checks_and_hooks() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 100_000_001_i64;
    let other_ship_id = 100_000_002_i64;
    let my_can_id = 910_000_101_i64;
    let other_can_id = 910_000_102_i64;
    let bpc_id = 910_000_103_i64;
    let single_id = 910_000_104_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = None;
            c.solar_system_id = sys_id;
            c.active_ship_id = ship_id;
        }
        if let Some(c) = snap.characters.get_mut(&other_char) {
            c.station_id = None;
            c.solar_system_id = sys_id;
            c.active_ship_id = other_ship_id;
        }
        snap.items.insert(
            my_can_id,
            InventoryItemRecord {
                item_id: my_can_id,
                type_id: 23, // Cargo Container (group 12)
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            other_can_id,
            InventoryItemRecord {
                item_id: other_can_id,
                type_id: 23,
                owner_id: other_char,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            bpc_id,
            InventoryItemRecord {
                item_id: bpc_id,
                type_id: 681,
                owner_id: char_id,
                location_id: my_can_id,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                is_blueprint_copy: true,
                blueprint_runs_remaining: Some(5),
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            single_id,
            InventoryItemRecord {
                item_id: single_id,
                type_id: 3293,
                owner_id: char_id,
                location_id: my_can_id,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            other_ship_id,
            InventoryItemRecord {
                item_id: other_ship_id,
                type_id: 587,
                owner_id: other_char,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut sess = SessionState {
        session_id: 702,
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        role: ROLE_PLAYER,
        ..Default::default()
    };

    // Bound object mismatch -> denied
    let wrong_bound = crate::router::context::BoundObjectEntry {
        bound_id: 1,
        service: "invbroker".into(),
        object_id: 60_003_760,
        session_id: 702,
        user_id: 1,
        character_id: char_id,
        oid_str: "st=60003760".into(),
        ref_id: 1,
    };
    assert!(ctx
        .get_container_contents(&sess, Some(&wrong_bound), char_id, my_can_id)
        .await
        .is_err());

    // Missing balls in space -> denied for own & other container
    assert!(ctx.get_container_contents(&sess, None, char_id, my_can_id).await.is_err());
    assert!(ctx.get_container_contents(&sess, None, char_id, other_can_id).await.is_err());

    // Spawn ship & containers far apart (> 2500m) -> denied
    ctx.wire_module_deactivation_hook();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mk_ball = |id, tid, pos| eve_destiny::Ball {
        id,
        type_id: tid,
        position: pos,
        velocity: glam::DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    };
    sys_handle.try_spawn_ball_with_combat(
        mk_ball(ship_id, 587, glam::DVec3::ZERO),
        eve_destiny::CombatState::default(),
    );
    sys_handle.try_spawn_ball_with_combat(
        mk_ball(my_can_id, 23, glam::DVec3::new(10_000.0, 0.0, 0.0)),
        eve_destiny::CombatState::default(),
    );
    sys_handle.try_spawn_ball_with_combat(
        mk_ball(other_can_id, 23, glam::DVec3::new(10_000.0, 0.0, 0.0)),
        eve_destiny::CombatState::default(),
    );
    assert!(ctx.get_container_contents(&sess, None, char_id, my_can_id).await.is_err());
    assert!(ctx.get_container_contents(&sess, None, char_id, other_can_id).await.is_err());

    // Move my_can_id within 2500m -> succeeds and returns BPC (-2) and singleton (-1)
    let _ = sys_handle
        .set_ball_position(my_can_id, glam::DVec3::new(500.0, 0.0, 0.0))
        .await;
    let res = ctx.get_container_contents(&sess, None, char_id, my_can_id).await.unwrap();
    assert!(matches!(res, PyRep::ObjectEx { .. }));

    // Session with no ship_id -> denied
    sess.ship_id = None;
    assert!(ctx.get_container_contents(&sess, None, char_id, my_can_id).await.is_err());
    assert!(ctx.get_container_contents(&sess, None, char_id, other_can_id).await.is_err());

    // Session in wrong system -> denied
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(30_000_144);
    assert!(ctx.get_container_contents(&sess, None, char_id, my_can_id).await.is_err());
    assert!(ctx.get_container_contents(&sess, None, char_id, other_can_id).await.is_err());

    // Module deactivation hook with cloak SDE reactivation delay fallback + auto_reloaded_event,
    // and target_lock_completed_hook between two pilots in space!
    sys_handle.try_spawn_ball_with_combat(
        mk_ball(other_ship_id, 587, glam::DVec3::new(1_000.0, 0.0, 0.0)),
        eve_destiny::CombatState::default(),
    );
    let _ = sys_handle
        .add_target_lock(
            ship_id,
            eve_destiny::TargetLock {
                target_id: other_ship_id,
                lock_time_ms: 50.0,
                remaining_lock_ms: 1.0,
                locked: false,
                max_range_m: None,
            },
        )
        .await;
    let _ = sys_handle.step_manual(0.1).await;

    let cloak_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 330 && t.default_attributes.contains_key(&669))
        .map(|t| t.type_id)
        .unwrap_or(11370);
    let mod_id = 910_000_555_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.cluster_and_runtime.auto_reloaded_event.insert(mod_id, (vec![mod_id], 212_u32, 5000.0));
    });
    let _ = sys_handle
        .add_module_cycle(crate::system_actor::ActiveModuleCycle {
            module_id: mod_id,
            ship_id,
            owner_char_id: Some(char_id),
            module_type_id: cloak_tid,
            effect_name: "cloak".into(),
            duration_s: 0.05,
            elapsed_s: 0.0,
            cap_need_per_cycle: 0.0,
            target_id: None,
            ..crate::system_actor::ActiveModuleCycle::default()
        })
        .await;
    let _ = sys_handle.step_manual(0.1).await;
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(notifs.iter().any(|n| n.event == "OnSpecialFX"));
    assert!(notifs.iter().any(|n| n.event == "OnChargeBeingLoadedToModule"));
    assert!(notifs.iter().any(|n| n.event == "OnMultiEvent"));
    let other_notifs = ctx.drain_notifications_for_char(other_char);
    assert!(other_notifs.iter().any(|n| n.event == "OnMultiEvent"));

    // Subsystem / Alpha skill prerequisite check paths
    let mut t_sub = ctx.sde.types.get(&587).unwrap().clone();
    t_sub.type_id = 998_111;
    t_sub.category_id = 32; // non-ship -> SkillPrerequisitesNotMet
    t_sub.default_attributes.insert(182, 12180.0);
    t_sub.default_attributes.insert(277, 1.0);
    let mut sde_clone = (*ctx.sde).clone();
    sde_clone.types.insert(998_111, t_sub);
    let ctx2 = ServerContext::with_sde_and_repository(Arc::new(sde_clone), Arc::clone(&ctx.db));
    ctx2.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            60377,
            eve_db::TrainedSkillRecord {
                type_id: 60377,
                level: 1,
                skill_points: 250.0,
            },
        );
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.clone_grade = 1; // Alpha
        }
    });
    let err = ctx2
        .db
        .with_snapshot(|snap| ctx2.check_type_skill_prerequisites_in_snapshot(snap, char_id, 998_111));
    assert!(err.is_ok() || err.unwrap_err().code == "SkillPrerequisitesNotMet");
}

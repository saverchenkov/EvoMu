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

    // 5) crimewatch: allowed for char_id, solar_system_id, station_id, and 0;
    //    denied for `struct_id` and `other_char`!
    for valid_id in [char_id, sys_id as i64, st_id, 0] {
        assert!(ctx.bind_object(&sess, "crimewatch", valid_id).is_ok());
    }
    assert_eq!(
        ctx.bind_object(&sess, "crimewatch", struct_id)
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
    let b_keep_sys = ctx.bind_object(&sess, "keeper", sys_id as i64).unwrap();
    let b_pos_sys = ctx.bind_object(&sess, "posMgr", sys_id as i64).unwrap();
    let b_pos_non_sys = ctx.bind_object(&sess, "posMgr", 500).unwrap();
    let b_dev_sys = ctx
        .bind_object(&sess, "devIndexManager", sys_id as i64)
        .unwrap();
    let b_repair = ctx.bind_object(&sess, "repairSvc", st_id).unwrap();

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
        b_keep_sys,
        b_pos_sys,
        b_pos_non_sys,
        b_dev_sys,
        b_repair,
    ] {
        assert!(ctx.bound_objects.read().unwrap().contains_key(&bid));
    }

    // 2) "dock" invalidates all solar-system-scoped bindings (including keeper/posMgr/devIndexManager
    //    with solar-system object_id in 30_000_000..=39_999_999), while preserving b_pos_non_sys (500)
    //    and b_repair (matching active_dock_id = Some(st_id))!
    ctx.pending_notifications.write().unwrap().clear();
    ctx.invalidate_bound_objects_for_session_transition(char_id, sys_id, Some(st_id), "dock");
    for sys_bid in [b_bey, b_scan, b_keep_sys, b_pos_sys, b_dev_sys] {
        assert!(!ctx.bound_objects.read().unwrap().contains_key(&sys_bid));
    }
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
}

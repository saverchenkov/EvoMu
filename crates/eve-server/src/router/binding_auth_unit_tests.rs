//! Comprehensive unit tests for `router::binding_auth`.

use super::test_ctx;
use crate::router::binding_auth::{
    authorize_service_binding, should_invalidate_bound_object, verify_service_binding_context,
};
use crate::router::context::BoundObjectEntry;
use eve_db::{
    InventoryFlag, InventoryItemRecord, PlanetaryColonyRecord, UpwellStructureRecord, WarRecord,
};
use eve_machonet::service_roles::ROLE_GML;
use eve_machonet::SessionState;

fn make_entry(service: &str, object_id: i64, char_id: i64) -> BoundObjectEntry {
    BoundObjectEntry {
        bound_id: 1,
        session_id: 10,
        user_id: 1,
        character_id: char_id,
        service: service.to_string(),
        object_id,
        oid_str: object_id.to_string(),
        ref_id: 0,
    }
}

#[test]
fn authorize_and_verify_binding_cover_every_service_and_edge_case() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32; // Jita
    let other_sys_id = 30_000_144_u32;
    let st_id = 60_003_760_i64; // Jita IV-4
    let other_st_id = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id != sys_id)
        .map(|s| s.station_id)
        .unwrap_or(60_008_494);
    let same_sys_other_st = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id == sys_id && s.station_id != st_id)
        .map(|s| s.station_id);
    let ship_id = 100_000_001_i64;
    let corp_id = 98_000_001_i64;
    let alliance_id = 99_000_001_i64;
    let fleet_id = 77_000_001_i64;
    let struct_here = 1_000_000_501_i64;
    let struct_corp = 1_000_000_502_i64;
    let struct_other = 1_000_000_503_i64;
    let item_in_ship = 880_001_i64;
    let item_in_sys = 880_002_i64;
    let item_elsewhere = 880_003_i64;
    let war_id = 700_100_001_i64;
    let colony_planet = 40_009_999_i64;

    let jita_planet = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .and_then(|s| s.planet_ids.first().copied())
        .unwrap_or(40_009_049);
    let jita_constellation = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.constellation_id)
        .unwrap_or(20_000_020);
    let jita_region = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.region_id)
        .unwrap_or(10_000_002);

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_here,
            UpwellStructureRecord {
                structure_id: struct_here,
                solar_system_id: sys_id,
                owner_corp_id: 98_999_999,
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_corp,
            UpwellStructureRecord {
                structure_id: struct_corp,
                solar_system_id: other_sys_id,
                owner_corp_id: corp_id,
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_other,
            UpwellStructureRecord {
                structure_id: struct_other,
                solar_system_id: other_sys_id,
                owner_corp_id: 98_999_999,
                ..Default::default()
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
                ..Default::default()
            },
        );
        snap.items.insert(
            item_in_sys,
            InventoryItemRecord {
                item_id: item_in_sys,
                type_id: 3293,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            item_elsewhere,
            InventoryItemRecord {
                item_id: item_elsewhere,
                type_id: 3293,
                owner_id: char_id,
                location_id: i64::from(other_sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.wars.insert(
            war_id,
            WarRecord {
                war_id,
                aggressor_corp_id: corp_id,
                defender_corp_id: 98_999_999,
                mutual: false,
                started_unix: 1,
                active: true,
            },
        );
        snap.planetary_colonies.insert(
            (char_id, colony_planet),
            PlanetaryColonyRecord {
                planet_id: colony_planet,
                owner_id: char_id,
                solar_system_id: sys_id,
                command_center_level: 1,
                pins_count: 0,
                extracted_commodities: Default::default(),
                last_extracted_unix: 0,
            },
        );
    });

    let mut sess = SessionState {
        session_id: 10,
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        constellation_id: Some(jita_constellation),
        region_id: Some(jita_region),
        station_id: Some(st_id),
        ship_id: Some(ship_id),
        corporation_id: Some(corp_id),
        alliance_id: Some(alliance_id),
        fleet_id: Some(fleet_id),
        ..Default::default()
    };

    // Negative object_id always denied for both authorize and verify
    for svc in ["populationCap", "beyonce", "i2", "agentMgr", "planetMgr"] {
        assert!(!authorize_service_binding(&ctx, &sess, char_id, svc, -1));
        assert!(!verify_service_binding_context(&ctx, &sess, &make_entry(svc, -1, char_id)));
    }

    // Helper asserting both authorize and verify agree on (service, object_id, expected)
    let assert_both = |s: &SessionState, svc: &str, oid: i64, expected: bool| {
        assert_eq!(
            authorize_service_binding(&ctx, s, char_id, svc, oid),
            expected,
            "authorize({svc}, {oid})"
        );
        assert_eq!(
            verify_service_binding_context(&ctx, s, &make_entry(svc, oid, char_id)),
            expected,
            "verify({svc}, {oid})"
        );
    };

    assert_both(&sess, "populationCap", char_id, true);
    assert_both(&sess, "populationCap", char_id + 1, false);

    for svc in ["beyonce", "scanMgr", "entity"] {
        assert_both(&sess, svc, i64::from(sys_id), true);
        assert_both(&sess, svc, i64::from(other_sys_id), false);
    }

    for svc in ["dogmaIM", "ship"] {
        assert_both(&sess, svc, ship_id, true);
        assert_both(&sess, svc, st_id, true);
        assert_both(&sess, svc, i64::from(sys_id), true);
        assert_both(&sess, svc, 999_999_999, false);
    }

    for svc in ["station", "stationSvc", "corpStationMgr", "repairSvc", "reprocessingSvc", "insuranceSvc"] {
        assert_both(&sess, svc, st_id, true);
        assert_both(&sess, svc, other_st_id, false);
    }

    for svc in ["skillMgr2", "charMgr", "jumpCloneSvc"] {
        assert_both(&sess, svc, char_id, true);
        assert_both(&sess, svc, i64::from(sys_id), true);
        assert_both(&sess, svc, st_id, true);
        assert_both(&sess, svc, 0, true);
        assert_both(&sess, svc, 999_999_999, false);
    }

    assert_both(&sess, "crimewatch", char_id, true);
    assert_both(&sess, "crimewatch", i64::from(sys_id), true);
    assert_both(&sess, "crimewatch", st_id, true);
    assert_both(&sess, "crimewatch", 0, true);
    assert_both(&sess, "crimewatch", 999_999_999, false);

    assert_both(&sess, "i2", 1, false);
    sess.role = ROLE_GML;
    assert_both(&sess, "i2", 1, true);
    assert_both(&sess, "i2", 0, false);

    for svc in ["corpRegistry", "corpmgr"] {
        assert_both(&sess, svc, 0, false);
        assert_both(&sess, svc, corp_id, true);
        assert_both(&sess, svc, st_id, true);
        assert_both(&sess, svc, corp_id + 1, false);
    }

    assert_both(&sess, "officeManager", 0, false);
    assert_both(&sess, "officeManager", corp_id, true);
    assert_both(&sess, "officeManager", st_id, true);
    assert_both(&sess, "officeManager", corp_id + 1, false);

    assert_both(&sess, "allianceRegistry", 0, false);
    assert_both(&sess, "allianceRegistry", alliance_id, true);
    assert_both(&sess, "allianceRegistry", alliance_id + 1, false);

    assert_both(&sess, "fleetObjectHandler", 0, false);
    assert_both(&sess, "fleetObjectHandler", fleet_id, true);
    assert_both(&sess, "fleetObjectHandler", fleet_id + 1, false);

    assert_both(&sess, "planetMgr", 0, false);
    assert_both(&sess, "planetMgr", jita_planet, true);
    assert_both(&sess, "planetMgr", colony_planet, true);
    assert_both(&sess, "planetMgr", 40_999_999, false);

    assert_both(&sess, "planetOrbitalRegistryBroker", i64::from(sys_id), true);
    assert_both(&sess, "planetOrbitalRegistryBroker", 99_999_999, false);

    assert_both(&sess, "structure", 0, false);
    assert_both(&sess, "structure", struct_here, true);
    assert_both(&sess, "structure", struct_corp, true);
    assert_both(&sess, "structure", struct_other, false);

    assert_both(&sess, "posMgr", 0, false);
    assert_both(&sess, "posMgr", i64::from(sys_id), true);
    assert_both(&sess, "posMgr", st_id, true);
    assert_both(&sess, "posMgr", corp_id, true);
    assert_both(&sess, "posMgr", struct_here, true);
    assert_both(&sess, "posMgr", struct_other, false);

    assert_both(&sess, "trademgr", 0, false);
    assert_both(&sess, "trademgr", st_id, true);
    assert_both(&sess, "trademgr", i64::from(sys_id), true);
    assert_both(&sess, "trademgr", i64::from(jita_region), true);
    if let Some(alt_st) = same_sys_other_st {
        assert_both(&sess, "trademgr", alt_st, true);
    }
    assert_both(&sess, "trademgr", other_st_id, false);
    assert_both(&sess, "trademgr", struct_here, true);
    assert_both(&sess, "trademgr", struct_corp, true);
    assert_both(&sess, "trademgr", struct_other, false);

    assert_both(&sess, "warRegistry", 0, false);
    assert_both(&sess, "warRegistry", corp_id, true);
    assert_both(&sess, "warRegistry", alliance_id, true);
    assert_both(&sess, "warRegistry", corp_id + 1, false);

    assert_both(&sess, "warStatisticMgr", 0, false);
    assert_both(&sess, "warStatisticMgr", war_id, true);
    assert_both(&sess, "warStatisticMgr", war_id + 1, false);

    assert_both(&sess, "devIndexManager", 0, false);
    assert_both(&sess, "devIndexManager", i64::from(sys_id), true);
    assert_both(&sess, "devIndexManager", i64::from(jita_constellation), true);
    assert_both(&sess, "devIndexManager", i64::from(jita_region), true);
    assert_both(&sess, "devIndexManager", i64::from(other_sys_id), false);

    assert_both(&sess, "voucher", 0, false);
    assert_both(&sess, "voucher", char_id, true);
    assert_both(&sess, "voucher", corp_id, true);
    assert_both(&sess, "voucher", char_id + 1, false);

    assert_both(&sess, "contractProxy", 0, false);
    assert_both(&sess, "contractProxy", char_id, true);
    assert_both(&sess, "contractProxy", corp_id, true);
    assert_both(&sess, "contractProxy", st_id, true);
    assert_both(&sess, "contractProxy", i64::from(sys_id), true);
    assert_both(&sess, "contractProxy", char_id + 1, false);

    for svc in ["agentMgr", "keeper", "tourneyMgr"] {
        assert_both(&sess, svc, 1, true);
        assert_both(&sess, svc, 0, false);
    }
    assert_both(&sess, "unknownService", 1, false);

    // invbroker docked vs undocked branches
    assert_both(&sess, "invbroker", char_id, true);
    assert_both(&sess, "invbroker", ship_id, true);
    assert_both(&sess, "invbroker", st_id, true);
    assert_both(&sess, "invbroker", i64::from(sys_id), false); // blocked while docked
    assert_both(&sess, "invbroker", item_in_ship, true);
    assert_both(&sess, "invbroker", item_in_sys, false);
    assert_both(&sess, "invbroker", struct_here, true);
    assert_both(&sess, "invbroker", struct_other, false);

    sess.station_id = None;
    sess.structure_id = Some(struct_here);
    assert_both(&sess, "dogmaIM", struct_here, true);
    assert_both(&sess, "station", struct_here, true);
    assert_both(&sess, "skillMgr2", struct_here, true);
    assert_both(&sess, "corpRegistry", struct_here, true);
    assert_both(&sess, "officeManager", struct_here, true);
    assert_both(&sess, "contractProxy", struct_here, true);
    assert_both(&sess, "invbroker", struct_here, true);
    assert_both(&sess, "crimewatch", struct_here, true);

    sess.structure_id = None; // undocked in space
    assert_both(&sess, "officeManager", corp_id, false);
    assert_both(&sess, "invbroker", i64::from(sys_id), true);
    assert_both(&sess, "invbroker", item_in_sys, true);
    assert_both(&sess, "invbroker", item_elsewhere, false);
}

#[test]
fn should_invalidate_bound_object_covers_all_reasons_and_services() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let other_sys = 30_000_144_u32;
    let st_id = 60_003_760_i64;
    let other_st = 60_008_494_i64;
    let ship_id = ctx.db.with_snapshot(|s| s.characters[&char_id].active_ship_id);
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    let struct_here = 1_000_000_601_i64;
    let struct_away = 1_000_000_602_i64;
    let item_in_st = 881_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.alliance_id = Some(99_000_001);
        ch.fleet_id = Some(77_000_001);
        snap.structures.insert(
            struct_here,
            UpwellStructureRecord {
                structure_id: struct_here,
                solar_system_id: sys_id,
                owner_corp_id: 98_999_999,
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_away,
            UpwellStructureRecord {
                structure_id: struct_away,
                solar_system_id: other_sys,
                owner_corp_id: 98_999_999,
                ..Default::default()
            },
        );
        snap.items.insert(
            item_in_st,
            InventoryItemRecord {
                item_id: item_in_st,
                type_id: 3293,
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    // set_account_key / login never invalidate
    for reason in ["set_account_key", "login"] {
        assert!(!should_invalidate_bound_object(
            &ctx,
            &make_entry("beyonce", i64::from(other_sys), char_id),
            sys_id,
            None,
            reason
        ));
    }

    // corp_change
    assert!(should_invalidate_bound_object(&ctx, &make_entry("corpRegistry", corp_id + 1, char_id), sys_id, Some(st_id), "corp_change"));
    assert!(!should_invalidate_bound_object(&ctx, &make_entry("corpRegistry", corp_id, char_id), sys_id, Some(st_id), "corp_change"));
    assert!(should_invalidate_bound_object(&ctx, &make_entry("allianceRegistry", 99_000_002, char_id), sys_id, Some(st_id), "corp_change"));
    assert!(!should_invalidate_bound_object(&ctx, &make_entry("allianceRegistry", 99_000_001, char_id), sys_id, Some(st_id), "corp_change"));
    assert!(should_invalidate_bound_object(&ctx, &make_entry("warRegistry", corp_id + 1, char_id), sys_id, Some(st_id), "corp_change"));
    assert!(!should_invalidate_bound_object(&ctx, &make_entry("warRegistry", 99_000_001, char_id), sys_id, Some(st_id), "corp_change"));
    assert!(should_invalidate_bound_object(&ctx, &make_entry("officeManager", corp_id + 1, char_id), sys_id, Some(st_id), "corp_change"));
    assert!(!should_invalidate_bound_object(&ctx, &make_entry("officeManager", st_id, char_id), sys_id, Some(st_id), "corp_change"));
    assert!(should_invalidate_bound_object(&ctx, &make_entry("voucher", corp_id + 1, char_id), sys_id, Some(st_id), "corp_change"));
    assert!(!should_invalidate_bound_object(&ctx, &make_entry("voucher", char_id, char_id), sys_id, Some(st_id), "corp_change"));
    assert!(should_invalidate_bound_object(&ctx, &make_entry("contractProxy", 123_456, char_id), sys_id, Some(st_id), "corp_change"));
    assert!(!should_invalidate_bound_object(&ctx, &make_entry("contractProxy", i64::from(sys_id), char_id), sys_id, Some(st_id), "corp_change"));
    assert!(should_invalidate_bound_object(&ctx, &make_entry("structure", struct_away, char_id), sys_id, None, "corp_change"));
    assert!(!should_invalidate_bound_object(&ctx, &make_entry("posMgr", struct_here, char_id), sys_id, None, "corp_change"));
    assert!(!should_invalidate_bound_object(&ctx, &make_entry("beyonce", i64::from(sys_id), char_id), sys_id, None, "corp_change"));

    // fleet_change
    assert!(should_invalidate_bound_object(&ctx, &make_entry("fleetObjectHandler", 77_000_002, char_id), sys_id, None, "fleet_change"));
    assert!(!should_invalidate_bound_object(&ctx, &make_entry("fleetObjectHandler", 77_000_001, char_id), sys_id, None, "fleet_change"));
    assert!(!should_invalidate_bound_object(&ctx, &make_entry("beyonce", i64::from(sys_id), char_id), sys_id, None, "fleet_change"));

    // board_or_eject / ship_loss / clone_swap
    for reason in ["board_or_eject", "ship_loss", "clone_swap"] {
        assert!(should_invalidate_bound_object(&ctx, &make_entry("ship", ship_id + 1, char_id), sys_id, Some(st_id), reason));
        assert!(!should_invalidate_bound_object(&ctx, &make_entry("dogmaIM", ship_id, char_id), sys_id, Some(st_id), reason));
        assert!(!should_invalidate_bound_object(&ctx, &make_entry("invbroker", item_in_st, char_id), sys_id, Some(st_id), reason));
        assert!(should_invalidate_bound_object(&ctx, &make_entry("invbroker", 999_999, char_id), sys_id, Some(st_id), reason));
        assert!(should_invalidate_bound_object(&ctx, &make_entry("station", other_st, char_id), sys_id, Some(st_id), reason));
        assert!(!should_invalidate_bound_object(&ctx, &make_entry("station", st_id, char_id), sys_id, Some(st_id), reason));
        assert!(should_invalidate_bound_object(&ctx, &make_entry("officeManager", other_st, char_id), sys_id, Some(st_id), reason));
        assert!(!should_invalidate_bound_object(&ctx, &make_entry("officeManager", corp_id, char_id), sys_id, Some(st_id), reason));
        assert!(should_invalidate_bound_object(&ctx, &make_entry("structure", struct_away, char_id), sys_id, None, reason));
        assert!(should_invalidate_bound_object(&ctx, &make_entry("beyonce", i64::from(other_sys), char_id), sys_id, None, reason));
        assert!(!should_invalidate_bound_object(&ctx, &make_entry("agentMgr", 3_018_922, char_id), sys_id, None, reason));
    }

    // Space jumps & dock/podding/clone_jump transitions
    for jump in ["stargate_jump", "wormhole_jump", "jump_drive", "cyno_jump", "filamentJump", "filament_jump", "jump_bridge", "shipcaster"] {
        assert!(should_invalidate_bound_object(&ctx, &make_entry("beyonce", i64::from(sys_id), char_id), sys_id, None, jump));
        assert!(should_invalidate_bound_object(&ctx, &make_entry("keeper", i64::from(sys_id), char_id), sys_id, None, jump));
        assert!(should_invalidate_bound_object(&ctx, &make_entry("crimewatch", i64::from(sys_id), char_id), sys_id, None, jump));
    }
    for dock in ["dock", "podding", "clone_jump", "turbo_dock", "self_destruct_clone"] {
        assert!(should_invalidate_bound_object(&ctx, &make_entry("beyonce", i64::from(sys_id), char_id), sys_id, Some(st_id), dock));
        assert!(!should_invalidate_bound_object(&ctx, &make_entry("crimewatch", st_id, char_id), sys_id, Some(st_id), dock));
        assert!(!should_invalidate_bound_object(&ctx, &make_entry("crimewatch", char_id, char_id), sys_id, Some(st_id), dock));
        assert!(!should_invalidate_bound_object(&ctx, &make_entry("crimewatch", 0, char_id), sys_id, Some(st_id), dock));
    }
    assert!(!should_invalidate_bound_object(&ctx, &make_entry("keeper", 500, char_id), sys_id, Some(st_id), "dock"));
    assert!(should_invalidate_bound_object(&ctx, &make_entry("allianceRegistry", 99_000_002, char_id), sys_id, None, "undock"));
    assert!(should_invalidate_bound_object(&ctx, &make_entry("fleetObjectHandler", 77_000_002, char_id), sys_id, None, "undock"));
    assert!(should_invalidate_bound_object(&ctx, &make_entry("warRegistry", 123_456, char_id), sys_id, None, "undock"));
    assert!(!should_invalidate_bound_object(&ctx, &make_entry("agentMgr", 3_018_922, char_id), sys_id, None, "undock"));
}

#[test]
fn issue_803_invbroker_destroyed_dynamic_ship_stub_auth_verify_and_invalidation() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let sys_id = 30_000_142_u32;
    let st_id = 60_003_760_i64;
    let other_st = 60_008_494_i64;
    let capsule_id = 200_294_811_i64;
    let destroyed_ship_id = 200_246_015_i64;
    let foreign_live_ship_id = 200_888_777_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = capsule_id;
        }
        snap.items.insert(
            foreign_live_ship_id,
            InventoryItemRecord {
                item_id: foreign_live_ship_id,
                type_id: 587,
                owner_id: other_char,
                location_id: other_st,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.killmails.insert(
            1,
            eve_db::KillmailRecord {
                kill_id: 1,
                solar_system_id: sys_id,
                killed_unix: 1_800_000_010,
                victim_character_id: char_id,
                ship_type_id: 587,
                ..Default::default()
            },
        );
    });

    let sess = SessionState {
        session_id: 42,
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(capsule_id),
        ..Default::default()
    };

    // Destroyed dynamic ship stub is authorized and verified for the owning session's character.
    assert!(authorize_service_binding(
        &ctx,
        &sess,
        char_id,
        "invbroker",
        destroyed_ship_id
    ));
    let owned_entry = make_entry("invbroker", destroyed_ship_id, char_id);
    assert!(verify_service_binding_context(&ctx, &sess, &owned_entry));

    // Teardown and dock transitions do not invalidate the destroyed ship stub invbroker binding.
    for (dock_opt, reason) in [
        (None, "ship_loss"),
        (Some(st_id), "dock"),
        (Some(st_id), "session_location_change"),
        (Some(st_id), "podding"),
    ] {
        assert!(
            !should_invalidate_bound_object(&ctx, &owned_entry, sys_id, dock_opt, reason),
            "reason={reason} should preserve destroyed-ship invbroker stub"
        );
    }

    // Mismatched character/session ownership, unauthenticated session, non-stub IDs,
    // and foreign live dynamic ships at another location remain rejected.
    let other_sess = SessionState {
        session_id: 43,
        user_id: Some(2),
        character_id: Some(other_char),
        solar_system_id: Some(sys_id),
        ship_id: Some(capsule_id),
        ..Default::default()
    };
    assert!(!verify_service_binding_context(&ctx, &other_sess, &owned_entry));

    let unauth_sess = SessionState {
        user_id: None,
        ..sess.clone()
    };
    assert!(!authorize_service_binding(
        &ctx,
        &unauth_sess,
        char_id,
        "invbroker",
        destroyed_ship_id
    ));
    assert!(!verify_service_binding_context(
        &ctx,
        &unauth_sess,
        &owned_entry
    ));

    for bad_id in [199_999_999_i64, 999_999, 1_000_000_000_000, foreign_live_ship_id] {
        assert!(
            !authorize_service_binding(&ctx, &sess, char_id, "invbroker", bad_id),
            "bad_id={bad_id} must not be authorized"
        );
        assert!(
            !verify_service_binding_context(
                &ctx,
                &sess,
                &make_entry("invbroker", bad_id, char_id)
            ),
            "bad_id={bad_id} must not verify"
        );
        assert!(should_invalidate_bound_object(
            &ctx,
            &make_entry("invbroker", bad_id, char_id),
            sys_id,
            None,
            "ship_loss"
        ));
    }
}

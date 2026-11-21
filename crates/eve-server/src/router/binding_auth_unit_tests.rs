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
fn ship_binding_rejects_dogma_station_accessibility_while_dogma_im_stays_symmetric() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let other_sys_id = 30_000_144_u32;
    let st_id = 60_003_760_i64;
    let other_st_id = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id != sys_id && s.station_id != st_id)
        .map(|s| s.station_id)
        .unwrap_or(60_008_494);
    let same_sys_other_st = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id == sys_id && s.station_id != st_id)
        .map(|s| s.station_id)
        .expect("second station in Jita");
    let struct_here = 1_000_000_601_i64;
    let ship_id = 100_000_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.home_station_id = st_id;
        }
        snap.structures.insert(
            struct_here,
            UpwellStructureRecord {
                structure_id: struct_here,
                solar_system_id: sys_id,
                owner_corp_id: 98_999_999,
                ..Default::default()
            },
        );
    });

    let mut undocked = SessionState::new(91_150);
    undocked.user_id = Some(1);
    undocked.character_id = Some(char_id);
    undocked.solar_system_id = Some(sys_id);
    undocked.ship_id = Some(ship_id);

    for target in [st_id, same_sys_other_st, struct_here, other_st_id, 999_999_999] {
        assert!(
            !authorize_service_binding(&ctx, &undocked, char_id, "ship", target),
            "undocked ship bind must deny {target}"
        );
        assert!(
            !verify_service_binding_context(&ctx, &undocked, &make_entry("ship", target, char_id)),
            "undocked ship verify must deny {target}"
        );
    }
    for allowed in [ship_id, i64::from(sys_id)] {
        assert!(authorize_service_binding(&ctx, &undocked, char_id, "ship", allowed));
        assert!(verify_service_binding_context(
            &ctx,
            &undocked,
            &make_entry("ship", allowed, char_id)
        ));
    }

    for accessible in [ship_id, i64::from(sys_id), st_id, same_sys_other_st, struct_here] {
        assert!(
            authorize_service_binding(&ctx, &undocked, char_id, "dogmaIM", accessible),
            "dogmaIM authorize should allow {accessible}"
        );
        assert!(
            verify_service_binding_context(
                &ctx,
                &undocked,
                &make_entry("dogmaIM", accessible, char_id)
            ),
            "dogmaIM verify should agree symmetrically for {accessible}"
        );
    }
    assert!(!verify_service_binding_context(
        &ctx,
        &undocked,
        &make_entry("dogmaIM", st_id, char_id + 1)
    ));
    assert!(!authorize_service_binding(&ctx, &undocked, char_id, "dogmaIM", other_st_id));
    assert!(!verify_service_binding_context(
        &ctx,
        &undocked,
        &make_entry("dogmaIM", other_st_id, char_id)
    ));

    let mut docked = undocked.clone();
    docked.station_id = Some(st_id);
    assert!(authorize_service_binding(&ctx, &docked, char_id, "ship", st_id));
    assert!(verify_service_binding_context(
        &ctx,
        &docked,
        &make_entry("ship", st_id, char_id)
    ));
    assert!(!authorize_service_binding(&ctx, &docked, char_id, "ship", same_sys_other_st));
    assert!(!authorize_service_binding(&ctx, &docked, char_id, "ship", struct_here));
    let _ = other_sys_id;
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

    assert_both(&sess, "agentMgr", 3_008_416, true);
    assert_both(&sess, "agentMgr", 1, false);
    assert_both(&sess, "agentMgr", 0, false);

    assert_both(&sess, "keeper", i64::from(sys_id), true);
    assert_both(&sess, "keeper", i64::from(other_sys_id), false);
    assert_both(&sess, "keeper", 1, false);
    assert_both(&sess, "keeper", 0, false);
    if let Some(&dungeon_id) = ctx.sde.dungeons.keys().next() {
        assert_both(&sess, "keeper", i64::from(dungeon_id), true);
        let mut player_sess = sess.clone();
        player_sess.role = 0;
        assert_both(&player_sess, "keeper", i64::from(dungeon_id), false);
    }

    assert_both(&sess, "tourneyMgr", 1, false);
    assert_both(&sess, "tourneyMgr", 0, false);
    let tourney_match_id = 5_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut m = eve_db::TourneyMatch::new(
            tourney_match_id,
            77,
            i64::from(other_sys_id),
            100,
            600,
            120,
            1,
        );
        m.red_players.push(eve_db::TourneyPilot {
            character_id: char_id,
            ship_type_id: 587,
            ship_id: 0,
            points: 5,
        });
        snap.tourney_state.matches.insert(tourney_match_id, m);
    });
    assert_both(&sess, "tourneyMgr", tourney_match_id, true);
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

#[test]
fn invbroker_solar_system_binding_and_invalidation_for_upwell_structures() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let other_sys = 30_000_144_u32;
    let st_id = 60_003_760_i64;
    let struct_here = 1_000_991_062_i64;
    let struct_away = 1_000_991_063_i64;
    let space_item_in_sys = 1_000_991_064_i64;
    let hangar_item_in_struct = 1_000_991_065_i64;

    ctx.db.with_mut_snapshot(|snap| {
        for (sid, s_sys) in [(struct_here, sys_id), (struct_away, other_sys)] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: 35_832,
                    owner_corp_id: 98_999_001,
                    solar_system_id: s_sys,
                    name: format!("Struct {sid}"),
                    state: "ShieldVulnerable".into(),
                    ..Default::default()
                },
            );
        }
        snap.items.insert(
            space_item_in_sys,
            InventoryItemRecord {
                item_id: space_item_in_sys,
                type_id: 23,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            hangar_item_in_struct,
            InventoryItemRecord {
                item_id: hangar_item_in_struct,
                type_id: 587,
                owner_id: char_id,
                location_id: struct_here,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let mut sess = SessionState {
        session_id: 1062,
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(1_000_170),
        solar_system_id: Some(sys_id),
        station_id: None,
        structure_id: Some(struct_here),
        ship_id: Some(100_000_001),
        ..Default::default()
    };

    // #1062: while docked in an Upwell structure (`station_id == None`), V24 `invbroker` binds
    // to `solar_system_id` and accepts items in `structure_id`, while blocking space cans
    // floating in `solar_system_id` and foreign-system bindings.
    assert!(authorize_service_binding(
        &ctx,
        &sess,
        char_id,
        "invbroker",
        i64::from(sys_id)
    ));
    assert!(authorize_service_binding(
        &ctx,
        &sess,
        char_id,
        "invbroker",
        struct_here
    ));
    assert!(authorize_service_binding(
        &ctx,
        &sess,
        char_id,
        "invbroker",
        hangar_item_in_struct
    ));
    assert!(!authorize_service_binding(
        &ctx,
        &sess,
        char_id,
        "invbroker",
        i64::from(other_sys)
    ));
    assert!(!authorize_service_binding(
        &ctx,
        &sess,
        char_id,
        "invbroker",
        space_item_in_sys
    ));

    // Docking at an Upwell structure keeps `invbroker(sys_id)` valid while invalidating space
    // cans in `sys_id`; docking at an NPC station invalidates `invbroker(sys_id)`.
    assert!(!should_invalidate_bound_object(
        &ctx,
        &make_entry("invbroker", i64::from(sys_id), char_id),
        sys_id,
        Some(struct_here),
        "dock"
    ));
    assert!(should_invalidate_bound_object(
        &ctx,
        &make_entry("invbroker", space_item_in_sys, char_id),
        sys_id,
        Some(struct_here),
        "dock"
    ));
    assert!(should_invalidate_bound_object(
        &ctx,
        &make_entry("invbroker", i64::from(sys_id), char_id),
        sys_id,
        Some(st_id),
        "dock"
    ));
    assert!(should_invalidate_bound_object(
        &ctx,
        &make_entry("invbroker", i64::from(sys_id), char_id),
        other_sys,
        Some(struct_away),
        "jump"
    ));

    // NPC station docking still blocks `invbroker` binding directly to `solar_system_id`.
    sess.station_id = Some(st_id);
    sess.structure_id = None;
    assert!(!authorize_service_binding(
        &ctx,
        &sess,
        char_id,
        "invbroker",
        i64::from(sys_id)
    ));
}

#[test]
fn binding_auth_1350_invbroker_office_war_keeper_agent_tourney_boundaries() {
    use eve_db::{
        corp_roles, CorporationRecord, PersistedNegKind, PersistedWarNegotiation, TourneyArenaSystem,
        TourneyMatch, TourneyPilot,
    };
    use eve_machonet::service_roles::{ROLE_GMH, ROLE_PLAYER_DEFAULT};

    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let other_sys_id = 30_000_144_u32;
    let st_id = 60_003_760_i64;
    let ceo_char = 90_000_001_i64;
    let member_char = 90_000_002_i64;
    let outsider_char = 90_000_777_i64;
    let corp_id = 98_000_001_i64;
    let same_ally_corp = 98_000_050_i64;
    let accepted_ally_corp = 98_000_051_i64;
    let future_ally_corp = 98_000_052_i64;
    let unrelated_corp = 98_000_099_i64;
    let war_id = 700_555_001_i64;
    let alliance_id = 99_555_001_i64;

    let other_ship_in_station = 880_555_001_i64;
    let corp_item_in_station = 880_555_002_i64;
    let own_item_in_station = 880_555_003_i64;
    let other_item_in_space = 880_555_004_i64;
    let tourney_match_id = 910_001_i64;
    let tourney_id = 42_i64;

    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.alliance_id = Some(alliance_id);
        }
        snap.corporations.insert(
            same_ally_corp,
            {
                let mut c = CorporationRecord::default();
                c.corporation_id = same_ally_corp;
                c.name = "Same Ally".into();
                c.ticker = "SAL".into();
                c.ceo_id = outsider_char;
                c.alliance_id = Some(alliance_id);
                c
            },
        );
        for cid in [accepted_ally_corp, future_ally_corp, unrelated_corp] {
            snap.corporations.insert(
                cid,
                {
                    let mut c = CorporationRecord::default();
                    c.corporation_id = cid;
                    c.name = format!("Corp {cid}");
                    c.ticker = "CRP".into();
                    c.ceo_id = outsider_char;
                    c
                },
            );
        }
        snap.items.insert(
            other_ship_in_station,
            InventoryItemRecord {
                item_id: other_ship_in_station,
                type_id: 587,
                owner_id: outsider_char,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            corp_item_in_station,
            InventoryItemRecord {
                item_id: corp_item_in_station,
                type_id: 3293,
                owner_id: corp_id,
                location_id: st_id,
                flag: InventoryFlag::CorpSAG1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            own_item_in_station,
            InventoryItemRecord {
                item_id: own_item_in_station,
                type_id: 34,
                owner_id: member_char,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            other_item_in_space,
            InventoryItemRecord {
                item_id: other_item_in_space,
                type_id: 3293,
                owner_id: outsider_char,
                location_id: i64::from(sys_id),
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
                defender_corp_id: unrelated_corp,
                mutual: false,
                started_unix: now_unix.saturating_sub(100),
                active: true,
            },
        );
        snap.war_state.negotiations.push(PersistedWarNegotiation {
            id: 1,
            war_id,
            kind: PersistedNegKind::Ally,
            from_corp: accepted_ally_corp,
            isk: Default::default(),
            message: String::new(),
            status: "Accepted".into(),
            effective_unix: Some(now_unix.saturating_sub(10)),
        });
        snap.war_state.negotiations.push(PersistedWarNegotiation {
            id: 2,
            war_id,
            kind: PersistedNegKind::Ally,
            from_corp: future_ally_corp,
            isk: Default::default(),
            message: String::new(),
            status: "Accepted".into(),
            effective_unix: Some(now_unix + 3_600),
        });
        let mut tm = TourneyMatch::new(
            tourney_match_id,
            tourney_id,
            i64::from(other_sys_id),
            100,
            600,
            120,
            1,
        );
        tm.blue_players.push(TourneyPilot {
            character_id: member_char,
            ship_type_id: 587,
            ship_id: 0,
            points: 10,
        });
        tm.referees.push(ceo_char);
        snap.tourney_state.matches.insert(tourney_match_id, tm);
        snap.tourney_state
            .tournaments
            .insert(tourney_id, "AT".into());
        snap.tourney_state.arena_systems.insert(
            i64::from(other_sys_id),
            TourneyArenaSystem::default(),
        );
    });

    let before = ctx.db.snapshot_tables();

    let mut member_sess = SessionState {
        session_id: 601,
        user_id: Some(2),
        character_id: Some(member_char),
        solar_system_id: Some(sys_id),
        station_id: Some(st_id),
        corporation_id: Some(corp_id),
        role: ROLE_PLAYER_DEFAULT,
        ..Default::default()
    };

    // 1. invbroker item ownership + corp hangar role checks (including pending role exclusion)
    assert!(!authorize_service_binding(
        &ctx,
        &member_sess,
        member_char,
        "invbroker",
        other_ship_in_station
    ));
    assert!(!verify_service_binding_context(
        &ctx,
        &member_sess,
        &make_entry("invbroker", other_ship_in_station, member_char)
    ));
    assert!(authorize_service_binding(
        &ctx,
        &member_sess,
        member_char,
        "invbroker",
        own_item_in_station
    ));
    assert!(!authorize_service_binding(
        &ctx,
        &member_sess,
        member_char,
        "invbroker",
        corp_item_in_station
    ));

    assert_eq!(ctx.db.snapshot_tables(), before);

    // Pending (unmatured) corp hangar grant must NOT authorize corp item bind yet;
    // matured hangar take role / director / CEO must authorize it.
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_governance
            .entry(corp_id)
            .or_default()
            .pending_role_grants
            .insert(
                member_char,
                (corp_roles::ROLE_HANGAR_CAN_TAKE[0], now_unix + 86_400),
            );
    });
    assert!(!authorize_service_binding(
        &ctx,
        &member_sess,
        member_char,
        "invbroker",
        corp_item_in_station
    ));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(gov) = snap.corp_governance.get_mut(&corp_id) {
            gov.pending_role_grants.remove(&member_char);
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.member_roles
                .insert(member_char, corp_roles::ROLE_HANGAR_CAN_QUERY[0]);
        }
    });
    assert!(authorize_service_binding(
        &ctx,
        &member_sess,
        member_char,
        "invbroker",
        corp_item_in_station
    ));
    assert!(verify_service_binding_context(
        &ctx,
        &member_sess,
        &make_entry("invbroker", corp_item_in_station, member_char)
    ));

    // Undocked space items still reject cross-owner binding
    member_sess.station_id = None;
    assert!(!authorize_service_binding(
        &ctx,
        &member_sess,
        member_char,
        "invbroker",
        other_item_in_space
    ));

    // 2. officeManager: docked corp members may bind corp_id or docked station/structure; undocked & foreign corps denied.
    assert!(!authorize_service_binding(
        &ctx,
        &member_sess,
        member_char,
        "officeManager",
        st_id
    ));
    assert!(!authorize_service_binding(
        &ctx,
        &member_sess,
        member_char,
        "officeManager",
        corp_id
    ));
    member_sess.station_id = Some(st_id);
    assert!(authorize_service_binding(
        &ctx,
        &member_sess,
        member_char,
        "officeManager",
        corp_id
    )); // holds ROLE_HANGAR_CAN_QUERY[0]
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(member_char, 0);
        }
    });
    assert!(authorize_service_binding(
        &ctx,
        &member_sess,
        member_char,
        "officeManager",
        corp_id
    )); // docked corp member allowed even with 0 corp roles
    assert!(authorize_service_binding(
        &ctx,
        &member_sess,
        member_char,
        "officeManager",
        st_id
    ));
    assert!(!authorize_service_binding(
        &ctx,
        &member_sess,
        member_char,
        "officeManager",
        unrelated_corp
    ));

    // Capture snapshot baseline after role transitions so read-only checks prove zero mutation.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(member_char, 0);
        }
    });
    let before = ctx.db.snapshot_tables();

    // 3. warStatisticMgr: involvement + existence checks (participant, same alliance, active ally, GM)
    let mut ally_sess = SessionState {
        session_id: 602,
        user_id: Some(3),
        character_id: Some(outsider_char),
        corporation_id: Some(same_ally_corp),
        ..Default::default()
    };
    assert!(authorize_service_binding(
        &ctx,
        &ally_sess,
        outsider_char,
        "warStatisticMgr",
        war_id
    ));
    ally_sess.corporation_id = Some(accepted_ally_corp);
    assert!(authorize_service_binding(
        &ctx,
        &ally_sess,
        outsider_char,
        "warStatisticMgr",
        war_id
    ));
    ally_sess.corporation_id = Some(future_ally_corp);
    assert!(!authorize_service_binding(
        &ctx,
        &ally_sess,
        outsider_char,
        "warStatisticMgr",
        war_id
    ));
    ally_sess.corporation_id = Some(98_999_888);
    assert!(!authorize_service_binding(
        &ctx,
        &ally_sess,
        outsider_char,
        "warStatisticMgr",
        war_id
    ));
    ally_sess.role = ROLE_GMH;
    assert!(authorize_service_binding(
        &ctx,
        &ally_sess,
        outsider_char,
        "warStatisticMgr",
        war_id
    ));
    assert!(!authorize_service_binding(
        &ctx,
        &ally_sess,
        outsider_char,
        "warStatisticMgr",
        war_id + 999
    )); // nonexistent war rejected even for GM

    // 4. tourneyMgr & keeper & agentMgr scoping
    assert!(authorize_service_binding(
        &ctx,
        &member_sess,
        member_char,
        "tourneyMgr",
        tourney_match_id
    ));
    assert!(authorize_service_binding(
        &ctx,
        &member_sess,
        member_char,
        "tourneyMgr",
        tourney_id
    ));
    assert!(authorize_service_binding(
        &ctx,
        &member_sess,
        member_char,
        "tourneyMgr",
        i64::from(other_sys_id)
    )); // via participant match in other_sys_id
    let mut unrelated_sess = SessionState {
        session_id: 603,
        user_id: Some(4),
        character_id: Some(outsider_char),
        solar_system_id: Some(sys_id),
        corporation_id: Some(98_999_888),
        ..Default::default()
    };
    assert!(!authorize_service_binding(
        &ctx,
        &unrelated_sess,
        outsider_char,
        "tourneyMgr",
        tourney_match_id
    ));
    assert!(!authorize_service_binding(
        &ctx,
        &unrelated_sess,
        outsider_char,
        "tourneyMgr",
        tourney_id
    ));
    assert!(!authorize_service_binding(
        &ctx,
        &unrelated_sess,
        outsider_char,
        "tourneyMgr",
        i64::from(other_sys_id)
    ));
    unrelated_sess.solar_system_id = Some(other_sys_id);
    // Solar-system bystanders must NOT bind tourneyMgr matches, tournaments, or arena systems.
    assert!(!authorize_service_binding(
        &ctx,
        &unrelated_sess,
        outsider_char,
        "tourneyMgr",
        tourney_match_id
    ));
    assert!(!authorize_service_binding(
        &ctx,
        &unrelated_sess,
        outsider_char,
        "tourneyMgr",
        tourney_id
    ));
    assert!(!authorize_service_binding(
        &ctx,
        &unrelated_sess,
        outsider_char,
        "tourneyMgr",
        i64::from(other_sys_id)
    ));
    unrelated_sess.role = ROLE_GMH;
    assert!(authorize_service_binding(
        &ctx,
        &unrelated_sess,
        outsider_char,
        "tourneyMgr",
        i64::from(other_sys_id)
    ));
    assert!(!authorize_service_binding(
        &ctx,
        &unrelated_sess,
        outsider_char,
        "agentMgr",
        1
    ));
    assert!(authorize_service_binding(
        &ctx,
        &unrelated_sess,
        outsider_char,
        "agentMgr",
        3_008_416
    ));
    assert!(!authorize_service_binding(
        &ctx,
        &unrelated_sess,
        outsider_char,
        "keeper",
        1
    ));
    assert_eq!(ctx.db.snapshot_tables(), before);
}

#[test]
fn binding_auth_war_stat_resolves_alliance_wars_without_session_alliance_id() {
    use eve_db::{CorporationRecord, PersistedNegKind, PersistedWarNegotiation};

    let ctx = test_ctx();
    let alliance_id = 99_777_001_i64;
    let enemy_alliance_id = 99_777_002_i64;
    let member_corp = 98_777_001_i64;
    let bystander_corp = 98_777_002_i64;
    let member_char = 90_000_002_i64;
    let alliance_war_id = 700_777_001_i64;
    let alliance_ally_war_id = 700_777_002_i64;
    let now_unix = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(member_corp, {
            let mut c = CorporationRecord::default();
            c.corporation_id = member_corp;
            c.name = "Ally Member".into();
            c.ticker = "ALM".into();
            c.ceo_id = member_char;
            c.alliance_id = Some(alliance_id);
            c
        });
        snap.corporations.insert(bystander_corp, {
            let mut c = CorporationRecord::default();
            c.corporation_id = bystander_corp;
            c.name = "No Alliance".into();
            c.ticker = "NOA".into();
            c.ceo_id = 90_999_777;
            c.alliance_id = None;
            c
        });
        snap.wars.insert(
            alliance_war_id,
            WarRecord {
                war_id: alliance_war_id,
                aggressor_corp_id: alliance_id,
                defender_corp_id: enemy_alliance_id,
                mutual: false,
                started_unix: now_unix,
                active: true,
            },
        );
        snap.wars.insert(
            alliance_ally_war_id,
            WarRecord {
                war_id: alliance_ally_war_id,
                aggressor_corp_id: enemy_alliance_id,
                defender_corp_id: bystander_corp,
                mutual: false,
                started_unix: now_unix,
                active: true,
            },
        );
        snap.war_state.negotiations.push(PersistedWarNegotiation {
            id: 90_777,
            war_id: alliance_ally_war_id,
            kind: PersistedNegKind::Ally,
            from_corp: alliance_id,
            isk: Default::default(),
            message: String::new(),
            status: "Accepted".into(),
            effective_unix: Some(now_unix),
        });
    });

    let sess = SessionState {
        session_id: 610,
        user_id: Some(2),
        character_id: Some(member_char),
        corporation_id: Some(member_corp),
        alliance_id: None,
        ..Default::default()
    };
    assert!(authorize_service_binding(
        &ctx,
        &sess,
        member_char,
        "warStatisticMgr",
        alliance_war_id
    ));
    assert!(authorize_service_binding(
        &ctx,
        &sess,
        member_char,
        "warStatisticMgr",
        alliance_ally_war_id
    ));

    let bystander_sess = SessionState {
        session_id: 611,
        user_id: Some(3),
        character_id: Some(90_999_777),
        corporation_id: Some(bystander_corp),
        alliance_id: None,
        ..Default::default()
    };
    assert!(!authorize_service_binding(
        &ctx,
        &bystander_sess,
        90_999_777,
        "warStatisticMgr",
        alliance_war_id
    ));
}

#[tokio::test]
async fn dispatch_rpc_invbroker_bound_handles_subobjects_end_to_end() {
    use eve_db::{corp_roles, CorporationRecord, CrabBeaconPhase, CrabBeaconRecord, FleetRecord};
    use eve_machonet::service_roles::{ROLE_GMH, ROLE_PLAYER_DEFAULT};
    use eve_proto::PyRep;
    use std::collections::HashMap;

    let ctx = test_ctx();
    let kw = HashMap::new();
    let sys_id = 30_000_142_u32;
    let st_id = 60_003_760_i64;
    let now_u = ctx.now_unix();

    let owner_char = 90_000_001_i64;
    let guest_char = 90_000_002_i64;
    let corp_id = 98_000_001_i64;

    let fh_ship_type = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 6 && t.default_attributes.get(&912).copied().unwrap_or(0.0) > 0.0)
        .map(|t| t.type_id)
        .unwrap_or(11_567);
    let container_type = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 12 && t.capacity > 0.0)
        .map(|t| t.type_id)
        .unwrap_or(3_293);

    let fleet_ship_id = 881_100_001_i64;
    let nested_fh_can_id = 881_100_002_i64;
    let jetcan_id = 881_100_003_i64;
    let corp_office_id = 881_100_004_i64;
    let nested_office_can_id = 881_100_005_i64;
    let crab_beacon_id = 881_100_006_i64;
    let storage_unit_id = 881_100_007_i64;
    let guest_ship_id = 881_100_008_i64;
    let outsider_item_id = 881_100_009_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.members.insert(guest_char);
            corp.member_roles
                .insert(guest_char, corp_roles::ROLE_HANGAR_CAN_TAKE[0]);
        } else {
            let mut c = CorporationRecord::default();
            c.corporation_id = corp_id;
            c.name = "Corp".into();
            c.ticker = "CRP".into();
            c.ceo_id = owner_char;
            c.members.insert(owner_char);
            c.members.insert(guest_char);
            c.member_roles
                .insert(guest_char, corp_roles::ROLE_HANGAR_CAN_TAKE[0]);
            snap.corporations.insert(corp_id, c);
        }
        let mut guest_ch = snap.characters[&owner_char].clone();
        guest_ch.character_id = guest_char;
        guest_ch.user_id = 2;
        guest_ch.corporation_id = corp_id;
        guest_ch.station_id = Some(st_id);
        guest_ch.structure_id = None;
        guest_ch.solar_system_id = sys_id;
        snap.characters.insert(guest_char, guest_ch);

        snap.fleets.insert(
            777_135,
            FleetRecord {
                fleet_id: 777_135,
                boss_id: owner_char,
                members: vec![owner_char, guest_char],
                ..Default::default()
            },
        );
        snap.items.insert(
            fleet_ship_id,
            InventoryItemRecord {
                item_id: fleet_ship_id,
                type_id: fh_ship_type,
                owner_id: owner_char,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            nested_fh_can_id,
            InventoryItemRecord {
                item_id: nested_fh_can_id,
                type_id: container_type,
                owner_id: owner_char,
                location_id: fleet_ship_id,
                flag: InventoryFlag::FleetHangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            guest_ship_id,
            InventoryItemRecord {
                item_id: guest_ship_id,
                type_id: 587,
                owner_id: guest_char,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            jetcan_id,
            InventoryItemRecord {
                item_id: jetcan_id,
                type_id: 23,
                owner_id: 0,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            corp_office_id,
            InventoryItemRecord {
                item_id: corp_office_id,
                type_id: 27,
                owner_id: corp_id,
                location_id: st_id,
                flag: InventoryFlag::CorpSAG1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            nested_office_can_id,
            InventoryItemRecord {
                item_id: nested_office_can_id,
                type_id: container_type,
                owner_id: corp_id,
                location_id: corp_office_id,
                flag: InventoryFlag::CorpSAG1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            crab_beacon_id,
            InventoryItemRecord {
                item_id: crab_beacon_id,
                type_id: crate::services::pve_crab_beacons::TYPE_CRAB_BEACON,
                owner_id: guest_char,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.crab_beacons.beacons.insert(
            crab_beacon_id,
            CrabBeaconRecord {
                beacon_id: crab_beacon_id,
                type_id: crate::services::pve_crab_beacons::TYPE_CRAB_BEACON,
                deployer_char_id: guest_char,
                solar_system_id: sys_id,
                position: [0.0, 0.0, 0.0],
                deployed_at_sec: now_u.saturating_sub(1_000),
                phase: CrabBeaconPhase::LootRestricted,
                linked_char_id: Some(guest_char),
                linked_ship_id: Some(guest_ship_id),
                link_started_at_sec: 0,
                link_completes_at_sec: 0,
                scan_completes_at_sec: now_u.saturating_sub(10),
                self_destruct_at_sec: now_u + 120,
                waves_spawned: 3,
                wave_npc_ball_ids: Vec::new(),
                open_container_id: None,
            },
        );
        snap.items.insert(
            storage_unit_id,
            InventoryItemRecord {
                item_id: storage_unit_id,
                type_id: container_type,
                owner_id: owner_char,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            outsider_item_id,
            InventoryItemRecord {
                item_id: outsider_item_id,
                type_id: container_type,
                owner_id: 90_999_999,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    ctx.db.with_mut_snapshot(|snap| {
        crate::services::fitting_containers_ships::set_ship_access_config_snap(
            snap,
            fleet_ship_id,
            &crate::services::fitting_containers_ships::ShipAccessConfig {
                allow_fleet_hangar: true,
                password: None,
                ..Default::default()
            },
            0,
        )
        .unwrap();
    });
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.authorized_shipping_storage
            .insert((storage_unit_id, guest_char));
    });
    {
        use eve_destiny::{Ball, DVec3, MovementMode};
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_handle
            .spawn_ball(Ball {
                id: guest_ship_id,
                type_id: 587,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
        let _ = sys_handle
            .spawn_ball(Ball {
                id: storage_unit_id,
                type_id: container_type,
                position: DVec3::new(2_500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 100_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }

    let mut guest_sess = SessionState {
        session_id: 620,
        user_id: Some(2),
        character_id: Some(guest_char),
        corporation_id: Some(corp_id),
        solar_system_id: Some(sys_id),
        station_id: Some(st_id),
        fleet_id: Some(777_135),
        role: ROLE_PLAYER_DEFAULT,
        ..Default::default()
    };

    // Docked: GetInventoryFromId -> GetSelfInvItem / List / ListByFlags for FleetHangar + nested corp-office container
    let PyRep::Tuple(st_bind) = ctx
        .dispatch_rpc(
            &mut guest_sess,
            "invbroker",
            None,
            "MachoBindObject",
            &[PyRep::Integer(st_id), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("bind docked invbroker")
    else {
        panic!("expected tuple from MachoBindObject");
    };
    let st_mgr_bid = st_bind[0].as_i64().unwrap() as u64;

    for target_id in [fleet_ship_id, nested_fh_can_id, nested_office_can_id] {
        let sub_bid = ctx
            .dispatch_rpc(
                &mut guest_sess,
                "invbroker",
                Some(st_mgr_bid),
                "GetInventoryFromId",
                &[PyRep::Integer(target_id), PyRep::Integer(0)],
                &kw,
                None,
            )
            .await
            .unwrap_or_else(|e| panic!("GetInventoryFromId({target_id}) failed: {e:?}"))
            .as_i64()
            .unwrap() as u64;
        ctx.dispatch_rpc(
            &mut guest_sess,
            "invbroker",
            Some(sub_bid),
            "GetSelfInvItem",
            &[],
            &kw,
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("GetSelfInvItem({target_id}) failed: {e:?}"));
        let list_method = if target_id == fleet_ship_id {
            "ListByFlags"
        } else {
            "List"
        };
        let list_args = if target_id == fleet_ship_id {
            vec![PyRep::List(vec![PyRep::Integer(155)])]
        } else {
            vec![]
        };
        ctx.dispatch_rpc(
            &mut guest_sess,
            "invbroker",
            Some(sub_bid),
            list_method,
            &list_args,
            &kw,
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("{list_method}({target_id}) failed: {e:?}"));
    }

    // Outsider docked item cannot be bound or opened via GetInventoryFromId
    assert_eq!(
        ctx.dispatch_rpc(
            &mut guest_sess,
            "invbroker",
            None,
            "MachoBindObject",
            &[PyRep::Integer(outsider_item_id), PyRep::None],
            &kw,
            None,
        )
        .await
        .unwrap_err()
        .code,
        "BindAuthorizationDenied"
    );

    // Undocked space checks: open jetcan, read-pure CRAB loot, authorized shipping storage (<=5000m)
    guest_sess.station_id = None;
    guest_sess.ship_id = Some(guest_ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&guest_char).unwrap();
        ch.station_id = None;
        ch.active_ship_id = guest_ship_id;
    });
    let PyRep::Tuple(sys_bind) = ctx
        .dispatch_rpc(
            &mut guest_sess,
            "invbroker",
            None,
            "MachoBindObject",
            &[PyRep::Integer(i64::from(sys_id)), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("bind system invbroker")
    else {
        panic!("expected tuple from MachoBindObject");
    };
    let sys_mgr_bid = sys_bind[0].as_i64().unwrap() as u64;

    for space_target in [jetcan_id, crab_beacon_id, storage_unit_id] {
        let before_space = (space_target == crab_beacon_id).then(|| ctx.db.snapshot_tables());
        let ok = authorize_service_binding(&ctx, &guest_sess, guest_char, "invbroker", space_target);
        assert!(ok, "authorize_service_binding failed for {space_target}");
        if let Some(before) = before_space {
            assert_eq!(ctx.db.snapshot_tables(), before, "CRAB auth check must stay read-pure");
        }
        let sub_bid = ctx
            .dispatch_rpc(
                &mut guest_sess,
                "invbroker",
                Some(sys_mgr_bid),
                "GetInventoryFromId",
                &[PyRep::Integer(space_target), PyRep::Integer(0)],
                &kw,
                None,
            )
            .await
            .unwrap_or_else(|e| panic!("GetInventoryFromId({space_target}) failed: {e:?}"))
            .as_i64()
            .unwrap() as u64;
        ctx.dispatch_rpc(
            &mut guest_sess,
            "invbroker",
            Some(sub_bid),
            "List",
            &[],
            &kw,
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("List({space_target}) failed: {e:?}"));
    }

    // Shipping storage > 5_000m is denied
    {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_handle
            .set_ball_position(storage_unit_id, eve_destiny::DVec3::new(10_000.0, 0.0, 0.0))
            .await;
    }
    assert!(!authorize_service_binding(
        &ctx,
        &guest_sess,
        guest_char,
        "invbroker",
        storage_unit_id
    ));

    // GM role authorizes cross-owner invbroker target
    guest_sess.role = ROLE_GMH;
    guest_sess.station_id = Some(st_id);
    assert!(authorize_service_binding(
        &ctx,
        &guest_sess,
        guest_char,
        "invbroker",
        outsider_item_id
    ));
}

#[test]
fn binding_auth_war_stat_ally_expiry_and_corp_vs_alliance_boundaries() {
    use eve_db::{
        clock::VirtualServerClock, AllianceRecord, CorporationRecord, PersistedNegKind,
        PersistedWarNegotiation,
    };
    use std::sync::Arc;

    let ctx = test_ctx();
    let t0 = 1_700_000_000_u64;
    let clock = Arc::new(VirtualServerClock::with_fixed_base(t0));
    ctx.set_clock(clock.clone());

    let agg_corp = 98_888_001_i64;
    let def_corp = 98_888_002_i64;
    let corp_ally = 98_888_010_i64;
    let corp_ally_sibling = 98_888_011_i64;
    let corp_ally_shared_alliance = 99_888_050_i64;
    let ally_alliance = 99_888_010_i64;
    let alliance_member_corp = 98_888_020_i64;
    let agg_alliance = 99_888_001_i64;
    let agg_alliance_member = 98_888_030_i64;

    let war_corp_ally = 700_888_001_i64;
    let war_alliance_ally = 700_888_002_i64;
    let war_alliance_party = 700_888_003_i64;

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, all) in [
            (agg_corp, Some(agg_alliance)),
            (agg_alliance_member, Some(agg_alliance)),
            (def_corp, None),
            (corp_ally, Some(corp_ally_shared_alliance)),
            (corp_ally_sibling, Some(corp_ally_shared_alliance)),
            (alliance_member_corp, Some(ally_alliance)),
        ] {
            let mut c = CorporationRecord::default();
            c.corporation_id = cid;
            c.name = format!("Corp {cid}");
            c.ticker = format!("C{}", cid % 100);
            c.ceo_id = 90_000_002;
            c.alliance_id = all;
            snap.corporations.insert(cid, c);
        }
        snap.alliances.insert(
            ally_alliance,
            AllianceRecord {
                alliance_id: ally_alliance,
                name: "Ally Alliance".into(),
                short_name: "ALYA".into(),
                executor_corp_id: alliance_member_corp,
                member_corp_ids: [alliance_member_corp].into_iter().collect(),
            },
        );
        for (wid, agg, def) in [
            (war_corp_ally, agg_corp, def_corp),
            (war_alliance_ally, agg_corp, def_corp),
            (war_alliance_party, agg_alliance, def_corp),
        ] {
            snap.wars.insert(
                wid,
                WarRecord {
                    war_id: wid,
                    aggressor_corp_id: agg,
                    defender_corp_id: def,
                    mutual: false,
                    started_unix: t0,
                    active: true,
                },
            );
        }
        snap.war_state.negotiations.push(PersistedWarNegotiation {
            id: 91_001,
            war_id: war_corp_ally,
            kind: PersistedNegKind::Ally,
            from_corp: corp_ally,
            isk: Default::default(),
            message: String::new(),
            status: "Accepted".into(),
            effective_unix: Some(t0),
        });
        snap.war_state
            .ally_expires_unix
            .insert((war_corp_ally, corp_ally), t0 + 3_600);

        snap.war_state.negotiations.push(PersistedWarNegotiation {
            id: 91_002,
            war_id: war_alliance_ally,
            kind: PersistedNegKind::Ally,
            from_corp: ally_alliance,
            isk: Default::default(),
            message: String::new(),
            status: "Accepted".into(),
            effective_unix: Some(t0),
        });
        snap.war_state
            .ally_expires_unix
            .insert((war_alliance_ally, ally_alliance), t0 + 7_200);
    });

    let char_id = 90_000_002_i64;
    let mut sess = SessionState {
        session_id: 612,
        user_id: Some(4),
        character_id: Some(char_id),
        corporation_id: Some(corp_ally),
        alliance_id: None,
        ..Default::default()
    };
    let entry_corp_ally = make_entry("warStatisticMgr", war_corp_ally, char_id);
    assert!(authorize_service_binding(&ctx, &sess, char_id, "warStatisticMgr", war_corp_ally));
    assert!(verify_service_binding_context(&ctx, &sess, &entry_corp_ally));

    // Sibling corp in the same alliance as a corp-level ally must NOT bind.
    sess.corporation_id = Some(corp_ally_sibling);
    assert!(!authorize_service_binding(&ctx, &sess, char_id, "warStatisticMgr", war_corp_ally));
    assert!(!verify_service_binding_context(&ctx, &sess, &entry_corp_ally));

    // Alliance-war aggressor member corp binds without session.alliance_id.
    sess.corporation_id = Some(agg_alliance_member);
    assert!(authorize_service_binding(&ctx, &sess, char_id, "warStatisticMgr", war_alliance_party));

    // Alliance-ID ally member corp binds without session.alliance_id, and explicit session.alliance_id without corp also binds.
    let entry_alliance_ally = make_entry("warStatisticMgr", war_alliance_ally, char_id);
    sess.corporation_id = Some(alliance_member_corp);
    assert!(authorize_service_binding(&ctx, &sess, char_id, "warStatisticMgr", war_alliance_ally));
    assert!(verify_service_binding_context(&ctx, &sess, &entry_alliance_ally));

    let alliance_only_sess = SessionState {
        session_id: 613,
        user_id: Some(5),
        character_id: Some(90_888_777),
        corporation_id: None,
        alliance_id: Some(ally_alliance),
        ..Default::default()
    };
    assert!(authorize_service_binding(
        &ctx,
        &alliance_only_sess,
        90_888_777,
        "warStatisticMgr",
        war_alliance_ally
    ));

    // Boundary: t0 + 3599 (exp - 1) keeps corp_ally active; t0 + 3600 (== exp) deactivates it.
    clock.advance_secs_and_read_offset(3_599);
    sess.corporation_id = Some(corp_ally);
    assert!(authorize_service_binding(&ctx, &sess, char_id, "warStatisticMgr", war_corp_ally));
    assert!(verify_service_binding_context(&ctx, &sess, &entry_corp_ally));

    clock.advance_secs_and_read_offset(1);
    assert!(!authorize_service_binding(&ctx, &sess, char_id, "warStatisticMgr", war_corp_ally));
    assert!(!verify_service_binding_context(&ctx, &sess, &entry_corp_ally));

    // At t0 + 3600, alliance-ID ally (exp = t0 + 7200) is still active; at t0 + 7200 it expires too.
    sess.corporation_id = Some(alliance_member_corp);
    assert!(authorize_service_binding(&ctx, &sess, char_id, "warStatisticMgr", war_alliance_ally));
    clock.advance_secs_and_read_offset(3_600);
    assert!(!authorize_service_binding(&ctx, &sess, char_id, "warStatisticMgr", war_alliance_ally));
    assert!(!verify_service_binding_context(&ctx, &sess, &entry_alliance_ally));
    assert!(!authorize_service_binding(
        &ctx,
        &alliance_only_sess,
        90_888_777,
        "warStatisticMgr",
        war_alliance_ally
    ));
}

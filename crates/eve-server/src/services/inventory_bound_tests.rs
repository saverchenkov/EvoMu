#![forbid(unsafe_code)]

use super::*;
use eve_db::{
    CorpMemberRoleAssignment, CorporationRecord, InventoryFlag, InventoryItemRecord,
    UpwellStructureRecord, VirtualServerClock, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn test_ctx() -> ServerContext {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

fn bound_invbroker(char_id: i64, object_id: i64) -> BoundObjectEntry {
    BoundObjectEntry {
        bound_id: 1,
        session_id: 1,
        user_id: char_id as u64,
        character_id: char_id,
        service: "invbroker".into(),
        object_id,
        oid_str: format!("N=1:{object_id}"),
        ref_id: 1,
    }
}

#[test]
fn i2_is_primed_and_get_item_enforce_gm_and_cover_item_character_structure_station() {
    let ctx = test_ctx();
    let char_id = 95_300_001;
    let corp_id = 98_300_001;
    let station_id = 60_003_760;
    let struct_id = 1_030_000_000_101;
    let bpc_id = 1_000_953_001;
    let ship_id = 1_000_953_002;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = "Inspector Pilot".into();
        ch.corporation_id = corp_id;
        ch.station_id = Some(station_id);
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            bpc_id,
            InventoryItemRecord {
                item_id: bpc_id,
                type_id: 684,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                is_blueprint_copy: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Jita Astrahus".into(),
                state: "ShieldVulnerable".into(),
                position: [10_000.0, 0.0, 0.0],
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

    let non_gm = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        ..Default::default()
    };
    assert_eq!(
        handle_i2_is_primed(&ctx, &non_gm, Some(bpc_id))
            .unwrap_err()
            .code,
        "GMRoleRequired"
    );

    let gm = SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        role: 0x7FFF_FFFF_FFFF_FFFF,
        ..Default::default()
    };
    assert_eq!(handle_i2_is_primed(&ctx, &gm, Some(0)).unwrap_err().code, "InvalidArgument");
    assert_eq!(handle_i2_is_primed(&ctx, &gm, Some(bpc_id)).unwrap(), PyRep::Bool(true));
    assert_eq!(handle_i2_is_primed(&ctx, &gm, Some(char_id)).unwrap(), PyRep::Bool(true));
    assert_eq!(handle_i2_is_primed(&ctx, &gm, Some(struct_id)).unwrap(), PyRep::Bool(true));
    assert_eq!(handle_i2_is_primed(&ctx, &gm, Some(30_000_142)).unwrap(), PyRep::Bool(true));
    assert_eq!(handle_i2_is_primed(&ctx, &gm, Some(999_999_999_999)).unwrap(), PyRep::Bool(false));

    // GetItem for BPC (singleton=2, quantity=-2), singleton ship (singleton=1, quantity=-1), character, structure, station, and missing
    let bpc_rep = handle_i2_get_item(&ctx, &gm, Some(bpc_id)).unwrap();
    assert_eq!(bpc_rep.dict_get("singleton").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(bpc_rep.dict_get("quantity").and_then(|v| v.as_i64()), Some(-2));

    let ship_rep = handle_i2_get_item(&ctx, &gm, Some(ship_id)).unwrap();
    assert_eq!(ship_rep.dict_get("singleton").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(ship_rep.dict_get("quantity").and_then(|v| v.as_i64()), Some(-1));

    let char_rep = handle_i2_get_item(&ctx, &gm, Some(char_id)).unwrap();
    assert_eq!(char_rep.dict_get("typeID").and_then(|v| v.as_i64()), Some(1373));

    let st_rep = handle_i2_get_item(&ctx, &gm, Some(struct_id)).unwrap();
    assert_eq!(st_rep.dict_get("itemID").and_then(|v| v.as_i64()), Some(struct_id));

    if ctx.sde.stations.contains_key(&station_id) {
        let npc_st_rep = handle_i2_get_item(&ctx, &gm, Some(station_id)).unwrap();
        assert_eq!(npc_st_rep.dict_get("itemID").and_then(|v| v.as_i64()), Some(station_id));
    }
    assert_eq!(handle_i2_get_item(&ctx, &gm, Some(999_999_999_999)).unwrap(), PyRep::None);
}

#[test]
fn invbroker_bound_methods_turret_slots_and_corp_roles_at_location() {
    let ctx = test_ctx();
    let char_id = 95_300_010;
    let corp_id = 98_300_010;
    let hq_station = 60_003_760;
    let base_station = 60_008_494;
    let other_station = 60_004_588;
    let ship_id = 1_000_953_010;
    let foreign_ship_id = 1_000_953_011;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.corporation_id = corp_id;
        ch.station_id = Some(hq_station);
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Test Bound Corp".into();
                __corp.ticker = "TBC".into();
                __corp.ceo_id = char_id;
                __corp.hq_station_id = Some(hq_station);
                __corp },
        );
        snap.corp_governance
            .entry(corp_id)
            .or_default()
            .member_bases
            .insert(char_id, base_station);
        snap.corp_role_assignments.insert(
            (corp_id, char_id),
            CorpMemberRoleAssignment {
                character_id: char_id,
                roles: 1,
                roles_at_hq: 2,
                roles_at_base: 4,
                roles_at_other: 8,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: hq_station,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            foreign_ship_id,
            InventoryItemRecord {
                item_id: foreign_ship_id,
                type_id: 587,
                owner_id: 99_999_999,
                location_id: hq_station,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let sess = SessionState {
        session_id: 1,
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        station_id: Some(hq_station),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    // Unbound call fails with InvalidBoundObject
    assert_eq!(
        handle(&ctx, &sess, None, "invbroker", "GetSelfInvItem", &[], None)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidBoundObject"
    );

    // GetSelfInvItem on ship and on station
    let b_ship = bound_invbroker(char_id, ship_id);
    assert!(handle(&ctx, &sess, Some(&b_ship), "invbroker", "GetSelfInvItem", &[], None)
        .unwrap()
        .is_ok());

    let b_station = bound_invbroker(char_id, hq_station);
    if ctx.sde.stations.contains_key(&hq_station) {
        assert!(handle(&ctx, &sess, Some(&b_station), "invbroker", "GetSelfInvItem", &[], None)
            .unwrap()
            .is_ok());
    }

    // GetAvailableTurretSlots on Rifter (587 has 3 turret slots, 0 fitted -> 3 available)
    let free_turrets = handle(&ctx, &sess, Some(&b_ship), "invbroker", "GetAvailableTurretSlots", &[], None)
        .unwrap()
        .expect("GetAvailableTurretSlots");
    assert_eq!(free_turrets.as_i64(), Some(3));

    // GetInventoryFromId for accessible ship vs inaccessible foreign ship
    assert!(handle(
        &ctx,
        &sess,
        Some(&b_station),
        "invbroker",
        "GetInventoryFromId",
        &[PyRep::Integer(ship_id), PyRep::Integer(0)],
        None,
    )
    .unwrap()
    .is_ok());
    assert_eq!(
        handle(
            &ctx,
            &sess,
            Some(&b_station),
            "invbroker",
            "GetInventoryFromId",
            &[PyRep::Integer(foreign_ship_id), PyRep::Integer(0)],
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InventoryNotAccessible"
    );

    // GetInventory(10004) at docked station vs wrong station
    assert!(handle(
        &ctx,
        &sess,
        Some(&b_station),
        "invbroker",
        "GetInventory",
        &[PyRep::Integer(10_004)],
        None,
    )
    .unwrap()
    .is_ok());
    let b_wrong = bound_invbroker(char_id, other_station);
    assert_eq!(
        handle(
            &ctx,
            &sess,
            Some(&b_wrong),
            "invbroker",
            "GetInventory",
            &[PyRep::Integer(10_004)],
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CharacterNotAtStation"
    );

    // Corp roles at HQ (1|2=3), base (1|4=5), and other (1|8=9)
    let (is_ceo, hq_roles) = get_effective_corp_roles_at_location(&ctx, corp_id, char_id, Some(hq_station));
    assert!(is_ceo);
    assert_eq!(hq_roles, 3);
    assert_eq!(get_effective_corp_roles_at_location(&ctx, corp_id, char_id, Some(base_station)).1, 5);
    assert_eq!(get_effective_corp_roles_at_location(&ctx, corp_id, char_id, Some(other_station)).1, 9);

    // Colocation & destination checks in space
    let mut space_sess = sess.clone();
    space_sess.station_id = None;
    space_sess.solar_system_id = Some(30_000_142);
    let mut pre_balls = HashMap::new();
    pre_balls.insert(ship_id, (DVec3::ZERO, 50.0));
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).unwrap();
        assert!(is_inventory_location_colocated(snap, &space_sess, ch, &pre_balls, 30_000_142));
    });
}

#[test]
fn issue_552_scoped_stasis_and_corp_office_container_take_rbacs() {
    let ctx = test_ctx();
    let (corp_id, ceo_id, mem_id) = (98_000_020_i64, 90_000_001_i64, 90_000_002_i64);
    let hq = 60_003_760_i64;
    let other_st = 60_003_761_i64;
    let (office_id, cont_id, item_in_cont, item_in_office) = (81_100_001_i64, 81_100_002_i64, 81_100_003_i64, 81_100_004_i64);
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.ceo_id = ceo_id;
        corp.hq_station_id = Some(hq);
        corp.name = "ScopedCorp".into();
        corp.ticker = "SCP".into();
        corp.members.insert(ceo_id);
        corp.members.insert(mem_id);
        corp.member_roles.insert(mem_id, 0);
        snap.corporations.insert(corp_id, corp);
        if let Some(ch) = snap.characters.get_mut(&mem_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(hq);
        }
        snap.corp_role_assignments.insert(
            (corp_id, mem_id),
            CorpMemberRoleAssignment {
                character_id: mem_id,
                roles_at_hq: eve_db::corp_roles::ROLE_HANGAR_CAN_TAKE[0],
                roles_at_other: eve_db::corp_roles::ROLE_HANGAR_CAN_TAKE[1],
                ..Default::default()
            },
        );
        // Unmatured scoped grant for Container Take 1 at HQ
        snap.corp_governance.entry(corp_id).or_default().pending_scoped_role_grants.insert(
            mem_id,
            (eve_db::corp_roles::ROLE_CONTAINER_CAN_TAKE[0], 0, 0, now + 3_600),
        );
        // Corp office at HQ containing a loose item in CorpSAG1 and a corp container in CorpSAG1
        snap.items.insert(office_id, InventoryItemRecord {
            item_id: office_id, type_id: 27, owner_id: corp_id, location_id: hq,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: true, ..Default::default()
        });
        snap.items.insert(cont_id, InventoryItemRecord {
            item_id: cont_id, type_id: 17366, owner_id: corp_id, location_id: office_id,
            flag: InventoryFlag::CorpSAG1, quantity: 1, singleton: true, ..Default::default()
        });
        snap.items.insert(item_in_cont, InventoryItemRecord {
            item_id: item_in_cont, type_id: 34, owner_id: corp_id, location_id: cont_id,
            flag: InventoryFlag::Unlocked, quantity: 100, ..Default::default()
        });
        snap.items.insert(item_in_office, InventoryItemRecord {
            item_id: item_in_office, type_id: 34, owner_id: corp_id, location_id: office_id,
            flag: InventoryFlag::CorpSAG1, quantity: 100, ..Default::default()
        });
    });

    // Scoped location check: at HQ, has Hangar Take 1 (not Hangar Take 2); Container Take 1 is still in stasis
    let (_, r_hq) = get_effective_corp_roles_at_location(&ctx, corp_id, mem_id, Some(hq));
    let (_, r_other) = get_effective_corp_roles_at_location(&ctx, corp_id, mem_id, Some(other_st));
    assert_ne!(r_hq & eve_db::corp_roles::ROLE_HANGAR_CAN_TAKE[0], 0);
    assert_eq!(r_hq & eve_db::corp_roles::ROLE_HANGAR_CAN_TAKE[1], 0);
    assert_eq!(r_hq & eve_db::corp_roles::ROLE_CONTAINER_CAN_TAKE[0], 0, "unmatured scoped grant must be masked");
    assert_ne!(r_other & eve_db::corp_roles::ROLE_HANGAR_CAN_TAKE[1], 0);

    ctx.db.with_snapshot(|snap| {
        // Direct child of corp office in CorpSAG1 checks Hangar Take 1 -> ALLOWED
        let it_off = &snap.items[&item_in_office];
        assert!(validate_corp_source_item_take_access_in_snap(&ctx, snap, mem_id, corp_id, it_off).is_ok());
        // Child of corp container in CorpSAG1 requires Container Take 1 (still in stasis) -> DENIED
        let it_cont = &snap.items[&item_in_cont];
        let err = validate_corp_source_item_take_access_in_snap(&ctx, snap, mem_id, corp_id, it_cont).unwrap_err();
        assert_eq!(err.code, "CorpRoleDenied");
    });

    // Advance past stasis -> Container Take 1 matures at HQ and container item take succeeds
    ctx.advance_virtual_clock(3_601);
    ctx.db.with_snapshot(|snap| {
        let it_cont = &snap.items[&item_in_cont];
        assert!(validate_corp_source_item_take_access_in_snap(&ctx, snap, mem_id, corp_id, it_cont).is_ok());
    });
}

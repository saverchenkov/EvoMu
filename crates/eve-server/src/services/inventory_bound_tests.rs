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
    let member_id = 95_300_012;
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
            .insert(member_id, base_station);
        snap.corp_role_assignments.insert(
            (corp_id, member_id),
            CorpMemberRoleAssignment {
                character_id: member_id,
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

    // GetInventory(10004, 10012, 10002) at docked station vs wrong station
    for container_id in [10_004i64, 10_012, 10_002] {
        assert!(handle(
            &ctx,
            &sess,
            Some(&b_station),
            "invbroker",
            "GetInventory",
            &[PyRep::Integer(container_id)],
            None,
        )
        .unwrap()
        .is_ok());
    }
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

    // CEO receives full role bitmask via #589 immunity; non-CEO member gets HQ (1|2=3), base (1|4=5), other (1|8=9)
    assert_eq!(
        get_effective_corp_roles_at_location(&ctx, corp_id, char_id, Some(hq_station)),
        (true, u64::MAX)
    );
    let (is_ceo_mem, hq_roles) =
        get_effective_corp_roles_at_location(&ctx, corp_id, member_id, Some(hq_station));
    assert!(!is_ceo_mem);
    assert_eq!(hq_roles, 3);
    assert_eq!(
        get_effective_corp_roles_at_location(&ctx, corp_id, member_id, Some(base_station)).1,
        5
    );
    assert_eq!(
        get_effective_corp_roles_at_location(&ctx, corp_id, member_id, Some(other_station)).1,
        9
    );

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

#[cfg(test)]
#[path = "inventory_bound_extra_unit_tests.rs"]
mod inventory_bound_extra_unit_tests;

#[test]
fn issue_803_get_inventory_from_id_and_self_inv_item_on_destroyed_dynamic_ship() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let destroyed_ship_id = 200_125_728_i64;
    let capsule_id = 200_126_481_i64;
    let sys_id = 30_000_142_u32;

    ctx.db.with_mut_snapshot(|snap| {
        snap.killmails.insert(
            1,
            eve_db::KillmailRecord {
                kill_id: 1,
                solar_system_id: sys_id,
                killed_unix: 1_700_000_100,
                victim_character_id: char_id,
                victim_corp_id: 1_000_009,
                victim_alliance_id: None,
                ship_type_id: 587,
                ..Default::default()
            },
        );
    });

    let sess = SessionState {
        session_id: 1,
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(sys_id),
        ship_id: Some(capsule_id),
        ..Default::default()
    };

    let b_sys = bound_invbroker(char_id, i64::from(sys_id));
    let get_inv = handle(
        &ctx,
        &sess,
        Some(&b_sys),
        "invbroker",
        "GetInventoryFromId",
        &[PyRep::Integer(destroyed_ship_id), PyRep::Integer(0)],
        None,
    )
    .unwrap();
    assert!(
        get_inv.is_ok(),
        "GetInventoryFromId on a just-destroyed dynamic ship must return a stub inventory instead of FakeItemNotFound"
    );

    let b_destroyed = bound_invbroker(char_id, destroyed_ship_id);
    let self_row = handle(
        &ctx,
        &sess,
        Some(&b_destroyed),
        "invbroker",
        "GetSelfInvItem",
        &[],
        None,
    )
    .unwrap()
    .expect("GetSelfInvItem on destroyed dynamic ship");
    let PyRep::DBRow { columns, .. } = self_row else {
        panic!("expected DBRow from GetSelfInvItem, got {self_row:?}");
    };
    let col = |name: &str| columns.iter().find(|(k, _)| k == name).map(|(_, v)| v);
    assert_eq!(col("itemID"), Some(&PyRep::Integer(destroyed_ship_id)));
    assert_eq!(col("typeID"), Some(&PyRep::Integer(587)));
    assert_eq!(col("ownerID"), Some(&PyRep::Integer(char_id)));
    assert_eq!(col("locationID"), Some(&PyRep::Integer(i64::from(sys_id))));
    assert_eq!(col("flagID"), Some(&PyRep::Integer(0)));
    assert_eq!(col("quantity"), Some(&PyRep::Integer(-1)));
    assert_eq!(col("groupID"), Some(&PyRep::Integer(25)));
    assert_eq!(col("categoryID"), Some(&PyRep::Integer(6)));
    assert_eq!(col("customInfo"), Some(&PyRep::String("Rifter".into())));

    // Verify locationID fallback chain (station_id -> structure_id -> solar_system_id -> 30_000_142)
    // and unknown stub_type_id fallback (groupID=29, categoryID=6, customInfo="")
    for (st_id, str_id, sol_id, exp_loc) in [
        (Some(60_003_760_i64), Some(1_000_000_000_777_i64), Some(sys_id), 60_003_760_i64),
        (None, Some(1_000_000_000_777_i64), Some(sys_id), 1_000_000_000_777_i64),
        (None, None, None, 30_000_142_i64),
    ] {
        let mut alt_sess = sess.clone();
        alt_sess.station_id = st_id;
        alt_sess.structure_id = str_id;
        alt_sess.solar_system_id = sol_id;
        let alt_row = handle(
            &ctx,
            &alt_sess,
            Some(&b_destroyed),
            "invbroker",
            "GetSelfInvItem",
            &[],
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(alt_row.dbrow_get("locationID"), Some(&PyRep::Integer(exp_loc)));
    }

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(km) = snap.killmails.get_mut(&1) {
            km.ship_type_id = 999_888_777;
        }
    });
    let unk_row = handle(
        &ctx,
        &sess,
        Some(&b_destroyed),
        "invbroker",
        "GetSelfInvItem",
        &[],
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(unk_row.dbrow_get("typeID"), Some(&PyRep::Integer(999_888_777)));
    assert_eq!(unk_row.dbrow_get("groupID"), Some(&PyRep::Integer(29)));
    assert_eq!(unk_row.dbrow_get("categoryID"), Some(&PyRep::Integer(6)));
    assert_eq!(unk_row.dbrow_get("customInfo"), Some(&PyRep::String(String::new())));

    let free_turrets = handle(
        &ctx,
        &sess,
        Some(&b_destroyed),
        "invbroker",
        "GetAvailableTurretSlots",
        &[],
        None,
    )
    .unwrap()
    .expect("GetAvailableTurretSlots on destroyed dynamic ship");
    assert_eq!(free_turrets, PyRep::Integer(0));
}

fn extract_macho_bound_id(rep: &PyRep) -> u64 {
    let sub_rep = match rep {
        PyRep::Tuple(items) => items.first().unwrap_or(rep),
        other => other,
    };
    let PyRep::SubStruct(sub) = sub_rep else {
        panic!("expected SubStruct bound object ref, got {rep:?}");
    };
    let PyRep::SubStream(inner) = sub.as_ref() else {
        panic!("expected SubStream bound object ref, got {sub:?}");
    };
    let PyRep::Tuple(items) = inner.as_ref() else {
        panic!("expected Tuple bound object ref, got {inner:?}");
    };
    let oid_str = items
        .first()
        .and_then(|v| v.as_str())
        .expect("bound oid string");
    oid_str
        .split(':')
        .nth(1)
        .and_then(|n| n.parse::<u64>().ok())
        .expect("numeric bound id")
}

#[tokio::test]
async fn issue_803_macho_dispatch_verify_bound_object_for_destroyed_ship_stub() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_char = 95_803_002_i64;
    let sys_id = 30_000_142_u32;
    let station_id = 60_003_760_i64;
    let other_station_id = 60_008_494_i64;
    let destroyed_ship_id = 200_246_015_i64;
    let capsule_id = 200_294_811_i64;
    let foreign_live_ship_id = 200_888_777_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = capsule_id;
        }
        let mut ch2 = snap.characters[&char_id].clone();
        ch2.character_id = other_char;
        ch2.user_id = other_char as u64;
        ch2.name = "Other Pilot".into();
        ch2.station_id = Some(other_station_id);
        ch2.active_ship_id = foreign_live_ship_id;
        snap.characters.insert(other_char, ch2);

        snap.items.insert(
            capsule_id,
            InventoryItemRecord {
                item_id: capsule_id,
                type_id: 670,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            foreign_live_ship_id,
            InventoryItemRecord {
                item_id: foreign_live_ship_id,
                type_id: 587,
                owner_id: other_char,
                location_id: other_station_id,
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
                killed_unix: 1_800_000_100,
                victim_character_id: char_id,
                victim_corp_id: 1_000_009,
                ship_type_id: 587,
                ..Default::default()
            },
        );
    });

    let mut sess = SessionState {
        session_id: 8031,
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(sys_id),
        ship_id: Some(capsule_id),
        ..Default::default()
    };
    let mut other_sess = SessionState {
        session_id: 8032,
        user_id: Some(other_char as u64),
        character_id: Some(other_char),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(sys_id),
        station_id: Some(other_station_id),
        ship_id: Some(foreign_live_ship_id),
        ..Default::default()
    };
    let kw = HashMap::new();

    // Pre-existing sub-object bound before ship loss survives `"ship_loss"` invalidation.
    let pre_bound_stub = ctx
        .bind_verified_sub_object(&sess, "invbroker", destroyed_ship_id)
        .unwrap();
    ctx.invalidate_bound_objects_for_session_transition(char_id, sys_id, None, "ship_loss");
    assert!(ctx.verify_bound_object(&sess, pre_bound_stub).is_ok());

    // Real MachoBindObject -> GetInventoryFromId(destroyed_ship_id, 0) -> verify_bound_object + GetSelfInvItem.
    let bind_sys_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MachoBindObject",
            &[PyRep::Integer(i64::from(sys_id)), PyRep::None],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("bind invbroker to solar system");
    let sys_bid = extract_macho_bound_id(&bind_sys_rep);

    let inv_stub_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(sys_bid),
            "GetInventoryFromId",
            &[PyRep::Integer(destroyed_ship_id), PyRep::Integer(0)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("GetInventoryFromId(destroyed_ship_id, 0)");
    let stub_bid = extract_macho_bound_id(&inv_stub_rep);
    assert!(ctx.verify_bound_object(&sess, stub_bid).is_ok());

    let self_item_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(stub_bid),
            "GetSelfInvItem",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("stub_bid.GetSelfInvItem()");
    assert_eq!(self_item_rep.dbrow_get("itemID"), Some(&PyRep::Integer(destroyed_ship_id)));
    assert_eq!(self_item_rep.dbrow_get("typeID"), Some(&PyRep::Integer(587)));
    assert_eq!(self_item_rep.dbrow_get("ownerID"), Some(&PyRep::Integer(char_id)));
    assert_eq!(
        self_item_rep.dbrow_get("locationID"),
        Some(&PyRep::Integer(i64::from(sys_id)))
    );

    let list_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(stub_bid),
            "List",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("stub_bid.List()");
    assert!(matches!(list_rep, PyRep::ObjectEx { .. }));

    // Transition to docked at station (`InventoryStation` init): stub binding remains valid.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
        }
    });
    sess.station_id = Some(station_id);
    ctx.invalidate_bound_objects_for_session_transition(char_id, sys_id, Some(station_id), "dock");
    let docked_self = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(stub_bid),
            "GetSelfInvItem",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("stub_bid.GetSelfInvItem() after dock");
    assert_eq!(
        docked_self.dbrow_get("locationID"),
        Some(&PyRep::Integer(station_id))
    );

    let bind_st_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MachoBindObject",
            &[PyRep::Integer(station_id), PyRep::None],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("bind invbroker to station");
    let st_bid = extract_macho_bound_id(&bind_st_rep);

    // Negative 1: Another character cannot verify or invoke GetSelfInvItem on char_id's stub bound object.
    assert_eq!(
        ctx.verify_bound_object(&other_sess, stub_bid)
            .unwrap_err()
            .code,
        "BoundObjectOwnershipViolation"
    );
    assert_eq!(
        ctx.dispatch_rpc(
            &mut other_sess,
            "invbroker",
            Some(stub_bid),
            "GetSelfInvItem",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .unwrap_err()
        .code,
        "BoundObjectOwnershipViolation"
    );

    // Negative 2: Bogus/non-stub IDs are still rejected by GetInventoryFromId and verify_bound_object.
    for bogus_id in [199_999_999_i64, 999_999, 1_000_000_000_000] {
        assert_eq!(
            ctx.dispatch_rpc(
                &mut sess,
                "invbroker",
                Some(st_bid),
                "GetInventoryFromId",
                &[PyRep::Integer(bogus_id), PyRep::Integer(0)],
                &kw,
                Some("machoNet:26000"),
            )
            .await
            .unwrap_err()
            .code,
            "FakeItemNotFound",
            "bogus_id={bogus_id} must still raise FakeItemNotFound"
        );
        let bogus_bid = ctx
            .bind_verified_sub_object(&sess, "invbroker", bogus_id)
            .unwrap();
        assert_eq!(
            ctx.verify_bound_object(&sess, bogus_bid).unwrap_err().code,
            "BoundObjectOwnershipViolation" // rpc-lint: allow error code assertion
        );
    }

    // Negative 3: Foreign live dynamic ship at another location is rejected across bind/verify/GetInventoryFromId.
    assert_eq!(
        ctx.bind_object(&sess, "invbroker", foreign_live_ship_id)
            .unwrap_err()
            .code,
        "BindAuthorizationDenied" // rpc-lint: allow error code assertion
    );
    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(st_bid),
            "GetInventoryFromId",
            &[PyRep::Integer(foreign_live_ship_id), PyRep::Integer(0)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .unwrap_err()
        .code,
        "InventoryNotAccessible"
    );
}

#[test]
fn issue_885_recheck_invbroker_add_auth_in_snap_rejects_mid_flight_revocations() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_id = 90_000_002_i64;
    let corp_id = 98_885_001_i64;
    let station_id = 60_003_760_i64;
    let ship_id = 1_000_885_001_i64;
    let cont_id = 1_000_885_002_i64;
    let item_id = 1_000_885_003_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(station_id);
            ch.active_ship_id = ship_id;
        }
        let mut corp = snap.corporations.values().next().cloned().unwrap();
        corp.corporation_id = corp_id;
        corp.ceo_id = other_id;
        corp.members.insert(other_id);
        corp.members.insert(char_id);
        corp.member_roles.insert(char_id, u64::MAX);
        snap.corporations.insert(corp_id, corp);

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
        snap.items.insert(
            cont_id,
            InventoryItemRecord {
                item_id: cont_id,
                type_id: 17366, // Station container
                owner_id: corp_id,
                location_id: station_id,
                flag: InventoryFlag::CorpSAG1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id: 34,
                owner_id: corp_id,
                location_id: cont_id,
                flag: InventoryFlag::Cargo,
                quantity: 100,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let mut sess = SessionState::new(1);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.station_id = Some(station_id);
    sess.ship_id = Some(ship_id);
    let orig_src = ctx.db.with_snapshot(|snap| snap.items[&item_id].clone());
    let kw = HashMap::new();

    // 1. Baseline passes when roles and container state match
    ctx.db.with_snapshot(|snap| {
        super::recheck_invbroker_add_auth_in_snap(
            &ctx,
            snap,
            &sess,
            char_id,
            &orig_src,
            Some(&orig_src),
            ship_id,
            InventoryFlag::Cargo,
            &[],
            &kw,
        )
        .expect("baseline recheck ok");
    });

    // 2. Mid-flight ALSC lock on source item -> ALSCItemLocked
    {
        let mut st = crate::services::fitting_containers_ships::read_container_ship_state(&ctx);
        st.alsc_locked_items.insert(item_id);
        crate::services::fitting_containers_ships::save_container_ship_state(&ctx, &st);
    }
    ctx.db.with_snapshot(|snap| {
        let err = super::recheck_invbroker_add_auth_in_snap(
            &ctx,
            snap,
            &sess,
            char_id,
            &orig_src,
            Some(&orig_src),
            ship_id,
            InventoryFlag::Cargo,
            &[],
            &kw,
        )
        .unwrap_err();
        assert_eq!(err.code, "ALSCItemLocked");
    });
    {
        let mut st = crate::services::fitting_containers_ships::read_container_ship_state(&ctx);
        st.alsc_locked_items.clear();
        st.container_passwords.insert(cont_id, "secret".into());
        crate::services::fitting_containers_ships::save_container_ship_state(&ctx, &st);
    }

    // 3. Mid-flight container password set without unlock -> ALSCPasswordNeededToOpen
    ctx.db.with_snapshot(|snap| {
        let err = super::recheck_invbroker_add_auth_in_snap(
            &ctx,
            snap,
            &sess,
            char_id,
            &orig_src,
            Some(&orig_src),
            ship_id,
            InventoryFlag::Cargo,
            &[],
            &kw,
        )
        .unwrap_err();
        assert_eq!(err.code, "ALSCPasswordNeededToOpen");
    });
    {
        let mut st = crate::services::fitting_containers_ships::read_container_ship_state(&ctx);
        st.container_passwords.clear();
        crate::services::fitting_containers_ships::save_container_ship_state(&ctx, &st);
    }

    // 4. Mid-flight revocation of corporation take roles -> CorpRoleDenied
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(char_id, 0);
    });
    ctx.db.with_snapshot(|snap| {
        let err = super::recheck_invbroker_add_auth_in_snap(
            &ctx,
            snap,
            &sess,
            char_id,
            &orig_src,
            Some(&orig_src),
            ship_id,
            InventoryFlag::Cargo,
            &[],
            &kw,
        )
        .unwrap_err();
        assert_eq!(err.code, "CorpRoleDenied");
    });

    // 5. Fuel Technician (StarbaseOperator) withdrawing from structure SpecializedFuelBay succeeds
    let struct_id = 1_000_885_010_i64;
    let other_struct_id = 1_000_885_011_i64;
    let fuel_item_id = 1_000_885_012_i64;
    let fuel_item = InventoryItemRecord {
        item_id: fuel_item_id,
        type_id: 4051,
        owner_id: corp_id,
        location_id: struct_id,
        flag: InventoryFlag::SpecializedFuelBay,
        quantity: 50,
        singleton: false,
        ..Default::default()
    };
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(char_id, eve_db::corp_roles::ROLE_STARBASE_OPERATOR);
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Fuel Astrahus".into(),
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
        snap.structures.insert(
            other_struct_id,
            UpwellStructureRecord {
                structure_id: other_struct_id,
                type_id: 35832,
                owner_corp_id: 1_000_125,
                solar_system_id: 30_000_142,
                name: "Public Freeport".into(),
                state: "ShieldVulnerable".into(),
                position: [20_000.0, 0.0, 0.0],
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
        snap.items.insert(fuel_item_id, fuel_item.clone());
    });
    ctx.db.with_snapshot(|snap| {
        super::recheck_invbroker_add_auth_in_snap(
            &ctx,
            snap,
            &sess,
            char_id,
            &fuel_item,
            Some(&fuel_item),
            ship_id,
            InventoryFlag::Cargo,
            &[],
            &kw,
        )
        .expect("fuel technician should retain structure fuel bay withdrawal access");
    });

    // 6. Mid-flight structure docking ACL revocation -> DestinationAccessDenied
    let personal_item = InventoryItemRecord {
        item_id: 1_000_885_013_i64,
        type_id: 34,
        owner_id: char_id,
        location_id: ship_id,
        flag: InventoryFlag::Cargo,
        quantity: 10,
        singleton: false,
        ..Default::default()
    };
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(personal_item.item_id, personal_item.clone());
        snap.structure_docking_acl_denied
            .entry(other_struct_id)
            .or_default()
            .insert(char_id);
    });
    ctx.db.with_snapshot(|snap| {
        let err = super::recheck_invbroker_add_auth_in_snap(
            &ctx,
            snap,
            &sess,
            char_id,
            &personal_item,
            Some(&personal_item),
            other_struct_id,
            InventoryFlag::Hangar,
            &[],
            &kw,
        )
        .unwrap_err();
        assert_eq!(err.code, "DestinationAccessDenied");
    });
}

#[tokio::test]
async fn issue_885_rejects_ex_corp_container_personal_withdrawal_other_corp_deposit_and_uses_live_corp() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_id = 90_000_002_i64;
    let old_corp = 98_885_101_i64;
    let new_corp = 98_885_102_i64;
    let station_id = 60_003_760_i64;
    let ship_id = 1_000_885_101_i64;
    let ex_corp_cont_id = 1_000_885_102_i64;
    let personal_in_ex_cont_id = 1_000_885_103_i64;
    let ship_cargo_stack_id = 1_000_885_104_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = new_corp;
            ch.station_id = Some(station_id);
            ch.active_ship_id = ship_id;
        }
        let template = snap.corporations.values().next().cloned().unwrap();
        let mut corp_old = template.clone();
        corp_old.corporation_id = old_corp;
        corp_old.ceo_id = other_id;
        corp_old.members.insert(other_id);
        corp_old.members.remove(&char_id);
        corp_old.member_roles.remove(&char_id);
        snap.corporations.insert(old_corp, corp_old);

        let mut corp_new = template;
        corp_new.corporation_id = new_corp;
        corp_new.ceo_id = char_id;
        corp_new.members.insert(char_id);
        corp_new.member_roles.insert(char_id, u64::MAX);
        snap.corporations.insert(new_corp, corp_new);

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
        snap.items.insert(
            ex_corp_cont_id,
            InventoryItemRecord {
                item_id: ex_corp_cont_id,
                type_id: 17366, // Station container (non-space)
                owner_id: old_corp,
                location_id: station_id,
                flag: InventoryFlag::CorpSAG1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            personal_in_ex_cont_id,
            InventoryItemRecord {
                item_id: personal_in_ex_cont_id,
                type_id: 34,
                owner_id: char_id, // personal item inside ex-corp container
                location_id: ex_corp_cont_id,
                flag: InventoryFlag::Cargo,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship_cargo_stack_id,
            InventoryItemRecord {
                item_id: ship_cargo_stack_id,
                type_id: 34,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 100,
                singleton: false,
                ..Default::default()
            },
        );
    });

    // Session holds stale corporation_id (old_corp), while character is live in new_corp.
    let mut sess = SessionState::new(1);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(old_corp);
    sess.station_id = Some(station_id);
    sess.ship_id = Some(ship_id);
    let kw = HashMap::new();

    // 1. Non-GM withdrawing personal item from ex-corp container must be rejected (CorpRoleDenied)
    let personal_item = ctx.db.get_item(personal_in_ex_cont_id).unwrap();
    ctx.db.with_snapshot(|snap| {
        let err = super::recheck_invbroker_add_auth_in_snap(
            &ctx,
            snap,
            &sess,
            char_id,
            &personal_item,
            Some(&personal_item),
            ship_id,
            InventoryFlag::Cargo,
            &[],
            &kw,
        )
        .unwrap_err();
        assert_eq!(err.code, "CorpRoleDenied");
    });

    // 2. Non-GM depositing into non-space container owned by another corp must be rejected
    let ship_stack = ctx.db.get_item(ship_cargo_stack_id).unwrap();
    ctx.db.with_snapshot(|snap| {
        let err = super::recheck_invbroker_add_auth_in_snap(
            &ctx,
            snap,
            &sess,
            char_id,
            &ship_stack,
            Some(&ship_stack),
            ex_corp_cont_id,
            InventoryFlag::Cargo,
            &[],
            &kw,
        )
        .unwrap_err();
        assert_eq!(err.code, "DestinationAccessDenied");
    });

    // 3. End-to-end invbroker.Add split and full move into CorpSAG1 (116) must stamp live new_corp,
    // not stale session.corporation_id (old_corp).
    let bound = bound_invbroker(char_id, station_id);
    let mut split_kw = HashMap::new();
    split_kw.insert("qty".to_string(), PyRep::Integer(25));
    let split_res = crate::services::inventory_fitting::handle(
        &ctx,
        &mut sess,
        Some(&bound),
        "invbroker",
        "Add",
        &[
            PyRep::Integer(ship_cargo_stack_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::CorpSAG1 as i64),
        ],
        &split_kw,
    )
    .await
    .expect("invbroker.Add handled")
    .expect("split into CorpSAG1 succeeds");
    let split_id = split_res
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("split itemID returned");
    let split_rec = ctx.db.get_item(split_id).expect("split item exists");
    assert_eq!(split_rec.location_id, station_id);
    assert_eq!(split_rec.flag, InventoryFlag::CorpSAG1);
    assert_eq!(split_rec.owner_id, new_corp);

    crate::services::inventory_fitting::handle(
        &ctx,
        &mut sess,
        Some(&bound),
        "invbroker",
        "Add",
        &[
            PyRep::Integer(ship_cargo_stack_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::CorpSAG1 as i64),
        ],
        &kw,
    )
    .await
    .expect("invbroker.Add handled")
    .expect("full move into CorpSAG1 succeeds");
    let moved_rec = ctx.db.get_item(ship_cargo_stack_id).expect("moved item exists");
    assert_eq!(moved_rec.location_id, station_id);
    assert_eq!(moved_rec.flag, InventoryFlag::CorpSAG1);
    assert_eq!(moved_rec.owner_id, new_corp);
}

#[tokio::test]
async fn issue_1021_1022_capsuleer_deliveries_get_inventory_list_and_rbacs() {
    let ctx = test_ctx();
    let station_id = 60_003_760_i64;
    let other_station = 60_008_494_i64;
    let corp_id = 98_100_001_i64;
    let other_corp_id = 98_100_099_i64;
    let ceo_id = 90_000_001_i64;
    let member_id = 90_000_002_i64;
    let deliv_item_id = 92_100_001_i64;
    let other_corp_deliv_item_id = 92_100_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "Test Corp".into();
        corp.ticker = "TSTC".into();
        corp.ceo_id = ceo_id;
        corp.hq_station_id = Some(station_id);
        corp.members.insert(ceo_id);
        corp.members.insert(member_id);
        corp.member_roles.insert(member_id, 0);
        snap.corporations.insert(corp_id, corp);
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(station_id);
            ch.structure_id = None;
        }
        if let Some(ch) = snap.characters.get_mut(&member_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(station_id);
            ch.structure_id = None;
        }
        snap.corp_role_assignments.insert(
            (corp_id, member_id),
            CorpMemberRoleAssignment {
                character_id: member_id,
                ..Default::default()
            },
        );
        snap.items.insert(
            deliv_item_id,
            eve_db::InventoryItemRecord {
                item_id: deliv_item_id,
                type_id: 34,
                owner_id: corp_id,
                location_id: station_id,
                flag: InventoryFlag::CapsuleerDeliveries,
                quantity: 500,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            other_corp_deliv_item_id,
            eve_db::InventoryItemRecord {
                item_id: other_corp_deliv_item_id,
                type_id: 34,
                owner_id: other_corp_id,
                location_id: station_id,
                flag: InventoryFlag::CapsuleerDeliveries,
                quantity: 999,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let mut ceo_sess = SessionState {
        session_id: 1,
        user_id: Some(ceo_id as u64),
        character_id: Some(ceo_id),
        corporation_id: Some(corp_id),
        station_id: Some(station_id),
        ship_id: Some(100_000_001),
        ..Default::default()
    };
    let mut mem_sess = SessionState {
        session_id: 2,
        user_id: Some(member_id as u64),
        character_id: Some(member_id),
        corporation_id: Some(corp_id),
        station_id: Some(station_id),
        ship_id: Some(100_000_002),
        ..Default::default()
    };
    let b_station = bound_invbroker(ceo_id, station_id);
    let b_mem = bound_invbroker(member_id, station_id);
    let b_wrong = bound_invbroker(ceo_id, other_station);

    // 1. GetInventory(10015, corp_id) at docked station succeeds (containerCapsuleerDeliveries).
    let inv_rep = handle(
        &ctx,
        &ceo_sess,
        Some(&b_station),
        "invbroker",
        "GetInventory",
        &[PyRep::Integer(10_015), PyRep::Integer(corp_id)],
        None,
    )
    .expect("GetInventory(10015) handled")
    .expect("GetInventory(10015) succeeds");
    assert!(inv_rep.as_i64().is_some());

    // 2. Bound List(flag=187) returns own corp's CapsuleerDeliveries items and excludes other corp's items.
    let mut kw = HashMap::new();
    kw.insert("flag".to_string(), PyRep::Integer(187));
    let listed = crate::services::inventory_fitting::handle(
        &ctx,
        &mut mem_sess,
        Some(&b_mem),
        "invbroker",
        "List",
        &[],
        &kw,
    )
    .await
    .expect("List handled")
    .expect("List(flag=187) succeeds");
    let PyRep::ObjectEx { list: rows, .. } = listed else {
        panic!("expected CRowset ObjectEx from invbroker.List");
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].dbrow_get("itemID"), Some(&PyRep::Integer(deliv_item_id)));

    // 3. Negative checks: cross-corp request, wrong station, and missing corporation are rejected.
    let cross_corp_err = handle(
        &ctx,
        &ceo_sess,
        Some(&b_station),
        "invbroker",
        "GetInventory",
        &[PyRep::Integer(10_015), PyRep::Integer(other_corp_id)],
        None,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(cross_corp_err.code, "InventoryNotAccessible");

    let wrong_station_err = handle(
        &ctx,
        &ceo_sess,
        Some(&b_wrong),
        "invbroker",
        "GetInventory",
        &[PyRep::Integer(10_015), PyRep::Integer(corp_id)],
        None,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(wrong_station_err.code, "CharacterNotAtStation");

    ceo_sess.corporation_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&ceo_id).unwrap().corporation_id = 0;
    });
    let no_corp_err = handle(
        &ctx,
        &ceo_sess,
        Some(&b_station),
        "invbroker",
        "GetInventory",
        &[PyRep::Integer(10_015)],
        None,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(no_corp_err.code, "InventoryNotAccessible");

    // 4. Taking out of CapsuleerDeliveries (flag 187) via Add enforces deliveries take role.
    let mut take_kw = HashMap::new();
    take_kw.insert("qty".to_string(), PyRep::Integer(50));
    let denied = crate::services::inventory_fitting::handle(
        &ctx,
        &mut mem_sess,
        Some(&b_mem),
        "invbroker",
        "Add",
        &[
            PyRep::Integer(deliv_item_id),
            PyRep::Integer(station_id),
            PyRep::Integer(InventoryFlag::Hangar as i64),
        ],
        &take_kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(denied.code, "CorpRoleDenied");

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(member_id, eve_db::corp_roles::ROLE_DELIVERIES_HANGAR_TAKE);
        snap.corp_role_assignments
            .get_mut(&(corp_id, member_id))
            .unwrap()
            .roles_at_hq = eve_db::corp_roles::ROLE_DELIVERIES_HANGAR_TAKE;
    });
    let ok_take = crate::services::inventory_fitting::handle(
        &ctx,
        &mut mem_sess,
        Some(&b_mem),
        "invbroker",
        "Add",
        &[
            PyRep::Integer(deliv_item_id),
            PyRep::Integer(station_id),
            PyRep::Integer(InventoryFlag::Hangar as i64),
        ],
        &take_kw,
    )
    .await
    .unwrap();
    assert!(ok_take.is_ok(), "member with ROLE_DELIVERIES_HANGAR_TAKE should take from CapsuleerDeliveries");
}

#[tokio::test]
async fn hf1020_01_capsuleer_deliveries_multi_add_ownership_and_deposit_rejection() {
    let ctx = test_ctx();
    let station_id = 60_003_760_i64;
    let corp_id = 98_100_011_i64;
    let ceo_id = 90_000_001_i64;
    let member_id = 90_000_002_i64;
    let personal_cont_id = 92_100_100_i64;
    let cap_item_1 = 92_100_101_i64;
    let cap_item_2 = 92_100_102_i64;
    let corp_deliv_item = 92_100_103_i64;
    let personal_item_id = 92_100_104_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "Delivery Test Corp".into();
        corp.ticker = "DTST".into();
        corp.ceo_id = ceo_id;
        corp.hq_station_id = Some(station_id);
        corp.members.insert(ceo_id);
        corp.members.insert(member_id);
        corp.member_roles.insert(member_id, 0);
        snap.corporations.insert(corp_id, corp);
        if let Some(ch) = snap.characters.get_mut(&member_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(station_id);
            ch.structure_id = None;
        }
        snap.corp_role_assignments.insert(
            (corp_id, member_id),
            CorpMemberRoleAssignment {
                character_id: member_id,
                ..Default::default()
            },
        );
        snap.items.insert(
            personal_cont_id,
            eve_db::InventoryItemRecord {
                item_id: personal_cont_id,
                type_id: 17366,
                owner_id: member_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        for (iid, flag, qty) in [
            (cap_item_1, InventoryFlag::CapsuleerDeliveries, 100),
            (cap_item_2, InventoryFlag::CapsuleerDeliveries, 200),
            (corp_deliv_item, InventoryFlag::CorpDeliveries, 50),
        ] {
            snap.items.insert(
                iid,
                eve_db::InventoryItemRecord {
                    item_id: iid,
                    type_id: 34,
                    owner_id: corp_id,
                    location_id: station_id,
                    flag,
                    quantity: qty,
                    singleton: false,
                    ..Default::default()
                },
            );
        }
        snap.items.insert(
            personal_item_id,
            eve_db::InventoryItemRecord {
                item_id: personal_item_id,
                type_id: 34,
                owner_id: member_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 75,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let mut mem_sess = SessionState {
        session_id: 12,
        user_id: Some(member_id as u64),
        character_id: Some(member_id),
        corporation_id: Some(corp_id),
        station_id: Some(station_id),
        ship_id: Some(100_000_002),
        ..Default::default()
    };
    let b_mem = bound_invbroker(member_id, station_id);
    let empty_kw = HashMap::new();

    // 1. Non-Director without deliveries take role is denied on MultiAdd from CapsuleerDeliveries.
    let denied = crate::services::inventory_fitting::handle(
        &ctx,
        &mut mem_sess,
        Some(&b_mem),
        "invbroker",
        "MultiAdd",
        &[
            PyRep::List(vec![PyRep::Integer(cap_item_1), PyRep::Integer(cap_item_2)]),
            PyRep::Integer(personal_cont_id),
            PyRep::Integer(InventoryFlag::Unlocked as i64),
        ],
        &empty_kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(denied.code, "CorpRoleDenied");

    // 2. Grant ROLE_DELIVERIES_HANGAR_TAKE (not Director/CEO) -> MultiAdd into personal container transfers owner_id to member_id.
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(member_id, eve_db::corp_roles::ROLE_DELIVERIES_HANGAR_TAKE);
        snap.corp_role_assignments
            .get_mut(&(corp_id, member_id))
            .unwrap()
            .roles_at_hq = eve_db::corp_roles::ROLE_DELIVERIES_HANGAR_TAKE;
    });
    let multi_ok = crate::services::inventory_fitting::handle(
        &ctx,
        &mut mem_sess,
        Some(&b_mem),
        "invbroker",
        "MultiAdd",
        &[
            PyRep::List(vec![PyRep::Integer(cap_item_1), PyRep::Integer(cap_item_2)]),
            PyRep::Integer(personal_cont_id),
            PyRep::Integer(InventoryFlag::Unlocked as i64),
        ],
        &empty_kw,
    )
    .await
    .unwrap()
    .expect(
        "non-Director with ROLE_DELIVERIES_HANGAR_TAKE should MultiAdd from CapsuleerDeliveries",
    );
    assert_eq!(
        multi_ok.dict_get("movedCount").and_then(|v| v.as_i64()),
        Some(2)
    );
    ctx.db.with_snapshot(|snap| {
        for iid in [cap_item_1, cap_item_2] {
            let it = snap.items.get(&iid).expect("moved item exists");
            assert_eq!(it.location_id, personal_cont_id);
            assert_eq!(it.flag, InventoryFlag::Unlocked);
            assert_eq!(
                it.owner_id, member_id,
                "corp delivery item {iid} must transfer ownership to char_id"
            );
        }
    });

    // 3. Single Add from CorpDeliveries (62) into personal container with flag=0 also transfers owner_id to member_id.
    let add_ok = crate::services::inventory_fitting::handle(
        &ctx,
        &mut mem_sess,
        Some(&b_mem),
        "invbroker",
        "Add",
        &[
            PyRep::Integer(corp_deliv_item),
            PyRep::Integer(personal_cont_id),
            PyRep::Integer(0),
        ],
        &empty_kw,
    )
    .await
    .unwrap();
    assert!(
        add_ok.is_ok(),
        "Add from CorpDeliveries into personal container should succeed"
    );
    ctx.db.with_snapshot(|snap| {
        let it = snap
            .items
            .get(&corp_deliv_item)
            .expect("moved CorpDeliveries item exists");
        assert_eq!(it.location_id, personal_cont_id);
        assert_eq!(
            it.owner_id, member_id,
            "Add from CorpDeliveries to personal container must transfer ownership to char_id"
        );
    });

    // 4. MultiAdd & Add reject deposits into read-only delivery flags (62, 173, 187).
    for deliv_flag in [62_i64, 173_i64, 187_i64] {
        let multi_err = crate::services::inventory_fitting::handle(
            &ctx,
            &mut mem_sess,
            Some(&b_mem),
            "invbroker",
            "MultiAdd",
            &[
                PyRep::List(vec![PyRep::Integer(personal_item_id)]),
                PyRep::Integer(station_id),
                PyRep::Integer(deliv_flag),
            ],
            &empty_kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(
            multi_err.code, "InvalidInventoryFlag",
            "MultiAdd to delivery flag {deliv_flag} must be rejected"
        );

        let add_err = crate::services::inventory_fitting::handle(
            &ctx,
            &mut mem_sess,
            Some(&b_mem),
            "invbroker",
            "Add",
            &[
                PyRep::Integer(personal_item_id),
                PyRep::Integer(station_id),
                PyRep::Integer(deliv_flag),
            ],
            &empty_kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(
            add_err.code, "InvalidInventoryFlag",
            "Add to delivery flag {deliv_flag} must be rejected"
        );
    }
    ctx.db.with_snapshot(|snap| {
        let it = snap
            .items
            .get(&personal_item_id)
            .expect("personal item remains");
        assert_eq!(it.location_id, station_id);
        assert_eq!(it.flag, InventoryFlag::Hangar);
        assert_eq!(it.owner_id, member_id);
    });
}

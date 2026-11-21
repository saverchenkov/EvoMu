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

    // Verify locationID fallback chain (session station_id -> structure_id -> solar_system_id -> DB character),
    // failing closed with LocationNotFound when no location can be resolved.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = 30_002_187;
        }
    });
    for (st_id, str_id, sol_id, exp_loc) in [
        (Some(60_003_760_i64), Some(1_000_000_000_777_i64), Some(sys_id), 60_003_760_i64),
        (None, Some(1_000_000_000_777_i64), Some(sys_id), 1_000_000_000_777_i64),
        (None, None, None, 30_002_187_i64),
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
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = 0;
        }
    });
    let mut no_loc_sess = sess.clone();
    no_loc_sess.station_id = None;
    no_loc_sess.structure_id = None;
    no_loc_sess.solar_system_id = None;
    let missing_loc_err = handle(
        &ctx,
        &no_loc_sess,
        Some(&b_destroyed),
        "invbroker",
        "GetSelfInvItem",
        &[],
        None,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(missing_loc_err.code, "LocationNotFound");

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
    ctx.db.with_mut_snapshot(|snap| {
        crate::services::fitting_containers_ships::set_alsc_item_locked_snap(snap, item_id, true, 0)
            .unwrap();
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
        assert_eq!(err.code, "ALSCItemLocked");
    });
    ctx.db.with_mut_snapshot(|snap| {
        crate::services::fitting_containers_ships::set_alsc_item_locked_snap(snap, item_id, false, 0)
            .unwrap();
        crate::services::fitting_containers_ships::set_container_password_snap(
            snap, cont_id, 1, "secret", 0,
        )
        .unwrap();
    });

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
    ctx.db.with_mut_snapshot(|snap| {
        crate::services::fitting_containers_ships::set_container_password_snap(
            snap, cont_id, 1, "", 0,
        )
        .unwrap();
    });

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

#[tokio::test]
async fn invbroker_upwell_structure_bound_on_solar_system_supports_structure_containers_and_enforces_auth() {
    let ctx = test_ctx();
    let char_id = 95_301_062;
    let corp_id = 98_301_062;
    let other_corp_id = 98_991_062;
    let sys_id = 30_000_142u32;
    let other_sys = 30_002_187u32;
    let struct_id = 1_030_001_062_001i64;
    let other_sys_struct_id = 1_030_001_062_002i64;
    let active_ship_id = 1_000_953_620i64;
    let hangar_ship_id = 1_000_953_621i64;
    let foreign_ship_id = 1_000_953_622i64;
    let space_can_id = 1_000_953_623i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = "Structure Dock Pilot".into();
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = Some(struct_id);
        ch.active_ship_id = active_ship_id;
        snap.characters.insert(char_id, ch);

        for (sid, s_sys) in [(struct_id, sys_id), (other_sys_struct_id, other_sys)] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: 35_832,
                    owner_corp_id: other_corp_id,
                    solar_system_id: s_sys,
                    name: format!("Public Citadel {sid}"),
                    state: "ShieldVulnerable".into(),
                    ..Default::default()
                },
            );
        }
        for (item_id, type_id, owner_id, location_id, flag) in [
            (active_ship_id, 587, char_id, struct_id, InventoryFlag::Hangar),
            (hangar_ship_id, 588, char_id, struct_id, InventoryFlag::Hangar),
            (foreign_ship_id, 587, 99_999_999, struct_id, InventoryFlag::Hangar),
            (space_can_id, 23, char_id, i64::from(sys_id), InventoryFlag::None),
        ] {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id,
                    owner_id,
                    location_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });

    let mut sess = SessionState {
        session_id: 1062,
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(sys_id),
        station_id: None,
        structure_id: Some(struct_id),
        ship_id: Some(active_ship_id),
        ..Default::default()
    };
    let sys_bid = ctx
        .bind_object(&sess, "invbroker", i64::from(sys_id))
        .expect("invbroker must bind to solar_system_id while docked in Upwell structure");
    let sys_entry = ctx.verify_bound_object(&sess, sys_bid).expect("verify sys invbroker");

    // 1. GetInventoryFromId(struct_id, 1) on sys-bound invbroker succeeds even when structure is owned by another corp.
    let struct_inv_rep = handle(
        &ctx,
        &sess,
        Some(&sys_entry),
        "invbroker",
        "GetInventoryFromId",
        &[PyRep::Integer(struct_id), PyRep::Integer(1)],
        None,
    )
    .expect("handled")
    .expect("GetInventoryFromId(struct_id) must succeed when docked there");
    let struct_sub_bid = struct_inv_rep.as_i64().expect("bound sub-object id") as u64;
    let struct_sub_entry = ctx.verify_bound_object(&sess, struct_sub_bid).expect("verify struct sub-object");
    assert_eq!(struct_sub_entry.object_id, struct_id);

    let self_row = handle(
        &ctx,
        &sess,
        Some(&struct_sub_entry),
        "invbroker",
        "GetSelfInvItem",
        &[],
        None,
    )
    .expect("handled")
    .expect("GetSelfInvItem on structure inventory");
    assert_eq!(self_row.dbrow_get("itemID"), Some(&PyRep::Integer(struct_id)));

    // 2. GetInventoryFromId for another ship in the structure hangar succeeds.
    let hangar_ship_rep = handle(
        &ctx,
        &sess,
        Some(&sys_entry),
        "invbroker",
        "GetInventoryFromId",
        &[PyRep::Integer(hangar_ship_id), PyRep::Integer(0)],
        None,
    )
    .expect("handled")
    .expect("GetInventoryFromId(hangar_ship_id) must succeed");
    assert!(hangar_ship_rep.as_i64().is_some());

    // 3. GetInventory(10014, None) (`containerStructure`) binds sub-object to `struct_id` and lists hangar items.
    let container_rep = handle(
        &ctx,
        &sess,
        Some(&sys_entry),
        "invbroker",
        "GetInventory",
        &[PyRep::Integer(CONTAINER_STRUCTURE), PyRep::None],
        None,
    )
    .expect("handled")
    .expect("GetInventory(10014) must succeed while docked in structure");
    let container_bid = container_rep.as_i64().expect("container sub-object id") as u64;
    let container_entry = ctx.verify_bound_object(&sess, container_bid).expect("verify container sub-object");
    assert_eq!(container_entry.object_id, struct_id);

    let listed = crate::services::inventory_fitting::handle(
        &ctx,
        &mut sess,
        Some(&container_entry),
        "invbroker",
        "List",
        &[PyRep::Integer(i64::from(InventoryFlag::Hangar as u16))],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("List(Hangar) on structure container");
    let listed_ids: Vec<i64> = listed
        .crowset_rows()
        .expect("CRowset")
        .iter()
        .filter_map(|r| r.dbrow_get("itemID").and_then(|v| v.as_i64()))
        .collect();
    assert!(listed_ids.contains(&active_ship_id));
    assert!(listed_ids.contains(&hangar_ship_id));
    assert!(!listed_ids.contains(&foreign_ship_id));

    // 4. Negative security checks: foreign ship, space can while docked, foreign structure, and undocked GetInventory(10014).
    for denied_id in [foreign_ship_id, space_can_id, other_sys_struct_id] {
        let err = handle(
            &ctx,
            &sess,
            Some(&sys_entry),
            "invbroker",
            "GetInventoryFromId",
            &[PyRep::Integer(denied_id), PyRep::Integer(0)],
            None,
        )
        .expect("handled")
        .unwrap_err();
        assert_eq!(
            err.code,
            "InventoryNotAccessible", // rpc-lint: allow error code assertion
            "GetInventoryFromId({denied_id}) must be denied"
        );
    }

    let wrong_sys_entry = bound_invbroker(char_id, i64::from(other_sys));
    let wrong_sys_err = handle(
        &ctx,
        &sess,
        Some(&wrong_sys_entry),
        "invbroker",
        "GetInventory",
        &[PyRep::Integer(CONTAINER_STRUCTURE), PyRep::None],
        None,
    )
    .expect("handled")
    .unwrap_err();
    assert_eq!(wrong_sys_err.code, "CharacterNotAtStation"); // rpc-lint: allow error code assertion

    sess.structure_id = None;
    let undocked_err = handle(
        &ctx,
        &sess,
        Some(&sys_entry),
        "invbroker",
        "GetInventory",
        &[PyRep::Integer(CONTAINER_STRUCTURE), PyRep::None],
        None,
    )
    .expect("handled")
    .unwrap_err();
    assert_eq!(undocked_err.code, "CharacterNotAtStation"); // rpc-lint: allow error code assertion
}

#[test]
fn get_inventory_from_id_accepts_item_descriptors_and_rejects_malformed_args() {
    let ctx = test_ctx();
    let char_id = 95_900_001_i64;
    let corp_id = 98_900_001_i64;
    let station_id = 60_003_760_i64;
    let active_ship_id = 1_896_945_100_i64;
    let assembled_shuttle_id = 1_896_945_262_i64;
    let foreign_ship_id = 1_896_945_999_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = "Descriptor Pilot".into();
        ch.corporation_id = corp_id;
        ch.station_id = Some(station_id);
        ch.solar_system_id = 30_000_142;
        ch.active_ship_id = active_ship_id;
        snap.characters.insert(char_id, ch);
        for (item_id, owner_id) in [
            (active_ship_id, char_id),
            (assembled_shuttle_id, char_id),
            (foreign_ship_id, 99_999_999),
        ] {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id: 587,
                    owner_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 4,
                    ..Default::default()
                },
            );
        }
    });

    let sess = SessionState {
        session_id: 1,
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        station_id: Some(station_id),
        solar_system_id: Some(30_000_142),
        ship_id: Some(active_ship_id),
        ..Default::default()
    };
    let b_station = bound_invbroker(char_id, station_id);

    let keyval_desc = PyRep::keyval([
        ("itemID", PyRep::Integer(assembled_shuttle_id)),
        ("singleton", PyRep::Bool(true)),
        ("version", PyRep::Integer(4)),
    ]);
    let dict_desc = PyRep::Dict(vec![
        (PyRep::String("itemID".into()), PyRep::Integer(assembled_shuttle_id)),
        (PyRep::String("singleton".into()), PyRep::Bool(true)),
        (PyRep::String("version".into()), PyRep::Integer(4)),
    ]);
    let dbrow_desc = DBRowDescriptor::inv_item_header().build_dbrow(vec![
        PyRep::Integer(assembled_shuttle_id),
        PyRep::Integer(587),
        PyRep::Integer(char_id),
        PyRep::Integer(station_id),
        PyRep::Integer(InventoryFlag::Hangar as i64),
        PyRep::Integer(-1),
        PyRep::Integer(25),
        PyRep::Integer(6),
        PyRep::String(String::new()),
    ]);
    let row_obj_desc = PyRep::Object {
        type_name: "util.Row".into(),
        args: Box::new(PyRep::Tuple(vec![
            PyRep::List(vec![PyRep::String("itemID".into())]),
            PyRep::List(vec![PyRep::Integer(assembled_shuttle_id)]),
        ])),
    };
    let obj_ex_desc = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::Token("utillib.KeyVal".into()),
            PyRep::Tuple(Vec::new()),
        ])),
        list: Vec::new(),
        dict: vec![(
            PyRep::String("itemID".into()),
            PyRep::Integer(assembled_shuttle_id),
        )],
    };

    for (label, call_args) in [
        ("int + int passive=0", vec![PyRep::Integer(assembled_shuttle_id), PyRep::Integer(0)]),
        ("KeyVal + int passive=0", vec![keyval_desc.clone(), PyRep::Integer(0)]),
        ("KeyVal + int passive=1", vec![keyval_desc.clone(), PyRep::Integer(1)]),
        ("KeyVal + bool passive=false", vec![keyval_desc.clone(), PyRep::Bool(false)]),
        ("KeyVal + bool passive=true", vec![keyval_desc.clone(), PyRep::Bool(true)]),
        ("KeyVal omitted passive", vec![keyval_desc]),
        ("Dict descriptor", vec![dict_desc, PyRep::Integer(0)]),
        ("DBRow descriptor", vec![dbrow_desc, PyRep::Integer(0)]),
        ("util.Row Object descriptor", vec![row_obj_desc, PyRep::Integer(0)]),
        ("ObjectEx descriptor", vec![obj_ex_desc, PyRep::Integer(0)]),
    ] {
        let rep = handle(
            &ctx,
            &sess,
            Some(&b_station),
            "invbroker",
            "GetInventoryFromId",
            &call_args,
            None,
        )
        .unwrap()
        .unwrap_or_else(|e| panic!("{label} must succeed, got {e:?}"));
        let bid = rep.as_i64().expect("bound id") as u64;
        let bound = ctx.verify_bound_object(&sess, bid).expect("verified sub-object");
        assert_eq!(bound.service, "invbroker", "{label}");
        assert_eq!(bound.object_id, assembled_shuttle_id, "{label}");
    }

    // Missing item and foreign ship inside KeyVal descriptors must still hit normal guards.
    let missing_rep = PyRep::keyval([("itemID", PyRep::Integer(199_888_777))]);
    assert_eq!(
        handle(
            &ctx,
            &sess,
            Some(&b_station),
            "invbroker",
            "GetInventoryFromId",
            &[missing_rep, PyRep::Integer(0)],
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "FakeItemNotFound"
    );
    let foreign_rep = PyRep::keyval([("itemID", PyRep::Integer(foreign_ship_id))]);
    assert_eq!(
        handle(
            &ctx,
            &sess,
            Some(&b_station),
            "invbroker",
            "GetInventoryFromId",
            &[foreign_rep, PyRep::Integer(0)],
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InventoryNotAccessible"
    );

    // Genuinely invalid arguments must still be rejected with InvalidArguments.
    for bad_args in [
        vec![],
        vec![PyRep::None, PyRep::Integer(0)],
        vec![PyRep::Bool(true), PyRep::Integer(0)],
        vec![PyRep::String("1896945262".into()), PyRep::Integer(0)],
        vec![PyRep::List(vec![PyRep::Integer(assembled_shuttle_id)]), PyRep::Integer(0)],
        vec![
            PyRep::List(vec![PyRep::keyval([("itemID", PyRep::Integer(assembled_shuttle_id))])]),
            PyRep::Integer(0),
        ],
        vec![PyRep::Dict(Vec::new()), PyRep::Integer(0)],
        vec![
            PyRep::keyval([
                ("singleton", PyRep::Bool(true)),
                ("version", PyRep::Integer(4)),
            ]),
            PyRep::Integer(0),
        ],
        vec![
            PyRep::keyval([("itemID", PyRep::String("bad".into()))]),
            PyRep::Integer(0),
        ],
        vec![PyRep::keyval([("itemID", PyRep::Bool(true))]), PyRep::Integer(0)],
    ] {
        let err = handle(
            &ctx,
            &sess,
            Some(&b_station),
            "invbroker",
            "GetInventoryFromId",
            &bad_args,
            None,
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(err.code, "InvalidArguments", "args: {bad_args:?}");
    }
}

#[tokio::test]
async fn moveme_with_inflight_ship_and_station_invbroker_allows_get_self_inv_item_within_grace_window_gh1146()
{
    let ctx = test_ctx();
    let user_id = 1_u64;
    let char_id = 90_000_001_i64;
    let other_char = 95_114_602_i64;
    let sys_from = 30_000_144_u32;
    let sys_jita = 30_000_142_u32;
    let station_jita = 60_003_760_i64;
    let prev_ship_id = 1_897_301_923_i64;
    let active_ship_id = 1_897_147_421_i64;
    let other_ship_id = 1_897_999_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_from;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = active_ship_id;
        }
        let mut ch2 = snap.characters[&char_id].clone();
        ch2.character_id = other_char;
        ch2.user_id = other_char as u64;
        ch2.name = "Other Pilot".into();
        ch2.solar_system_id = sys_jita;
        ch2.station_id = None;
        ch2.structure_id = None;
        ch2.active_ship_id = other_ship_id;
        snap.characters.insert(other_char, ch2);

        for (item_id, type_id, owner_id, location_id) in [
            (prev_ship_id, 587_u32, char_id, i64::from(sys_from)),
            (active_ship_id, 606_u32, char_id, i64::from(sys_from)),
            (other_ship_id, 587_u32, other_char, i64::from(sys_jita)),
        ] {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id,
                    owner_id,
                    location_id,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });

    ctx.db
        .acquire_online_lock(user_id, 9101, Some(char_id), false, ctx.now_unix())
        .expect("acquire primary online lock");
    let _ = ctx.drain_notifications_for_char(char_id);

    let kw = HashMap::new();
    let mut sess = SessionState {
        session_id: 9101,
        user_id: Some(user_id),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(sys_from),
        ship_id: Some(active_ship_id),
        role: eve_machonet::service_roles::ROLE_GM_MASK,
        ..Default::default()
    };
    let mut other_sess = SessionState {
        session_id: 9102,
        user_id: Some(other_char as u64),
        character_id: Some(other_char),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(sys_jita),
        ship_id: Some(other_ship_id),
        ..Default::default()
    };

    // 1. Bind system invbroker in Perimeter, plus active ship and prior ship (floating in Perimeter).
    let bind_sys_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MachoBindObject",
            &[PyRep::Integer(i64::from(sys_from)), PyRep::None],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("bind system invbroker");
    let sys_bid = extract_macho_bound_id(&bind_sys_rep);

    let active_inv_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(sys_bid),
            "GetInventoryFromId",
            &[PyRep::Integer(active_ship_id), PyRep::Integer(1)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("GetInventoryFromId(active_ship_id, 1)");
    let active_bid = extract_macho_bound_id(&active_inv_rep);

    let prev_inv_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(sys_bid),
            "GetInventoryFromId",
            &[PyRep::Integer(prev_ship_id), PyRep::Integer(0)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("GetInventoryFromId(prev_ship_id, 0)");
    let prev_bid = extract_macho_bound_id(&prev_inv_rep);
    let prev_oid = ctx
        .verify_bound_object(&sess, prev_bid)
        .expect("prev_bid valid before /moveme")
        .oid_str;

    // 2. Teleport to Jita (/moveme 30000142) while prev_bid.GetSelfInvItem() is in flight.
    ctx.dispatch_rpc(
        &mut sess,
        "slash",
        None,
        "SlashCmd",
        &[PyRep::String(format!("/moveme {sys_jita}"))],
        &kw,
        Some("machoNet:26000"),
    )
    .await
    .expect("/moveme 30000142 succeeds");
    assert_eq!(sess.solar_system_id, Some(sys_jita));

    // Active ship binding remains live, while remote prev_bid was invalidated and disconnected.
    assert!(ctx.verify_bound_object(&sess, active_bid).is_ok());
    assert_eq!(
        ctx.verify_bound_object(&sess, prev_bid).unwrap_err().code,
        "InvalidBoundObject"
    );
    let disconnect_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(disconnect_notifs.iter().any(|n| {
        n.event == "OnMachoObjectDisconnect"
            && n.args.first() == Some(&PyRep::ascii_bytes(prev_oid.clone()))
    }));

    // 3. Already-queued prev_bid.GetSelfInvItem() arrives seconds after /moveme and succeeds.
    ctx.advance_secs(5);
    let prev_self = ctx
        .dispatch_rpc(
            &mut sess,
            "machoNet",
            Some(prev_bid),
            "GetSelfInvItem",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("in-flight prev_bid.GetSelfInvItem() succeeds within grace window (#1146)");
    assert_eq!(prev_self.dbrow_get("itemID"), Some(&PyRep::Integer(prev_ship_id)));

    // Active ship List() continues working as before.
    assert!(
        ctx.dispatch_rpc(
            &mut sess,
            "machoNet",
            Some(active_bid),
            "List",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .is_ok()
    );

    // Negative: remote List() on invalidated prev_bid and foreign character GetSelfInvItem() fail.
    let list_err = ctx
        .dispatch_rpc(
            &mut sess,
            "machoNet",
            Some(prev_bid),
            "List",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .unwrap_err();
    assert_eq!(list_err.code, "InvalidBoundObject");

    let other_err = ctx
        .dispatch_rpc(
            &mut other_sess,
            "machoNet",
            Some(prev_bid),
            "GetSelfInvItem",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .unwrap_err();
    assert_eq!(other_err.code, "InvalidBoundObject");

    // 4. Dock in Jita 4-4, bind station invbroker, /moveme out into space -> station GetSelfInvItem
    //    succeeds during grace window while station List() fails and explicit release purges it.
    ctx.db.dock_character(char_id, station_jita, false).expect("dock in Jita 4-4");
    sess.station_id = Some(station_jita);
    let bind_sta_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MachoBindObject",
            &[PyRep::Integer(station_jita), PyRep::None],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("bind station invbroker");
    let sta_bid = extract_macho_bound_id(&bind_sta_rep);
    let sta_oid = ctx
        .verify_bound_object(&sess, sta_bid)
        .expect("station bound valid while docked")
        .oid_str;

    ctx.dispatch_rpc(
        &mut sess,
        "slash",
        None,
        "SlashCmd",
        &[PyRep::String(format!("/moveme {sys_from}"))],
        &kw,
        Some("machoNet:26000"),
    )
    .await
    .expect("/moveme from station into Perimeter space");
    assert_eq!(sess.station_id, None);
    assert_eq!(
        ctx.verify_bound_object(&sess, sta_bid).unwrap_err().code,
        "InvalidBoundObject"
    );

    let sta_self = ctx
        .dispatch_rpc(
            &mut sess,
            "machoNet",
            Some(sta_bid),
            "GetSelfInvItem",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("in-flight station GetSelfInvItem() succeeds within grace window");
    assert_eq!(sta_self.dbrow_get("itemID"), Some(&PyRep::Integer(station_jita)));

    let sta_list_err = ctx
        .dispatch_rpc(
            &mut sess,
            "machoNet",
            Some(sta_bid),
            "List",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .unwrap_err();
    assert_eq!(sta_list_err.code, "InvalidBoundObject");

    // Client release notification clears station grace immediately; prev_bid expires after 31s.
    ctx.dispatch_rpc(
        &mut sess,
        "machoNet",
        None,
        "ClientHasReleasedTheseObjects",
        &[PyRep::ascii_bytes(sta_oid)],
        &kw,
        Some("machoNet:26000"),
    )
    .await
    .expect("ClientHasReleasedTheseObjects");
    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess,
            "machoNet",
            Some(sta_bid),
            "GetSelfInvItem",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .unwrap_err()
        .code,
        "InvalidBoundObject"
    );

    ctx.advance_secs(31);
    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess,
            "machoNet",
            Some(prev_bid),
            "GetSelfInvItem",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .unwrap_err()
        .code,
        "InvalidBoundObject"
    );
}

#[test]
fn issue_1290_ship_bay_targets_foreign_bay_auth_and_station_fuel_redirect() {
    use crate::services::inventory_fitting::fitting_containers_ships::{
        set_ship_access_config_snap, ShipAccessConfig,
    };
    use eve_db::FleetRecord;

    let owner_id = 95_900_001i64;
    let mate_id = 95_900_002i64;
    let owner_corp = 98_900_001i64;
    let mate_corp = 98_900_002i64;
    let station_id = 60_003_760i64;
    let keepstar_id = 1_030_900_000_001i64;
    let owner_titan = 1_000_959_001i64;
    let mate_ship = 1_000_959_002i64;
    let fuel_item = 1_000_959_010i64;
    let mate_station_item = 1_000_959_011i64;
    let mate_owned_bay_item = 1_000_959_012i64;
    let box_id = 1_000_959_013i64;
    let titan_tid = 999_901u32;

    let ctx = {
        let mut sde_mut = (*shared_sde()).clone();
        let mut titan = sde_mut.types[&587].clone();
        titan.type_id = titan_tid;
        titan.group_id = 30;
        titan.category_id = 6;
        titan.name = "Synthetic Keepstar Titan".into();
        titan.default_attributes.insert(908, 1_000_000.0);
        titan.default_attributes.insert(912, 10_000.0);
        titan.default_attributes.insert(1549, 50_000.0);
        sde_mut.types.insert(titan_tid, titan);
        let c = ServerContext::with_sde_and_repository(Arc::new(sde_mut), Arc::new(WorldRepository::with_demo_seed()));
        c.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
        c
    };
    let _ = ctx.db;

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().cloned().expect("seed char");
        let mut owner = base_ch.clone();
        owner.character_id = owner_id;
        owner.user_id = owner_id as u64;
        owner.name = "TitanOwner".into();
        owner.corporation_id = owner_corp;
        owner.station_id = Some(station_id);
        owner.structure_id = None;
        owner.active_ship_id = owner_titan;
        snap.characters.insert(owner_id, owner);

        let mut mate = base_ch;
        mate.character_id = mate_id;
        mate.user_id = mate_id as u64;
        mate.name = "TitanMate".into();
        mate.corporation_id = mate_corp;
        mate.station_id = Some(station_id);
        mate.structure_id = None;
        mate.active_ship_id = mate_ship;
        snap.characters.insert(mate_id, mate);

        snap.structures.insert(
            keepstar_id,
            UpwellStructureRecord {
                structure_id: keepstar_id,
                type_id: 35834,
                owner_corp_id: owner_corp,
                solar_system_id: 30_000_142,
                name: "Test Keepstar".into(),
                state: "ShieldVulnerable".into(),
                position: [0.0, 0.0, 0.0],
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        snap.fleets.insert(
            777_901,
            FleetRecord {
                fleet_id: 777_901,
                boss_id: owner_id,
                members: vec![owner_id, mate_id],
                ..Default::default()
            },
        );

        let mk = |item_id, type_id, oid, loc, flag, qty, singleton| InventoryItemRecord {
            item_id,
            type_id,
            owner_id: oid,
            location_id: loc,
            flag,
            quantity: qty,
            singleton,
            version: 1,
            ..Default::default()
        };
        snap.items.insert(owner_titan, mk(owner_titan, titan_tid, owner_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(mate_ship, mk(mate_ship, 587, mate_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(fuel_item, mk(fuel_item, 16274, owner_id, station_id, InventoryFlag::Hangar, 50, false));
        snap.items.insert(mate_station_item, mk(mate_station_item, 34, mate_id, station_id, InventoryFlag::Hangar, 10, false));
        snap.items.insert(mate_owned_bay_item, mk(mate_owned_bay_item, 34, mate_id, owner_titan, InventoryFlag::FleetHangar, 10, false));
        snap.items.insert(box_id, mk(box_id, 3293, owner_id, station_id, InventoryFlag::Hangar, 1, true));
    });
    ctx.db.with_mut_snapshot(|snap| {
        set_ship_access_config_snap(
            snap,
            owner_titan,
            &ShipAccessConfig {
                allow_fleet_smb: true,
                allow_corp_smb: false,
                allow_fleet_hangar: true,
                allow_corp_hangar: false,
                password: Some("pw123".into()),
            },
            0,
        )
        .unwrap();
    });

    let owner_sess = SessionState {
        user_id: Some(owner_id as u64),
        character_id: Some(owner_id),
        corporation_id: Some(owner_corp),
        station_id: Some(station_id),
        solar_system_id: Some(30_000_142),
        ship_id: Some(owner_titan),
        ..Default::default()
    };
    let mate_sess = SessionState {
        user_id: Some(mate_id as u64),
        character_id: Some(mate_id),
        corporation_id: Some(mate_corp),
        station_id: Some(station_id),
        solar_system_id: Some(30_000_142),
        ship_id: Some(mate_ship),
        ..Default::default()
    };

    // 1. resolve_inv_target_location: ship-only bay flags (155, 177, 179, 181, 182, 185) + NPC station 133 redirect to ship,
    //    while unbound 133 at an Upwell structure stays targeted at the structure's own fuel bay.
    let station_bound = bound_invbroker(owner_id, station_id);
    for bay_flag in [133u16, 155, 177, 179, 181, 182, 185] {
        assert_eq!(
            resolve_inv_target_location(
                &ctx,
                &owner_sess,
                Some(&station_bound),
                &[PyRep::Integer(fuel_item), PyRep::Integer(station_id), PyRep::Integer(i64::from(bay_flag))],
                Some(station_id),
                Some(bay_flag),
                false,
            ),
            Some(owner_titan),
            "station-bound flag {bay_flag} must redirect onto ship"
        );
    }
    let ks_bound = bound_invbroker(owner_id, keepstar_id);
    assert_eq!(
        resolve_inv_target_location(
            &ctx,
            &owner_sess,
            Some(&ks_bound),
            &[PyRep::Integer(fuel_item), PyRep::Integer(keepstar_id), PyRep::Integer(133)],
            Some(keepstar_id),
            Some(133),
            false,
        ),
        Some(keepstar_id),
        "structure fuel bay flag 133 must NOT redirect onto ship"
    );
    assert_eq!(
        resolve_inv_target_location(
            &ctx,
            &owner_sess,
            Some(&ks_bound),
            &[PyRep::Integer(fuel_item), PyRep::Integer(keepstar_id), PyRep::Integer(155)],
            Some(keepstar_id),
            Some(155),
            false,
        ),
        Some(owner_titan),
        "structure-bound FleetHangar flag 155 must redirect onto ship"
    );

    // 2. Non-ship destinations reject ship-only bay flags with InvalidInventoryFlag.
    ctx.db.with_snapshot(|snap| {
        for flag_u16 in [155u16, 177, 179, 181, 182, 185] {
            for non_ship in [station_id, keepstar_id, box_id] {
                let err = validate_ship_only_bay_target_in_snap(&ctx, snap, non_ship, flag_u16).unwrap_err();
                assert_eq!(err.code, "InvalidInventoryFlag");
            }
        }
        assert!(validate_ship_only_bay_target_in_snap(&ctx, snap, owner_titan, 155).is_ok());

        // 3. recheck_invbroker_add_auth_in_snap blocks foreign Cargo (flag 5) and enforces ACL+password on foreign 155 even when item.owner_id == mate_id.
        let kw_empty = HashMap::new();
        let mut kw_pw = HashMap::new();
        kw_pw.insert("password".to_string(), PyRep::String("pw123".into()));

        let src_hangar = &snap.items[&mate_station_item];
        assert!(
            recheck_invbroker_add_auth_in_snap(&ctx, snap, &mate_sess, mate_id, src_hangar, Some(src_hangar), owner_titan, InventoryFlag::Cargo, &[], &kw_pw).is_err(),
            "foreign ship Cargo (flag 5) must be blocked"
        );
        let dep_no_pw = recheck_invbroker_add_auth_in_snap(&ctx, snap, &mate_sess, mate_id, src_hangar, Some(src_hangar), owner_titan, InventoryFlag::FleetHangar, &[], &kw_empty).unwrap_err();
        assert_eq!(dep_no_pw.code, "FleetHangarAccessDenied");
        assert!(recheck_invbroker_add_auth_in_snap(&ctx, snap, &mate_sess, mate_id, src_hangar, Some(src_hangar), owner_titan, InventoryFlag::FleetHangar, &[], &kw_pw).is_ok());

        let src_bay = &snap.items[&mate_owned_bay_item];
        let ext_no_pw = recheck_invbroker_add_auth_in_snap(&ctx, snap, &mate_sess, mate_id, src_bay, Some(src_bay), station_id, InventoryFlag::Hangar, &[], &kw_empty).unwrap_err();
        assert_eq!(ext_no_pw.code, "FleetHangarAccessDenied");
        assert!(recheck_invbroker_add_auth_in_snap(&ctx, snap, &mate_sess, mate_id, src_bay, Some(src_bay), station_id, InventoryFlag::Hangar, &[], &kw_pw).is_ok());
    });
}

#[tokio::test]
async fn gh_1307_get_inventory_from_id_supports_docked_station_and_current_solar_system() {
    let ctx = test_ctx();
    let char_id = 95_910_001_i64;
    let other_char_id = 95_910_002_i64;
    let corp_id = 98_910_001_i64;
    let station_id = 60_002_956_i64; // Perimeter IX - Caldari Constructions Production Plant (SDE)
    let system_id = 30_000_144_u32; // Perimeter
    let remote_station_id = 60_003_760_i64; // Jita 4-4
    let remote_system_id = 30_000_142_u32; // Jita
    let venture_id = 1_000_959_901_i64;
    let veldspar_id = 1_000_959_902_i64;
    let foreign_ship_id = 1_000_959_903_i64;

    assert!(ctx.sde.stations.contains_key(&station_id), "Perimeter station 60002956 must exist in SDE");
    assert!(ctx.sde.stations.contains_key(&remote_station_id), "Jita station 60003760 must exist in SDE");
    assert!(ctx.sde.solar_systems.contains_key(&system_id), "Perimeter 30000144 must exist in SDE");
    assert!(ctx.sde.solar_systems.contains_key(&remote_system_id), "Jita 30000142 must exist in SDE");

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = "Miner1307".into();
        ch.corporation_id = corp_id;
        ch.station_id = Some(station_id);
        ch.structure_id = None;
        ch.solar_system_id = system_id;
        ch.active_ship_id = venture_id;
        snap.characters.insert(char_id, ch);

        let mk = |item_id, type_id, oid, loc, flag, qty, singleton| InventoryItemRecord {
            item_id,
            type_id,
            owner_id: oid,
            location_id: loc,
            flag,
            quantity: qty,
            singleton,
            version: 1,
            ..Default::default()
        };
        // Venture (32880) with Veldspar II-Grade (17470) in Mining Hold (134) + foreign ship at same station.
        snap.items.insert(venture_id, mk(venture_id, 32880, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(
            veldspar_id,
            mk(veldspar_id, 17470, char_id, venture_id, InventoryFlag::SpecializedOreHold, 250, false),
        );
        snap.items.insert(
            foreign_ship_id,
            mk(foreign_ship_id, 587, other_char_id, station_id, InventoryFlag::Hangar, 1, true),
        );
    });

    let mut docked_sess = SessionState {
        session_id: 1,
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        station_id: Some(station_id),
        solar_system_id: Some(system_id),
        ship_id: Some(venture_id),
        ..Default::default()
    };
    let b_station = bound_invbroker(char_id, station_id);

    // 1. GH #1307 core repro: GetInventoryFromId(60002956, 0) while docked at 60002956 must bind
    //    station inventory cleanly (no FakeItemNotFound) and expose canonical station DBRow + List.
    let st_inv_rep = handle(
        &ctx,
        &docked_sess,
        Some(&b_station),
        "invbroker",
        "GetInventoryFromId",
        &[PyRep::Integer(station_id), PyRep::Integer(0)],
        None,
    )
    .expect("GetInventoryFromId handled")
    .expect("docked GetInventoryFromId(station_id, 0) succeeds");
    let st_bid = st_inv_rep.as_i64().expect("sub-object bound id") as u64;
    let st_entry = ctx
        .verify_bound_object(&docked_sess, st_bid)
        .expect("station sub-object verifies for caller session");
    assert_eq!(st_entry.service, "invbroker");
    assert_eq!(st_entry.object_id, station_id);

    let st_self = handle(&ctx, &docked_sess, Some(&st_entry), "invbroker", "GetSelfInvItem", &[], None)
        .expect("GetSelfInvItem handled")
        .expect("station GetSelfInvItem succeeds");
    let st_info = &ctx.sde.stations[&station_id];
    let st_type = &ctx.sde.types[&st_info.station_type_id];
    let PyRep::DBRow { columns: ref st_cols, .. } = st_self else {
        panic!("expected DBRow from station GetSelfInvItem, got {st_self:?}");
    };
    let st_col = |k: &str| st_cols.iter().find(|(n, _)| n == k).map(|(_, v)| v);
    assert_eq!(st_col("itemID"), Some(&PyRep::Integer(station_id)));
    assert_eq!(st_col("typeID"), Some(&PyRep::Integer(i64::from(st_info.station_type_id))));
    assert_eq!(st_col("ownerID"), Some(&PyRep::Integer(st_info.owner_corporation_id)));
    assert_eq!(st_col("locationID"), Some(&PyRep::Integer(i64::from(st_info.solar_system_id))));
    assert_eq!(st_col("flagID"), Some(&PyRep::Integer(0)));
    assert_eq!(st_col("quantity"), Some(&PyRep::Integer(-1)));
    assert_eq!(st_col("groupID"), Some(&PyRep::Integer(i64::from(st_type.group_id))));
    assert_eq!(st_col("categoryID"), Some(&PyRep::Integer(i64::from(st_type.category_id))));
    assert_eq!(st_col("customInfo"), Some(&PyRep::String(st_info.name.clone())));

    let listed_station = crate::services::inventory_fitting::handle(
        &ctx,
        &mut docked_sess,
        Some(&st_entry),
        "invbroker",
        "List",
        &[PyRep::Integer(i64::from(InventoryFlag::Hangar as u16))],
        &HashMap::new(),
    )
    .await
    .expect("List handled")
    .expect("station List(Hangar) succeeds");
    let PyRep::ObjectEx { list: station_rows, .. } = listed_station else {
        panic!("expected CRowset ObjectEx from station List");
    };
    assert!(
        station_rows.iter().any(|r| r.dbrow_get("itemID") == Some(&PyRep::Integer(venture_id))),
        "station hangar List must include caller's Venture"
    );
    assert!(
        !station_rows.iter().any(|r| r.dbrow_get("itemID") == Some(&PyRep::Integer(foreign_ship_id))),
        "station hangar List must exclude foreign character's ship"
    );

    // 2. Venture & its Mining Hold (SpecializedOreHold = 134) remain accessible via GetInventoryFromId.
    let v_rep = handle(
        &ctx,
        &docked_sess,
        Some(&b_station),
        "invbroker",
        "GetInventoryFromId",
        &[PyRep::Integer(venture_id), PyRep::Integer(0)],
        None,
    )
    .unwrap()
    .expect("GetInventoryFromId(venture_id, 0) succeeds");
    let v_entry = ctx
        .verify_bound_object(&docked_sess, v_rep.as_i64().unwrap() as u64)
        .expect("venture sub-object verifies");
    let mut kw_ore = HashMap::new();
    kw_ore.insert("flag".to_string(), PyRep::Integer(134));
    let listed_ore = crate::services::inventory_fitting::handle(
        &ctx,
        &mut docked_sess,
        Some(&v_entry),
        "invbroker",
        "List",
        &[],
        &kw_ore,
    )
    .await
    .unwrap()
    .expect("List(flag=134) succeeds");
    let PyRep::ObjectEx { list: ore_rows, .. } = listed_ore else {
        panic!("expected CRowset ObjectEx from Venture Mining Hold List");
    };
    assert_eq!(ore_rows.len(), 1);
    assert_eq!(ore_rows[0].dbrow_get("itemID"), Some(&PyRep::Integer(veldspar_id)));

    // 3. Current solar system (30000144) is rejected while NPC-station docked (matching
    //    `authorize_invbroker_binding`) and binds cleanly in space / non-NPC-station contexts.
    assert_eq!(
        handle(
            &ctx,
            &docked_sess,
            Some(&b_station),
            "invbroker",
            "GetInventoryFromId",
            &[PyRep::Integer(i64::from(system_id)), PyRep::Integer(0)],
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InventoryNotAccessible"
    );

    let space_sess = SessionState {
        session_id: 2,
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        station_id: None,
        structure_id: None,
        solar_system_id: Some(system_id),
        ship_id: Some(venture_id),
        ..Default::default()
    };
    let b_sys = BoundObjectEntry {
        session_id: 2,
        ..bound_invbroker(char_id, i64::from(system_id))
    };
    let sys_space_rep = handle(
        &ctx,
        &space_sess,
        Some(&b_sys),
        "invbroker",
        "GetInventoryFromId",
        &[PyRep::Integer(i64::from(system_id)), PyRep::Integer(0)],
        None,
    )
    .unwrap()
    .expect("space GetInventoryFromId(current_solar_system_id, 0) succeeds");
    let sys_space_entry = ctx
        .verify_bound_object(&space_sess, sys_space_rep.as_i64().unwrap() as u64)
        .expect("space solar system sub-object verifies");
    let sys_self = handle(&ctx, &space_sess, Some(&sys_space_entry), "invbroker", "GetSelfInvItem", &[], None)
        .unwrap()
        .expect("solar system GetSelfInvItem succeeds");
    let sys_info = &ctx.sde.solar_systems[&system_id];
    let PyRep::DBRow { columns: ref sys_cols, .. } = sys_self else {
        panic!("expected DBRow from solar system GetSelfInvItem, got {sys_self:?}");
    };
    let sys_col = |k: &str| sys_cols.iter().find(|(n, _)| n == k).map(|(_, v)| v);
    assert_eq!(sys_col("itemID"), Some(&PyRep::Integer(i64::from(system_id))));
    assert_eq!(sys_col("typeID"), Some(&PyRep::Integer(5)));
    assert_eq!(sys_col("ownerID"), Some(&PyRep::Integer(1)));
    assert_eq!(sys_col("locationID"), Some(&PyRep::Integer(i64::from(sys_info.constellation_id))));
    assert_eq!(sys_col("flagID"), Some(&PyRep::Integer(0)));
    assert_eq!(sys_col("quantity"), Some(&PyRep::Integer(-1)));
    assert_eq!(sys_col("groupID"), Some(&PyRep::Integer(5)));
    assert_eq!(sys_col("categoryID"), Some(&PyRep::Integer(2)));
    assert_eq!(sys_col("customInfo"), Some(&PyRep::String(sys_info.name.clone())));

    // 4. Negative / fail-closed cases: remote station, remote solar system, undocked station,
    //    stale moniker, foreign ship (#1290 ACL preserved), and non-existent IDs.
    let err_code = |sess: &SessionState, bound: &BoundObjectEntry, target_id: i64| {
        handle(
            &ctx,
            sess,
            Some(bound),
            "invbroker",
            "GetInventoryFromId",
            &[PyRep::Integer(target_id), PyRep::Integer(0)],
            None,
        )
        .expect("handled")
        .expect_err("must reject invalid or unreachable inventory")
        .code
    };
    assert_eq!(err_code(&docked_sess, &b_station, remote_station_id), "InventoryNotAccessible");
    assert_eq!(err_code(&docked_sess, &b_station, i64::from(remote_system_id)), "InventoryNotAccessible");
    let b_stale_station = bound_invbroker(char_id, remote_station_id);
    assert_eq!(err_code(&docked_sess, &b_stale_station, station_id), "InventoryNotAccessible");
    assert_eq!(err_code(&docked_sess, &b_station, foreign_ship_id), "InventoryNotAccessible");
    assert_eq!(err_code(&space_sess, &b_sys, station_id), "InventoryNotAccessible");
    assert_eq!(err_code(&space_sess, &b_sys, i64::from(remote_system_id)), "InventoryNotAccessible");
    assert_eq!(err_code(&docked_sess, &b_station, 60_999_999), "FakeItemNotFound");
    assert_eq!(err_code(&docked_sess, &b_station, 199_888_777), "FakeItemNotFound");
}

#[tokio::test]
async fn issue_1371_removed_starter_ship_inventory_stub_and_guards() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_demo_char = 90_000_002_i64;
    let third_char = 95_371_003_i64;
    let station_id = 60_002_956_i64; // Perimeter IX - Caldari Constructions Production Plant (SDE)
    let remote_station_id = 60_003_760_i64; // Jita 4-4
    let system_id = 30_000_144_u32; // Perimeter
    let removed_starter_id = 100_000_001_i64;
    let venture_id = 1_897_814_543_i64;
    let capsule_id = 200_371_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&removed_starter_id);
        snap.items.retain(|_, item| item.location_id != removed_starter_id);
        snap.items.insert(
            venture_id,
            InventoryItemRecord {
                item_id: venture_id,
                type_id: 32_880, // Venture
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            capsule_id,
            InventoryItemRecord {
                item_id: capsule_id,
                type_id: 670, // Capsule
                owner_id: char_id,
                location_id: i64::from(system_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
            ch.structure_id = None;
            ch.solar_system_id = system_id;
            ch.active_ship_id = venture_id;
        }
        let mut ch3 = snap.characters[&char_id].clone();
        ch3.character_id = third_char;
        ch3.user_id = third_char as u64;
        ch3.name = "Foreign Observer".into();
        ch3.station_id = Some(station_id);
        ch3.solar_system_id = system_id;
        ch3.active_ship_id = 0;
        snap.characters.insert(third_char, ch3);
    });

    let mut docked_sess = SessionState {
        session_id: 13_710,
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(system_id),
        station_id: Some(station_id),
        ship_id: Some(venture_id),
        ..Default::default()
    };
    let space_venture_sess = SessionState {
        session_id: 13_711,
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(system_id),
        station_id: None,
        ship_id: Some(venture_id),
        ..Default::default()
    };
    let space_capsule_sess = SessionState {
        session_id: 13_712,
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(system_id),
        station_id: None,
        ship_id: Some(capsule_id),
        ..Default::default()
    };
    let space_no_ship_sess = SessionState {
        ship_id: None,
        ..space_capsule_sess.clone()
    };
    let unauth_docked_sess = SessionState {
        user_id: None,
        ..docked_sess.clone()
    };
    let third_docked_sess = SessionState {
        session_id: 13_713,
        user_id: Some(third_char as u64),
        character_id: Some(third_char),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(system_id),
        station_id: Some(station_id),
        ship_id: None,
        ..Default::default()
    };
    let other_demo_docked_sess = SessionState {
        session_id: 13_714,
        user_id: Some(2),
        character_id: Some(other_demo_char),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(system_id),
        station_id: Some(station_id),
        ship_id: Some(100_000_002),
        ..Default::default()
    };

    // 0. With `seed_demo = false` (non-demo production / AdminAccountConfig char `90_000_001`),
    //    removed starter ship IDs fail closed everywhere (`FakeItemNotFound` / `BindAuthorizationDenied`).
    assert!(!ctx.feature_flags_snapshot().seed_demo);
    let b_station = bound_invbroker(char_id, station_id);
    assert_eq!(
        resolve_missing_ship_stub_type_id_for_session(&ctx, &docked_sess, char_id, removed_starter_id),
        None
    );
    assert_eq!(
        resolve_missing_ship_stub_type_id_for_session(
            &ctx,
            &space_venture_sess,
            char_id,
            removed_starter_id,
        ),
        None
    );
    assert_eq!(
        resolve_missing_ship_stub_type_id(&ctx, char_id, removed_starter_id, true, true, None),
        None
    );
    assert_eq!(
        handle(
            &ctx,
            &docked_sess,
            Some(&b_station),
            "invbroker",
            "GetInventoryFromId",
            &[PyRep::Integer(removed_starter_id), PyRep::Integer(0)],
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "FakeItemNotFound"
    );
    assert_eq!(
        ctx.bind_object(&docked_sess, "invbroker", removed_starter_id)
            .unwrap_err()
            .code,
        "BindAuthorizationDenied" // rpc-lint: allow error code assertion
    );
    assert_eq!(
        handle(
            &ctx,
            &docked_sess,
            Some(&bound_invbroker(char_id, removed_starter_id)),
            "invbroker",
            "GetSelfInvItem",
            &[],
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "UnimplementedRPC"
    );

    ctx.feature_flags.write().unwrap().seed_demo = true;

    // 1. Stub resolver boundaries (`seed_demo = true`): Rifter (587) only for owner when docked or in non-capsule hull.
    assert_eq!(
        resolve_missing_ship_stub_type_id_for_session(&ctx, &docked_sess, char_id, removed_starter_id),
        Some(587)
    );
    assert_eq!(
        resolve_missing_ship_stub_type_id_for_session(
            &ctx,
            &space_venture_sess,
            char_id,
            removed_starter_id,
        ),
        Some(587)
    );
    assert_eq!(
        resolve_missing_ship_stub_type_id_for_session(
            &ctx,
            &space_capsule_sess,
            char_id,
            removed_starter_id,
        ),
        None
    );
    assert_eq!(
        resolve_missing_ship_stub_type_id_for_session(
            &ctx,
            &space_no_ship_sess,
            char_id,
            removed_starter_id,
        ),
        None
    );
    assert_eq!(
        resolve_missing_ship_stub_type_id_for_session(
            &ctx,
            &unauth_docked_sess,
            char_id,
            removed_starter_id,
        ),
        None
    );
    assert_eq!(
        resolve_missing_ship_stub_type_id_for_session(
            &ctx,
            &third_docked_sess,
            third_char,
            removed_starter_id,
        ),
        None
    );
    assert_eq!(
        resolve_missing_ship_stub_type_id_for_session(
            &ctx,
            &docked_sess,
            char_id,
            100_000_002,
        ),
        None
    );
    assert_eq!(
        resolve_missing_ship_stub_type_id_for_session(
            &ctx,
            &docked_sess,
            char_id,
            199_999_999,
        ),
        None
    );
    assert_eq!(
        resolve_missing_ship_stub_type_id(&ctx, char_id, removed_starter_id, true, true, None),
        Some(587)
    );

    // 2. Docked `GetInventoryFromId(100_000_001, 0)` + `GetSelfInvItem` + `GetAvailableTurretSlots` + `List`.
    let docked_inv_rep = handle(
        &ctx,
        &docked_sess,
        Some(&b_station),
        "invbroker",
        "GetInventoryFromId",
        &[PyRep::Integer(removed_starter_id), PyRep::Integer(0)],
        None,
    )
    .expect("GetInventoryFromId handled")
    .expect("docked GetInventoryFromId(100_000_001, 0) must not fail with FakeItemNotFound");
    let stub_bid = docked_inv_rep
        .as_i64()
        .filter(|&bid| bid > 0)
        .expect("stub bound id") as u64;
    let verified_entry = ctx
        .verify_bound_object(&docked_sess, stub_bid)
        .expect("docked stub sub-object must pass authorize_invbroker_binding");
    assert_eq!(verified_entry.service, "invbroker");
    assert_eq!(verified_entry.object_id, removed_starter_id);

    // Foreign and in-space-capsule sessions cannot verify or bind the removed starter ship stub.
    assert_eq!(
        ctx.verify_bound_object(&third_docked_sess, stub_bid)
            .unwrap_err()
            .code,
        "BoundObjectOwnershipViolation"
    );
    assert_eq!(
        ctx.bind_object(&third_docked_sess, "invbroker", removed_starter_id)
            .unwrap_err()
            .code,
        "BindAuthorizationDenied" // rpc-lint: allow error code assertion
    );
    assert_eq!(
        ctx.bind_object(&space_capsule_sess, "invbroker", removed_starter_id)
            .unwrap_err()
            .code,
        "BindAuthorizationDenied" // rpc-lint: allow error code assertion
    );
    assert_eq!(
        ctx.bind_object(&docked_sess, "invbroker", 199_999_999)
            .unwrap_err()
            .code,
        "BindAuthorizationDenied" // rpc-lint: allow error code assertion
    );

    // `should_invalidate_bound_object`: survives non-capsule `"undock"` / `"dock"`, invalidates on
    // in-space `"ship_loss"` into a capsule.
    ctx.invalidate_bound_objects_for_session_transition(char_id, system_id, None, "undock");
    let mut undocked_same_sess = docked_sess.clone();
    undocked_same_sess.station_id = None;
    assert!(ctx.verify_bound_object(&undocked_same_sess, stub_bid).is_ok());

    ctx.invalidate_bound_objects_for_session_transition(
        char_id,
        system_id,
        Some(station_id),
        "dock",
    );
    assert!(ctx.verify_bound_object(&docked_sess, stub_bid).is_ok());

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.active_ship_id = capsule_id;
        }
    });
    ctx.invalidate_bound_objects_for_session_transition(char_id, system_id, None, "ship_loss");
    assert_eq!(
        ctx.verify_bound_object(&docked_sess, stub_bid)
            .unwrap_err()
            .code,
        "InvalidBoundObject"
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
            ch.active_ship_id = venture_id;
        }
    });

    let b_stub = bound_invbroker(char_id, removed_starter_id);
    let self_row = handle(
        &ctx,
        &docked_sess,
        Some(&b_stub),
        "invbroker",
        "GetSelfInvItem",
        &[],
        None,
    )
    .expect("GetSelfInvItem handled")
    .expect("docked GetSelfInvItem on removed starter ship stub");
    assert_eq!(self_row.dbrow_get("itemID"), Some(&PyRep::Integer(removed_starter_id)));
    assert_eq!(self_row.dbrow_get("typeID"), Some(&PyRep::Integer(587)));
    assert_eq!(self_row.dbrow_get("ownerID"), Some(&PyRep::Integer(char_id)));
    assert_eq!(self_row.dbrow_get("locationID"), Some(&PyRep::Integer(station_id)));
    assert_eq!(self_row.dbrow_get("flagID"), Some(&PyRep::Integer(0)));
    assert_eq!(self_row.dbrow_get("quantity"), Some(&PyRep::Integer(-1)));
    assert_eq!(self_row.dbrow_get("groupID"), Some(&PyRep::Integer(25)));
    assert_eq!(self_row.dbrow_get("categoryID"), Some(&PyRep::Integer(6)));
    assert_eq!(self_row.dbrow_get("customInfo"), Some(&PyRep::String("Rifter".into())));

    let free_turrets = handle(
        &ctx,
        &docked_sess,
        Some(&b_stub),
        "invbroker",
        "GetAvailableTurretSlots",
        &[],
        None,
    )
    .expect("GetAvailableTurretSlots handled")
    .expect("docked GetAvailableTurretSlots on removed starter ship stub");
    assert_eq!(free_turrets, PyRep::Integer(0));

    let listed = crate::services::inventory_fitting::handle(
        &ctx,
        &mut docked_sess,
        Some(&b_stub),
        "invbroker",
        "List",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("List handled")
    .expect("List on removed starter ship stub returns empty CRowset");
    let PyRep::ObjectEx { list: rows, .. } = listed else {
        panic!("expected CRowset ObjectEx from List, got {listed:?}");
    };
    assert!(rows.is_empty());

    // Stale remote station `invbroker` moniker (`!at_location`) is rejected for removed starter ship stub.
    let b_remote_station = bound_invbroker(char_id, remote_station_id);
    assert_eq!(
        handle(
            &ctx,
            &docked_sess,
            Some(&b_remote_station),
            "invbroker",
            "GetInventoryFromId",
            &[PyRep::Integer(removed_starter_id), PyRep::Integer(0)],
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InventoryNotAccessible"
    );

    // 3. Undocked in non-capsule Venture also resolves `100_000_001` cleanly with solar-system location.
    let b_sys = bound_invbroker(char_id, i64::from(system_id));
    assert!(
        handle(
            &ctx,
            &space_venture_sess,
            Some(&b_sys),
            "invbroker",
            "GetInventoryFromId",
            &[PyRep::Integer(removed_starter_id), PyRep::Integer(0)],
            None,
        )
        .unwrap()
        .is_ok()
    );
    let space_self_row = handle(
        &ctx,
        &space_venture_sess,
        Some(&b_stub),
        "invbroker",
        "GetSelfInvItem",
        &[],
        None,
    )
    .unwrap()
    .expect("in-space non-capsule GetSelfInvItem on removed starter ship stub");
    assert_eq!(
        space_self_row.dbrow_get("locationID"),
        Some(&PyRep::Integer(i64::from(system_id)))
    );

    // 4. Negative / security guards:
    //    (a) In-space capsule right after ship loss still raises `FakeItemNotFound` (`abyssal_real_client_sync` HoldTracker).
    let cap_inv_err = handle(
        &ctx,
        &space_capsule_sess,
        Some(&b_sys),
        "invbroker",
        "GetInventoryFromId",
        &[PyRep::Integer(removed_starter_id), PyRep::Integer(0)],
        None,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(cap_inv_err.code, "FakeItemNotFound");
    assert_eq!(
        handle(&ctx, &space_capsule_sess, Some(&b_stub), "invbroker", "GetSelfInvItem", &[], None)
            .unwrap()
            .unwrap_err()
            .code,
        "UnimplementedRPC"
    );

    //    (b) Foreign / other demo characters cannot obtain `100_000_001` stub when missing.
    for (foreign_sess, foreign_char) in [
        (&third_docked_sess, third_char),
        (&other_demo_docked_sess, other_demo_char),
    ] {
        let foreign_bound = bound_invbroker(foreign_char, station_id);
        let err = handle(
            &ctx,
            foreign_sess,
            Some(&foreign_bound),
            "invbroker",
            "GetInventoryFromId",
            &[PyRep::Integer(removed_starter_id), PyRep::Integer(0)],
            None,
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(err.code, "FakeItemNotFound");
        let foreign_stub = bound_invbroker(foreign_char, removed_starter_id);
        assert_eq!(
            handle(&ctx, foreign_sess, Some(&foreign_stub), "invbroker", "GetSelfInvItem", &[], None)
                .unwrap()
                .unwrap_err()
                .code,
            "UnimplementedRPC"
        );
    }

    //    (c) Bogus IDs remain rejected with `FakeItemNotFound`, and unauthenticated callers with `Unauthorized`.
    for bogus_id in [100_000_099_i64, 199_999_999, 999_999] {
        let err = handle(
            &ctx,
            &docked_sess,
            Some(&b_station),
            "invbroker",
            "GetInventoryFromId",
            &[PyRep::Integer(bogus_id), PyRep::Integer(0)],
            None,
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(err.code, "FakeItemNotFound");
    }
    assert_eq!(
        handle(
            &ctx,
            &unauth_docked_sess,
            Some(&b_station),
            "invbroker",
            "GetInventoryFromId",
            &[PyRep::Integer(removed_starter_id), PyRep::Integer(0)],
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "Unauthorized"
    );

    //    (d) If `100_000_001` exists and is owned by someone else elsewhere, the stub never bypasses ACLs.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            removed_starter_id,
            InventoryItemRecord {
                item_id: removed_starter_id,
                type_id: 587,
                owner_id: third_char,
                location_id: remote_station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        resolve_missing_ship_stub_type_id_for_session(
            &ctx,
            &docked_sess,
            char_id,
            removed_starter_id,
        ),
        None
    );
    assert_eq!(
        handle(
            &ctx,
            &docked_sess,
            Some(&b_station),
            "invbroker",
            "GetInventoryFromId",
            &[PyRep::Integer(removed_starter_id), PyRep::Integer(0)],
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InventoryNotAccessible"
    );

    //    (e) Non-item entity-table collisions (`structures`, `characters`, `corporations`, `alliances`,
    //        `wars`, `fleets`) block stub synthesis even when `seed_demo = true`.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&removed_starter_id);
    });
    assert_eq!(
        resolve_missing_ship_stub_type_id_for_session(&ctx, &docked_sess, char_id, removed_starter_id),
        Some(587)
    );

    // Check every non-item entity table independently.
    let sample_char = ctx.db.with_snapshot(|snap| snap.characters[&char_id].clone());
    for table in ["structures", "characters", "corporations", "alliances", "wars", "fleets"] {
        ctx.db.with_mut_snapshot(|snap| match table {
            "structures" => {
                snap.structures.insert(removed_starter_id, UpwellStructureRecord::default());
            }
            "characters" => {
                let mut ch = sample_char.clone();
                ch.character_id = removed_starter_id;
                snap.characters.insert(removed_starter_id, ch);
            }
            "corporations" => {
                snap.corporations.insert(removed_starter_id, CorporationRecord::default());
            }
            "alliances" => {
                snap.alliances.insert(removed_starter_id, eve_db::AllianceRecord::default());
            }
            "wars" => {
                snap.wars.insert(removed_starter_id, eve_db::WarRecord::default());
            }
            "fleets" => {
                snap.fleets.insert(removed_starter_id, eve_db::FleetRecord::default());
            }
            _ => unreachable!(),
        });
        assert_eq!(
            resolve_missing_ship_stub_type_id_for_session(
                &ctx,
                &docked_sess,
                char_id,
                removed_starter_id,
            ),
            None,
            "collision in {table} must block stub synthesis"
        );
        assert_eq!(
            ctx.bind_object(&docked_sess, "invbroker", removed_starter_id)
                .unwrap_err()
                .code,
            "BindAuthorizationDenied", // rpc-lint: allow error code assertion
            "collision in {table} must fail bind authorization"
        );
        let expected_inv_err = if table == "structures" {
            "InventoryNotAccessible"
        } else {
            "FakeItemNotFound"
        };
        assert_eq!(
            handle(
                &ctx,
                &docked_sess,
                Some(&b_station),
                "invbroker",
                "GetInventoryFromId",
                &[PyRep::Integer(removed_starter_id), PyRep::Integer(0)],
                None,
            )
            .unwrap()
            .unwrap_err()
            .code,
            expected_inv_err
        );
        ctx.db.with_mut_snapshot(|snap| match table {
            "structures" => {
                snap.structures.remove(&removed_starter_id);
            }
            "characters" => {
                snap.characters.remove(&removed_starter_id);
            }
            "corporations" => {
                snap.corporations.remove(&removed_starter_id);
            }
            "alliances" => {
                snap.alliances.remove(&removed_starter_id);
            }
            "wars" => {
                snap.wars.remove(&removed_starter_id);
            }
            "fleets" => {
                snap.fleets.remove(&removed_starter_id);
            }
            _ => unreachable!(),
        });
    }
}

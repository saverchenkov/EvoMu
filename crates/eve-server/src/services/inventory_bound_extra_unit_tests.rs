//! Extra branch-coverage and mutation-killing unit tests for `inventory_bound.rs`.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::combat_crimewatch::with_mut_combat_runtime;
use crate::services::inventory_bound::{
    get_effective_corp_roles, get_effective_corp_roles_at_location, handle, handle_i2_get_item,
    handle_i2_is_primed, is_inventory_location_colocated, item_to_packed_cells,
    validate_multiadd_destination, validate_multiadd_structure_fuel_transfers,
};
use eve_db::clock::VirtualServerClock;
use eve_db::{
    CorpMemberRoleAssignment, CorporationRecord, InventoryFlag, InventoryItemRecord,
    UpwellStructureRecord, WorldRepository,
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
    Arc::clone(SDE.get_or_init(|| {
        let mut sde = SdeRegistry::with_canonical_seed();
        let mut t = sde.types.values().next().cloned().expect("canonical seed");
        t.type_id = 991_484;
        t.category_id = 7;
        t.group_id = 53;
        t.default_attributes.clear();
        t.effect_ids = vec![12, 42];
        sde.types.insert(991_484, t);
        Arc::new(sde)
    }))
}

fn test_ctx() -> ServerContext {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_805_000_000)));
    ctx
}

fn i2_svc_name() -> String {
    String::from_utf8(vec![b'i', b'2']).unwrap()
}

fn make_bound(service: &str, char_id: i64, object_id: i64) -> BoundObjectEntry {
    BoundObjectEntry {
        bound_id: 10,
        session_id: 1,
        user_id: char_id as u64,
        character_id: char_id,
        service: service.into(),
        object_id,
        oid_str: format!("N=10:{object_id}"),
        ref_id: 1,
    }
}

fn call_invbroker(
    ctx: &ServerContext,
    session: &SessionState,
    bound: Option<&BoundObjectEntry>,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle(ctx, session, bound, "invbroker", method, args, Some("trace"))
}

fn call_i2(
    ctx: &ServerContext,
    session: &SessionState,
    bound: Option<&BoundObjectEntry>,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let svc = i2_svc_name();
    handle(ctx, session, bound, &svc, method, args, Some("trace"))
}

#[test]
fn routing_i2_bound_fallback_and_item_to_packed_cells_branches() {
    let ctx = test_ctx();
    let char_id = 96_100_001_i64;
    let station_id = 60_003_760_i64;
    let stack_item_id = 1_600_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().unwrap();
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.station_id = Some(station_id);
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            stack_item_id,
            InventoryItemRecord {
                item_id: stack_item_id,
                type_id: 34,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 42,
                singleton: false,
                is_blueprint_copy: false,
                ..Default::default()
            },
        );
        snap.item_custom_names
            .insert(stack_item_id, "Named Tritanium".into());
    });

    let item_rec = ctx.db.get_item(stack_item_id).unwrap();
    let cells = item_to_packed_cells(&ctx, &item_rec);
    assert_eq!(cells[5], PyRep::Integer(42));
    assert_eq!(cells[8], PyRep::String("Named Tritanium".into()));

    let gm_sess = SessionState {
        session_id: 1,
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        station_id: Some(station_id),
        role: 0x7FFF_FFFF_FFFF_FFFF,
        ..Default::default()
    };
    assert!(handle(&ctx, &gm_sess, None, "dogmaIM", "GetAllInfo", &[], None).is_none());
    assert!(call_invbroker(&ctx, &gm_sess, None, "List", &[]).is_none());
    assert!(call_i2(&ctx, &gm_sess, None, "List", &[]).is_none());

    // i2 without GM role via handle() returns GMRoleRequired
    let mut non_gm = gm_sess.clone();
    non_gm.role = 0;
    assert_eq!(
        call_i2(&ctx, &non_gm, None, "IsPrimed", &[PyRep::Integer(stack_item_id)])
            .unwrap()
            .unwrap_err()
            .code,
        "GMRoleRequired"
    );
    assert_eq!(
        handle_i2_get_item(&ctx, &non_gm, Some(stack_item_id))
            .unwrap_err()
            .code,
        "GMRoleRequired"
    );
    assert_eq!(
        handle_i2_get_item(&ctx, &gm_sess, None).unwrap_err().code,
        "InvalidArgument"
    );

    // i2 and invbroker without character_id fail require_character
    let mut no_char_gm = gm_sess.clone();
    no_char_gm.character_id = None;
    assert!(call_i2(&ctx, &no_char_gm, None, "IsPrimed", &[]).unwrap().is_err());
    assert!(call_invbroker(&ctx, &no_char_gm, None, "GetSelfInvItem", &[])
        .unwrap()
        .is_err());

    // i2 target_id resolved from bound_entry when args is empty
    let i2_bound = make_bound(&i2_svc_name(), char_id, stack_item_id);
    assert_eq!(
        call_i2(&ctx, &gm_sess, Some(&i2_bound), "IsPrimed", &[])
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    let stack_kv = call_i2(&ctx, &gm_sess, Some(&i2_bound), "GetItem", &[])
        .unwrap()
        .unwrap();
    assert_eq!(stack_kv.dict_get("singleton").and_then(|v| v.as_i64()), Some(0));
    assert_eq!(stack_kv.dict_get("quantity").and_then(|v| v.as_i64()), Some(42));
    assert_eq!(handle_i2_is_primed(&ctx, &gm_sess, Some(station_id)).unwrap(), PyRep::Bool(true));
}

#[tokio::test]
async fn invbroker_turret_slots_shipping_storage_open_can_and_corp_office_branches() {
    let ctx = test_ctx();
    let char_id = 96_100_010_i64;
    let corp_id = 98_100_010_i64;
    let sys_id = 30_000_142_u32;
    let station_id = 60_003_760_i64;
    let ship_id = 1_610_001_i64;
    let turret_mod_id = 1_610_002_i64;
    let office_id = 1_610_003_i64;
    let corp_can_in_office = 1_610_004_i64;
    let open_space_can = 1_610_005_i64;
    let auth_storage_id = 1_610_006_i64;

    let turret_type_id = 991_484_u32;
    let container_with_cap = ctx
        .sde
        .types
        .values()
        .find(|t| t.capacity > 0.0)
        .map(|t| t.type_id)
        .unwrap_or(23);

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().unwrap();
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.corporation_id = corp_id;
        ch.station_id = Some(station_id);
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        for (id, tid, owner, loc, flag) in [
            (ship_id, 587, char_id, station_id, InventoryFlag::Hangar),
            (turret_mod_id, turret_type_id, char_id, ship_id, InventoryFlag::HiSlot0),
            (office_id, 27, corp_id, station_id, InventoryFlag::None),
            (corp_can_in_office, container_with_cap, corp_id, office_id, InventoryFlag::CorpSAG1),
            (open_space_can, 23, 0, sys_id as i64, InventoryFlag::None),
            (auth_storage_id, container_with_cap, 0, sys_id as i64, InventoryFlag::None),
        ] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: owner,
                    location_id: loc,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });

    let docked_sess = SessionState {
        session_id: 1,
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        station_id: Some(station_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    // GetAvailableTurretSlots with 1 fitted turret on Rifter (3 - 1 = 2)
    let b_ship = make_bound("invbroker", char_id, ship_id);
    assert_eq!(
        call_invbroker(&ctx, &docked_sess, Some(&b_ship), "GetAvailableTurretSlots", &[])
            .unwrap()
            .unwrap(),
        PyRep::Integer(2)
    );

    // GetAvailableTurretSlots when neither bound item nor session.ship_id exists -> 0
    let mut no_ship_sess = docked_sess.clone();
    no_ship_sess.ship_id = None;
    let b_unknown = make_bound("invbroker", char_id, 999_888_777);
    assert_eq!(
        call_invbroker(&ctx, &no_ship_sess, Some(&b_unknown), "GetAvailableTurretSlots", &[])
            .unwrap()
            .unwrap(),
        PyRep::Integer(0)
    );

    // GetSelfInvItem for unowned/unknown object_id -> UnimplementedRPC
    assert_eq!(
        call_invbroker(&ctx, &docked_sess, Some(&b_unknown), "GetSelfInvItem", &[])
            .unwrap()
            .unwrap_err()
            .code,
        "UnimplementedRPC"
    );

    let b_sta = make_bound("invbroker", char_id, station_id);
    assert_eq!(
        call_invbroker(&ctx, &docked_sess, Some(&b_sta), "GetInventoryFromId", &[])
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArguments"
    );
    assert!(call_invbroker(
        &ctx,
        &docked_sess,
        Some(&b_sta),
        "GetInventoryFromId",
        &[PyRep::Integer(char_id)],
    )
    .unwrap()
    .is_ok());
    assert!(call_invbroker(
        &ctx,
        &docked_sess,
        Some(&b_sta),
        "GetInventoryFromId",
        &[PyRep::Integer(corp_can_in_office)],
    )
    .unwrap()
    .is_ok());
    assert_eq!(
        call_invbroker(
            &ctx,
            &docked_sess,
            Some(&b_sta),
            "GetInventory",
            &[PyRep::Integer(10_001)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "UnimplementedRPC"
    );

    // In-space GetInventoryFromId for open_space_can and authorized_shipping_storage within 5,000m
    let space_sess = SessionState {
        session_id: 1,
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let b_sys = make_bound("invbroker", char_id, sys_id as i64);
    assert!(call_invbroker(
        &ctx,
        &space_sess,
        Some(&b_sys),
        "GetInventoryFromId",
        &[PyRep::Integer(open_space_can)],
    )
    .unwrap()
    .is_ok());

    with_mut_combat_runtime(&ctx, |rt| {
        rt.authorized_shipping_storage.insert((auth_storage_id, char_id));
    });
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for (id, x) in [(ship_id, 0.0), (auth_storage_id, 4_000.0)] {
        let _ = sys_h
            .spawn_ball(Ball {
                id,
                type_id: 587,
                position: DVec3::new(x, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1.0e6,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
    }
    assert!(call_invbroker(
        &ctx,
        &space_sess,
        Some(&b_sys),
        "GetInventoryFromId",
        &[PyRep::Integer(auth_storage_id)],
    )
    .unwrap()
    .is_ok());
}

#[test]
fn colocation_multiadd_destination_and_structure_fuel_transfer_branches() {
    let ctx = test_ctx();
    let char_id = 96_100_020_i64;
    let corp_id = 98_100_020_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 1_620_001_i64;
    let struct_id = 1_030_000_000_500_i64;
    let destroyed_struct = 1_030_000_000_501_i64;
    let foreign_struct = 1_030_000_000_502_i64;
    let deed_item_id = 1_620_002_i64;
    let regular_item_id = 1_620_003_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().unwrap();
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.corporation_id = corp_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Fuel Corp".into();
                __corp.ticker = "FUEL".into();
                __corp.ceo_id = char_id;
                __corp.hq_station_id = Some(60_003_760);
                __corp },
        );
        snap.corp_role_assignments.insert(
            (corp_id, char_id),
            CorpMemberRoleAssignment {
                character_id: char_id,
                roles: 1,
                roles_at_hq: 2,
                ..Default::default()
            },
        );
        for (sid, owner, state) in [
            (struct_id, corp_id, "ShieldVulnerable"),
            (destroyed_struct, corp_id, "Destroyed"),
            (foreign_struct, 99_888_777, "ShieldVulnerable"),
        ] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: 35_832,
                    owner_corp_id: owner,
                    solar_system_id: sys_id,
                    name: format!("S-{sid}"),
                    state: state.into(),
                    position: [1_000.0, 0.0, 0.0],
                    fuel_blocks: 100,
                    quantum_core_installed: true,
                    ..Default::default()
                },
            );
        }
        for (id, tid, owner, loc, flag) in [
            (ship_id, 587, char_id, sys_id as i64, InventoryFlag::None),
            (deed_item_id, 56_201, corp_id, struct_id, InventoryFlag::StructureDeedBay),
            (regular_item_id, 34, char_id, struct_id, InventoryFlag::Hangar),
        ] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: owner,
                    location_id: loc,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });

    let (is_ceo, roles) = get_effective_corp_roles(&ctx, corp_id, char_id);
    assert!(is_ceo && roles >= 1);
    let _ = get_effective_corp_roles_at_location(&ctx, corp_id, char_id, Some(struct_id));

    let sess = SessionState {
        session_id: 1,
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    let mut pre_balls = HashMap::new();
    pre_balls.insert(ship_id, (DVec3::ZERO, 50.0));
    pre_balls.insert(struct_id, (DVec3::new(2_000.0, 0.0, 0.0), 1_000.0));

    ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        assert!(is_inventory_location_colocated(snap, &sess, ch, &pre_balls, struct_id));
        assert!(validate_multiadd_destination(snap, &sess, ch, char_id, destroyed_struct, true, &pre_balls).is_ok());
        assert_eq!(
            validate_multiadd_destination(snap, &sess, ch, char_id, destroyed_struct, false, &pre_balls)
                .unwrap_err()
                .code,
            "StructureDestroyed"
        );
        assert_eq!(
            validate_multiadd_destination(snap, &sess, ch, char_id, foreign_struct, false, &pre_balls)
                .unwrap_err()
                .code,
            "DestinationAccessDenied"
        );
        assert!(validate_multiadd_destination(snap, &sess, ch, char_id, ship_id, false, &pre_balls).is_ok());

        let deed = &snap.items[&deed_item_id];
        assert_eq!(
            validate_multiadd_structure_fuel_transfers(
                &ctx, snap, &sess, char_id, deed, ship_id, false, 0, sys_id as i64, ship_id, &pre_balls,
            )
            .unwrap_err()
            .code,
            "ItemLocked"
        );

        let reg_it = &snap.items[&regular_item_id];
        assert!(!validate_multiadd_structure_fuel_transfers(
            &ctx, snap, &sess, char_id, reg_it, ship_id, false, struct_id, sys_id as i64, ship_id, &pre_balls,
        )
        .unwrap());
    });
}

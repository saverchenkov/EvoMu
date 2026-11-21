//! Extra unit and mutation-killing tests for `services::inventory_fitting`.

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::inventory_fitting::{csprng_unit_f64, handle};
use eve_db::{
    corp_roles, CorporationRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord,
    WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "inventory_fitting_moves_and_stacks_unit_tests.rs"]
mod inventory_fitting_moves_and_stacks_unit_tests;

pub(super) fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

pub(super) fn mk_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
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
    }
}

pub(super) async fn call_add(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    handle(ctx, session, None, "invbroker", "Add", args, &HashMap::new())
        .await
        .unwrap()
}

pub(super) fn setup_inv_ctx() -> (ServerContext, SessionState, i64, i64, i64, i64) {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 96_600_001i64;
    let corp_id = 98_600_001i64;
    let station_id = 60_003_760i64;
    let ship_id = 96_600_100i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 1;
        ch.name = "InvExtraPilot".into();
        ch.corporation_id = corp_id;
        ch.station_id = Some(station_id);
        ch.structure_id = None;
        ch.solar_system_id = 30_000_142;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "InvExtra Corp".into();
                __corp.ticker = "IEXC".into();
                __corp.ceo_id = 96_699_999;
                __corp.members = [char_id, 96_699_999].into_iter().collect();
                __corp },
        );

        snap.items.insert(
            ship_id,
            mk_item(ship_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true),
        );

        for &sk in &[3300u32, 3302, 3318, 3392, 3426, 26252, 26253] {
            snap.skills.entry(char_id).or_default().insert(
                sk,
                TrainedSkillRecord {
                    type_id: sk,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
    });

    let mut session = SessionState::default();
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.corporation_id = Some(corp_id);
    session.station_id = Some(station_id);
    session.solar_system_id = Some(30_000_142);
    session.ship_id = Some(ship_id);
    (ctx, session, char_id, corp_id, station_id, ship_id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invbroker_add_argument_flag_quantity_and_corp_role_validations() {
    let (ctx, mut session, char_id, corp_id, station_id, ship_id) = setup_inv_ctx();
    let kw = HashMap::new();
    assert!((0.0..1.0).contains(&csprng_unit_f64(&ctx)));

    // 1. Unauthenticated GetItemDescriptor & Add rejected
    let mut unauth = SessionState::default();
    assert_eq!(
        handle(&ctx, &mut unauth, None, "invbroker", "GetItemDescriptor", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "Unauthorized"
    );
    assert_eq!(call_add(&ctx, &mut unauth, &[]).await.unwrap_err().code, "Unauthorized");

    // 2. Missing item_id -> InvalidArgument
    assert_eq!(call_add(&ctx, &mut session, &[]).await.unwrap_err().code, "InvalidArgument");

    // 3. Missing target location when session has no ship/station/structure -> InvalidLocation
    let mut bare_sess = SessionState::default();
    bare_sess.user_id = Some(1);
    bare_sess.character_id = Some(char_id);
    assert_eq!(
        call_add(&ctx, &mut bare_sess, &[PyRep::Integer(ship_id)])
            .await
            .unwrap_err()
            .code,
        "InvalidLocation"
    );

    // 4. Invalid inventory flag (999) and Fighter Bay (158) with non-fighter item (Tritanium 34)
    let trit_id = 96_600_201i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            trit_id,
            mk_item(trit_id, 34, char_id, station_id, InventoryFlag::Hangar, 100, false),
        );
    });
    for bad_flag in [999_i64, 158_i64] {
        assert_eq!(
            call_add(
                &ctx,
                &mut session,
                &[PyRep::Integer(trit_id), PyRep::Integer(ship_id), PyRep::Integer(bad_flag)],
            )
            .await
            .unwrap_err()
            .code,
            "InvalidInventoryFlag"
        );
    }

    // 5. InvalidQuantity (<= 0 or overflow i32)
    let mut bad_kw = HashMap::new();
    bad_kw.insert("quantity".into(), PyRep::Integer(0));
    assert_eq!(
        handle(
            &ctx,
            &mut session,
            None,
            "invbroker",
            "Add",
            &[PyRep::Integer(trit_id), PyRep::Integer(ship_id), PyRep::Integer(5)],
            &bad_kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidQuantity"
    );
    assert_eq!(
        call_add(
            &ctx,
            &mut session,
            &[
                PyRep::Integer(trit_id),
                PyRep::Integer(ship_id),
                PyRep::Integer(5),
                PyRep::Integer(i64::from(i32::MAX) + 10),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidQuantity"
    );

    // 6. Taking from CorpSAG1 (116) without division 1 take role -> CorpRoleDenied, then with role -> succeeds
    let corp_item_id = 96_600_202i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            corp_item_id,
            mk_item(corp_item_id, 34, corp_id, station_id, InventoryFlag::CorpSAG1, 50, false),
        );
    });
    assert_eq!(
        call_add(
            &ctx,
            &mut session,
            &[PyRep::Integer(corp_item_id), PyRep::Integer(ship_id), PyRep::Integer(5)],
        )
        .await
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.member_roles
                .insert(char_id, corp_roles::ROLE_DIRECTOR | (1u64 << 13));
        }
    });
    let bound = BoundObjectEntry {
        bound_id: 1,
        session_id: 1,
        user_id: 1,
        character_id: char_id,
        service: "invbroker".into(),
        object_id: ship_id,
        oid_str: "N=1:1".into(),
        ref_id: 1,
    };
    let moved_corp = handle(
        &ctx,
        &mut session,
        Some(&bound),
        "invbroker",
        "Add",
        &[PyRep::Integer(corp_item_id), PyRep::Integer(station_id), PyRep::Integer(5)],
        &kw,
    )
    .await
    .unwrap()
    .expect("director can take from CorpSAG1 into bound ship");
    assert_eq!(moved_corp.dict_get("locationID").and_then(|v| v.as_i64()), Some(ship_id));
}

#[test]
fn ship_hold_capacity_error_code_and_error_message_pin_all_bay_flags() {
    use crate::services::inventory_fitting::{ship_hold_capacity_error, ship_hold_capacity_error_code};

    assert_eq!(ship_hold_capacity_error_code(87), "NotEnoughDroneBaySpace");
    assert_eq!(ship_hold_capacity_error_code(158), "NotEnoughFighterBaySpace");
    for special_flag in [133_u16, 140, 151, 177, 179, 181, 182, 185] {
        assert_eq!(
            ship_hold_capacity_error_code(special_flag),
            "NotEnoughSpecialBaySpace"
        );
    }
    assert_eq!(ship_hold_capacity_error_code(5), "NotEnoughCargoSpace");

    let err_pos = ship_hold_capacity_error(87, 100.0, 40.0, 75.0);
    assert_eq!(err_pos.code, "NotEnoughDroneBaySpace");
    assert_eq!(err_pos.message, "volume=75, available=60");
    assert_eq!(
        err_pos.to_client_error("invbroker", "Add"),
        eve_proto::ClientError::User {
            key: eve_proto::UserErrorKey::NotEnoughDroneBaySpace,
            args: vec![
                ("volume".into(), PyRep::Real(75.0)),
                ("available".into(), PyRep::Real(60.0)),
            ],
        }
    );

    let err_clamped = ship_hold_capacity_error(5, 100.0, 125.0, 10.0);
    assert_eq!(err_clamped.code, "NotEnoughCargoSpace");
    assert_eq!(err_clamped.message, "volume=10, available=0");
    assert_eq!(
        err_clamped.to_client_error("invbroker", "Add"),
        eve_proto::ClientError::User {
            key: eve_proto::UserErrorKey::NotEnoughCargoSpace,
            args: vec![
                ("volume".into(), PyRep::Real(10.0)),
                ("available".into(), PyRep::Real(0.0)),
            ],
        }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_dispatch_split_loot_merge_version_idempotency_and_escape_bay_edge_cases() {
    let (mut ctx, mut session, char_id, corp_id, station_id, ship_id) = setup_inv_ctx();
    let kw = HashMap::new();
    let sys_id = 30_000_142_u32;

    // Seed synthetic SDE types for Battleship (Frigate Escape Bay attr 3020 = 1.0, capacity = 500.0)
    // + a Frigate (group 25, category 6) and a zero-drone-capacity shuttle so `hold_capacity <= 0.0` triggers cleanly.
    let bb_tid = 999_601_u32;
    let shuttle_tid = 999_602_u32;
    let rig_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 7 && t.default_attributes.get(&1547).copied() == Some(1.0))
        .map(|t| t.type_id)
        .unwrap_or(31_358);
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        let mut bb = sde_mut.types[&587].clone();
        bb.type_id = bb_tid;
        bb.group_id = 27; // Battleship
        bb.category_id = 6;
        bb.capacity = 500.0;
        bb.default_attributes.insert(3020, 1.0); // Frigate Escape Bay enabled
        bb.default_attributes.insert(eve_sde::attr_ids::DRONE_CAPACITY, 25.0);
        sde_mut.types.insert(bb_tid, bb);

        let mut sh = sde_mut.types[&587].clone();
        sh.type_id = shuttle_tid;
        sh.group_id = 31; // Shuttle (category 6)
        sh.category_id = 6;
        sh.capacity = 10.0;
        sh.default_attributes.insert(eve_sde::attr_ids::DRONE_CAPACITY, 0.0);
        sde_mut.types.insert(shuttle_tid, sh);
    }

    let container_id = 96_650_001_i64;
    let cont_child_id = 96_650_002_i64;
    let stack_a = 96_650_010_i64;
    let merge_src = 96_650_011_i64;
    let courier_item = 96_650_012_i64;
    let dyn_item = 96_650_013_i64;
    let muta_item = 96_650_014_i64;
    let merge_dest = 96_650_015_i64;
    let sag_item = 96_650_016_i64;
    let bb_id = 96_650_020_i64;
    let shuttle_id = 96_650_021_i64;
    let frig_ok = 96_650_022_i64;
    let frig_cargo_item = 96_650_023_i64;
    let frig_ammo_item = 96_650_024_i64;
    let frig_second = 96_650_025_i64;
    let frig_unass = 96_650_026_i64;
    let rig_on_bb = 96_650_027_i64;
    let drone_heavy = 96_650_030_i64;
    let corp_structure_id = 1_066_650_001_i64;

    // Pick a real drone type in canonical SDE
    let drone_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 18 && t.volume > 0.0)
        .map(|t| (t.type_id, t.volume))
        .unwrap_or((2454, 5.0));

    ctx.db.with_mut_snapshot(|snap| {
        // Station container + child for `GetContainerContents`
        snap.items.insert(
            container_id,
            mk_item(container_id, 23, char_id, station_id, InventoryFlag::Hangar, 1, true),
        );
        snap.items.insert(
            cont_child_id,
            mk_item(cont_child_id, 34, char_id, container_id, InventoryFlag::Cargo, 50, false),
        );

        // Stacks for split, expected_version, idempotency, owner rules, and merge guards
        snap.items.insert(
            stack_a,
            mk_item(stack_a, 34, char_id, station_id, InventoryFlag::Hangar, 20, false),
        );
        snap.items.insert(
            merge_src,
            mk_item(merge_src, 34, char_id, station_id, InventoryFlag::Hangar, 15, false),
        );
        snap.items.insert(
            merge_dest,
            mk_item(merge_dest, 34, char_id, ship_id, InventoryFlag::Cargo, 10, false),
        );

        // Blocked non-mergeable siblings already inside ship cargo (must NOT eat merge_src!)
        snap.items.insert(
            courier_item,
            mk_item(courier_item, 34, char_id, ship_id, InventoryFlag::Cargo, 5, false),
        );
        snap.item_custom_names.insert(courier_item, "courier_cargo:999".into());

        let mut di = mk_item(dyn_item, 34, char_id, ship_id, InventoryFlag::Cargo, 5, false);
        di.dynamic_attributes.insert(2, 1.0);
        snap.items.insert(dyn_item, di);

        let mut mi = mk_item(muta_item, 34, char_id, ship_id, InventoryFlag::Cargo, 5, false);
        mi.source_mutaplasmid_id = Some(49712);
        snap.items.insert(muta_item, mi);

        // CorpSAG1 item owned by corp -> moving into personal ship Cargo resets owner_id -> char_id
        snap.items.insert(
            sag_item,
            mk_item(sag_item, 34, corp_id, station_id, InventoryFlag::CorpSAG1, 12, false),
        );
        // Grant caller Director role so CorpSAG1 take succeeds
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.member_roles
                .insert(char_id, eve_db::corp_roles::ROLE_DIRECTOR);
        }

        // Battleship + Shuttle + Frigates + Rig
        snap.items.insert(
            bb_id,
            mk_item(bb_id, bb_tid, char_id, station_id, InventoryFlag::Hangar, 1, true),
        );
        snap.items.insert(
            shuttle_id,
            mk_item(shuttle_id, shuttle_tid, char_id, station_id, InventoryFlag::Hangar, 1, true),
        );
        snap.items.insert(
            frig_ok,
            mk_item(frig_ok, 587, char_id, station_id, InventoryFlag::Hangar, 1, true),
        );
        snap.items.insert(
            frig_ammo_item,
            mk_item(frig_ammo_item, 182, char_id, frig_ok, InventoryFlag::Cargo, 20, false), // category 8 charge allowed!
        );
        snap.items.insert(
            frig_cargo_item,
            mk_item(frig_cargo_item, 34, char_id, frig_ok, InventoryFlag::Cargo, 1, false), // non-charge cargo blocks
        );
        snap.items.insert(
            frig_second,
            mk_item(frig_second, 587, char_id, station_id, InventoryFlag::Hangar, 1, true),
        );
        snap.items.insert(
            frig_unass,
            mk_item(frig_unass, 587, char_id, station_id, InventoryFlag::Hangar, 1, false),
        );
        snap.items.insert(
            rig_on_bb,
            mk_item(rig_on_bb, rig_tid, char_id, bb_id, InventoryFlag::RigSlot0, 1, true),
        );
        // Over-capacity drone count (`volume * qty` > 25.0 m3)
        let over_qty = ((25.0 / drone_tid.1).ceil() as i32) + 2;
        snap.items.insert(
            drone_heavy,
            mk_item(drone_heavy, drone_tid.0, char_id, station_id, InventoryFlag::Hangar, over_qty, false),
        );

        // Also register an Upwell structure so `handle` dogmaIM fallback + structure weapon guard run cleanly
        snap.structures.insert(
            corp_structure_id,
            eve_db::UpwellStructureRecord {
                structure_id: corp_structure_id,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "TestCitadel".into(),
                position: [0.0; 3],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 10_000.0,
                armor_hp: 10_000.0,
                hull_hp: 10_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });

    // 1. `handle` dispatch coverage: `GetContainerContents`, `MultiAdd`
    let cont_res = handle(
        &ctx,
        &mut session,
        None,
        "invbroker",
        "GetContainerContents",
        &[PyRep::Integer(container_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("GetContainerContents succeeds");
    assert_ne!(cont_res, PyRep::None);

    // 2. Rig unfit (`RigSlot0 -> Hangar`) cleans up rig from snap.items
    let unfit_rig_rep = call_add(
        &ctx,
        &mut session,
        &[PyRep::Integer(rig_on_bb), PyRep::Integer(station_id), PyRep::Integer(4)],
    )
    .await
    .expect("rig unfit removes item");
    assert_eq!(unfit_rig_rep.dict_get("itemID").and_then(|v| v.as_i64()), Some(rig_on_bb));
    ctx.db.with_snapshot(|snap| assert!(!snap.items.contains_key(&rig_on_bb)));

    // 3. Frigate Escape Bay (`177`):
    //    - unassembled frigate -> ShipMustBeAssembled
    assert_eq!(
        call_add(
            &ctx,
            &mut session,
            &[PyRep::Integer(frig_unass), PyRep::Integer(bb_id), PyRep::Integer(177)],
        )
        .await
        .unwrap_err()
        .code,
        "ShipMustBeAssembled"
    );
    //    - frigate containing non-charge cargo -> ShipHasItemsInCargo
    assert_eq!(
        call_add(
            &ctx,
            &mut session,
            &[PyRep::Integer(frig_ok), PyRep::Integer(bb_id), PyRep::Integer(177)],
        )
        .await
        .unwrap_err()
        .code,
        "ShipHasItemsInCargo"
    );
    //    - remove non-charge cargo, leave category 8 ammo -> SUCCEEDS!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&frig_cargo_item);
    });
    assert!(
        call_add(
            &ctx,
            &mut session,
            &[PyRep::Integer(frig_ok), PyRep::Integer(bb_id), PyRep::Integer(177)],
        )
        .await
        .is_ok(),
        "frigate containing only category-8 ammo must fit in Frigate Escape Bay"
    );
    //    - second frigate -> CapacityExceeded
    assert_eq!(
        call_add(
            &ctx,
            &mut session,
            &[PyRep::Integer(frig_second), PyRep::Integer(bb_id), PyRep::Integer(177)],
        )
        .await
        .unwrap_err()
        .code,
        "CapacityExceeded"
    );

    // 4. Hold capacity: `shuttle_id` has 0.0 DroneBay capacity; `bb_id` has 25.0 m3 DroneBay
    assert_eq!(
        call_add(
            &ctx,
            &mut session,
            &[PyRep::Integer(drone_heavy), PyRep::Integer(shuttle_id), PyRep::Integer(87)],
        )
        .await
        .unwrap_err()
        .code,
        "NotEnoughDroneBaySpace"
    );
    assert_eq!(
        call_add(
            &ctx,
            &mut session,
            &[PyRep::Integer(drone_heavy), PyRep::Integer(bb_id), PyRep::Integer(87)],
        )
        .await
        .unwrap_err()
        .code,
        "NotEnoughDroneBaySpace"
    );

    // 5. Split + expected_version + idempotency_key + CorpSAG1->Cargo owner reset + stack merge
    let mut bad_ver_kw = HashMap::new();
    bad_ver_kw.insert("expected_version".into(), PyRep::Integer(99));
    assert_eq!(
        handle(
            &ctx,
            &mut session,
            None,
            "invbroker",
            "Add",
            &[PyRep::Integer(stack_a), PyRep::Integer(ship_id), PyRep::Integer(5)],
            &bad_ver_kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ItemVersionMismatch"
    );

    // Split qty=5 from stack_a (20) with `expected_version = 1` & `idempotency_key` -> 15 remains (v2), split 5 merges or lands in cargo
    let mut split_kw = HashMap::new();
    split_kw.insert("qty".into(), PyRep::Integer(5));
    split_kw.insert("expected_version".into(), PyRep::Integer(1));
    split_kw.insert("idempotency_key".into(), PyRep::String("idem-split-1".into()));
    let split_rep = handle(
        &ctx,
        &mut session,
        None,
        "invbroker",
        "Add",
        &[PyRep::Integer(stack_a), PyRep::Integer(bb_id), PyRep::Integer(5)],
        &split_kw,
    )
    .await
    .unwrap()
    .expect("split into bb cargo succeeds");
    let split_new_id = split_rep.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    assert_ne!(split_new_id, stack_a);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&stack_a].quantity, 15);
        assert_eq!(snap.items[&stack_a].version, 2);
        assert_eq!(snap.items[&split_new_id].quantity, 5);
        assert_eq!(snap.items[&split_new_id].version, 2); // 1 created + 1 moved
        assert_eq!(snap.items[&split_new_id].owner_id, char_id);
        assert!(snap.committed_idempotency_keys.contains(&(char_id, "idem-split-1".into())));
    });

    // Also check stale `expected_version` while splitting -> InventoryMoveFailed
    let mut split_bad_ver_kw = HashMap::new();
    split_bad_ver_kw.insert("qty".into(), PyRep::Integer(5));
    split_bad_ver_kw.insert("expected_version".into(), PyRep::Integer(99));
    assert_eq!(
        handle(
            &ctx,
            &mut session,
            None,
            "invbroker",
            "Add",
            &[PyRep::Integer(stack_a), PyRep::Integer(bb_id), PyRep::Integer(5)],
            &split_bad_ver_kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ItemVersionMismatch"
    );

    // Move `sag_item` (CorpSAG1 -> shuttle_id Cargo, no Tritanium stack in shuttle) -> owner_id resets to char_id
    call_add(
        &ctx,
        &mut session,
        &[PyRep::Integer(sag_item), PyRep::Integer(shuttle_id), PyRep::Integer(5)],
    )
    .await
    .expect("CorpSAG1 -> ship cargo resets owner to char_id");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&sag_item].owner_id, char_id);
        assert_eq!(snap.items[&sag_item].location_id, shuttle_id);
    });

    // Idempotent replay without qty on full item (`sag_item`) with 4,096 cache eviction check:
    ctx.db.with_mut_snapshot(|snap| {
        for i in 0..4_096_i64 {
            snap.committed_idempotency_keys.insert((char_id, format!("fill-{i}")));
        }
    });
    let mut move_idem_kw = HashMap::new();
    move_idem_kw.insert("idempotency_key".into(), PyRep::String("idem-evict-trigger".into()));
    handle(
        &ctx,
        &mut session,
        None,
        "invbroker",
        "Add",
        &[PyRep::Integer(sag_item), PyRep::Integer(station_id), PyRep::Integer(4)],
        &move_idem_kw,
    )
    .await
    .unwrap()
    .expect("eviction trigger move succeeds");
    // Second call with same key hits cached AddTxOutcome and skips mutation + cache evicted >= 4096 down to 1!
    let replay_rep = handle(
        &ctx,
        &mut session,
        None,
        "invbroker",
        "Add",
        &[PyRep::Integer(sag_item), PyRep::Integer(shuttle_id), PyRep::Integer(5)],
        &move_idem_kw,
    )
    .await
    .unwrap()
    .expect("idempotent replay returns moved item without changing location");
    assert_eq!(replay_rep.dict_get("locationID").and_then(|v| v.as_i64()), Some(station_id));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.committed_idempotency_keys.len(), 1);
    });

    // Merge `merge_src` (15) into `merge_dest` (10, avoiding courier/dyn/muta siblings!) -> 25
    let merged_rep = call_add(
        &ctx,
        &mut session,
        &[PyRep::Integer(merge_src), PyRep::Integer(ship_id), PyRep::Integer(5)],
    )
    .await
    .expect("merge into matching non-courier stack");
    assert_eq!(merged_rep.dict_get("itemID").and_then(|v| v.as_i64()), Some(merge_dest));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&merge_src));
        assert_eq!(snap.items[&merge_dest].quantity, 25);
        assert_eq!(snap.items[&courier_item].quantity, 5);
        assert_eq!(snap.items[&dyn_item].quantity, 5);
        assert_eq!(snap.items[&muta_item].quantity, 5);
    });

    // MultiAdd (`handle` match arm) moves `cont_child_id` into `ship_id`
    assert!(
        handle(
            &ctx,
            &mut session,
            None,
            "invbroker",
            "MultiAdd",
            &[PyRep::List(vec![PyRep::Integer(cont_child_id)]), PyRep::Integer(ship_id), PyRep::Integer(5)],
            &kw,
        )
        .await
        .unwrap()
        .is_ok()
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&cont_child_id].location_id, ship_id);
    });

    // 6. Undock into space: test space loot split (`looted_from_space`), `ship.Jettison`, `ship.Drop`,
    //    and structure controller `dogmaIM.Activate` dispatch in `handle`.
    let wreck_space_id = 96_650_050_i64;
    let wreck_loot_id = 96_650_051_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.get_mut(&ship_id).unwrap().location_id = sys_id as i64;
        snap.items.get_mut(&ship_id).unwrap().flag = InventoryFlag::None;
        snap.items.insert(
            wreck_space_id,
            mk_item(wreck_space_id, 23, char_id, sys_id as i64, InventoryFlag::None, 1, true),
        );
        snap.item_positions.insert(wreck_space_id, [100.0, 0.0, 0.0]);
        snap.items.insert(
            wreck_loot_id,
            mk_item(wreck_loot_id, 34, char_id, wreck_space_id, InventoryFlag::Cargo, 10, false),
        );
    });
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h.try_spawn_ball_with_combat(
        eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        },
        ctx.build_ship_combat_state(char_id, ship_id, 587),
    );
    sys_h.try_spawn_ball_with_combat(
        eve_destiny::Ball {
            id: wreck_space_id,
            type_id: 23,
            position: glam::DVec3::new(100.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000.0,
            radius: 20.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        },
        eve_destiny::CombatState::default(),
    );

    let mut space_sess = session.clone();
    space_sess.station_id = None;
    space_sess.structure_id = None;
    space_sess.ship_id = Some(ship_id);
    space_sess.solar_system_id = Some(sys_id);

    // Split qty=4 from `wreck_loot_id` (10) inside nearby space can -> `bb_id` / active ship `ship_id` Cargo!
    // Use `bb_id`? No, active ship is `ship_id` in space. `cont_child_id` (Tritanium 34) is in ship_id; remove `cont_child_id` & `merge_dest` first so split stays unmerged OR merges cleanly!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&merge_dest);
        snap.items.remove(&cont_child_id);
    });
    let mut loot_split_kw = HashMap::new();
    loot_split_kw.insert("qty".into(), PyRep::Integer(4));
    let loot_rep = handle(
        &ctx,
        &mut space_sess,
        None,
        "invbroker",
        "Add",
        &[PyRep::Integer(wreck_loot_id), PyRep::Integer(ship_id), PyRep::Integer(5)],
        &loot_split_kw,
    )
    .await
    .unwrap()
    .expect("space loot split into ship cargo succeeds");
    let loot_new_id = loot_rep.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    assert_ne!(loot_new_id, wreck_loot_id);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&wreck_loot_id].quantity, 6);
        assert_eq!(snap.items[&wreck_loot_id].version, 2);
        assert_eq!(snap.items[&loot_new_id].quantity, 4);
        assert_eq!(snap.items[&loot_new_id].location_id, ship_id);
    });

    // `ship.Jettison` & `ship.Drop` match arms in `handle`
    let jettison_res = handle(
        &ctx,
        &mut space_sess,
        None,
        "ship",
        "Jettison",
        &[PyRep::List(vec![PyRep::Integer(loot_new_id)])],
        &kw,
    )
    .await;
    assert!(jettison_res.is_some());

    let drop_res = handle(
        &ctx,
        &mut space_sess,
        None,
        "ship",
        "Drop",
        &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(dyn_item), PyRep::Integer(1)])])],
        &kw,
    )
    .await;
    assert!(drop_res.is_some());

    // Structure controller `dogmaIM.Activate` routes to `handle_weapons` inside `handle`
    let mut ctrl_sess = space_sess.clone();
    ctrl_sess.structure_id = Some(corp_structure_id);
    ctrl_sess.ship_id = Some(corp_structure_id);
    let _ = handle(
        &ctx,
        &mut ctrl_sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(999_999_999)],
        &kw,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invbroker_add_quantum_core_auth_split_corp_owner_and_locked_flags() {
    let (ctx, mut session, char_id, corp_id, station_id, ship_id) = setup_inv_ctx();
    let struct_id = 1_066_660_001_i64;
    let other_corp = 98_660_999_i64;
    let fake_core = 96_660_001_i64;
    let corp_split_src = 96_660_002_i64;
    let locked_flag_item = 96_660_003_i64;
    let active_ship_err_flag = ship_id;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832, // Astrahus
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "CoreCitadel".into(),
                position: [0.0; 3],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: false,
                fuel_blocks: 100,
                shield_hp: 10_000.0,
                armor_hp: 10_000.0,
                hull_hp: 10_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.items.insert(
            fake_core,
            mk_item(fake_core, 34, char_id, station_id, InventoryFlag::Hangar, 1, false),
        );
        snap.items.insert(
            corp_split_src,
            mk_item(corp_split_src, 34, char_id, station_id, InventoryFlag::Hangar, 20, false),
        );
        snap.items.insert(
            locked_flag_item,
            mk_item(locked_flag_item, 34, char_id, station_id, InventoryFlag::Hangar, 5, false),
        );
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.member_roles
                .insert(char_id, eve_db::corp_roles::ROLE_DIRECTOR);
        }
    });

    // 1. Quantum Core deposit (`flag = 180 = StructureDeedBay`) while docked at NPC station -> DestinationAccessDenied
    assert_eq!(
        call_add(
            &ctx,
            &mut session,
            &[PyRep::Integer(fake_core), PyRep::Integer(struct_id), PyRep::Integer(180)],
        )
        .await
        .unwrap_err()
        .code,
        "DestinationAccessDenied"
    );

    // Dock in struct_id without Director/StationManager role -> CorpRoleDenied
    let mut struct_sess = session.clone();
    struct_sess.station_id = None;
    struct_sess.structure_id = Some(struct_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(fk) = snap.items.get_mut(&fake_core) {
            fk.location_id = struct_id;
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = Some(struct_id);
        }
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(char_id, eve_db::corp_roles::ROLE_TRADER);
    });
    assert_eq!(
        call_add(
            &ctx,
            &mut struct_sess,
            &[PyRep::Integer(fake_core), PyRep::Integer(struct_id), PyRep::Integer(180)],
        )
        .await
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    // Grant Station Manager role (`ROLE_STATION_MANAGER`) -> passes role check, fails `InvalidItemType` for Tritanium
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(char_id, eve_db::corp_roles::ROLE_STATION_MANAGER);
    });
    assert_eq!(
        call_add(
            &ctx,
            &mut struct_sess,
            &[PyRep::Integer(fake_core), PyRep::Integer(struct_id), PyRep::Integer(180)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidItemType"
    );

    // Different corp -> StructureOwnershipViolation
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&struct_id).unwrap().owner_corp_id = other_corp;
    });
    assert_eq!(
        call_add(
            &ctx,
            &mut struct_sess,
            &[PyRep::Integer(fake_core), PyRep::Integer(struct_id), PyRep::Integer(180)],
        )
        .await
        .unwrap_err()
        .code,
        "StructureOwnershipViolation"
    );

    // 2. Active ship move rejected & split into CorpSAG1 (`116`) sets split owner_id == corp_id
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.structure_id = None;
            ch.station_id = Some(station_id);
        }
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(char_id, eve_db::corp_roles::ROLE_DIRECTOR);
    });
    assert_eq!(
        call_add(
            &ctx,
            &mut session,
            &[
                PyRep::Integer(active_ship_err_flag),
                PyRep::Integer(station_id),
                PyRep::Integer(4),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InventoryMoveFailed"
    );

    let mut corp_split_kw = HashMap::new();
    corp_split_kw.insert("qty".into(), PyRep::Integer(7));
    let sag_split_rep = handle(
        &ctx,
        &mut session,
        None,
        "invbroker",
        "Add",
        &[
            PyRep::Integer(corp_split_src),
            PyRep::Integer(station_id),
            PyRep::Integer(116),
        ],
        &corp_split_kw,
    )
    .await
    .unwrap()
    .expect("split into CorpSAG1 succeeds");
    let sag_new_id = sag_split_rep.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&corp_split_src].quantity, 13);
        assert_eq!(snap.items[&sag_new_id].quantity, 7);
        assert_eq!(snap.items[&sag_new_id].owner_id, corp_id);
    });

    // Self-location move rejected
    assert_eq!(
        call_add(
            &ctx,
            &mut session,
            &[
                PyRep::Integer(locked_flag_item),
                PyRep::Integer(locked_flag_item),
                PyRep::Integer(5),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "UserError"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invbroker_add_preserves_native_ship_hangar_and_fleet_hangar_flags_and_capacities() {
    let (mut ctx, mut session, char_id, _corp_id, station_id, _default_ship_id) = setup_inv_ctx();
    let carrier_tid = 999_620u32;
    let shuttle_tid = 999_621u32;
    let isotope_tid = 999_622u32;
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        let mut carrier = sde_mut.types[&587].clone();
        carrier.type_id = carrier_tid;
        carrier.group_id = 30;
        carrier.category_id = 6;
        carrier.name = "Synthetic Titan".into();
        carrier.capacity = 11_250.0;
        carrier.volume = 155_000_000.0;
        carrier.default_attributes.insert(908, 5_000_000.0); // shipMaintenanceBayCapacity
        carrier.default_attributes.insert(912, 1_000.0); // fleetHangarCapacity
        carrier.default_attributes.insert(1549, 60_000.0); // specialFuelBayCapacity
        sde_mut.types.insert(carrier_tid, carrier);

        let mut shuttle = sde_mut.types[&587].clone();
        shuttle.type_id = shuttle_tid;
        shuttle.group_id = 31;
        shuttle.category_id = 6;
        shuttle.name = "Synthetic Frigate".into();
        shuttle.volume = 2_800_000.0;
        shuttle.capacity = 120.0;
        sde_mut.types.insert(shuttle_tid, shuttle);

        let mut isotope = sde_mut.types[&34].clone();
        isotope.type_id = isotope_tid;
        isotope.group_id = 423;
        isotope.category_id = 4;
        isotope.name = "Synthetic Isotope".into();
        isotope.volume = 100.0;
        sde_mut.types.insert(isotope_tid, isotope);
    }

    let titan_id = 96_620_001i64;
    let hull_1 = 96_620_002i64;
    let hull_2 = 96_620_003i64;
    let fh_item_1 = 96_620_004i64;
    let fh_item_2 = 96_620_005i64;
    let fh_overflow = 96_620_006i64;
    let cargo_item = 96_620_007i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            titan_id,
            mk_item(titan_id, carrier_tid, char_id, station_id, InventoryFlag::Hangar, 1, true),
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = titan_id;
        }
        snap.items.insert(
            hull_1,
            mk_item(hull_1, shuttle_tid, char_id, station_id, InventoryFlag::Hangar, 1, true),
        );
        snap.items.insert(
            hull_2,
            mk_item(hull_2, shuttle_tid, char_id, station_id, InventoryFlag::Hangar, 1, true),
        );
        // 500 m3 + 500 m3 fills fleet hangar (1,000 m3); cargo item verifies flag 155 != flag 5 merge/cap collision.
        snap.items.insert(
            fh_item_1,
            mk_item(fh_item_1, isotope_tid, char_id, station_id, InventoryFlag::Hangar, 5, false),
        );
        snap.items.insert(
            fh_item_2,
            mk_item(fh_item_2, isotope_tid, char_id, station_id, InventoryFlag::Hangar, 5, false),
        );
        snap.items.insert(
            fh_overflow,
            mk_item(fh_overflow, isotope_tid, char_id, station_id, InventoryFlag::Hangar, 1, false),
        );
        snap.items.insert(
            cargo_item,
            mk_item(cargo_item, isotope_tid, char_id, titan_id, InventoryFlag::Cargo, 5, false),
        );
    });
    session.ship_id = Some(titan_id);

    // 1. Add(..., 155) persists InventoryFlag::FleetHangar (155), does NOT merge with Cargo (5),
    //    and properly counts toward Fleet Hangar capacity on subsequent moves.
    let rep_fh1 = call_add(
        &ctx,
        &mut session,
        &[PyRep::Integer(fh_item_1), PyRep::Integer(titan_id), PyRep::Integer(155)],
    )
    .await
    .expect("first FleetHangar add");
    assert_eq!(rep_fh1.dict_get("flagID").and_then(|v| v.as_i64()), Some(155));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&fh_item_1].flag, InventoryFlag::FleetHangar);
        assert_eq!(snap.items[&cargo_item].quantity, 5);
    });

    let rep_fh2 = call_add(
        &ctx,
        &mut session,
        &[PyRep::Integer(fh_item_2), PyRep::Integer(titan_id), PyRep::Integer(155)],
    )
    .await
    .expect("second FleetHangar add merges into fh_item_1 at flag 155");
    assert_eq!(rep_fh2.dict_get("itemID").and_then(|v| v.as_i64()), Some(fh_item_1));
    assert_eq!(rep_fh2.dict_get("flagID").and_then(|v| v.as_i64()), Some(155));
    assert_eq!(rep_fh2.dict_get("quantity").and_then(|v| v.as_i64()), Some(10));

    let err_fh = call_add(
        &ctx,
        &mut session,
        &[PyRep::Integer(fh_overflow), PyRep::Integer(titan_id), PyRep::Integer(155)],
    )
    .await
    .unwrap_err();
    let (fh_key, fh_kv) = decode_user_error(&err_fh);
    assert_eq!(fh_key, "NotEnoughSpecialBaySpace");
    assert_eq!(fh_kv.get("volume").copied(), Some(100.0));
    assert_eq!(fh_kv.get("available").copied(), Some(0.0));

    // Bound `GetInventoryFromId(titan_id)` + `Add(fh_overflow, station_id, 155)` also enforces NotEnoughSpecialBaySpace.
    let titan_bound = BoundObjectEntry {
        bound_id: 901,
        session_id: session.session_id,
        user_id: char_id as u64,
        character_id: char_id,
        service: "invbroker".into(),
        object_id: titan_id,
        oid_str: format!("N=901:{titan_id}"),
        ref_id: 1,
    };
    let bound_fh_err = handle(
        &ctx,
        &mut session,
        Some(&titan_bound),
        "invbroker",
        "Add",
        &[PyRep::Integer(fh_overflow), PyRep::Integer(station_id), PyRep::Integer(155)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap_err();
    let (bfh_key, bfh_kv) = decode_user_error(&bound_fh_err);
    assert_eq!(bfh_key, "NotEnoughSpecialBaySpace");
    assert_eq!(bfh_kv.get("volume").copied(), Some(100.0));
    assert_eq!(bfh_kv.get("available").copied(), Some(0.0));

    // 2. Add(..., 90) persists InventoryFlag::ShipHangar (90), enforces atomic SMA rules,
    //    and counts 90 | 138 against SMA capacity (5,000,000 m3) with NotEnoughSpecialBaySpace.
    assert_eq!(
        call_add(
            &ctx,
            &mut session,
            &[PyRep::Integer(fh_overflow), PyRep::Integer(titan_id), PyRep::Integer(90)],
        )
        .await
        .unwrap_err()
        .code,
        "CannotStoreCargoInSMA"
    );
    let unpackaged_hull = 96_620_008i64;
    let loaded_hull = 96_620_009i64;
    let loaded_cargo = 96_620_010i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            unpackaged_hull,
            mk_item(unpackaged_hull, shuttle_tid, char_id, station_id, InventoryFlag::Hangar, 1, false),
        );
        snap.items.insert(
            loaded_hull,
            mk_item(loaded_hull, shuttle_tid, char_id, station_id, InventoryFlag::Hangar, 1, true),
        );
        snap.items.insert(
            loaded_cargo,
            mk_item(loaded_cargo, isotope_tid, char_id, loaded_hull, InventoryFlag::Cargo, 1, false),
        );
    });
    assert_eq!(
        call_add(
            &ctx,
            &mut session,
            &[PyRep::Integer(unpackaged_hull), PyRep::Integer(titan_id), PyRep::Integer(90)],
        )
        .await
        .unwrap_err()
        .code,
        "OnlyAssembledShipsInSMA"
    );
    assert_eq!(
        call_add(
            &ctx,
            &mut session,
            &[PyRep::Integer(loaded_hull), PyRep::Integer(titan_id), PyRep::Integer(90)],
        )
        .await
        .unwrap_err()
        .code,
        "ShipHasNonChargeCargo"
    );

    let rep_sma = call_add(
        &ctx,
        &mut session,
        &[PyRep::Integer(hull_1), PyRep::Integer(titan_id), PyRep::Integer(90)],
    )
    .await
    .expect("first SMA add");
    assert_eq!(rep_sma.dict_get("flagID").and_then(|v| v.as_i64()), Some(90));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&hull_1].flag, InventoryFlag::ShipHangar);
    });

    let err_sma = call_add(
        &ctx,
        &mut session,
        &[PyRep::Integer(hull_2), PyRep::Integer(titan_id), PyRep::Integer(90)],
    )
    .await
    .unwrap_err();
    let (sma_key, sma_kv) = decode_user_error(&err_sma);
    assert_eq!(sma_key, "NotEnoughSpecialBaySpace");
    assert_eq!(sma_kv.get("volume").copied(), Some(2_800_000.0));
    assert_eq!(sma_kv.get("available").copied(), Some(2_200_000.0));
}

fn decode_user_error(err: &BridgeRpcError) -> (String, HashMap<String, f64>) {
    let PyRep::Tuple(parts) = err.to_client_error("invbroker", "Add").to_macho_error_body(1) else {
        panic!("expected ErrorResponse tuple");
    };
    let PyRep::Tuple(inner) = &parts[2] else {
        panic!("expected inner tuple");
    };
    let PyRep::Bytes(raw) = &inner[0] else {
        panic!("expected marshaled bytes");
    };
    let PyRep::ObjectEx { header, .. } = eve_proto::marshal::decode(raw).expect("decode UserError") else {
        panic!("expected ObjectEx");
    };
    let PyRep::Tuple(hdr) = *header else {
        panic!("expected header tuple");
    };
    assert_eq!(hdr[0], PyRep::Token("eveexceptions.UserError".into()));
    let PyRep::Tuple(u_args) = &hdr[1] else {
        panic!("expected UserError args tuple");
    };
    let key = u_args[0].as_str().expect("UserError key").to_string();
    let PyRep::Dict(entries) = &u_args[1] else {
        panic!("expected UserError dict");
    };
    let kv = entries
        .iter()
        .map(|(k, v)| (k.as_str().expect("dict key").to_string(), v.as_f64().expect("real arg")))
        .collect();
    (key, kv)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invbroker_add_enforces_foreign_ship_bay_acl_transfers_owner_and_notifies() {
    use crate::services::inventory_fitting::fitting_containers_ships::{
        set_ship_access_config_snap, ShipAccessConfig,
    };
    use eve_db::FleetRecord;

    let (mut ctx, _owner_sess, owner_id, _owner_corp, station_id, _default_ship) = setup_inv_ctx();
    let mate_id = 96_630_002i64;
    let mate_corp = 98_630_002i64;
    let carrier_tid = 999_630u32;
    let shuttle_tid = 999_631u32;
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        let mut carrier = sde_mut.types[&587].clone();
        carrier.type_id = carrier_tid;
        carrier.group_id = 30;
        carrier.category_id = 6;
        carrier.name = "Synthetic Carrier".into();
        carrier.capacity = 5_000.0;
        carrier.volume = 100_000_000.0;
        carrier.default_attributes.insert(908, 10_000_000.0);
        carrier.default_attributes.insert(912, 10_000.0);
        sde_mut.types.insert(carrier_tid, carrier);

        let mut shuttle = sde_mut.types[&587].clone();
        shuttle.type_id = shuttle_tid;
        shuttle.group_id = 31;
        shuttle.category_id = 6;
        shuttle.name = "Synthetic Shuttle".into();
        shuttle.volume = 50_000.0;
        sde_mut.types.insert(shuttle_tid, shuttle);
    }

    let owner_carrier = 96_630_100i64;
    let mate_carrier = 96_630_101i64;
    let owner_fh_trit = 96_630_201i64;
    let mate_hangar_trit = 96_630_202i64;
    let mate_hangar_pyerite = 96_630_203i64;
    let mate_hangar_mex = 96_630_204i64;
    let mate_legacy_fh_item = 96_630_205i64;
    let mate_legacy_sma_hull = 96_630_206i64;
    let sma_box = 96_630_207i64;
    let nested_sma_item = 96_630_208i64;
    let pub_can = 96_630_209i64;
    let foreign_can_item = 96_630_210i64;

    let mut mate_uid = 2u64;
    let mut owner_uid = 1u64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut mate = snap.characters[&owner_id].clone();
        owner_uid = mate.user_id;
        mate_uid = if owner_uid == 2 { 22 } else { 2 };
        mate.character_id = mate_id;
        mate.user_id = mate_uid;
        mate.name = "FleetMate".into();
        mate.corporation_id = mate_corp;
        mate.station_id = Some(station_id);
        mate.structure_id = None;
        mate.active_ship_id = mate_carrier;
        snap.characters.insert(mate_id, mate);

        let mut c2 = CorporationRecord::default();
        c2.corporation_id = mate_corp;
        c2.name = "Mate Corp".into();
        c2.ticker = "MATE".into();
        c2.ceo_id = mate_id;
        c2.members = [mate_id].into_iter().collect();
        snap.corporations.insert(mate_corp, c2);

        snap.fleets.insert(
            777_630,
            FleetRecord {
                fleet_id: 777_630,
                boss_id: owner_id,
                members: vec![owner_id, mate_id],
                ..Default::default()
            },
        );

        snap.items.insert(owner_carrier, mk_item(owner_carrier, carrier_tid, owner_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(mate_carrier, mk_item(mate_carrier, carrier_tid, mate_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(owner_fh_trit, mk_item(owner_fh_trit, 34, owner_id, owner_carrier, InventoryFlag::FleetHangar, 10, false));
        snap.items.insert(mate_hangar_trit, mk_item(mate_hangar_trit, 34, mate_id, station_id, InventoryFlag::Hangar, 5, false));
        snap.items.insert(mate_hangar_pyerite, mk_item(mate_hangar_pyerite, 35, mate_id, station_id, InventoryFlag::Hangar, 20, false));
        snap.items.insert(mate_hangar_mex, mk_item(mate_hangar_mex, 36, mate_id, station_id, InventoryFlag::Hangar, 15, false));
        // Deliberately owned by `mate_id` inside `owner_carrier`'s bays to verify ACL/password cannot be bypassed by `item.owner_id`.
        snap.items.insert(mate_legacy_fh_item, mk_item(mate_legacy_fh_item, 37, mate_id, owner_carrier, InventoryFlag::FleetHangar, 4, false));
        snap.items.insert(mate_legacy_sma_hull, mk_item(mate_legacy_sma_hull, shuttle_tid, mate_id, owner_carrier, InventoryFlag::ShipHangar, 1, true));
        snap.items.insert(sma_box, mk_item(sma_box, 17_366, mate_id, owner_carrier, InventoryFlag::ShipHangar, 1, true));
        snap.items.insert(nested_sma_item, mk_item(nested_sma_item, 34, mate_id, sma_box, InventoryFlag::Cargo, 6, false));
        snap.items.insert(pub_can, mk_item(pub_can, 23, 0, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(foreign_can_item, mk_item(foreign_can_item, 38, owner_id, pub_can, InventoryFlag::Cargo, 9, false));
    });
    let now_ts = ctx.now_unix();
    ctx.db
        .acquire_online_lock(owner_uid, 1_001, Some(owner_id), true, now_ts)
        .unwrap();
    ctx.db
        .acquire_online_lock(mate_uid, 1_002, Some(mate_id), true, now_ts)
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        set_ship_access_config_snap(
            snap,
            owner_carrier,
            &ShipAccessConfig {
                allow_fleet_smb: true,
                allow_corp_smb: false,
                allow_fleet_hangar: true,
                allow_corp_hangar: false,
                password: Some("bay-pw".into()),
            },
            0,
        )
        .unwrap();
    });

    let mut mate_sess = SessionState::new(2);
    mate_sess.user_id = Some(mate_uid);
    mate_sess.character_id = Some(mate_id);
    mate_sess.corporation_id = Some(mate_corp);
    mate_sess.station_id = Some(station_id);
    mate_sess.solar_system_id = Some(30_000_142);
    mate_sess.ship_id = Some(mate_carrier);

    let mut pw_kw = HashMap::new();
    pw_kw.insert("password".into(), PyRep::String("bay-pw".into()));

    // 1. Foreign ship Cargo (flag 5) is NEVER writable even by a fleetmate with bay password.
    assert!(handle(
        &ctx,
        &mut mate_sess,
        None,
        "invbroker",
        "Add",
        &[PyRep::Integer(mate_hangar_trit), PyRep::Integer(owner_carrier), PyRep::Integer(5)],
        &pw_kw,
    )
    .await
    .unwrap()
    .is_err());

    // 2. Missing password rejects both deposit and own-item extraction in FleetHangar/SMA.
    assert_eq!(
        call_add(&ctx, &mut mate_sess, &[PyRep::Integer(mate_hangar_trit), PyRep::Integer(owner_carrier), PyRep::Integer(155)])
            .await
            .unwrap_err()
            .code,
        "FleetHangarAccessDenied"
    );
    assert_eq!(
        call_add(&ctx, &mut mate_sess, &[PyRep::Integer(mate_legacy_fh_item), PyRep::Integer(station_id), PyRep::Integer(4)])
            .await
            .unwrap_err()
            .code,
        "FleetHangarAccessDenied"
    );
    assert_eq!(
        call_add(&ctx, &mut mate_sess, &[PyRep::Integer(mate_legacy_sma_hull), PyRep::Integer(station_id), PyRep::Integer(4)])
            .await
            .unwrap_err()
            .code,
        "SMAAccessDenied"
    );
    assert_eq!(
        call_add(&ctx, &mut mate_sess, &[PyRep::Integer(nested_sma_item), PyRep::Integer(station_id), PyRep::Integer(4)])
            .await
            .unwrap_err()
            .code,
        "SMAAccessDenied"
    );

    // 3. Cross-pilot deposit (full stack merge, direct move, and qty split) transfers owner_id = owner_id & notifies owner_id.
    let _ = ctx.drain_notifications_for_char(owner_id);
    let merged_rep = handle(
        &ctx,
        &mut mate_sess,
        None,
        "invbroker",
        "Add",
        &[PyRep::Integer(mate_hangar_trit), PyRep::Integer(owner_carrier), PyRep::Integer(155)],
        &pw_kw,
    )
    .await
    .unwrap()
    .expect("merge into owner fleet hangar stack");
    assert_eq!(merged_rep.dict_get("itemID").and_then(|v| v.as_i64()), Some(owner_fh_trit));
    assert_eq!(merged_rep.dict_get("quantity").and_then(|v| v.as_i64()), Some(15));

    let moved_rep = handle(
        &ctx,
        &mut mate_sess,
        None,
        "invbroker",
        "Add",
        &[PyRep::Integer(mate_hangar_mex), PyRep::Integer(owner_carrier), PyRep::Integer(155)],
        &pw_kw,
    )
    .await
    .unwrap()
    .expect("direct move into owner fleet hangar");
    let moved_id = moved_rep.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();

    let mut split_pw_kw = pw_kw.clone();
    split_pw_kw.insert("qty".into(), PyRep::Integer(7));
    let split_rep = handle(
        &ctx,
        &mut mate_sess,
        None,
        "invbroker",
        "Add",
        &[PyRep::Integer(mate_hangar_pyerite), PyRep::Integer(owner_carrier), PyRep::Integer(155)],
        &split_pw_kw,
    )
    .await
    .unwrap()
    .expect("split move into owner fleet hangar");
    let split_id = split_rep.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&owner_fh_trit].owner_id, owner_id);
        assert_eq!(snap.items[&owner_fh_trit].quantity, 15);
        assert_eq!(snap.items[&moved_id].owner_id, owner_id);
        assert_eq!(snap.items[&split_id].owner_id, owner_id);
        assert_eq!(snap.items[&split_id].quantity, 7);
        assert_eq!(snap.items[&mate_hangar_pyerite].owner_id, mate_id);
        assert_eq!(snap.items[&mate_hangar_pyerite].quantity, 13);
    });
    let owner_notifs = ctx.drain_notifications_for_char(owner_id);
    assert!(owner_notifs.iter().any(|n| n.event == "OnItemChange"), "missing OnItemChange for {owner_id}: {owner_notifs:?}");
    assert!(owner_notifs.iter().any(|n| n.event == "ProcessInventoryChange"), "missing ProcessInventoryChange for {owner_id}");

    // 4. Moving foreign-owned item from public container into own FleetHangar transfers owner_id = mate_id.
    call_add(
        &ctx,
        &mut mate_sess,
        &[PyRep::Integer(foreign_can_item), PyRep::Integer(mate_carrier), PyRep::Integer(155)],
    )
    .await
    .expect("public container item moves into own FleetHangar");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&foreign_can_item].owner_id, mate_id);
        assert_eq!(snap.items[&foreign_can_item].location_id, mate_carrier);
        assert_eq!(snap.items[&foreign_can_item].flag, InventoryFlag::FleetHangar);
    });

    // 5. Kicked out of fleet: Pilot B cannot extract own legacy items even WITH bay password.
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.get_mut(&777_630).unwrap().members.retain(|&m| m != mate_id);
    });
    assert_eq!(
        handle(
            &ctx,
            &mut mate_sess,
            None,
            "invbroker",
            "Add",
            &[PyRep::Integer(mate_legacy_fh_item), PyRep::Integer(station_id), PyRep::Integer(4)],
            &pw_kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FleetHangarAccessDenied"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut mate_sess,
            None,
            "invbroker",
            "Add",
            &[PyRep::Integer(mate_legacy_sma_hull), PyRep::Integer(station_id), PyRep::Integer(4)],
            &pw_kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "SMAAccessDenied"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn venture_station_hangar_and_active_cargo_capacity_for_issue_1301() {
    let (mut ctx, mut session, char_id, _corp_id, orig_station_id, default_rifter) = setup_inv_ctx();
    let npc_station = 60_004_423i64;
    let kw = HashMap::new();

    let venture_tid = 32_880u32;
    let probe_tid = 30_013u32;
    let veldspar_tid = 1_230u32;
    let filler_tid = 999_302u32;
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        let mut venture = sde_mut.types[&587].clone();
        venture.type_id = venture_tid;
        venture.group_id = 25;
        venture.category_id = 6;
        venture.name = "Venture".into();
        venture.capacity = 50.0;
        venture.volume = 29_500.0;
        venture.default_attributes.remove(&38);
        venture.default_attributes.insert(1556, 5_000.0);
        sde_mut.types.insert(venture_tid, venture);

        let mut probe = sde_mut.types[&34].clone();
        probe.type_id = probe_tid;
        probe.group_id = 479;
        probe.category_id = 8;
        probe.name = "Core Scanner Probe I".into();
        probe.volume = 0.1;
        probe.capacity = 0.0;
        sde_mut.types.insert(probe_tid, probe);

        let trit_proto = sde_mut.types[&34].clone();
        sde_mut.types.entry(veldspar_tid).or_insert_with(|| {
            let mut ore = trit_proto.clone();
            ore.type_id = veldspar_tid;
            ore.group_id = 462;
            ore.category_id = 25;
            ore.name = "Veldspar".into();
            ore.volume = 0.1;
            ore.capacity = 0.0;
            ore
        });

        let mut filler = sde_mut.types[&34].clone();
        filler.type_id = filler_tid;
        filler.group_id = 18;
        filler.category_id = 4;
        filler.name = "Cargo Filler".into();
        filler.volume = 0.1;
        filler.capacity = 0.0;
        sde_mut.types.insert(filler_tid, filler);

        if !sde_mut.stations.contains_key(&npc_station) {
            if let Some(mut st) = sde_mut
                .stations
                .get(&orig_station_id)
                .cloned()
                .or_else(|| sde_mut.stations.values().next().cloned())
            {
                st.station_id = npc_station;
                sde_mut.stations.insert(npc_station, st);
            }
        }
    }

    let active_venture = 96_640_100i64;
    let spare_venture = 96_640_101i64;
    let probes_active = 96_640_201i64;
    let filler_active = 96_640_202i64;
    let probes_overflow = 96_640_203i64;
    let veldspar_fit = 96_640_204i64;
    let veldspar_overflow = 96_640_205i64;
    let hull_for_sma = 96_640_206i64;
    let probes_spare = 96_640_207i64;
    let filler_spare = 96_640_208i64;
    let probes_spare_overflow = 96_640_209i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(npc_station);
            ch.structure_id = None;
            ch.active_ship_id = active_venture;
        }
        snap.items.insert(
            active_venture,
            mk_item(active_venture, venture_tid, char_id, npc_station, InventoryFlag::Hangar, 1, true),
        );
        snap.items.insert(
            spare_venture,
            mk_item(spare_venture, venture_tid, char_id, npc_station, InventoryFlag::Hangar, 1, true),
        );
        snap.items.insert(
            probes_active,
            mk_item(probes_active, probe_tid, char_id, npc_station, InventoryFlag::Hangar, 16, false),
        );
        snap.items.insert(
            filler_active,
            mk_item(filler_active, filler_tid, char_id, npc_station, InventoryFlag::Hangar, 484, false),
        );
        snap.items.insert(
            probes_overflow,
            mk_item(probes_overflow, probe_tid, char_id, npc_station, InventoryFlag::Hangar, 16, false),
        );
        snap.items.insert(
            veldspar_fit,
            mk_item(veldspar_fit, veldspar_tid, char_id, npc_station, InventoryFlag::Hangar, 50_000, false),
        );
        snap.items.insert(
            veldspar_overflow,
            mk_item(veldspar_overflow, veldspar_tid, char_id, npc_station, InventoryFlag::Hangar, 10, false),
        );
        snap.items.insert(
            hull_for_sma,
            mk_item(hull_for_sma, 587, char_id, npc_station, InventoryFlag::Hangar, 1, true),
        );
        snap.items.insert(
            probes_spare,
            mk_item(probes_spare, probe_tid, char_id, npc_station, InventoryFlag::Hangar, 16, false),
        );
        snap.items.insert(
            filler_spare,
            mk_item(filler_spare, filler_tid, char_id, npc_station, InventoryFlag::Hangar, 500, false),
        );
        snap.items.insert(
            probes_spare_overflow,
            mk_item(probes_spare_overflow, probe_tid, char_id, npc_station, InventoryFlag::Hangar, 16, false),
        );
    });
    session.station_id = Some(npc_station);
    session.structure_id = None;
    session.ship_id = Some(active_venture);

    let station_bound = BoundObjectEntry {
        bound_id: 950,
        session_id: session.session_id,
        user_id: char_id as u64,
        character_id: char_id,
        service: "invbroker".into(),
        object_id: npc_station,
        oid_str: format!("N=950:{npc_station}"),
        ref_id: 1,
    };

    // Active Venture bound via GetInventoryFromId(active_venture, 0) + Add(probes, 60004423) (#1301):
    assert!(crate::services::inventory_bound::handle(
        &ctx,
        &mut session,
        Some(&station_bound),
        "invbroker",
        "GetInventoryFromId",
        &[PyRep::Integer(active_venture), PyRep::Integer(0)],
        None,
    )
    .unwrap()
    .is_ok());

    let active_bound = BoundObjectEntry {
        bound_id: 951,
        session_id: session.session_id,
        user_id: char_id as u64,
        character_id: char_id,
        service: "invbroker".into(),
        object_id: active_venture,
        oid_str: format!("N=951:{active_venture}"),
        ref_id: 1,
    };

    // 1. Empty active Venture has 50.0 m3 main cargo -> moving 1.6 m3 Core Scanner Probe I succeeds.
    let ok_active = handle(
        &ctx,
        &mut session,
        Some(&active_bound),
        "invbroker",
        "Add",
        &[PyRep::Integer(probes_active), PyRep::Integer(npc_station)],
        &kw,
    )
    .await
    .unwrap()
    .expect("1.6 m3 probes must fit in empty 50.0 m3 Venture Cargo");
    assert_eq!(ok_active.dict_get("locationID").and_then(|v| v.as_i64()), Some(active_venture));
    assert_eq!(ok_active.dict_get("flagID").and_then(|v| v.as_i64()), Some(5));

    // Fill remaining 48.4 m3 cleanly (total 50.0 / 50.0 m3 used, 0.0 m3 free).
    handle(
        &ctx,
        &mut session,
        Some(&active_bound),
        "invbroker",
        "Add",
        &[PyRep::Integer(filler_active), PyRep::Integer(npc_station)],
        &kw,
    )
    .await
    .unwrap()
    .expect("exact 50.0 m3 fill in Venture Cargo must succeed");

    // 2. Full Venture cargo returns NotEnoughCargoSpace(volume=1.6, available=0.0) without partial move (#1301).
    let full_err = handle(
        &ctx,
        &mut session,
        Some(&active_bound),
        "invbroker",
        "Add",
        &[PyRep::Integer(probes_overflow), PyRep::Integer(npc_station)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    let (full_key, full_kv) = decode_user_error(&full_err);
    assert_eq!(full_key, "NotEnoughCargoSpace");
    assert!((full_kv["volume"] - 1.6).abs() < 1e-6);
    assert!((full_kv["available"] - 0.0).abs() < 1e-6);
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&probes_overflow].location_id), npc_station);

    // 3. Venture Mining Hold (flag 134, 5,000 m3) rejects probes and holds exactly 5,000 m3 of Veldspar.
    assert_eq!(
        call_add(&ctx, &mut session, &[PyRep::Integer(probes_overflow), PyRep::Integer(active_venture), PyRep::Integer(134)])
            .await
            .unwrap_err()
            .code,
        "InvalidItemTypeForHold"
    );
    let ok_ore = call_add(&ctx, &mut session, &[PyRep::Integer(veldspar_fit), PyRep::Integer(active_venture), PyRep::Integer(134)])
        .await
        .expect("5,000 m3 Veldspar fits Venture Mining Hold");
    assert_eq!(ok_ore.dict_get("flagID").and_then(|v| v.as_i64()), Some(134));
    let ore_err = call_add(&ctx, &mut session, &[PyRep::Integer(veldspar_overflow), PyRep::Integer(active_venture), PyRep::Integer(134)])
        .await
        .unwrap_err();
    let (ore_key, ore_kv) = decode_user_error(&ore_err);
    assert_eq!(ore_key, "NotEnoughSpecialBaySpace");
    assert!((ore_kv["available"] - 0.0).abs() < 1e-6);

    // 4. Venture has no Ship Maintenance Bay (flag 90): both probes and hulls return NoShipMaintenanceBay (never NotEnoughCargoSpace).
    assert_eq!(
        call_add(&ctx, &mut session, &[PyRep::Integer(probes_overflow), PyRep::Integer(active_venture), PyRep::Integer(90)])
            .await
            .unwrap_err()
            .code,
        "NoShipMaintenanceBay"
    );
    assert_eq!(
        call_add(&ctx, &mut session, &[PyRep::Integer(hull_for_sma), PyRep::Integer(active_venture), PyRep::Integer(90)])
            .await
            .unwrap_err()
            .code,
        "NoShipMaintenanceBay"
    );

    // 5. Non-active spare Venture sitting in station Ship Hangar (while active ship is another hull)
    //    must accept Add via bound GetInventoryFromId(spare_venture) onto spare_venture Cargo (NOT redirected to active ship).
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = default_rifter;
        }
    });
    session.ship_id = Some(default_rifter);
    assert!(crate::services::inventory_bound::handle(
        &ctx,
        &mut session,
        Some(&station_bound),
        "invbroker",
        "GetInventoryFromId",
        &[PyRep::Integer(spare_venture), PyRep::Integer(0)],
        None,
    )
    .unwrap()
    .is_ok());
    let spare_bound = BoundObjectEntry {
        bound_id: 952,
        session_id: session.session_id,
        user_id: char_id as u64,
        character_id: char_id,
        service: "invbroker".into(),
        object_id: spare_venture,
        oid_str: format!("N=952:{spare_venture}"),
        ref_id: 1,
    };
    let ok_spare = handle(
        &ctx,
        &mut session,
        Some(&spare_bound),
        "invbroker",
        "Add",
        &[PyRep::Integer(probes_spare), PyRep::Integer(npc_station)],
        &kw,
    )
    .await
    .unwrap()
    .expect("empty non-active Venture in station hangar accepts 1.6 m3 probes");
    assert_eq!(ok_spare.dict_get("locationID").and_then(|v| v.as_i64()), Some(spare_venture));
    assert_eq!(ok_spare.dict_get("flagID").and_then(|v| v.as_i64()), Some(5));

    // Overfilling the non-active Venture reports NotEnoughCargoSpace cleanly.
    let spare_full = handle(
        &ctx,
        &mut session,
        Some(&spare_bound),
        "invbroker",
        "Add",
        &[PyRep::Integer(filler_spare), PyRep::Integer(npc_station)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    let (sf_key, sf_kv) = decode_user_error(&spare_full);
    assert_eq!(sf_key, "NotEnoughCargoSpace");
    assert!((sf_kv["volume"] - 50.0).abs() < 1e-6);
    assert!((sf_kv["available"] - (50.0 - 1.6)).abs() < 1e-6);

    // Bring spare_venture to 50.0 m3 full and check 0.0 available for extra probes.
    let mut split_kw = HashMap::new();
    split_kw.insert("qty".into(), PyRep::Integer(484));
    handle(
        &ctx,
        &mut session,
        Some(&spare_bound),
        "invbroker",
        "Add",
        &[PyRep::Integer(filler_spare), PyRep::Integer(npc_station)],
        &split_kw,
    )
    .await
    .unwrap()
    .expect("exact 48.4 m3 split fills remaining space in non-active Venture");
    let spare_zero = handle(
        &ctx,
        &mut session,
        Some(&spare_bound),
        "invbroker",
        "Add",
        &[PyRep::Integer(probes_spare_overflow), PyRep::Integer(npc_station)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    let (sz_key, sz_kv) = decode_user_error(&spare_zero);
    assert_eq!(sz_key, "NotEnoughCargoSpace");
    assert!((sz_kv["volume"] - 1.6).abs() < 1e-6);
    assert!((sz_kv["available"] - 0.0).abs() < 1e-6);
}

fn dbrow_i64(row: &PyRep, col: &str) -> Option<i64> {
    let PyRep::DBRow { columns, .. } = row else {
        return None;
    };
    columns
        .iter()
        .find(|(name, _)| name == col)
        .and_then(|(_, val)| val.as_i64())
}

fn list_row_ids(rep: &PyRep) -> Vec<i64> {
    let PyRep::ObjectEx { list, .. } = rep else {
        panic!("expected ObjectEx rowset, got {rep:?}");
    };
    list.iter().filter_map(|r| dbrow_i64(r, "itemID")).collect()
}

fn install_r2_synthetic_types(ctx: &mut ServerContext) -> (u32, u32, u32, u32, u32, u32, u32) {
    let carrier_tid = 999_670u32;
    let hull_tid = 999_671u32;
    let mod_tid = 999_672u32;
    let rig_tid = 999_673u32;
    let drone_tid = 999_674u32;
    let charge_tid = 999_675u32;
    let box_tid = 999_676u32;

    let sde_mut = Arc::make_mut(&mut ctx.sde);
    let mut carrier = sde_mut.types[&587].clone();
    carrier.type_id = carrier_tid;
    carrier.group_id = 30;
    carrier.category_id = 6;
    carrier.name = "Synthetic R2 Titan".into();
    carrier.radius = 9_500.0;
    carrier.capacity = 5_000.0;
    carrier.volume = 100_000_000.0;
    carrier.default_attributes.insert(908, 120_000.0);
    carrier.default_attributes.insert(912, 1_000.0);
    sde_mut.types.insert(carrier_tid, carrier);

    let mut hull = sde_mut.types[&587].clone();
    hull.type_id = hull_tid;
    hull.group_id = 25;
    hull.category_id = 6;
    hull.name = "Synthetic R2 Frigate".into();
    hull.radius = 50.0;
    hull.capacity = 500.0;
    hull.volume = 50_000.0;
    sde_mut.types.insert(hull_tid, hull);

    let mut module = sde_mut.types[&34].clone();
    module.type_id = mod_tid;
    module.group_id = 53;
    module.category_id = 7;
    module.name = "Synthetic High Module".into();
    module.volume = 5.0;
    sde_mut.types.insert(mod_tid, module);

    let mut rig = sde_mut.types[&34].clone();
    rig.type_id = rig_tid;
    rig.group_id = 773;
    rig.category_id = 7;
    rig.name = "Synthetic Rig".into();
    rig.volume = 10.0;
    sde_mut.types.insert(rig_tid, rig);

    let mut drone = sde_mut.types[&34].clone();
    drone.type_id = drone_tid;
    drone.group_id = 100;
    drone.category_id = 18;
    drone.name = "Synthetic Drone".into();
    drone.volume = 5.0;
    sde_mut.types.insert(drone_tid, drone);

    let mut charge = sde_mut.types[&34].clone();
    charge.type_id = charge_tid;
    charge.group_id = 83;
    charge.category_id = 8;
    charge.name = "Synthetic Charge".into();
    charge.volume = 0.01;
    sde_mut.types.insert(charge_tid, charge);

    let mut cbox = sde_mut.types[&34].clone();
    cbox.type_id = box_tid;
    cbox.group_id = 12;
    cbox.category_id = 2;
    cbox.name = "Synthetic Bay Box".into();
    cbox.capacity = 1_000.0;
    cbox.volume = 100.0;
    cbox.radius = 20.0;
    sde_mut.types.insert(box_tid, cbox);

    (carrier_tid, hull_tid, mod_tid, rig_tid, drone_tid, charge_tid, box_tid)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1290_r2_01_blocks_bare_space_and_piloted_ship_teleport_and_fails_closed() {
    use crate::services::inventory_fitting::fitting_containers_ships::{
        set_ship_access_config_snap, ShipAccessConfig,
    };
    use eve_db::{DisconnectedShipEntry, FleetRecord};

    let (mut ctx, mut ceo_sess, ceo_id, corp_id, station_id, _ceo_default_ship) = setup_inv_ctx();
    let (carrier_tid, hull_tid, _, _, _, _, _) = install_r2_synthetic_types(&mut ctx);
    let sys_id = 30_000_142_i64;
    let mate_id = 96_670_002_i64;
    let ceo_carrier = 96_670_100_i64;
    let mate_active_ship = 96_670_101_i64;
    let disc_ship = 96_670_102_i64;
    let space_spare_ship = 96_670_103_i64;
    let foreign_space_carrier = 96_670_104_i64;
    let foreign_space_trit = 96_670_105_i64;
    let space_jetcan = 96_670_106_i64;
    let jetcan_trit = 96_670_107_i64;
    let jetcan_pyerite = 96_670_108_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().ceo_id = ceo_id;
        snap.corporations.get_mut(&corp_id).unwrap().members.insert(mate_id);
        snap.characters.get_mut(&ceo_id).unwrap().active_ship_id = ceo_carrier;

        let mut mate = snap.characters[&ceo_id].clone();
        mate.character_id = mate_id;
        mate.user_id = 702;
        mate.name = "CorpMateR2".into();
        mate.corporation_id = corp_id;
        mate.station_id = Some(station_id);
        mate.structure_id = None;
        mate.solar_system_id = sys_id as u32;
        mate.active_ship_id = mate_active_ship;
        snap.characters.insert(mate_id, mate);

        snap.fleets.insert(
            777_670,
            FleetRecord {
                fleet_id: 777_670,
                boss_id: mate_id,
                members: vec![ceo_id, mate_id],
                ..Default::default()
            },
        );

        snap.items.insert(ceo_carrier, mk_item(ceo_carrier, carrier_tid, ceo_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(mate_active_ship, mk_item(mate_active_ship, hull_tid, mate_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(disc_ship, mk_item(disc_ship, hull_tid, ceo_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.sp6_lifecycle.disconnected_ships.insert(
            mate_id,
            DisconnectedShipEntry {
                char_id: mate_id,
                ship_id: disc_ship,
                ship_type_id: hull_tid,
                solar_system_id: sys_id as u32,
                disconnected_at_unix: 1_700_000_000,
                remove_at_unix: 1_700_000_900,
                emergency_warp_dest: None,
                last_position: [0.0, 0.0, 0.0],
                last_velocity: [0.0, 0.0, 0.0],
                removed_from_space: false,
                pre_warp_position: None,
            },
        );
        // Spare owned ship floating in bare space near the system origin, NOT inside any structure or bay.
        snap.items.insert(space_spare_ship, mk_item(space_spare_ship, hull_tid, ceo_id, sys_id, InventoryFlag::Hangar, 1, true));
        snap.item_positions.insert(space_spare_ship, [100.0, 0.0, 0.0]);

        // Foreign carrier in space with NO Ballpark or item_positions -> must fail closed.
        snap.items.insert(foreign_space_carrier, mk_item(foreign_space_carrier, carrier_tid, mate_id, sys_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(foreign_space_trit, mk_item(foreign_space_trit, 34, mate_id, foreign_space_carrier, InventoryFlag::FleetHangar, 10, false));
        // Space jetcan WITH item_positions within 2,500m -> must still loot cleanly.
        snap.items.insert(space_jetcan, mk_item(space_jetcan, 23, mate_id, sys_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(jetcan_trit, mk_item(jetcan_trit, 34, mate_id, space_jetcan, InventoryFlag::Cargo, 10, false));
        snap.items.insert(jetcan_pyerite, mk_item(jetcan_pyerite, 35, mate_id, space_jetcan, InventoryFlag::Cargo, 12, false));
    });
    ctx.db.with_mut_snapshot(|snap| {
        set_ship_access_config_snap(
            snap,
            foreign_space_carrier,
            &ShipAccessConfig {
                allow_fleet_smb: true,
                allow_corp_smb: true,
                allow_fleet_hangar: true,
                allow_corp_hangar: true,
                password: None,
            },
            0,
        )
        .unwrap();
    });

    ceo_sess.ship_id = Some(ceo_carrier);
    ceo_sess.corp_role = corp_roles::ROLE_DIRECTOR;
    let kw = HashMap::new();

    // 1. Bare in-space spare ship cannot be teleported into docked carrier SMA via Add or MultiAdd.
    assert_eq!(
        call_add(&ctx, &mut ceo_sess, &[PyRep::Integer(space_spare_ship), PyRep::Integer(ceo_carrier), PyRep::Integer(90)])
            .await
            .unwrap_err()
            .code,
        "InvalidLocation"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut ceo_sess,
            None,
            "invbroker",
            "MultiAdd",
            &[PyRep::List(vec![PyRep::Integer(space_spare_ship)]), PyRep::Integer(ceo_carrier), PyRep::Integer(90)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidLocation"
    );

    // 2. CEO/Director cannot abduct corpmate's actively piloted ship or a disconnected ship via Add or MultiAdd.
    for protected_ship in [mate_active_ship, disc_ship] {
        assert_eq!(
            call_add(&ctx, &mut ceo_sess, &[PyRep::Integer(protected_ship), PyRep::Integer(ceo_carrier), PyRep::Integer(90)])
                .await
                .unwrap_err()
                .code,
            "CannotMoveActiveShip"
        );
        assert_eq!(
            handle(
                &ctx,
                &mut ceo_sess,
                None,
                "invbroker",
                "MultiAdd",
                &[PyRep::List(vec![PyRep::Integer(protected_ship)]), PyRep::Integer(ceo_carrier), PyRep::Integer(90)],
                &kw,
            )
            .await
            .unwrap()
            .unwrap_err()
            .code,
            "CannotMoveActiveShip"
        );
    }

    // 3. Undocked caller: missing Ballpark / item_positions on foreign_space_carrier fails closed on all invbroker paths.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&ceo_id).unwrap().station_id = None;
        snap.items.get_mut(&ceo_carrier).unwrap().location_id = sys_id;
        snap.item_positions.insert(ceo_carrier, [0.0, 0.0, 0.0]);
        snap.item_positions.insert(space_jetcan, [400.0, 0.0, 0.0]);
    });
    ceo_sess.station_id = None;
    let space_bound = BoundObjectEntry {
        bound_id: 970,
        session_id: ceo_sess.session_id,
        user_id: ceo_id as u64,
        character_id: ceo_id,
        service: "invbroker".into(),
        object_id: sys_id,
        oid_str: format!("N=970:{sys_id}"),
        ref_id: 1,
    };
    let foreign_bound = BoundObjectEntry {
        bound_id: 971,
        session_id: ceo_sess.session_id,
        user_id: ceo_id as u64,
        character_id: ceo_id,
        service: "invbroker".into(),
        object_id: foreign_space_carrier,
        oid_str: format!("N=971:{foreign_space_carrier}"),
        ref_id: 1,
    };
    assert!(crate::services::inventory_bound::handle(
        &ctx,
        &mut ceo_sess,
        Some(&space_bound),
        "invbroker",
        "GetInventoryFromId",
        &[PyRep::Integer(foreign_space_carrier), PyRep::Integer(0)],
        None,
    )
    .unwrap()
    .is_err());
    assert!(crate::services::inventory_bound::handle(
        &ctx,
        &mut ceo_sess,
        Some(&foreign_bound),
        "invbroker",
        "GetSelfInvItem",
        &[],
        None,
    )
    .unwrap()
    .is_err());
    assert!(handle(&ctx, &mut ceo_sess, Some(&foreign_bound), "invbroker", "List", &[], &kw)
        .await
        .unwrap()
        .is_err());
    assert!(call_add(&ctx, &mut ceo_sess, &[PyRep::Integer(foreign_space_trit), PyRep::Integer(ceo_carrier), PyRep::Integer(5)])
        .await
        .is_err());
    assert!(handle(
        &ctx,
        &mut ceo_sess,
        None,
        "invbroker",
        "MultiAdd",
        &[PyRep::List(vec![PyRep::Integer(foreign_space_trit)]), PyRep::Integer(ceo_carrier), PyRep::Integer(5)],
        &kw,
    )
    .await
    .unwrap()
    .is_err());

    // 4. Jetcan WITH item_positions within 2,500m loots cleanly via both Add and MultiAdd.
    call_add(&ctx, &mut ceo_sess, &[PyRep::Integer(jetcan_trit), PyRep::Integer(ceo_carrier), PyRep::Integer(5)])
        .await
        .expect("Add loot from nearby jetcan must succeed");
    handle(
        &ctx,
        &mut ceo_sess,
        None,
        "invbroker",
        "MultiAdd",
        &[PyRep::List(vec![PyRep::Integer(jetcan_pyerite)]), PyRep::Integer(ceo_carrier), PyRep::Integer(5)],
        &kw,
    )
    .await
    .unwrap()
    .expect("MultiAdd loot from nearby jetcan must succeed");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&jetcan_trit].location_id, ceo_carrier);
        assert_eq!(snap.items[&jetcan_pyerite].location_id, ceo_carrier);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1290_r2_02_recursive_sma_owner_transfer_and_nested_bay_containers() {
    use crate::services::inventory_fitting::fitting_containers_ships::{
        set_ship_access_config_snap, ShipAccessConfig,
    };
    use eve_db::FleetRecord;

    let (mut ctx, owner_sess, owner_id, _owner_corp, station_id, _default_ship) = setup_inv_ctx();
    let (carrier_tid, hull_tid, mod_tid, rig_tid, drone_tid, charge_tid, box_tid) =
        install_r2_synthetic_types(&mut ctx);
    let mate_id = 96_671_002_i64;
    let stranger_id = 96_671_003_i64;
    let owner_carrier = 96_671_100_i64;
    let mate_ship = 96_671_101_i64;
    let stored_hull = 96_671_102_i64;
    let fitted_mod = 96_671_103_i64;
    let fitted_rig = 96_671_104_i64;
    let stored_drone = 96_671_105_i64;
    let stored_charge = 96_671_106_i64;
    let fh_box = 96_671_110_i64;
    let fh_box_existing = 96_671_111_i64;
    let mate_box_trit = 96_671_112_i64;
    let mate_box_pyerite = 96_671_113_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&owner_id).unwrap().active_ship_id = owner_carrier;
        for (cid, uid, name, active_ship) in [
            (mate_id, 711_u64, "FleetMateA", mate_ship),
            (stranger_id, 712_u64, "StrangerA", 96_671_199_i64),
        ] {
            let mut ch = snap.characters[&owner_id].clone();
            ch.character_id = cid;
            ch.user_id = uid;
            ch.name = name.into();
            ch.corporation_id = 98_671_000 + cid;
            ch.station_id = Some(station_id);
            ch.structure_id = None;
            ch.active_ship_id = active_ship;
            snap.characters.insert(cid, ch);
        }
        snap.fleets.insert(
            777_671,
            FleetRecord {
                fleet_id: 777_671,
                boss_id: owner_id,
                members: vec![owner_id, mate_id],
                ..Default::default()
            },
        );
        snap.items.insert(owner_carrier, mk_item(owner_carrier, carrier_tid, owner_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(mate_ship, mk_item(mate_ship, hull_tid, mate_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(stored_hull, mk_item(stored_hull, hull_tid, mate_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(fitted_mod, mk_item(fitted_mod, mod_tid, mate_id, stored_hull, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(fitted_rig, mk_item(fitted_rig, rig_tid, mate_id, stored_hull, InventoryFlag::RigSlot0, 1, true));
        snap.items.insert(stored_drone, mk_item(stored_drone, drone_tid, mate_id, stored_hull, InventoryFlag::DroneBay, 2, false));
        snap.items.insert(stored_charge, mk_item(stored_charge, charge_tid, mate_id, stored_hull, InventoryFlag::Cargo, 50, false));
        snap.items.insert(fh_box, mk_item(fh_box, box_tid, owner_id, owner_carrier, InventoryFlag::FleetHangar, 1, true));
        snap.items.insert(fh_box_existing, mk_item(fh_box_existing, charge_tid, owner_id, fh_box, InventoryFlag::Cargo, 7, false));
        snap.items.insert(mate_box_trit, mk_item(mate_box_trit, 34, mate_id, station_id, InventoryFlag::Hangar, 9, false));
        snap.items.insert(mate_box_pyerite, mk_item(mate_box_pyerite, 35, mate_id, station_id, InventoryFlag::Hangar, 11, false));
    });
    let now_ts = ctx.now_unix();
    let owner_uid = ctx.db.with_snapshot(|s| s.characters[&owner_id].user_id);
    let _ = ctx.db.acquire_online_lock(owner_uid, 2001, Some(owner_id), true, now_ts);
    let _ = ctx.db.acquire_online_lock(711, 2002, Some(mate_id), true, now_ts);

    ctx.db.with_mut_snapshot(|snap| {
        set_ship_access_config_snap(
            snap,
            owner_carrier,
            &ShipAccessConfig {
                allow_fleet_smb: true,
                allow_corp_smb: false,
                allow_fleet_hangar: true,
                allow_corp_hangar: false,
                password: Some("bay-pw".into()),
            },
            0,
        )
        .unwrap();
    });

    let mut owner_live_sess = owner_sess;
    owner_live_sess.ship_id = Some(owner_carrier);
    let mut mate_sess = SessionState::new(3);
    mate_sess.user_id = Some(711);
    mate_sess.character_id = Some(mate_id);
    mate_sess.corporation_id = Some(98_671_000 + mate_id);
    mate_sess.station_id = Some(station_id);
    mate_sess.solar_system_id = Some(30_000_142);
    mate_sess.ship_id = Some(mate_ship);

    let mut stranger_sess = SessionState::new(4);
    stranger_sess.user_id = Some(712);
    stranger_sess.character_id = Some(stranger_id);
    stranger_sess.corporation_id = Some(98_671_000 + stranger_id);
    stranger_sess.station_id = Some(station_id);
    stranger_sess.solar_system_id = Some(30_000_142);
    stranger_sess.ship_id = Some(96_671_199);

    let kw = HashMap::new();
    let mate_station_bound = BoundObjectEntry {
        bound_id: 972,
        session_id: mate_sess.session_id,
        user_id: 711,
        character_id: mate_id,
        service: "invbroker".into(),
        object_id: station_id,
        oid_str: format!("N=972:{station_id}"),
        ref_id: 1,
    };
    let mate_box_bound = BoundObjectEntry {
        bound_id: 973,
        session_id: mate_sess.session_id,
        user_id: 711,
        character_id: mate_id,
        service: "invbroker".into(),
        object_id: fh_box,
        oid_str: format!("N=973:{fh_box}"),
        ref_id: 1,
    };
    let owner_stored_ship_bound = BoundObjectEntry {
        bound_id: 974,
        session_id: owner_live_sess.session_id,
        user_id: owner_uid,
        character_id: owner_id,
        service: "invbroker".into(),
        object_id: stored_hull,
        oid_str: format!("N=974:{stored_hull}"),
        ref_id: 1,
    };

    // 1. Nested container inside locked foreign FleetHangar rejects BEFORE password unlock.
    assert!(crate::services::inventory_bound::handle(
        &ctx,
        &mut mate_sess,
        Some(&mate_station_bound),
        "invbroker",
        "GetInventoryFromId",
        &[PyRep::Integer(fh_box), PyRep::Integer(0)],
        None,
    )
    .unwrap()
    .is_err());
    assert_eq!(
        call_add(&ctx, &mut mate_sess, &[PyRep::Integer(mate_box_trit), PyRep::Integer(fh_box), PyRep::Integer(5)])
            .await
            .unwrap_err()
            .code,
        "FleetHangarAccessDenied"
    );

    // 2. Positional password in Add unlocks the ancestor bay, moves into fh_box, sets owner_id = owner_id, and notifies owner_id.
    let _ = ctx.drain_notifications_for_char(owner_id);
    call_add(
        &ctx,
        &mut mate_sess,
        &[PyRep::Integer(mate_box_trit), PyRep::Integer(fh_box), PyRep::Integer(5), PyRep::String("bay-pw".into())],
    )
    .await
    .expect("Add into nested FleetHangar box with positional password");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&mate_box_trit].location_id, fh_box);
        assert_eq!(snap.items[&mate_box_trit].owner_id, owner_id);
    });
    assert!(ctx.drain_notifications_for_char(owner_id).iter().any(|n| {
        n.event == "OnItemChange" && dbrow_i64(&n.args[0], "itemID") == Some(mate_box_trit)
    }));

    // 3. After unlock: GetInventoryFromId(fh_box), GetSelfInvItem(fh_box), and List(fh_box) succeed for mate_id; MultiAdd also transfers owner_id & notifies.
    assert!(crate::services::inventory_bound::handle(
        &ctx,
        &mut mate_sess,
        Some(&mate_station_bound),
        "invbroker",
        "GetInventoryFromId",
        &[PyRep::Integer(fh_box), PyRep::Integer(0)],
        None,
    )
    .unwrap()
    .is_ok());
    let self_item = crate::services::inventory_bound::handle(
        &ctx,
        &mut mate_sess,
        Some(&mate_box_bound),
        "invbroker",
        "GetSelfInvItem",
        &[],
        None,
    )
    .unwrap()
    .expect("GetSelfInvItem on nested box inside unlocked foreign FleetHangar");
    assert_eq!(dbrow_i64(&self_item, "itemID"), Some(fh_box));
    let listed = list_row_ids(
        &handle(&ctx, &mut mate_sess, Some(&mate_box_bound), "invbroker", "List", &[], &kw)
            .await
            .unwrap()
            .expect("List on nested box inside unlocked foreign FleetHangar"),
    );
    assert!(listed.contains(&fh_box_existing));
    assert!(listed.contains(&mate_box_trit));

    let _ = ctx.drain_notifications_for_char(owner_id);
    handle(
        &ctx,
        &mut mate_sess,
        None,
        "invbroker",
        "MultiAdd",
        &[
            PyRep::List(vec![PyRep::Integer(mate_box_pyerite)]),
            PyRep::Integer(fh_box),
            PyRep::Integer(5),
            PyRep::String("bay-pw".into()),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("MultiAdd into nested FleetHangar box");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&mate_box_pyerite].owner_id, owner_id);
        assert_eq!(snap.items[&mate_box_pyerite].location_id, fh_box);
    });
    assert!(ctx.drain_notifications_for_char(owner_id).iter().any(|n| {
        n.event == "OnItemChange" && dbrow_i64(&n.args[0], "itemID") == Some(mate_box_pyerite)
    }));

    // Stranger is always rejected on nested box even after fleetmate unlocked it.
    let stranger_station_bound = BoundObjectEntry {
        bound_id: 975,
        session_id: stranger_sess.session_id,
        user_id: 712,
        character_id: stranger_id,
        service: "invbroker".into(),
        object_id: station_id,
        oid_str: format!("N=975:{station_id}"),
        ref_id: 1,
    };
    let stranger_box_bound = BoundObjectEntry {
        bound_id: 976,
        session_id: stranger_sess.session_id,
        user_id: 712,
        character_id: stranger_id,
        service: "invbroker".into(),
        object_id: fh_box,
        oid_str: format!("N=976:{fh_box}"),
        ref_id: 1,
    };
    assert!(crate::services::inventory_bound::handle(
        &ctx,
        &mut stranger_sess,
        Some(&stranger_station_bound),
        "invbroker",
        "GetInventoryFromId",
        &[PyRep::Integer(fh_box), PyRep::Integer(0)],
        None,
    )
    .unwrap()
    .is_err());
    assert!(handle(&ctx, &mut stranger_sess, Some(&stranger_box_bound), "invbroker", "List", &[], &kw)
        .await
        .unwrap()
        .is_err());

    // 4. Deposit fitted hull into foreign SMA (positional password): hull + fitted mod + rig + drone + charge ALL transfer to owner_id,
    //    notify owner_id via OnItemChange, and appear in owner_id's List on stored_hull; extracting back transfers all to mate_id.
    let _ = ctx.drain_notifications_for_char(owner_id);
    call_add(
        &ctx,
        &mut mate_sess,
        &[PyRep::Integer(stored_hull), PyRep::Integer(owner_carrier), PyRep::Integer(90), PyRep::String("bay-pw".into())],
    )
    .await
    .expect("deposit fitted ship into foreign SMA with positional password");
    ctx.db.with_snapshot(|snap| {
        for &id in &[stored_hull, fitted_mod, fitted_rig, stored_drone, stored_charge] {
            assert_eq!(snap.items[&id].owner_id, owner_id, "child {id} must transfer owner to ship owner");
        }
    });
    let owner_notifs = ctx.drain_notifications_for_char(owner_id);
    for &id in &[stored_hull, fitted_mod, fitted_rig, stored_drone, stored_charge] {
        assert!(
            owner_notifs
                .iter()
                .any(|n| n.event == "OnItemChange" && dbrow_i64(&n.args[0], "itemID") == Some(id)),
            "expected OnItemChange for child item {id}"
        );
    }
    let owner_ship_contents = list_row_ids(
        &handle(&ctx, &mut owner_live_sess, Some(&owner_stored_ship_bound), "invbroker", "List", &[], &kw)
            .await
            .unwrap()
            .expect("carrier owner List on SMA-stored ship"),
    );
    for &id in &[fitted_mod, fitted_rig, stored_drone, stored_charge] {
        assert!(owner_ship_contents.contains(&id), "owner List missing {id}");
    }

    call_add(
        &ctx,
        &mut mate_sess,
        &[PyRep::Integer(stored_hull), PyRep::Integer(station_id), PyRep::Integer(4)],
    )
    .await
    .expect("extract fitted ship back from unlocked SMA");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&stored_hull].location_id, station_id);
        for &id in &[stored_hull, fitted_mod, fitted_rig, stored_drone, stored_charge] {
            assert_eq!(snap.items[&id].owner_id, mate_id, "extracted {id} must transfer owner back to mate");
        }
    });
}

#[test]
fn bf_1290_r2_03_space_titan_surface_range_self_inv_item_and_no_tokio_panic() {
    use crate::services::inventory_fitting::fitting_containers_ships::{
        set_ship_access_config_snap, ShipAccessConfig,
    };
    use eve_db::FleetRecord;

    let (mut ctx, _owner_sess, owner_id, _owner_corp, _station_id, _default_ship) = setup_inv_ctx();
    let (carrier_tid, hull_tid, _, _, _, _, box_tid) = install_r2_synthetic_types(&mut ctx);
    let sys_id = 30_000_142_i64;
    let mate_id = 96_672_002_i64;
    let owner_titan = 96_672_100_i64;
    let mate_ship = 96_672_101_i64;
    let titan_fh_box = 96_672_102_i64;
    let titan_fh_trit = 96_672_103_i64;
    let box_trit = 96_672_104_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&owner_id).unwrap().station_id = None;
        snap.characters.get_mut(&owner_id).unwrap().active_ship_id = owner_titan;

        let mut mate = snap.characters[&owner_id].clone();
        mate.character_id = mate_id;
        mate.user_id = 722;
        mate.name = "SpaceMate".into();
        mate.corporation_id = 98_672_002;
        mate.station_id = None;
        mate.structure_id = None;
        mate.solar_system_id = sys_id as u32;
        mate.active_ship_id = mate_ship;
        snap.characters.insert(mate_id, mate);

        snap.fleets.insert(
            777_672,
            FleetRecord {
                fleet_id: 777_672,
                boss_id: owner_id,
                members: vec![owner_id, mate_id],
                ..Default::default()
            },
        );

        snap.items.insert(owner_titan, mk_item(owner_titan, carrier_tid, owner_id, sys_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(mate_ship, mk_item(mate_ship, hull_tid, mate_id, sys_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(titan_fh_trit, mk_item(titan_fh_trit, 34, owner_id, owner_titan, InventoryFlag::FleetHangar, 15, false));
        snap.items.insert(titan_fh_box, mk_item(titan_fh_box, box_tid, owner_id, owner_titan, InventoryFlag::FleetHangar, 1, true));
        snap.items.insert(box_trit, mk_item(box_trit, 34, owner_id, titan_fh_box, InventoryFlag::Cargo, 18, false));

        // Titan radius = 9,500m, mate frigate radius = 50m -> center dist 11,500m => surface dist 1,950m (<= 2,500m).
        snap.item_positions.insert(owner_titan, [11_500.0, 0.0, 0.0]);
        snap.item_positions.insert(mate_ship, [0.0, 0.0, 0.0]);
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
                password: None,
            },
            0,
        )
        .unwrap();
    });

    let mut mate_sess = SessionState::new(5);
    mate_sess.user_id = Some(722);
    mate_sess.character_id = Some(mate_id);
    mate_sess.corporation_id = Some(98_672_002);
    mate_sess.solar_system_id = Some(sys_id as u32);
    mate_sess.ship_id = Some(mate_ship);

    let kw = HashMap::new();
    let space_bound = BoundObjectEntry {
        bound_id: 980,
        session_id: mate_sess.session_id,
        user_id: 722,
        character_id: mate_id,
        service: "invbroker".into(),
        object_id: sys_id,
        oid_str: format!("N=980:{sys_id}"),
        ref_id: 1,
    };
    let titan_bound = BoundObjectEntry {
        bound_id: 981,
        session_id: mate_sess.session_id,
        user_id: 722,
        character_id: mate_id,
        service: "invbroker".into(),
        object_id: owner_titan,
        oid_str: format!("N=981:{owner_titan}"),
        ref_id: 1,
    };
    let box_bound = BoundObjectEntry {
        bound_id: 982,
        session_id: mate_sess.session_id,
        user_id: 722,
        character_id: mate_id,
        service: "invbroker".into(),
        object_id: titan_fh_box,
        oid_str: format!("N=982:{titan_fh_box}"),
        ref_id: 1,
    };

    // Synchronous test outside Tokio runtime: none of GetInventoryFromId / GetSelfInvItem / handle_list panic,
    // and all succeed at 1,950m surface distance (11,500m center distance > 2,500m!).
    for target in [owner_titan, titan_fh_box] {
        assert!(
            crate::services::inventory_bound::handle(
                &ctx,
                &mate_sess,
                Some(&space_bound),
                "invbroker",
                "GetInventoryFromId",
                &[PyRep::Integer(target), PyRep::Integer(0)],
                None,
            )
            .unwrap()
            .is_ok(),
            "GetInventoryFromId({target}) must succeed at 1,950m surface range without Tokio runtime"
        );
    }
    let titan_row = crate::services::inventory_bound::handle(&ctx, &mate_sess, Some(&titan_bound), "invbroker", "GetSelfInvItem", &[], None)
        .unwrap()
        .expect("GetSelfInvItem on foreign Titan");
    assert_eq!(dbrow_i64(&titan_row, "itemID"), Some(owner_titan));
    let box_row = crate::services::inventory_bound::handle(&ctx, &mate_sess, Some(&box_bound), "invbroker", "GetSelfInvItem", &[], None)
        .unwrap()
        .expect("GetSelfInvItem on nested box inside foreign Titan");
    assert_eq!(dbrow_i64(&box_row, "itemID"), Some(titan_fh_box));

    assert!(list_row_ids(
        &crate::services::inventory_stacking::handle_list(&ctx, &mate_sess, Some(&titan_bound), &[], &kw)
            .expect("sync handle_list on in-space Titan")
    )
    .contains(&titan_fh_trit));
    assert!(list_row_ids(
        &crate::services::inventory_stacking::handle_list(&ctx, &mate_sess, Some(&box_bound), &[], &kw)
            .expect("sync handle_list on nested box inside Titan")
    )
    .contains(&box_trit));

    // Move Titan out to 12,100m center => 2,550m surface distance (> 2,500m) -> all reject cleanly.
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(owner_titan, [12_100.0, 0.0, 0.0]);
    });
    assert_eq!(
        crate::services::inventory_bound::handle(
            &ctx,
            &mate_sess,
            Some(&space_bound),
            "invbroker",
            "GetInventoryFromId",
            &[PyRep::Integer(owner_titan), PyRep::Integer(0)],
            None,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InventoryNotAccessible"
    );
    assert!(
        crate::services::inventory_bound::handle(&ctx, &mate_sess, Some(&box_bound), "invbroker", "GetSelfInvItem", &[], None)
            .unwrap()
            .is_err()
    );
    assert_eq!(
        crate::services::inventory_stacking::handle_list(&ctx, &mate_sess, Some(&titan_bound), &[], &kw)
            .unwrap_err()
            .code,
        "OutOfRange"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1290_r2_03_space_titan_surface_range_add_and_multiadd() {
    use crate::services::inventory_fitting::fitting_containers_ships::{
        set_ship_access_config_snap, ShipAccessConfig,
    };
    use eve_db::FleetRecord;

    let (mut ctx, _owner_sess, owner_id, _owner_corp, _station_id, _default_ship) = setup_inv_ctx();
    let (carrier_tid, hull_tid, _, _, _, _, _) = install_r2_synthetic_types(&mut ctx);
    let sys_id = 30_000_142_i64;
    let mate_id = 96_672_202_i64;
    let owner_titan = 96_672_210_i64;
    let mate_ship = 96_672_211_i64;
    let mate_trit_1 = 96_672_212_i64;
    let mate_trit_2 = 96_672_213_i64;
    let mate_trit_far = 96_672_214_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&owner_id).unwrap().station_id = None;
        snap.characters.get_mut(&owner_id).unwrap().active_ship_id = owner_titan;
        let mut mate = snap.characters[&owner_id].clone();
        mate.character_id = mate_id;
        mate.user_id = 723;
        mate.name = "SpaceMover".into();
        mate.corporation_id = 98_672_202;
        mate.station_id = None;
        mate.structure_id = None;
        mate.solar_system_id = sys_id as u32;
        mate.active_ship_id = mate_ship;
        snap.characters.insert(mate_id, mate);
        snap.fleets.insert(
            777_673,
            FleetRecord {
                fleet_id: 777_673,
                boss_id: owner_id,
                members: vec![owner_id, mate_id],
                ..Default::default()
            },
        );
        snap.items.insert(owner_titan, mk_item(owner_titan, carrier_tid, owner_id, sys_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(mate_ship, mk_item(mate_ship, hull_tid, mate_id, sys_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(mate_trit_1, mk_item(mate_trit_1, 34, mate_id, mate_ship, InventoryFlag::Cargo, 5, false));
        snap.items.insert(mate_trit_2, mk_item(mate_trit_2, 35, mate_id, mate_ship, InventoryFlag::Cargo, 5, false));
        snap.items.insert(mate_trit_far, mk_item(mate_trit_far, 36, mate_id, mate_ship, InventoryFlag::Cargo, 5, false));
        snap.item_positions.insert(owner_titan, [11_500.0, 0.0, 0.0]);
        snap.item_positions.insert(mate_ship, [0.0, 0.0, 0.0]);
    });
    ctx.db.with_mut_snapshot(|snap| {
        set_ship_access_config_snap(
            snap,
            owner_titan,
            &ShipAccessConfig {
                allow_fleet_smb: false,
                allow_corp_smb: false,
                allow_fleet_hangar: true,
                allow_corp_hangar: false,
                password: None,
            },
            0,
        )
        .unwrap();
    });
    let mut mate_sess = SessionState::new(6);
    mate_sess.user_id = Some(723);
    mate_sess.character_id = Some(mate_id);
    mate_sess.corporation_id = Some(98_672_202);
    mate_sess.solar_system_id = Some(sys_id as u32);
    mate_sess.ship_id = Some(mate_ship);
    let kw = HashMap::new();

    // 1,950m surface distance succeeds on Add & MultiAdd into foreign Titan FleetHangar.
    call_add(&ctx, &mut mate_sess, &[PyRep::Integer(mate_trit_1), PyRep::Integer(owner_titan), PyRep::Integer(155)])
        .await
        .expect("Add within 2,500m Titan surface distance");
    handle(
        &ctx,
        &mut mate_sess,
        None,
        "invbroker",
        "MultiAdd",
        &[PyRep::List(vec![PyRep::Integer(mate_trit_2)]), PyRep::Integer(owner_titan), PyRep::Integer(155)],
        &kw,
    )
    .await
    .unwrap()
    .expect("MultiAdd within 2,500m Titan surface distance");

    // 2,550m surface distance rejects cleanly on both Add & MultiAdd.
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(owner_titan, [12_100.0, 0.0, 0.0]);
    });
    assert!(call_add(&ctx, &mut mate_sess, &[PyRep::Integer(mate_trit_far), PyRep::Integer(owner_titan), PyRep::Integer(155)])
        .await
        .is_err());
    assert!(handle(
        &ctx,
        &mut mate_sess,
        None,
        "invbroker",
        "MultiAdd",
        &[PyRep::List(vec![PyRep::Integer(mate_trit_far)]), PyRep::Integer(owner_titan), PyRep::Integer(155)],
        &kw,
    )
    .await
    .unwrap()
    .is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1290_r2_04_multiadd_sma_rules_positional_pw_and_split_notify() {
    use crate::services::inventory_fitting::fitting_containers_ships::{
        set_ship_access_config_snap, ShipAccessConfig,
    };
    use eve_db::FleetRecord;

    let (mut ctx, _owner_sess, owner_id, _owner_corp, station_id, _default_ship) = setup_inv_ctx();
    let (carrier_tid, hull_tid, _, _, _, _, _) = install_r2_synthetic_types(&mut ctx);
    let mate_id = 96_673_002_i64;
    let owner_carrier = 96_673_100_i64;
    let mate_ship = 96_673_101_i64;
    let owner_fh_trit = 96_673_102_i64;
    let mate_trit_pw = 96_673_103_i64;
    let unpackaged_hull = 96_673_104_i64;
    let loaded_hull = 96_673_105_i64;
    let loaded_cargo = 96_673_106_i64;
    let hull_a = 96_673_107_i64;
    let hull_b = 96_673_108_i64;
    let hull_c = 96_673_109_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&owner_id).unwrap().active_ship_id = owner_carrier;
        let mut mate = snap.characters[&owner_id].clone();
        mate.character_id = mate_id;
        mate.user_id = 732;
        mate.name = "MateR204".into();
        mate.corporation_id = 98_673_002;
        mate.station_id = Some(station_id);
        mate.structure_id = None;
        mate.active_ship_id = mate_ship;
        snap.characters.insert(mate_id, mate);
        snap.fleets.insert(
            777_674,
            FleetRecord {
                fleet_id: 777_674,
                boss_id: owner_id,
                members: vec![owner_id, mate_id],
                ..Default::default()
            },
        );
        snap.items.insert(owner_carrier, mk_item(owner_carrier, carrier_tid, owner_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(mate_ship, mk_item(mate_ship, hull_tid, mate_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(owner_fh_trit, mk_item(owner_fh_trit, 34, owner_id, owner_carrier, InventoryFlag::FleetHangar, 20, false));
        snap.items.insert(mate_trit_pw, mk_item(mate_trit_pw, 34, mate_id, station_id, InventoryFlag::Hangar, 5, false));
        snap.items.insert(unpackaged_hull, mk_item(unpackaged_hull, hull_tid, mate_id, station_id, InventoryFlag::Hangar, 1, false));
        snap.items.insert(loaded_hull, mk_item(loaded_hull, hull_tid, mate_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(loaded_cargo, mk_item(loaded_cargo, 34, mate_id, loaded_hull, InventoryFlag::Cargo, 5, false));
        // carrier SMA capacity is 120,000 m3 and hull_tid is 50,000 m3 -> 2 hulls fit (100k), 3 hulls overflow (150k > 120k)!
        snap.items.insert(hull_a, mk_item(hull_a, hull_tid, mate_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(hull_b, mk_item(hull_b, hull_tid, mate_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(hull_c, mk_item(hull_c, hull_tid, mate_id, station_id, InventoryFlag::Hangar, 1, true));
    });
    let now_ts = ctx.now_unix();
    let owner_uid = ctx.db.with_snapshot(|s| s.characters[&owner_id].user_id);
    let _ = ctx.db.acquire_online_lock(owner_uid, 2005, Some(owner_id), true, now_ts);

    ctx.db.with_mut_snapshot(|snap| {
        set_ship_access_config_snap(
            snap,
            owner_carrier,
            &ShipAccessConfig {
                allow_fleet_smb: true,
                allow_corp_smb: false,
                allow_fleet_hangar: true,
                allow_corp_hangar: false,
                password: Some("bay-pw".into()),
            },
            0,
        )
        .unwrap();
    });

    let mut mate_sess = SessionState::new(7);
    mate_sess.user_id = Some(732);
    mate_sess.character_id = Some(mate_id);
    mate_sess.corporation_id = Some(98_673_002);
    mate_sess.station_id = Some(station_id);
    mate_sess.solar_system_id = Some(30_000_142);
    mate_sess.ship_id = Some(mate_ship);
    let kw = HashMap::new();

    // 1. MultiAdd into SMA enforces non-ship, unpackaged, non-charge cargo, and batch capacity checks (positional pw accepted).
    assert_eq!(
        handle(
            &ctx,
            &mut mate_sess,
            None,
            "invbroker",
            "MultiAdd",
            &[PyRep::List(vec![PyRep::Integer(mate_trit_pw)]), PyRep::Integer(owner_carrier), PyRep::Integer(90), PyRep::String("bay-pw".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CannotStoreCargoInSMA"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut mate_sess,
            None,
            "invbroker",
            "MultiAdd",
            &[PyRep::List(vec![PyRep::Integer(unpackaged_hull)]), PyRep::Integer(owner_carrier), PyRep::Integer(90), PyRep::String("bay-pw".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "OnlyAssembledShipsInSMA"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut mate_sess,
            None,
            "invbroker",
            "MultiAdd",
            &[PyRep::List(vec![PyRep::Integer(loaded_hull)]), PyRep::Integer(owner_carrier), PyRep::Integer(90), PyRep::String("bay-pw".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ShipHasNonChargeCargo"
    );
    let overflow_err = handle(
        &ctx,
        &mut mate_sess,
        None,
        "invbroker",
        "MultiAdd",
        &[
            PyRep::List(vec![PyRep::Integer(hull_a), PyRep::Integer(hull_b), PyRep::Integer(hull_c)]),
            PyRep::Integer(owner_carrier),
            PyRep::Integer(90),
            PyRep::String("bay-pw".into()),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    let (ov_key, ov_kv) = decode_user_error(&overflow_err);
    assert_eq!(ov_key, "NotEnoughSpecialBaySpace");
    assert_eq!(ov_kv.get("volume").copied(), Some(150_000.0));
    assert_eq!(ov_kv.get("available").copied(), Some(120_000.0));

    // Positional password works on MultiAdd into SMA when batch fits (2 * 50,000 <= 120,000), setting custom flag 90 & owner_id.
    handle(
        &ctx,
        &mut mate_sess,
        None,
        "invbroker",
        "MultiAdd",
        &[
            PyRep::List(vec![PyRep::Integer(hull_a), PyRep::Integer(hull_b)]),
            PyRep::Integer(owner_carrier),
            PyRep::Integer(90),
            PyRep::String("bay-pw".into()),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("MultiAdd batch into SMA with positional password");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&hull_a].flag, InventoryFlag::ShipHangar);
        assert_eq!(snap.items[&hull_a].owner_id, owner_id);
        assert_eq!(snap.items[&hull_b].flag, InventoryFlag::ShipHangar);
        assert_eq!(snap.items[&hull_b].owner_id, owner_id);
    });

    // 2. Split-extraction from foreign FleetHangar notifies owner_id ONLY for orig (qty 20->14) and NEVER for mate's new station stack.
    let _ = ctx.drain_notifications_for_char(owner_id);
    let mut split_kw = HashMap::new();
    split_kw.insert("qty".into(), PyRep::Integer(6));
    let split_rep = handle(
        &ctx,
        &mut mate_sess,
        None,
        "invbroker",
        "Add",
        &[PyRep::Integer(owner_fh_trit), PyRep::Integer(station_id), PyRep::Integer(4)],
        &split_kw,
    )
    .await
    .unwrap()
    .expect("split extraction from unlocked foreign FleetHangar");
    let new_split_id = split_rep.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    assert_ne!(new_split_id, owner_fh_trit);

    let owner_notifs = ctx.drain_notifications_for_char(owner_id);
    assert!(
        owner_notifs
            .iter()
            .any(|n| n.event == "OnItemChange" && dbrow_i64(&n.args[0], "itemID") == Some(owner_fh_trit)),
        "ship owner must be notified when original FleetHangar stack quantity shrinks"
    );
    assert!(
        !owner_notifs.iter().any(|n| {
            (n.event == "OnItemChange" && dbrow_i64(&n.args[0], "itemID") == Some(new_split_id))
                || (n.event == "ProcessInventoryChange" && n.args.first().and_then(|v| v.as_i64()) == Some(new_split_id))
        }),
        "ship owner must NOT receive OnItemChange or ProcessInventoryChange for mate's private extracted stack {new_split_id}: {owner_notifs:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1290_r3_split_stack_and_multiadd_recursive_owner_invariants() {
    use crate::services::inventory_fitting::fitting_containers_ships::{
        set_ship_access_config_snap, ShipAccessConfig,
    };
    use eve_db::FleetRecord;

    let (mut ctx, _owner_sess, owner_id, _owner_corp, station_id, _default_ship) = setup_inv_ctx();
    let (carrier_tid, hull_tid, mod_tid, rig_tid, drone_tid, charge_tid, box_tid) =
        install_r2_synthetic_types(&mut ctx);
    let sys_id = 30_000_142_i64;
    let mate_id = 96_674_002_i64;
    let owner_titan = 96_674_100_i64;
    let mate_ship = 96_674_101_i64;
    let docked_fh_trit = 96_674_102_i64;
    let docked_cargo_trit = 96_674_103_i64;
    let mate_hangar_trit = 96_674_104_i64;
    let fitted_hull = 96_674_105_i64;
    let hull_mod = 96_674_106_i64;
    let hull_rig = 96_674_107_i64;
    let hull_drone = 96_674_108_i64;
    let hull_charge = 96_674_109_i64;
    let packed_box = 96_674_110_i64;
    let box_trit = 96_674_111_i64;
    let bare_space_trit = 96_674_112_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&owner_id).unwrap().active_ship_id = owner_titan;
        let mut mate = snap.characters[&owner_id].clone();
        mate.character_id = mate_id;
        mate.user_id = 742;
        mate.name = "MateSplitR3".into();
        mate.corporation_id = 98_674_002;
        mate.station_id = Some(station_id);
        mate.structure_id = None;
        mate.solar_system_id = sys_id as u32;
        mate.active_ship_id = mate_ship;
        snap.characters.insert(mate_id, mate);
        snap.fleets.insert(
            777_675,
            FleetRecord {
                fleet_id: 777_675,
                boss_id: owner_id,
                members: vec![owner_id, mate_id],
                ..Default::default()
            },
        );
        snap.items.insert(owner_titan, mk_item(owner_titan, carrier_tid, owner_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(mate_ship, mk_item(mate_ship, hull_tid, mate_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(docked_fh_trit, mk_item(docked_fh_trit, 34, owner_id, owner_titan, InventoryFlag::FleetHangar, 30, false));
        snap.items.insert(docked_cargo_trit, mk_item(docked_cargo_trit, 34, owner_id, owner_titan, InventoryFlag::Cargo, 30, false));
        snap.items.insert(mate_hangar_trit, mk_item(mate_hangar_trit, 34, mate_id, station_id, InventoryFlag::Hangar, 20, false));
        snap.items.insert(fitted_hull, mk_item(fitted_hull, hull_tid, mate_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(hull_mod, mk_item(hull_mod, mod_tid, mate_id, fitted_hull, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(hull_rig, mk_item(hull_rig, rig_tid, mate_id, fitted_hull, InventoryFlag::RigSlot0, 1, true));
        snap.items.insert(hull_drone, mk_item(hull_drone, drone_tid, mate_id, fitted_hull, InventoryFlag::DroneBay, 2, false));
        snap.items.insert(hull_charge, mk_item(hull_charge, charge_tid, mate_id, fitted_hull, InventoryFlag::Cargo, 25, false));
        snap.items.insert(packed_box, mk_item(packed_box, box_tid, mate_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(box_trit, mk_item(box_trit, 34, mate_id, packed_box, InventoryFlag::Cargo, 15, false));
        snap.items.insert(bare_space_trit, mk_item(bare_space_trit, 34, mate_id, sys_id, InventoryFlag::Hangar, 10, false));
    });
    let now_ts = ctx.now_unix();
    let owner_uid = ctx.db.with_snapshot(|s| s.characters[&owner_id].user_id);
    let _ = ctx.db.acquire_online_lock(owner_uid, 2010, Some(owner_id), true, now_ts);

    ctx.db.with_mut_snapshot(|snap| {
        set_ship_access_config_snap(
            snap,
            owner_titan,
            &ShipAccessConfig {
                allow_fleet_smb: true,
                allow_corp_smb: false,
                allow_fleet_hangar: true,
                allow_corp_hangar: false,
                password: Some("bay-pw".into()),
            },
            0,
        )
        .unwrap();
    });

    let mut mate_sess = SessionState::new(8);
    mate_sess.user_id = Some(742);
    mate_sess.character_id = Some(mate_id);
    mate_sess.corporation_id = Some(98_674_002);
    mate_sess.station_id = Some(station_id);
    mate_sess.solar_system_id = Some(sys_id as u32);
    mate_sess.ship_id = Some(mate_ship);
    let kw = HashMap::new();

    // 1. Private station SplitStack emits caller-only notifications (never notifies owner_id).
    let _ = ctx.drain_notifications_for_char(owner_id);
    let priv_split = handle(
        &ctx,
        &mut mate_sess,
        None,
        "invbroker",
        "SplitStack",
        &[PyRep::Integer(station_id), PyRep::Integer(mate_hangar_trit), PyRep::Integer(5)],
        &kw,
    )
    .await
    .unwrap()
    .expect("private station SplitStack");
    let priv_new_id = priv_split.dict_get("newItemID").and_then(|v| v.as_i64()).unwrap();
    assert!(ctx.drain_notifications_for_char(owner_id).is_empty(), "private split must notify caller only");
    assert_eq!(ctx.db.get_item(priv_new_id).unwrap().owner_id, mate_id);

    // 2. Docked foreign FleetHangar SplitStack rejects without password and never touches foreign Cargo (flag 5).
    assert_eq!(
        handle(
            &ctx,
            &mut mate_sess,
            None,
            "invbroker",
            "SplitStack",
            &[PyRep::Integer(owner_titan), PyRep::Integer(docked_fh_trit), PyRep::Integer(8)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FleetHangarAccessDenied"
    );
    assert!(
        handle(
            &ctx,
            &mut mate_sess,
            None,
            "invbroker",
            "SplitStack",
            &[PyRep::Integer(owner_titan), PyRep::Integer(docked_cargo_trit), PyRep::Integer(8), PyRep::String("bay-pw".into())],
            &kw,
        )
        .await
        .unwrap()
        .is_err()
    );

    // 3. MultiAdd with fitted hull (SMA 90) and non-empty container (FH 155) using positional pw recursively transfers children & emits OnItemChange.
    let _ = ctx.drain_notifications_for_char(owner_id);
    handle(
        &ctx,
        &mut mate_sess,
        None,
        "invbroker",
        "MultiAdd",
        &[PyRep::List(vec![PyRep::Integer(fitted_hull)]), PyRep::Integer(owner_titan), PyRep::Integer(90), PyRep::String("bay-pw".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("MultiAdd fitted hull into SMA");
    handle(
        &ctx,
        &mut mate_sess,
        None,
        "invbroker",
        "MultiAdd",
        &[PyRep::List(vec![PyRep::Integer(packed_box)]), PyRep::Integer(owner_titan), PyRep::Integer(155)],
        &kw,
    )
    .await
    .unwrap()
    .expect("MultiAdd non-empty container into unlocked FleetHangar");
    ctx.db.with_snapshot(|snap| {
        for &id in &[fitted_hull, hull_mod, hull_rig, hull_drone, hull_charge, packed_box, box_trit] {
            assert_eq!(snap.items[&id].owner_id, owner_id, "MultiAdd child {id} must transfer owner to ship owner");
        }
    });
    let owner_notifs = ctx.drain_notifications_for_char(owner_id);
    for &id in &[fitted_hull, hull_mod, hull_rig, hull_drone, hull_charge, packed_box, box_trit] {
        assert!(
            owner_notifs
                .iter()
                .any(|n| n.event == "OnItemChange" && dbrow_i64(&n.args[0], "itemID") == Some(id)),
            "expected MultiAdd recursive OnItemChange for {id}"
        );
    }

    // 4. Docked FleetHangar SplitStack after unlock persists custom_item_flags[new]=155, keeps owner_id=owner_id, and notifies owner_id.
    let _ = ctx.drain_notifications_for_char(owner_id);
    let fh_split = handle(
        &ctx,
        &mut mate_sess,
        None,
        "invbroker",
        "SplitStack",
        &[PyRep::Integer(owner_titan), PyRep::Integer(docked_fh_trit), PyRep::Integer(10)],
        &kw,
    )
    .await
    .unwrap()
    .expect("SplitStack inside unlocked docked FleetHangar");
    let fh_split_id = fh_split.dict_get("newItemID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&fh_split_id].owner_id, owner_id);
        assert_eq!(snap.items[&fh_split_id].flag, InventoryFlag::FleetHangar);
        assert_eq!(
            crate::services::inventory_fitting::fitting_containers_ships::custom_item_flag_snap(
                snap,
                fh_split_id
            ),
            Some(155)
        );
    });
    assert!(ctx.drain_notifications_for_char(owner_id).iter().any(|n| {
        n.event == "OnItemChange" && dbrow_i64(&n.args[0], "itemID") == Some(fh_split_id)
    }));

    // 5. In-space SplitStack rejects bare in-space items, fails closed when positions missing, allows Titan at <= 2,500m surface distance, and rejects > 2,500m.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&mate_id).unwrap().station_id = None;
        snap.items.get_mut(&owner_titan).unwrap().location_id = sys_id;
        snap.items.get_mut(&mate_ship).unwrap().location_id = sys_id;
        snap.item_positions.insert(mate_ship, [0.0, 0.0, 0.0]);
    });
    mate_sess.station_id = None;
    assert_eq!(
        handle(
            &ctx,
            &mut mate_sess,
            None,
            "invbroker",
            "SplitStack",
            &[PyRep::Integer(sys_id), PyRep::Integer(bare_space_trit), PyRep::Integer(3)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidLocation"
    );
    assert!(
        handle(
            &ctx,
            &mut mate_sess,
            None,
            "invbroker",
            "SplitStack",
            &[PyRep::Integer(owner_titan), PyRep::Integer(docked_fh_trit), PyRep::Integer(4)],
            &kw,
        )
        .await
        .unwrap()
        .is_err(),
        "SplitStack on in-space ship with missing position must fail closed"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(owner_titan, [11_500.0, 0.0, 0.0]);
    });
    handle(
        &ctx,
        &mut mate_sess,
        None,
        "invbroker",
        "SplitStack",
        &[PyRep::Integer(owner_titan), PyRep::Integer(docked_fh_trit), PyRep::Integer(4)],
        &kw,
    )
    .await
    .unwrap()
    .expect("in-space SplitStack at 1,950m Titan surface distance must succeed");
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(owner_titan, [12_100.0, 0.0, 0.0]);
    });
    assert!(
        handle(
            &ctx,
            &mut mate_sess,
            None,
            "invbroker",
            "SplitStack",
            &[PyRep::Integer(owner_titan), PyRep::Integer(docked_fh_trit), PyRep::Integer(4)],
            &kw,
        )
        .await
        .unwrap()
        .is_err(),
        "in-space SplitStack at 2,550m Titan surface distance must reject"
    );
}



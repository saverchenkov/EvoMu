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
    assert_eq!(err_pos.message, "free=60, required=75");

    let err_clamped = ship_hold_capacity_error(5, 100.0, 125.0, 10.0);
    assert_eq!(err_clamped.code, "NotEnoughCargoSpace");
    assert_eq!(err_clamped.message, "free=0, required=10");
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

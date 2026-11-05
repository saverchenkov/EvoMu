//! Extra unit and mutation-killing tests for `services::inventory_fitting`.

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::fitting_containers_ships::{write_container_ship_state, ShipAccessConfig};
use crate::services::inventory_fitting::{csprng_unit_f64, handle};
use eve_db::{
    corp_roles, CorporationRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord,
    UpwellStructureRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn mk_item(
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

async fn call_add(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    handle(ctx, session, None, "invbroker", "Add", args, &HashMap::new())
        .await
        .unwrap()
}

fn setup_inv_ctx() -> (ServerContext, SessionState, i64, i64, i64, i64) {
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
            CorporationRecord {
                corporation_id: corp_id,
                name: "InvExtra Corp".into(),
                ticker: "IEXC".into(),
                ceo_id: 96_699_999,
                members: [char_id, 96_699_999].into_iter().collect(),
                ..Default::default()
            },
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

#[tokio::test]
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

#[tokio::test]
async fn invbroker_add_fitting_prechecks_rig_destruction_charge_load_and_unfit_rules() {
    let (ctx, mut session, char_id, _corp_id, station_id, ship_id) = setup_inv_ctx();

    // 1. ItemNotFound on fitting slot
    assert_eq!(
        call_add(
            &ctx,
            &mut session,
            &[PyRep::Integer(999_999_999), PyRep::Integer(ship_id), PyRep::Integer(27)],
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotFound"
    );

    // 2. Foreign-owned module, locked module, stacked module, non-module item, missing ship, non-ship target
    let foreign_mod = 96_600_301i64;
    let locked_mod = 96_600_302i64;
    let stacked_mod = 96_600_303i64;
    let non_mod = 96_600_304i64;
    let damaged_ehe = 96_600_305i64;
    let gun_mod = 96_600_306i64;
    let gun_mod2 = 96_600_307i64;
    let ammo_id = 96_600_308i64;
    let rig_id = 96_600_309i64;

    let rig_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 7 && t.default_attributes.get(&1547).copied() == Some(1.0))
        .map(|t| t.type_id)
        .unwrap_or(31_358);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            foreign_mod,
            mk_item(foreign_mod, 484, 96_699_999, station_id, InventoryFlag::Hangar, 1, true),
        );
        snap.items.insert(
            locked_mod,
            mk_item(locked_mod, 484, char_id, station_id, InventoryFlag::Locked, 1, true),
        );
        snap.items.insert(
            stacked_mod,
            mk_item(stacked_mod, 484, char_id, station_id, InventoryFlag::Hangar, 2, false),
        );
        snap.items.insert(
            non_mod,
            mk_item(non_mod, 34, char_id, station_id, InventoryFlag::Hangar, 1, true),
        );
        let mut ehe = mk_item(damaged_ehe, 40_714, char_id, station_id, InventoryFlag::Hangar, 1, true);
        ehe.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 10.0);
        snap.items.insert(damaged_ehe, ehe);
        snap.items.insert(
            gun_mod,
            mk_item(gun_mod, 484, char_id, station_id, InventoryFlag::Hangar, 1, true),
        );
        snap.items.insert(
            gun_mod2,
            mk_item(gun_mod2, 484, char_id, station_id, InventoryFlag::Hangar, 1, true),
        );
        snap.items.insert(
            ammo_id,
            mk_item(ammo_id, 182, char_id, station_id, InventoryFlag::Hangar, 100, false),
        );
        snap.items.insert(
            rig_id,
            mk_item(rig_id, rig_tid, char_id, ship_id, InventoryFlag::RigSlot0, 1, true),
        );
    });

    for (iid, dst, expected_code) in [
        (foreign_mod, ship_id, "ItemOwnershipViolation"),
        (locked_mod, ship_id, "ItemLocked"),
        (stacked_mod, ship_id, "CannotFitStackedModule"),
        (non_mod, ship_id, "ItemNotFittableModule"),
        (damaged_ehe, ship_id, "CannotRefitDamagedModule"),
        (gun_mod, 999_888_777, "ShipNotFound"),
        (gun_mod, non_mod, "InvalidFittingTarget"),
    ] {
        assert_eq!(
            call_add(
                &ctx,
                &mut session,
                &[PyRep::Integer(iid), PyRep::Integer(dst), PyRep::Integer(27)],
            )
            .await
            .unwrap_err()
            .code,
            expected_code,
            "failed precheck for item {iid} -> {dst}"
        );
    }

    // 3. Fit gun_mod to HiSlot0 (27) with explicit online=false, then try fitting gun_mod2 into same slot -> SlotAlreadyOccupied
    let mut fit_kw = HashMap::new();
    fit_kw.insert("online".into(), PyRep::Bool(false));
    handle(
        &ctx,
        &mut session,
        None,
        "invbroker",
        "Add",
        &[PyRep::Integer(gun_mod), PyRep::Integer(ship_id), PyRep::Integer(27)],
        &fit_kw,
    )
    .await
    .unwrap()
    .expect("fit gun_mod offline");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.module_online_states.get(&gun_mod).copied()),
        Some(false)
    );
    assert_eq!(
        call_add(
            &ctx,
            &mut session,
            &[PyRep::Integer(gun_mod2), PyRep::Integer(ship_id), PyRep::Integer(27)],
        )
        .await
        .unwrap_err()
        .code,
        "SlotAlreadyOccupied"
    );

    // 4. Load charge (category 8) into HiSlot0 (27)
    let charge_res = call_add(
        &ctx,
        &mut session,
        &[PyRep::Integer(ammo_id), PyRep::Integer(ship_id), PyRep::Integer(27)],
    )
    .await
    .expect("charge loaded into HiSlot0");
    assert_eq!(charge_res.dict_get("flagID").and_then(|v| v.as_i64()), Some(27));

    // 5. Removing a rig (flag 92 -> Hangar 4) destroys the rig
    let rig_res = call_add(
        &ctx,
        &mut session,
        &[PyRep::Integer(rig_id), PyRep::Integer(station_id), PyRep::Integer(4)],
    )
    .await
    .expect("rig removal returns destroyed=true");
    assert_eq!(rig_res.dict_get("destroyed"), Some(&PyRep::Bool(true)));
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&rig_id)));

    // 6. Unfitting gun_mod in space without Mobile Depot/SMA or with weapons timer -> rejected
    session.station_id = None;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(sh) = s.items.get_mut(&ship_id) {
            sh.location_id = 30_000_142;
        }
    });
    assert_eq!(
        call_add(
            &ctx,
            &mut session,
            &[PyRep::Integer(gun_mod), PyRep::Integer(ship_id), PyRep::Integer(5)],
        )
        .await
        .unwrap_err()
        .code,
        "CannotUnfitInSpace"
    );
    ctx.crimewatch
        .write()
        .unwrap()
        .states
        .entry(char_id)
        .or_default()
        .weapon_timer_until_unix = ctx.now_unix_secs() + 60;
    assert_eq!(
        call_add(
            &ctx,
            &mut session,
            &[PyRep::Integer(gun_mod), PyRep::Integer(ship_id), PyRep::Integer(5)],
        )
        .await
        .unwrap_err()
        .code,
        "CannotRefitWithWeaponsTimer"
    );
    ctx.crimewatch.write().unwrap().states.clear();

    // 7. Re-dock and unfit gun_mod to Hangar -> clears module_online_states
    session.station_id = Some(station_id);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(sh) = s.items.get_mut(&ship_id) {
            sh.location_id = station_id;
        }
    });
    call_add(
        &ctx,
        &mut session,
        &[PyRep::Integer(gun_mod), PyRep::Integer(station_id), PyRep::Integer(4)],
    )
    .await
    .expect("unfit gun_mod when docked");
    assert!(ctx.db.with_snapshot(|s| !s.module_online_states.contains_key(&gun_mod)));
}

#[tokio::test]
async fn invbroker_add_move_auth_quantum_core_escape_bay_and_stack_merge() {
    let (ctx, mut session, char_id, corp_id, station_id, ship_id) = setup_inv_ctx();
    let kw = HashMap::new();

    // 1. Moving active ship into corp hangar division -> CannotMoveActiveShip
    assert_eq!(
        call_add(
            &ctx,
            &mut session,
            &[PyRep::Integer(ship_id), PyRep::Integer(station_id), PyRep::Integer(116)],
        )
        .await
        .unwrap_err()
        .code,
        "CannotMoveActiveShip"
    );

    // 2. Version mismatch & courier_cargo restriction
    let item_a = 96_600_401i64;
    let item_b = 96_600_402i64;
    let courier_id = 96_600_403i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_a,
            mk_item(item_a, 34, char_id, station_id, InventoryFlag::Hangar, 100, false),
        );
        snap.items.insert(
            item_b,
            mk_item(item_b, 34, char_id, ship_id, InventoryFlag::Cargo, 50, false),
        );
        snap.items.insert(
            courier_id,
            mk_item(courier_id, 34, char_id, station_id, InventoryFlag::Hangar, 10, false),
        );
        snap.item_custom_names
            .insert(courier_id, "courier_cargo:9001".into());
    });

    let mut ver_kw = HashMap::new();
    ver_kw.insert("expected_version".into(), PyRep::Integer(99));
    assert_eq!(
        handle(
            &ctx,
            &mut session,
            None,
            "invbroker",
            "Add",
            &[PyRep::Integer(item_a), PyRep::Integer(ship_id), PyRep::Integer(5)],
            &ver_kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ItemVersionMismatch"
    );

    assert_eq!(
        call_add(
            &ctx,
            &mut session,
            &[PyRep::Integer(courier_id), PyRep::Integer(station_id), PyRep::Integer(116)],
        )
        .await
        .unwrap_err()
        .code,
        "CannotDeliverCourierCargo"
    );

    // 3. Moving item_a into ship Cargo merges with existing item_b (100 + 50 = 150)
    let merged = call_add(
        &ctx,
        &mut session,
        &[PyRep::Integer(item_a), PyRep::Integer(ship_id), PyRep::Integer(5)],
    )
    .await
    .expect("stack merge in ship cargo");
    assert_eq!(merged.dict_get("itemID").and_then(|v| v.as_i64()), Some(item_b));
    assert_eq!(merged.dict_get("quantity").and_then(|v| v.as_i64()), Some(150));

    // 4. Battleship Frigate Escape Bay (flag 177)
    let bs_id = 96_600_410i64;
    let pkg_frig = 96_600_411i64;
    let dirty_frig = 96_600_412i64;
    let dirty_cargo = 96_600_413i64;
    let clean_frig1 = 96_600_414i64;
    let clean_frig2 = 96_600_415i64;
    ctx.db.with_mut_snapshot(|snap| {
        for (iid, tid, loc, flag, qty, sing) in [
            (bs_id, 642, station_id, InventoryFlag::Hangar, 1, true),
            (pkg_frig, 587, station_id, InventoryFlag::Hangar, 1, false),
            (dirty_frig, 587, station_id, InventoryFlag::Hangar, 1, true),
            (dirty_cargo, 34, dirty_frig, InventoryFlag::Cargo, 1, false),
            (clean_frig1, 587, station_id, InventoryFlag::Hangar, 1, true),
            (clean_frig2, 587, station_id, InventoryFlag::Hangar, 1, true),
        ] {
            snap.items.insert(iid, mk_item(iid, tid, char_id, loc, flag, qty, sing));
        }
    });
    for (fid, exp_err) in [(pkg_frig, "ShipMustBeAssembled"), (dirty_frig, "ShipHasItemsInCargo")] {
        assert_eq!(
            call_add(&ctx, &mut session, &[PyRep::Integer(fid), PyRep::Integer(bs_id), PyRep::Integer(177)])
                .await
                .unwrap_err()
                .code,
            exp_err
        );
    }
    call_add(&ctx, &mut session, &[PyRep::Integer(clean_frig1), PyRep::Integer(bs_id), PyRep::Integer(177)])
        .await
        .expect("first assembled frigate fits in escape bay");
    assert_eq!(
        call_add(&ctx, &mut session, &[PyRep::Integer(clean_frig2), PyRep::Integer(bs_id), PyRep::Integer(177)])
            .await
            .unwrap_err()
            .code,
        "CapacityExceeded"
    );

    // 5. Upwell Structure Quantum Core deposit & removal lock
    let struct_id = 1_000_000_999_001i64;
    let core_id = 96_600_420i64;
    let wrong_core_id = 96_600_421i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832, // Astrahus (core 56201)
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Test Astrahus".into(),
                state: "HullVulnerable".into(),
                quantum_core_installed: false,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = None;
            c.structure_id = Some(struct_id);
        }
        snap.items.insert(
            core_id,
            mk_item(core_id, 56_201, char_id, struct_id, InventoryFlag::Hangar, 1, true),
        );
        snap.items.insert(
            wrong_core_id,
            mk_item(wrong_core_id, 34, char_id, struct_id, InventoryFlag::Hangar, 1, true),
        );
    });
    session.station_id = None;
    session.structure_id = Some(struct_id);

    assert_eq!(
        call_add(&ctx, &mut session, &[PyRep::Integer(core_id), PyRep::Integer(struct_id), PyRep::Integer(180)])
            .await
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    session.corp_role = corp_roles::ROLE_DIRECTOR;

    assert_eq!(
        call_add(&ctx, &mut session, &[PyRep::Integer(wrong_core_id), PyRep::Integer(struct_id), PyRep::Integer(180)])
            .await
            .unwrap_err()
            .code,
        "InvalidItemType"
    );

    call_add(&ctx, &mut session, &[PyRep::Integer(core_id), PyRep::Integer(struct_id), PyRep::Integer(180)])
        .await
        .expect("quantum core installed");
    assert_eq!(
        call_add(&ctx, &mut session, &[PyRep::Integer(core_id), PyRep::Integer(struct_id), PyRep::Integer(4)])
            .await
            .unwrap_err()
            .code,
        "ItemLocked"
    );

    write_container_ship_state(&ctx, |st| {
        st.ship_configs.insert(bs_id, ShipAccessConfig::default());
    });
    assert!(handle(&ctx, &mut session, None, "", "", &[], &kw).await.is_none());
}

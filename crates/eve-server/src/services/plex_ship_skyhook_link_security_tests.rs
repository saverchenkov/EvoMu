//! Security and exploit regression tests for:
//! - `invbroker.DepositPlexToVault` and `ship.AssembleShip` item-lock, Asset Safety, ALSC, location, and corp-role guards (`#491`)
//! - Orbital Skyhook Reagent Silo link `SetMaxSpeed` combat/lock preservation, shield hackability gate, and SDE shield resonances (`#513`)

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::inventory_fitting::fitting_containers_ships::{
    read_container_ship_state, save_container_ship_state,
};
use crate::services::sov_gameplay::sov_gameplay_skyhook::{
    apply_skyhook_weapon_damage, apply_skyhook_weapon_damage_typed, handle_skyhook_component_call,
    tick_skyhooks, SKYHOOK_BASE_SHIELD_HP, SKYHOOK_SILO_LINK_DURATION_SECS,
    SKYHOOK_SILO_LINK_MAX_SPEED_MS,
};
use crate::services::sov_services::{TYPE_MAGMATIC_GAS, TYPE_ORBITAL_SKYHOOK};
use crate::services::station_structure::structure_upwell_ops::{
    load_ext_state, save_ext_state, AssetSafetyWrapRecord,
};
use eve_db::{
    corp_roles, ContractRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    OwnerNoteRecord, UpwellStructureRecord,
};
use eve_destiny::{Ball, CombatState, DestinyEvent, MovementMode, TargetLock};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn deposit_plex_and_assemble_ship_enforce_locks_escrow_safety_location_and_corp_roles() {
    let ctx = ServerContext::new();
    let uid = ctx.db.provision_player_account("plex_ship_guard", "pass_ps");
    let char_id = 92_300_001i64;
    let corp_id = 98_300_001i64;
    let ceo_id = 92_300_099i64;
    let jita = 60_003_760i64;
    let amarr = 60_008_494i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&90_000_001].clone();
        ch.character_id = char_id;
        ch.user_id = uid;
        ch.corporation_id = corp_id;
        ch.station_id = Some(jita);
        snap.characters.insert(char_id, ch);
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Guard Corp".into();
                __corp.ticker = "GRDC".into();
                __corp.ceo_id = ceo_id;
                __corp.tax_rate = 0.05;
                __corp },
        );
        let mk_item =
            |id: i64, tid: u32, owner: i64, loc: i64, flag: InventoryFlag, qty: i32| {
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: owner,
                    location_id: loc,
                    flag,
                    quantity: qty,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                }
            };
        for rec in [
            mk_item(930_001, 44_992, char_id, jita, InventoryFlag::Hangar, 10),
            mk_item(930_002, 44_992, char_id, jita, InventoryFlag::Hangar, 10),
            mk_item(930_003, 44_992, char_id, jita, InventoryFlag::Hangar, 10),
            mk_item(930_004, 44_992, char_id, jita, InventoryFlag::Hangar, 10),
            mk_item(930_005, 44_992, corp_id, jita, InventoryFlag::CorpSAG1, 10),
            mk_item(930_006, 44_992, char_id, jita, InventoryFlag::Hangar, 10),
            mk_item(930_101, 587, char_id, jita, InventoryFlag::CorpDeliveries, 2),
            mk_item(930_102, 587, char_id, jita, InventoryFlag::Impounded, 2),
            mk_item(930_103, 587, char_id, jita, InventoryFlag::Deliveries, 1),
            mk_item(930_104, 587, char_id, jita, InventoryFlag::Locked, 2),
            mk_item(930_105, 587, char_id, jita, InventoryFlag::Hangar, 2),
            mk_item(930_106, 587, char_id, jita, InventoryFlag::Hangar, 1),
            mk_item(930_107, 3_468, char_id, jita, InventoryFlag::Hangar, 1),
            mk_item(930_108, 587, char_id, 930_107, InventoryFlag::Hangar, 1),
            mk_item(930_109, 587, corp_id, jita, InventoryFlag::CorpSAG1, 2),
        ] {
            snap.items.insert(rec.item_id, rec);
        }
        snap.contracts.insert(
            -73_001,
            ContractRecord {
                contract_id: -73_001,
                issuer_id: char_id,
                contract_type: "ItemExchange".into(),
                status: "InProgress".into(),
                item_ids: vec![930_001, 930_105],
                price: IskCents::ZERO,
                reward: IskCents::ZERO,
                collateral: IskCents::ZERO,
                ..Default::default()
            },
        );
        snap.owner_notes.insert(
            -73_002,
            OwnerNoteRecord {
                note_id: -73_002,
                character_id: -73,
                item_id: 73_002,
                label: "trade:73002".into(),
                note: r#"{"items_a":[[930002,10]],"items_b":[],"completed":false,"cancelled":false}"#.into(),
                timestamp_unix: 100,
            },
        );
        snap.owner_notes.insert(
            -74_001,
            OwnerNoteRecord {
                note_id: -74_001,
                character_id: -74,
                item_id: 74_001,
                label: "raffle:74001".into(),
                note: r#"{"item_id":930110,"winner_id":null,"expired":false}"#.into(),
                timestamp_unix: 100,
            },
        );
        snap.items.insert(
            930_110,
            mk_item(930_110, 587, char_id, jita, InventoryFlag::Hangar, 1),
        );
        let mut ext = load_ext_state(snap);
        ext.asset_safety_wraps.insert(
            930_999,
            AssetSafetyWrapRecord {
                wrap_id: 930_999,
                owner_id: char_id,
                is_corp: false,
                source_structure_id: 1_000_000_999,
                source_structure_name: "Destroyed Citadel".into(),
                solar_system_id: 30_000_142,
                created_unix: 100,
                deliverable_after_unix: 500_000,
                delivered_to_id: None,
                item_ids: vec![930_004, 930_106],
                estimated_value_isk: 1_000_000.0,
                nearest_npc_station_id: Some(jita),
                auto_delivered: false,
                retrieval_fee_paid: false,
            },
        );
        save_ext_state(snap, &ext);
    });
    {
        let mut cs = read_container_ship_state(&ctx);
        cs.alsc_locked_items.insert(930_003);
        save_container_ship_state(&ctx, &cs);
    }

    let mut sess = SessionState::new(88_491);
    sess.user_id = Some(uid);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.station_id = Some(jita);
    sess.solar_system_id = Some(30_000_142);
    let kw = HashMap::new();

    for locked_plex in [930_001i64, 930_002, 930_003, 930_004, 930_005] {
        let err = ctx
            .dispatch_rpc(
                &mut sess,
                "invbroker",
                None,
                "DepositPlexToVault",
                &[
                    PyRep::Integer(jita),
                    PyRep::Integer(locked_plex),
                    PyRep::Integer(5),
                ],
                &kw,
                None,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err.code.as_str(), "ItemLocked" | "CorpRoleDenied"),
            "unexpected code {} for item {locked_plex}",
            err.code
        );
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(char_id)), 0);

    ctx.db
        .with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().station_id = Some(amarr));
    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "DepositPlexToVault",
            &[PyRep::Integer(jita), PyRep::Integer(930_006), PyRep::Integer(5)],
            &kw,
            None,
        )
        .await
        .unwrap_err()
        .code,
        "NotDockedAtLocation"
    );
    assert!(ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "AssembleShip",
            &[PyRep::Integer(930_109)],
            &kw,
            None
        )
        .await
        .is_err());
    ctx.db
        .with_mut_snapshot(|s| s.characters.get_mut(&char_id).unwrap().station_id = Some(jita));

    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "DepositPlexToVault",
        &[PyRep::Integer(jita), PyRep::Integer(930_006), PyRep::Integer(4)],
        &kw,
        None,
    )
    .await
    .expect("clean personal PLEX deposit");
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(char_id)), 4);

    for bad_ship in [
        930_101i64, 930_102, 930_103, 930_104, 930_105, 930_106, 930_108, 930_109, 930_110,
    ] {
        assert!(
            ctx.dispatch_rpc(
                &mut sess,
                "ship",
                None,
                "AssembleShip",
                &[PyRep::Integer(bad_ship)],
                &kw,
                None
            )
            .await
            .is_err(),
            "ship {bad_ship} must not assemble while locked/wrapped/impounded/unauthorized"
        );
    }

    ctx.db.with_mut_snapshot(|s| {
        s.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(char_id, corp_roles::ROLE_HANGAR_CAN_TAKE[0]);
    });
    sess.corp_role = corp_roles::ROLE_HANGAR_CAN_TAKE[0];
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "DepositPlexToVault",
        &[PyRep::Integer(jita), PyRep::Integer(930_005), PyRep::Integer(6)],
        &kw,
        None,
    )
    .await
    .expect("authorized CorpSAG1 PLEX deposit");
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(char_id)), 10);

    let asm_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "AssembleShip",
            &[PyRep::Integer(930_109)],
            &kw,
            None,
        )
        .await
        .expect("authorized CorpSAG1 stack assemble");
    let new_ship_id = asm_rep.dict_get("itemID").and_then(PyRep::as_i64).unwrap();
    assert_ne!(new_ship_id, 930_109);
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items[&930_109].owner_id, corp_id);
        assert_eq!(s.items[&930_109].quantity, 1);
        assert_eq!(s.items[&new_ship_id].owner_id, corp_id);
        assert!(s.items[&new_ship_id].singleton);
    });
}

#[tokio::test]
async fn skyhook_silo_link_preserves_combat_state_and_locks_enforces_shield_gate_and_sde_resonance() {
    let ctx = ServerContext::new();
    let sys_id = 30_004_759u32;
    let skyhook_id = 1_088_000_001i64;
    let thief_cid = 90_000_001i64;
    let thief_ship = 1_088_000_010i64;
    let defender_ship = 1_088_000_020i64;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            skyhook_id,
            UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: 98_999_001,
                solar_system_id: sys_id,
                name: "Orbital Skyhook".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                shield_hp: SKYHOOK_BASE_SHIELD_HP,
                armor_hp: SKYHOOK_BASE_SHIELD_HP,
                hull_hp: SKYHOOK_BASE_SHIELD_HP,
                ..Default::default()
            },
        );
        snap.sov_runtime.skyhook_active.insert(skyhook_id, true);
        snap.sov_runtime
            .skyhook_reagent_bays
            .insert(skyhook_id, (TYPE_MAGMATIC_GAS, 120, 80, now));
        snap.sov_runtime
            .skyhook_theft_windows
            .insert(skyhook_id, (now.saturating_sub(60), now + 3_600));
    });

    let _ = ctx.drain_notifications_for_char(thief_cid);
    tick_skyhooks(&ctx, now).await;
    assert_eq!(
        ctx.db.with_snapshot(|s| {
            s.sov_runtime
                .skyhook_hackable
                .get(&skyhook_id)
                .copied()
                .unwrap_or(false)
        }),
        false
    );

    assert_eq!(
        apply_skyhook_weapon_damage(&ctx, skyhook_id, f64::NAN)
            .unwrap_err()
            .code,
        "InvalidDamage"
    );
    let hit1 = apply_skyhook_weapon_damage(&ctx, skyhook_id, 1_000.0).unwrap();
    assert_eq!(hit1.dict_get("shieldHP").and_then(PyRep::as_f64), Some(9_200.0));
    assert_eq!(
        hit1.dict_get("appliedDamage").and_then(PyRep::as_f64),
        Some(800.0)
    );
    assert_eq!(hit1.dict_get("hackable").and_then(PyRep::as_bool), Some(false));

    let hit2 =
        apply_skyhook_weapon_damage_typed(&ctx, skyhook_id, 3_000.0, 2_500.0, 2_500.0, 2_500.0)
            .unwrap();
    assert_eq!(hit2.dict_get("shieldHP").and_then(PyRep::as_f64), Some(800.0));
    assert_eq!(hit2.dict_get("hackable").and_then(PyRep::as_bool), Some(true));

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball(Ball {
            id: skyhook_id,
            type_id: TYPE_ORBITAL_SKYHOOK,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 1e8,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await;
    let mut custom_cs = CombatState::default();
    custom_cs.shield_hp = 321.0;
    custom_cs.max_shield_hp = 2_000.0;
    custom_cs.armor_hp = 654.0;
    custom_cs.max_armor_hp = 2_000.0;
    custom_cs.structure_hp = 987.0;
    custom_cs.max_structure_hp = 2_000.0;
    custom_cs.warp_scramble_strength = 2;
    let _ = sys_h
        .spawn_ball_with_combat(
            Ball {
                id: thief_ship,
                type_id: 620,
                position: DVec3::new(3_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 2_500.0,
                inertia: 0.5,
                mass: 1.1e7,
                radius: 120.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            custom_cs,
        )
        .await;
    let _ = sys_h
        .spawn_ball(Ball {
            id: defender_ship,
            type_id: 621,
            position: DVec3::new(4_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 1_800.0,
            inertia: 0.5,
            mass: 1.1e7,
            radius: 120.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    sys_h
        .add_target_lock(
            defender_ship,
            TargetLock {
                target_id: thief_ship,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(50_000.0),
            },
        )
        .await
        .unwrap();
    sys_h
        .add_target_lock(
            thief_ship,
            TargetLock {
                target_id: skyhook_id,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(50_000.0),
            },
        )
        .await
        .unwrap();
    let _ = sys_h.step_manual(0.0).await.unwrap();

    let mut thief_sess = SessionState::new(99_513);
    thief_sess.user_id = Some(1);
    thief_sess.character_id = Some(thief_cid);
    thief_sess.corporation_id = Some(1_000_009);
    thief_sess.solar_system_id = Some(sys_id);
    thief_sess.ship_id = Some(thief_ship);

    ctx.db
        .with_mut_snapshot(|s| s.structures.get_mut(&skyhook_id).unwrap().shield_hp = 5_000.0);
    let too_high = handle_skyhook_component_call(
        &ctx,
        &thief_sess,
        sys_id,
        thief_cid,
        thief_ship,
        skyhook_id,
        "linkWithShip",
        "InitiateLink",
        &[],
    )
    .await
    .unwrap_err();
    assert_eq!(too_high.code, "SkyhookSiloShieldTooHigh");
    ctx.db
        .with_mut_snapshot(|s| s.structures.get_mut(&skyhook_id).unwrap().shield_hp = 800.0);

    handle_skyhook_component_call(
        &ctx,
        &thief_sess,
        sys_id,
        thief_cid,
        thief_ship,
        skyhook_id,
        "linkWithShip",
        "InitiateLink",
        &[],
    )
    .await
    .expect("InitiateLink");
    let bc_init = sys_h.step_manual(0.0).await.unwrap();
    assert!(bc_init.events.iter().any(|e| matches!(
        e,
        DestinyEvent::SetMaxSpeed { ball_id, max_speed }
            if *ball_id == thief_ship && (*max_speed - SKYHOOK_SILO_LINK_MAX_SPEED_MS).abs() < 1e-6
    )));
    assert!(!bc_init
        .events
        .iter()
        .any(|e| matches!(e, DestinyEvent::RemoveBall { ball_id } if *ball_id == thief_ship)));

    tick_skyhooks(&ctx, now + 30).await;
    let _ = sys_h
        .set_ball_position(thief_ship, DVec3::new(22_000.0, 0.0, 0.0))
        .await;
    tick_skyhooks(&ctx, now + 40).await;
    assert_eq!(
        sys_h
            .get_ball_snapshot(thief_ship)
            .await
            .unwrap()
            .unwrap()
            .1
            .max_velocity,
        2_500.0
    );

    let _ = sys_h
        .set_ball_position(thief_ship, DVec3::new(3_000.0, 0.0, 0.0))
        .await;
    handle_skyhook_component_call(
        &ctx,
        &thief_sess,
        sys_id,
        thief_cid,
        thief_ship,
        skyhook_id,
        "linkWithShip",
        "InitiateLinkQuick",
        &[],
    )
    .await
    .expect("re-link");
    tick_skyhooks(&ctx, now + SKYHOOK_SILO_LINK_DURATION_SECS + 100).await;

    let (_, final_ball) = sys_h.get_ball_snapshot(thief_ship).await.unwrap().unwrap();
    assert_eq!(final_ball.max_velocity, 2_500.0);
    let cs_after = sys_h
        .get_combat_state(thief_ship)
        .await
        .unwrap()
        .expect("CombatState preserved");
    assert_eq!(cs_after.armor_hp, 654.0);
    assert_eq!(cs_after.structure_hp, 987.0);
    assert_eq!(cs_after.warp_scramble_strength, 2);
    assert_eq!(sys_h.get_target_locks(defender_ship).await.unwrap().len(), 1);
    assert_eq!(sys_h.get_target_locks(thief_ship).await.unwrap().len(), 1);
}

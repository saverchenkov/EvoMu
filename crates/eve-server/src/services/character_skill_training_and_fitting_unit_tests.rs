//! Semantic unit tests split from `character_skill_ops_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::super::*;
use super::*;

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::account_services::account_services_tests::{
    auth_session, test_ctx, PyRepTestExt,
};
use crate::services::character_skill_ops::{
    abort_training, get_respec_info, get_skill_changes_for_isis, handle,
    handle_omega_weekly_free_sp_offer, migrate_obsolete_skills_for_character,
    post_inventory_fitting_hook, purchase_skills_direct_for_char,
    verify_in_space_container_proximity,
};
use eve_db::{
    ContractRecord, InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord,
    SkillQueueEntry, TrainedSkillRecord, UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

#[test]
fn abort_training_and_respec_info_branches() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let sess = auth_session(1, cid, 98_000_001, 30_000_142);
    let unauth = unauth_session();

    assert!(abort_training(&ctx, &unauth).is_err());
    assert!(get_respec_info(&ctx, &unauth).is_err());
    assert!(get_skill_changes_for_isis(&unauth).is_err());

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(cid).or_default().remove(&99999);
        snap.skill_queues.insert(
            cid,
            vec![SkillQueueEntry {
                type_id: 99999,
                target_level: 1,
                started_at_unix: None,
                duration_seconds: 100.0,
                start_sp: 123.5,
                end_sp: 250.0,
            }],
        );
    });
    assert_eq!(abort_training(&ctx, &sess).unwrap(), PyRep::Bool(true));
    let head = ctx.db.with_snapshot(|s| s.skill_queues.get(&cid).unwrap()[0].clone());
    assert_eq!(head.start_sp, 123.5);
    assert!(head.started_at_unix.is_some());

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.neural.bonus_remaps = 3;
            ch.neural.last_remap_unix = Some(1_700_000_000);
            ch.neural.remap_cooldown_until_unix = Some(1_731_536_000);
        }
    });
    let info = get_respec_info(&ctx, &sess).unwrap();
    assert_eq!(info.dict_get("freeRespecs"), Some(&PyRep::Integer(3)));
    assert_eq!(
        info.dict_get("lastRespecDate"),
        Some(&PyRep::Integer(1_700_000_000))
    );
    assert_eq!(
        info.dict_get("nextTimedRespec"),
        Some(&PyRep::Integer(1_731_536_000))
    );

    let ghost_sess = auth_session(99, 888_777_666, 98_000_001, 30_000_142);
    assert_eq!(
        get_respec_info(&ctx, &ghost_sess).unwrap_err().code,
        "CharacterNotFound"
    );
}

#[tokio::test]
async fn strip_fitting_validation_flags_and_container_hooks() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let station_id = 60_003_760_i64;
    let struct_id = 1_040_000_000_001_i64;
    let ship_id = 100_000_001_i64;
    let mut sess = auth_session(1, cid, 98_000_001, sys_id);
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                state: "shield_vulnerable".into(),
                ..Default::default()
            },
        );
    });

    // Structure without quantum core rejects StripFitting with QuantumCoreRequired
    sess.station_id = None;
    sess.structure_id = Some(struct_id);
    sess.ship_id = Some(ship_id);
    assert_eq!(
        dispatch(&ctx, &mut sess, "dogmaIM", "StripFitting", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "QuantumCoreRequired"
    );

    // Dock at station for ship validation branches
    sess.structure_id = None;
    sess.station_id = Some(station_id);
    sess.ship_id = None;

    assert_eq!(
        dispatch(&ctx, &mut sess, "dogmaIM", "StripFitting", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "NoActiveShip"
    );

    let mut kw_ship = HashMap::new();
    kw_ship.insert("shipID".into(), PyRep::Integer(999_888_777));
    assert_eq!(
        dispatch(&ctx, &mut sess, "dogmaIM", "StripFitting", &[], &kw_ship)
            .unwrap()
            .unwrap_err()
            .code,
        "ShipNotFound"
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.owner_id = 90_000_002;
            ship.location_id = station_id;
        }
    });
    sess.ship_id = Some(ship_id);
    assert_eq!(
        dispatch(&ctx, &mut sess, "dogmaIM", "StripFitting", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "OwnershipViolation"
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.owner_id = cid;
            ship.location_id = struct_id;
        }
    });
    assert_eq!(
        dispatch(&ctx, &mut sess, "dogmaIM", "StripFitting", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "ShipNotAtStation"
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.location_id = station_id;
        }
        snap.contracts.insert(
            702,
            ContractRecord {
                contract_id: 702,
                issuer_id: cid,
                status: "in_progress".into(),
                item_ids: vec![ship_id],
                ..Default::default()
            },
        );
    });
    assert_eq!(
        dispatch(&ctx, &mut sess, "dogmaIM", "StripFitting", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "ItemLockedInContract"
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.contracts.get_mut(&702) {
            c.status = "finished".into();
        }
        snap.items.insert(
            891_001,
            mk_item(891_001, 487, cid, ship_id, InventoryFlag::LoSlot0, true),
        );
        snap.module_online_states.insert(891_001, true);
        snap.module_active_states.insert(891_001);
        snap.module_active_charges.insert(891_001, 891_002);
        snap.items.insert(
            891_003,
            mk_item(891_003, 487, cid, ship_id, InventoryFlag::SubSystemSlot0, false),
        );
    });

    let strip_ok = dispatch(&ctx, &mut sess, "dogmaIM", "StripFitting", &[], &kw)
        .unwrap()
        .unwrap();
    let stripped_ids = strip_ok
        .dict_get("strippedItemIDs")
        .and_then(|v| v.as_list())
        .unwrap();
    assert!(stripped_ids.contains(&PyRep::Integer(891_001)));
    assert!(!stripped_ids.contains(&PyRep::Integer(891_003)));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.module_online_states.contains_key(&891_001));
        assert!(!snap.module_active_states.contains(&891_001));
        assert!(!snap.module_active_charges.contains_key(&891_001));
    });

    // In-space proximity checks (in-range vs out-of-range) and post_inventory_fitting_hook branches
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    let cont_id = 892_001_i64;
    let cont_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 12)
        .map(|t| t.type_id)
        .unwrap_or(23);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            cont_id,
            mk_item(cont_id, cont_type_id, cid, sys_id as i64, InventoryFlag::None, false),
        );
        snap.sp6_lifecycle.space_object_meta.remove(&cont_id);
    });

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for (id, tid, pos, radius, mode) in [
        (ship_id, 606_u32, glam::DVec3::ZERO, 50.0, MovementMode::Stop),
        (
            cont_id,
            cont_type_id,
            glam::DVec3::new(1_000.0, 0.0, 0.0),
            20.0,
            MovementMode::Rigid,
        ),
    ] {
        let _ = sys_h
            .spawn_ball(Ball {
                id,
                type_id: tid,
                position: pos,
                velocity: glam::DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 1.0e5,
                radius,
                speed_fraction: 0.0,
                mode,
            })
            .await;
    }

    assert!(verify_in_space_container_proximity(&ctx, cid, sys_id as i64, cont_id)
        .await
        .is_ok());

    post_inventory_fitting_hook(&ctx, &sess, None, "Drop", &[]).await;
    assert!(ctx
        .db
        .with_snapshot(|s| s.sp6_lifecycle.space_object_meta.contains_key(&cont_id)));

    let bound = BoundObjectEntry {
        bound_id: 1,
        session_id: 1,
        user_id: 1,
        character_id: cid,
        service: "invbroker".into(),
        object_id: cont_id,
        oid_str: "oid=(1, 1)".into(),
        ref_id: 1,
    };
    post_inventory_fitting_hook(
        &ctx,
        &sess,
        Some(&bound),
        "ValidateItemListCanBeOpened",
        &[PyRep::List(vec![PyRep::Integer(cont_id)])],
    )
    .await;
    post_inventory_fitting_hook(&ctx, &sess, None, "Activate", &[]).await;

    let _ = sys_h.remove_ball(cont_id).await;
    let _ = sys_h
        .spawn_ball(Ball {
            id: cont_id,
            type_id: cont_type_id,
            position: glam::DVec3::new(5_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1.0e3,
            radius: 20.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await;
    assert_eq!(
        verify_in_space_container_proximity(&ctx, cid, sys_id as i64, cont_id)
            .await
            .unwrap_err()
            .code,
        "OutOfRange"
    );
}

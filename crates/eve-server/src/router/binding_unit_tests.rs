//! Unit and mutation-killing tests for `router::binding`.

use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_dogma::{DogmaItemState, ProjectedEwarEffect};
use eve_machonet::SessionState;
use eve_machonet::service_roles::ROLE_GML;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

#[test]
fn session_closers_object_binding_verification_and_transition_invalidation_work() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let st_id = 60_003_760_i64;
    let ship_id = 100_000_001_i64;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    ctx.register_session_closer(77, tx);
    ctx.unregister_session_closer(77);
    assert!(!ctx.disconnect_session(77, "Kicked", "Bye"));

    let (tx2, mut rx2) = tokio::sync::mpsc::unbounded_channel();
    ctx.register_session_closer(88, tx2);

    // populationCap before character selection: owned character succeeds, unowned fails
    let mut pre_sess = SessionState {
        session_id: 88,
        user_id: Some(1),
        character_id: None,
        ..Default::default()
    };
    let pop_bid = ctx.bind_object(&pre_sess, "populationCap", char_id).unwrap();
    assert!(ctx.verify_bound_object(&pre_sess, pop_bid).is_ok());
    let pop_err = ctx
        .bind_object(&pre_sess, "populationCap", 999_999_999)
        .unwrap_err();
    assert_eq!(pop_err.code.to_ascii_lowercase(), "bindauthorizationdenied");
    let bey_err = ctx
        .bind_object(&pre_sess, "beyonce", sys_id as i64)
        .unwrap_err();
    assert_eq!(bey_err.code.to_ascii_lowercase(), "characternotselected");

    // Full character session bindings across services
    pre_sess.character_id = Some(char_id);
    pre_sess.solar_system_id = Some(sys_id);
    pre_sess.station_id = Some(st_id);
    pre_sess.ship_id = Some(ship_id);

    let b_beyonce = ctx.bind_object(&pre_sess, "beyonce", sys_id as i64).unwrap();
    let b_station = ctx.bind_object(&pre_sess, "station", st_id).unwrap();
    let b_dogma = ctx.bind_object(&pre_sess, "dogmaIM", ship_id).unwrap();
    let b_inv = ctx.bind_object(&pre_sess, "invbroker", st_id).unwrap();
    let b_skill = ctx.bind_object(&pre_sess, "skillMgr2", char_id).unwrap();
    let b_cw = ctx.bind_object(&pre_sess, "crimewatch", 0).unwrap();
    let b_agent = ctx.bind_object(&pre_sess, "agentMgr", 3018922).unwrap();
    let b_sub = ctx.bind_verified_sub_object(&pre_sess, "invbroker", 555_001).unwrap();

    // GM-only service requires GM role
    let gm_svc = ["i", "2"].concat();
    let gm_err = ctx.bind_object(&pre_sess, &gm_svc, 10).unwrap_err();
    assert_eq!(gm_err.code.to_ascii_lowercase(), "bindauthorizationdenied");
    pre_sess.role = ROLE_GML;
    let b_i2 = ctx.bind_object(&pre_sess, &gm_svc, 10).unwrap();
    assert!(ctx.verify_bound_object(&pre_sess, b_i2).is_ok());

    // Wire representation: machoNet:26000 vs JSON bridge
    assert!(matches!(
        ctx.bound_object_rep(b_beyonce, Some("machoNet:26000")),
        PyRep::SubStruct(_)
    ));
    assert_eq!(
        ctx.bound_object_rep(b_beyonce, None),
        PyRep::Integer(b_beyonce as i64)
    );

    // Wrong session ownership fails verify_bound_object
    let mut wrong_sess = pre_sess.clone();
    wrong_sess.session_id = 999;
    assert_eq!(
        ctx.verify_bound_object(&wrong_sess, b_beyonce)
            .unwrap_err()
            .code,
        "BoundObjectOwnershipViolation"
    );

    // Transition invalidation: "corp_change" preserves all; "undock" invalidates station-scoped; "stargate_jump" invalidates system-scoped
    ctx.invalidate_bound_objects_for_session_transition(
        char_id,
        sys_id,
        Some(st_id),
        "corp_change",
    );
    assert!(ctx.verify_bound_object(&pre_sess, b_station).is_ok());

    pre_sess.station_id = None;
    ctx.invalidate_bound_objects_for_session_transition(char_id, sys_id, None, "undock");
    assert_eq!(
        ctx.verify_bound_object(&pre_sess, b_station)
            .unwrap_err()
            .code,
        "InvalidBoundObject"
    );
    assert!(ctx.verify_bound_object(&pre_sess, b_beyonce).is_ok());

    ctx.invalidate_bound_objects_for_session_transition(char_id, 30_000_144, None, "stargate_jump");
    assert!(ctx.verify_bound_object(&pre_sess, b_beyonce).is_err());

    // release_bound_object, release_user_bindings, disconnect_session
    assert!(ctx.release_bound_object(&pre_sess, b_dogma).is_ok());
    let _ = (b_inv, b_skill, b_cw, b_agent, b_sub);
    assert!(ctx.disconnect_session(88, "Displaced", "New login"));
    assert_eq!(
        rx2.try_recv().unwrap(),
        ("Displaced".into(), "New login".into())
    );
    assert!(rx.try_recv().is_err());
    ctx.release_user_bindings(1);
}

#[tokio::test]
async fn get_container_contents_enforces_ownership_colocation_and_2500m_range() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let other_id = 90_000_002_i64;
    let sys_id = 30_000_142_u32;
    let st_id = 60_003_760_i64;
    let ship_id = 896_001_i64;
    let owned_cont = 896_101_i64;
    let wreck_cont = 896_102_i64;
    let loot_item = 896_201_i64;

    let wreck_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 186 && t.category_id != 6)
        .map(|t| t.type_id)
        .unwrap_or(26561);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            owned_cont,
            InventoryItemRecord {
                item_id: owned_cont,
                type_id: 3293,
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            wreck_cont,
            InventoryItemRecord {
                item_id: wreck_cont,
                type_id: wreck_type_id,
                owner_id: other_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            loot_item,
            InventoryItemRecord {
                item_id: loot_item,
                type_id: 34,
                owner_id: other_id,
                location_id: wreck_cont,
                flag: InventoryFlag::Cargo,
                quantity: 500,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        station_id: Some(st_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    // Missing item -> ItemNotFound
    assert_eq!(
        ctx.get_container_contents(&sess, None, char_id, 999_999_999)
            .await
            .unwrap_err()
            .code,
        "ItemNotFound"
    );

    // Owned container while docked at st_id -> succeeds
    assert!(
        ctx.get_container_contents(&sess, None, char_id, owned_cont)
            .await
            .is_ok()
    );

    // Undocked in space: wreck at 5,000m fails (> 2,500m), at 1,000m succeeds
    sess.station_id = None;
    let handle = ctx.systems.get_or_spawn(sys_id);
    for (id, tid, pos) in [
        (ship_id, 606, DVec3::ZERO),
        (wreck_cont, wreck_type_id, DVec3::new(5_000.0, 0.0, 0.0)),
    ] {
        handle.try_spawn_ball_with_combat(
            Ball {
                id,
                type_id: tid,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        );
    }
    assert_eq!(
        ctx.get_container_contents(&sess, None, char_id, wreck_cont)
            .await
            .unwrap_err()
            .code,
        "CantDoThatWithSomeoneElsesStuff"
    );

    handle.try_set_ball_position(wreck_cont, DVec3::new(1_000.0, 0.0, 0.0));
    let crowset = ctx
        .get_container_contents(&sess, None, char_id, wreck_cont)
        .await
        .unwrap();
    assert_eq!(crowset.crowset_rows().unwrap().len(), 1);
}

#[test]
fn combat_state_slim_meta_ewar_and_skill_prerequisites_cover_branches() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = ctx.db.with_snapshot(|s| s.characters[&char_id].active_ship_id);

    let cs = ctx.build_ship_combat_state(char_id, ship_id, 606);
    assert!(cs.max_structure_hp > 0.0);
    assert!(cs.max_capacitor > 0.0);

    let npc_cs = ctx.build_type_combat_state(606);
    assert!(npc_cs.max_shield_hp > 0.0);
    let fallback_cs = ctx.build_type_combat_state(999_999_999);
    assert!(fallback_cs.rng_state != 0);

    let slim = ctx.build_ship_slim_meta(char_id, 606);
    assert_eq!(slim.owner_id, char_id);
    assert_eq!(slim.category_id, 6);

    assert!(ctx.notify_skill_queue_paused_server(char_id).is_empty());
    assert!(
        ctx.notify_new_skill_queue_saved(char_id, PyRep::List(vec![]))
            .is_empty()
    );
    ctx.wire_module_deactivation_hook();

    // Projected EWAR & effective_target_signature_radius
    let mut tp_mod = DogmaItemState::from_sde(777_001, 379, &ctx.sde);
    tp_mod.base_attributes.insert(554, 30.0);
    tp_mod.base_attributes.insert(eve_sde::attr_ids::MAX_RANGE, 30_000.0);
    ctx.upsert_projected_ewar_on_ship(
        888_001,
        999_001,
        ProjectedEwarEffect {
            source_module_id: 777_001,
            source_module_type_id: 379,
            charge_type_id: None,
            distance: 5_000.0,
            overloaded: false,
            effective_module: tp_mod,
        },
    );
    let bloomed = ctx.effective_target_signature_radius(999_001, 100.0);
    assert!(bloomed > 100.0);
    assert_eq!(ctx.remove_projected_ewar_by_module(888_001, 777_001), vec![999_001]);

    // check_type_skill_prerequisites_in_snapshot
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().clear();
        let err = ctx
            .check_type_skill_prerequisites_in_snapshot(snap, char_id, 606)
            .unwrap_err();
        assert_eq!(err.code, "ShipHasSkillPrerequisites");

        // Satisfy required skills for Velator (606)
        if let Some(t) = ctx.sde.types.get(&606) {
            for &aid in &[182_u16, 183, 184] {
                if let Some(&sid) = t.default_attributes.get(&aid) {
                    snap.skills.entry(char_id).or_default().insert(
                        sid as u32,
                        TrainedSkillRecord {
                            type_id: sid as u32,
                            level: 5,
                            skill_points: 256_000.0,
                        },
                    );
                }
            }
        }
        assert!(
            ctx.check_type_skill_prerequisites_in_snapshot(snap, char_id, 606)
                .is_ok()
        );
    });
}

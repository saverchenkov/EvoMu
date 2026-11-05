//! Unit tests for session validation and object binding lifecycle in `router::session` and `router::binding`.

use crate::router::binding::effect_id_for_module_sde;
use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_dogma::{DogmaItemState, ProjectedEwarEffect};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

#[test]
fn session_guards_validate_shutdown_auth_gm_character_and_bound_system() {
    let ctx = test_ctx();
    assert!(!ctx.is_shutting_down());
    ctx.cluster_state.write().unwrap().shutdown_in_progress = true;
    assert!(ctx.is_shutting_down());

    let mut session = SessionState::default();
    assert!(ServerContext::require_auth(&session).is_err());
    assert!(ServerContext::require_gm_role(&session).is_err());
    assert!(ServerContext::require_character(&session).is_err());
    assert!(ServerContext::require_bound_system(None, "beyonce").is_err());

    session.user_id = Some(42);
    assert_eq!(ServerContext::require_auth(&session).unwrap(), 42);
    assert!(ServerContext::require_gm_role(&session).is_err());
    assert!(ServerContext::require_character(&session).is_err());

    session.role = service_roles::ROLE_GMH;
    assert_eq!(ServerContext::require_gm_role(&session).unwrap(), 42);

    session.character_id = Some(90_000_001);
    assert_eq!(
        ServerContext::require_character(&session).unwrap(),
        (42, 90_000_001)
    );
    let mut wrong_svc_entry = crate::router::BoundObjectEntry {
        bound_id: 1,
        service: "invbroker".into(),
        object_id: 30_000_142,
        user_id: 42,
        character_id: 90_000_001,
        session_id: 10,
        oid_str: "N=1:1".into(),
        ref_id: 1,
    };
    assert!(ServerContext::require_bound_system(Some(&wrong_svc_entry), "beyonce").is_err());

    wrong_svc_entry.service = "beyonce".into();
    assert_eq!(
        ServerContext::require_bound_system(Some(&wrong_svc_entry), "beyonce").unwrap(),
        30_000_142
    );
}

#[tokio::test]
async fn object_binding_lifecycle_session_closer_ewar_and_prereqs() {
    let ctx = test_ctx();
    let mut session = SessionState::default();
    session.session_id = 555;
    session.user_id = Some(1);
    session.character_id = Some(90_000_001);
    session.solar_system_id = Some(30_000_142);
    session.ship_id = Some(100_000_001);

    // Bind across multiple services & sub-objects
    let bid1 = ctx.bind_object(&mut session, "beyonce", 30_000_142).unwrap();
    let bid2 = ctx.bind_object(&mut session, "invbroker", 100_000_001).unwrap();
    let sub_bid = ctx
        .bind_verified_sub_object(&mut session, "invbroker", 60_003_760)
        .unwrap();
    assert_ne!(bid1, bid2);
    assert_ne!(bid2, sub_bid);

    let rep_macho = ctx.bound_object_rep(bid1, Some("machoNet:26000"));
    assert!(matches!(rep_macho, PyRep::SubStruct(_)));
    let rep_direct = ctx.bound_object_rep(bid1, None);
    assert_eq!(rep_direct, PyRep::Integer(bid1 as i64));

    assert!(ctx.verify_bound_object(&session, bid1).is_ok());
    assert!(ctx.verify_bound_object(&session, sub_bid).is_ok());
    let mut wrong_sess = session.clone();
    wrong_sess.session_id = 999;
    assert!(ctx.verify_bound_object(&wrong_sess, bid1).is_err());

    // Invalidate system-scoped bindings on solar system transition
    ctx.invalidate_bound_objects_for_session_transition(
        90_000_001,
        30_000_144,
        None,
        "system_jump",
    );
    assert!(ctx.verify_bound_object(&session, bid1).is_err());
    assert!(ctx.verify_bound_object(&session, bid2).is_ok());

    assert!(ctx.release_bound_object(&session, bid2).is_ok());
    ctx.release_user_bindings(1);

    // Session closer + disconnect_session
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    ctx.register_session_closer(session.session_id, tx);
    assert!(ctx.disconnect_session(session.session_id, "Kick", "test disconnect"));
    let (code, msg) = rx.try_recv().unwrap();
    assert_eq!(code, "Kick");
    assert_eq!(msg, "test disconnect");
    ctx.unregister_session_closer(session.session_id);

    // Container contents, combat state, slim meta, and EWAR helpers
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_000_099,
            eve_db::InventoryItemRecord {
                item_id: 100_000_099,
                type_id: 3293,
                owner_id: 90_000_001,
                location_id: 100_000_001,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: Default::default(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let _contents = ctx
        .get_container_contents(&session, None, 90_000_001, 100_000_099)
        .await
        .unwrap();
    let ship_cs = ctx.build_ship_combat_state(90_000_001, 100_000_001, 587);
    assert!(ship_cs.max_shield_hp > 0.0);
    let type_cs = ctx.build_type_combat_state(587);
    assert!(type_cs.max_armor_hp > 0.0);
    let slim = ctx.build_ship_slim_meta(90_000_001, 587);
    assert_eq!(slim.char_id, Some(90_000_001));

    ctx.wire_module_deactivation_hook();
    let _ = ctx.db.acquire_online_lock(1, 101, Some(90_000_001), true, ctx.now_unix());
    assert!(!ctx.notify_skill_queue_paused_server(90_000_001).is_empty());
    let _saved_notifs = ctx.notify_new_skill_queue_saved(90_000_001, PyRep::List(vec![]));

    // Projected EWAR (Target Painter bloom) & skill prereq check
    let _ = ctx.get_or_rebuild_ship_fitting(90_000_001, 100_000_001);
    let eff = ProjectedEwarEffect {
        source_module_id: 900_800_001,
        source_module_type_id: 2185,
        charge_type_id: None,
        distance: 5_000.0,
        overloaded: false,
        effective_module: DogmaItemState::from_sde(900_800_001, 2185, &ctx.sde),
    };
    ctx.upsert_projected_ewar_on_ship(100_000_002, 100_000_001, eff);
    let eff_sig = ctx.effective_target_signature_radius(100_000_001, 40.0);
    assert!(eff_sig > 0.0);
    let affected = ctx.remove_projected_ewar_by_module(100_000_002, 900_800_001);
    assert_eq!(affected, vec![100_000_001]);

    ctx.db.with_snapshot(|snap| {
        let missing = ctx.check_type_skill_prerequisites_in_snapshot(snap, 90_000_001, 22456);
        assert!(missing.is_err());
    });
    let _eff_id = effect_id_for_module_sde(&ctx.sde, 587, None);
}

#[tokio::test]
async fn bind_object_authorization_and_space_container_distance_checks() {
    let ctx = test_ctx();
    let mut sess = SessionState::default();
    sess.session_id = 77_001;

    // Binding without character selected fails
    let bind_err = ctx.bind_object(&mut sess, "dogmaIM", 100_000_001).unwrap_err();
    assert_eq!(bind_err.code, "Unauthorized");

    sess.user_id = Some(101);
    sess.character_id = Some(90_000_001);
    sess.solar_system_id = Some(30_000_142);
    sess.ship_id = Some(100_000_001);
    sess.station_id = None;
    sess.structure_id = None;

    // Binding unowned character fails
    assert!(ctx.bind_object(&mut sess, "dogmaIM", 90_000_002).is_err());

    // Allowed bindings across services
    assert!(ctx.bind_object(&mut sess, "populationCap", 90_000_001).is_ok());
    assert!(ctx.bind_object(&mut sess, "beyonce", 30_000_142).is_ok());
    assert!(ctx.bind_object(&mut sess, "scanMgr", 30_000_142).is_ok());
    assert!(ctx.bind_object(&mut sess, "invbroker", 30_000_142).is_ok());
    assert!(ctx.bind_object(&mut sess, "skillMgr2", 0).is_ok());
    assert!(ctx.bind_object(&mut sess, "crimewatch", 0).is_ok());
    assert!(ctx.bind_object(&mut sess, "agentMgr", 3018418).is_ok());
    // Non-GM cannot bind `i2`
    assert!(ctx.bind_object(&mut sess, "i2", 1).is_err());

    // Space container contents within 2,500m vs beyond 2,500m vs non-openable ship
    let sys_id = 30_000_142;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let ship_pos = glam::DVec3::new(50_000.0, 0.0, 0.0);
    let cs = ctx.build_ship_combat_state(90_000_001, 100_000_001, 587);
    sys_handle.try_spawn_ball_with_combat(
        eve_destiny::Ball {
            id: 100_000_001,
            type_id: 587,
            position: ship_pos,
            velocity: glam::DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_100_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        },
        cs,
    );

    let near_can = 900_810_001;
    let far_can = 900_810_002;
    let other_ship = 900_810_003;
    ctx.db.with_mut_snapshot(|snap| {
        for (id, tid, owner) in [
            (near_can, 23u32, 90_000_002i64),
            (far_can, 23u32, 90_000_001i64),
            (other_ship, 587u32, 90_000_002i64),
        ] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: owner,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        }
    });
    for (id, tid, pos) in [
        (near_can, 23u32, ship_pos + glam::DVec3::new(1_000.0, 0.0, 0.0)),
        (far_can, 23u32, ship_pos + glam::DVec3::new(10_000.0, 0.0, 0.0)),
        (other_ship, 587u32, ship_pos + glam::DVec3::new(500.0, 0.0, 0.0)),
    ] {
        let _ = sys_handle
            .spawn_ball(eve_destiny::Ball {
                id,
                type_id: tid,
                position: pos,
                velocity: glam::DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000.0,
                radius: 20.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            })
            .await;
    }

    // Other player's jetcan within 2.5km is accessible; owned far jetcan (>2.5km) and other ship are denied
    assert!(ctx
        .get_container_contents(&sess, None, 90_000_001, near_can)
        .await
        .is_ok());
    assert_eq!(
        ctx.get_container_contents(&sess, None, 90_000_001, far_can)
            .await
            .unwrap_err()
            .code,
        "CantDoThatWithSomeoneElsesStuff"
    );
    assert_eq!(
        ctx.get_container_contents(&sess, None, 90_000_001, other_ship)
            .await
            .unwrap_err()
            .code,
        "CantDoThatWithSomeoneElsesStuff"
    );
}


//! Unit tests for `router::binding_transition_policy` (`#1190`).

use super::*;
use crate::router::context::{BoundObjectEntry, ServerContext};
use crate::router::session_transition::{SessionTransitionSpec, TransitionKind};
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    let ctx = if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let created = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&created.sde));
        created
    };
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(
        1_800_000_000,
    )));
    ctx
}

fn seed_ships_for_policy_tests(
    ctx: &ServerContext,
    char_id: i64,
    other_char: i64,
    st_id: i64,
    ships: &[i64],
    foreign_ship: i64,
    non_ship_item: i64,
) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(st_id);
            ch.structure_id = None;
            ch.solar_system_id = 30_000_142;
            ch.active_ship_id = ships[0];
        }
        for (idx, &ship_id) in ships.iter().enumerate() {
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: if idx == 2 { 670 } else { 587 },
                    owner_id: char_id,
                    location_id: st_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        snap.items.insert(
            foreign_ship,
            InventoryItemRecord {
                item_id: foreign_ship,
                type_id: 587,
                owner_id: other_char,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            non_ship_item,
            InventoryItemRecord {
                item_id: non_ship_item,
                type_id: 34,
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                ..Default::default()
            },
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scene_epoch_increments_monotonically_across_all_ten_transition_kinds() {
    let ctx = test_ctx();
    let user_id = 1_u64;
    let char_id = 90_000_001_i64;
    let sid = 880_001_u64;
    let jita_sys = 30_000_142_u32;
    let perimeter_sys = 30_000_144_u32;
    let jita_station = 60_003_760_i64;
    let ship_a = 1_897_400_001_i64;
    let ship_b = 1_897_400_002_i64;
    let capsule_id = 1_897_400_003_i64;

    seed_ships_for_policy_tests(
        &ctx,
        char_id,
        90_000_002,
        jita_station,
        &[ship_a, ship_b, capsule_id],
        1_897_400_098,
        1_897_400_099,
    );
    ctx.db
        .acquire_online_lock(user_id, sid, Some(char_id), true, ctx.now_unix())
        .expect("online lock");

    let mut sess = SessionState::new(sid);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(jita_sys);
    sess.station_id = Some(jita_station);
    sess.ship_id = Some(ship_a);

    assert_eq!(ctx.session_scene_epoch(sid), 0);

    let transitions: Vec<(TransitionKind, &str, bool)> = vec![
        (
            TransitionKind::CharacterLogin {
                solar_system_id: jita_sys,
                dock_id: Some((jita_station, false)),
                ship_id: Some(ship_a),
            },
            "login",
            false,
        ),
        (
            TransitionKind::Undock {
                solar_system_id: jita_sys,
            },
            "undock",
            true,
        ),
        (
            TransitionKind::JumpSystem {
                solar_system_id: perimeter_sys,
            },
            "stargate_jump",
            true,
        ),
        (
            TransitionKind::Dock {
                dock_id: jita_station,
                is_structure: false,
            },
            "dock",
            false,
        ),
        (
            TransitionKind::BoardShip { ship_id: ship_b },
            "board_ship",
            false,
        ),
        (
            TransitionKind::EjectToPod { capsule_id },
            "eject",
            false,
        ),
        (
            TransitionKind::CloneJump {
                solar_system_id: perimeter_sys,
                dock_id: None,
                ship_id: Some(ship_a),
            },
            "clone_jump",
            false,
        ),
        (
            TransitionKind::DeathRespawn {
                solar_system_id: jita_sys,
                dock_id: Some((jita_station, false)),
                ship_id: capsule_id,
            },
            "death_respawn",
            false,
        ),
        (
            TransitionKind::Teleport {
                solar_system_id: perimeter_sys,
                dock_id: None,
                ship_id: None,
            },
            "slash_tr",
            true,
        ),
        (TransitionKind::Logout, "logout", false),
    ];

    let mut expected_epoch = 0_u64;
    for (kind, reason, use_async) in transitions {
        let spec = SessionTransitionSpec::for_kind(char_id, kind, reason)
            .with_emit_on_session_changed(false)
            .with_sync_local_chat(false);
        let res = if use_async {
            ctx.execute_session_transition(Some(&mut sess), spec)
                .await
                .expect("async transition succeeds")
        } else {
            ctx.execute_session_transition_sync(Some(&mut sess), spec)
                .expect("sync transition succeeds")
        };
        expected_epoch += 1;
        assert_eq!(res.scene_epoch, expected_epoch, "kind={}", kind.label());
        assert_eq!(ctx.session_scene_epoch(sid), expected_epoch);
        assert!(ctx.verify_session_scene_epoch(sid, expected_epoch));
        assert!(!ctx.verify_session_scene_epoch(sid, expected_epoch - 1));
        let rec = ctx
            .current_transition_epoch_record(sid)
            .expect("atomic epoch record exists");
        assert_eq!(rec.departed_scene_epoch, expected_epoch - 1);
        assert_eq!(rec.entered_scene_epoch, expected_epoch);
        assert_eq!(rec.kind_label, kind.label());
    }

    // In-connection safe logoff releases bindings without rewinding scene_epoch;
    // selecting another character advances epoch monotonically from 10 -> 11.
    ctx.release_session_bindings(sid);
    assert_eq!(ctx.session_scene_epoch(sid), 10);
    let relogin = ctx
        .execute_session_transition_sync(
            Some(&mut sess),
            SessionTransitionSpec::for_kind(
                char_id,
                TransitionKind::CharacterLogin {
                    solar_system_id: jita_sys,
                    dock_id: Some((jita_station, false)),
                    ship_id: Some(ship_a),
                },
                "login",
            )
            .with_emit_on_session_changed(false),
        )
        .expect("relogin succeeds");
    assert_eq!(relogin.scene_epoch, 11);
    assert_eq!(ctx.session_scene_epoch(sid), 11);

    // Full transport disconnect cleans up epoch storage to prevent leaks.
    ctx.clear_session_scene_epoch(sid);
    assert_eq!(ctx.session_scene_epoch(sid), 0);
}

#[test]
fn atomic_transition_records_departed_scene_bindings_and_enforces_epoch_pin() {
    let ctx = test_ctx();
    let user_id = 1_u64;
    let char_id = 90_000_001_i64;
    let sid = 880_002_u64;
    let jita_sys = 30_000_142_u32;
    let perimeter_sys = 30_000_144_u32;
    let jita_station = 60_003_760_i64;
    let ship_a = 1_897_400_101_i64;

    seed_ships_for_policy_tests(
        &ctx,
        char_id,
        90_000_002,
        jita_station,
        &[ship_a, 1_897_400_102, 1_897_400_103],
        1_897_400_198,
        1_897_400_199,
    );
    ctx.db
        .acquire_online_lock(user_id, sid, Some(char_id), true, ctx.now_unix())
        .expect("online lock");

    let mut sess = SessionState::new(sid);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(jita_sys);
    sess.station_id = Some(jita_station);
    sess.ship_id = Some(ship_a);

    // Step 1: CharacterLogin -> epoch 1.
    let login_res = ctx
        .execute_session_transition_sync(
            Some(&mut sess),
            SessionTransitionSpec::for_kind(
                char_id,
                TransitionKind::CharacterLogin {
                    solar_system_id: jita_sys,
                    dock_id: Some((jita_station, false)),
                    ship_id: Some(ship_a),
                },
                "login",
            )
            .with_emit_on_session_changed(false),
        )
        .expect("login");
    assert_eq!(login_res.scene_epoch, 1);

    // Bind station-scoped objects in epoch 1.
    let b_station = ctx
        .bind_object(&sess, "stationSvc", jita_station)
        .expect("bind stationSvc");
    let b_ship = ctx
        .bind_object(&sess, "ship", jita_station)
        .expect("bind ship");
    let b_inv = ctx
        .bind_object(&sess, "invbroker", jita_station)
        .expect("bind invbroker");

    // Step 2: Undock -> epoch 1 -> 2 atomically invalidates bindings & records departed scene.
    let undock_res = ctx
        .execute_session_transition_sync(
            Some(&mut sess),
            SessionTransitionSpec::for_kind(
                char_id,
                TransitionKind::Undock {
                    solar_system_id: jita_sys,
                },
                "undock",
            )
            .with_emit_on_session_changed(false)
            .with_sync_local_chat(false),
        )
        .expect("undock");
    assert_eq!(undock_res.scene_epoch, 2);

    let rec = ctx
        .current_transition_epoch_record(sid)
        .expect("undock epoch record");
    assert_eq!(rec.departed_scene_epoch, 1);
    assert_eq!(rec.entered_scene_epoch, 2);
    assert_eq!(rec.departed_dock_id, Some(jita_station));
    assert!(rec.invalidated_bound_ids.contains(&b_station));
    assert!(rec.invalidated_bound_ids.contains(&b_ship));
    // Station invbroker in Jita remains valid while still in Jita solar system.
    assert!(!rec.invalidated_bound_ids.contains(&b_inv));

    let empty_kw = HashMap::new();
    // Pinned lookup on Undock-departed b_ship succeeds ONLY for departed epoch 1 (not 0 or 2).
    assert!(matches!(
        ctx.resolve_late_call_for_epoch(
            &sess,
            b_ship,
            1,
            "GetDirtTimestamp",
            &[PyRep::Integer(ship_a)],
            &empty_kw
        ),
        LateCallResolution::DispatchWithBinding(ref e) if e.bound_id == b_ship
    ));
    assert!(matches!(
        ctx.resolve_late_call_for_epoch(
            &sess,
            b_ship,
            0,
            "GetDirtTimestamp",
            &[PyRep::Integer(ship_a)],
            &empty_kw
        ),
        LateCallResolution::RejectStale
    ));
    assert!(matches!(
        ctx.resolve_late_call_for_epoch(
            &sess,
            b_ship,
            2,
            "GetDirtTimestamp",
            &[PyRep::Integer(ship_a)],
            &empty_kw
        ),
        LateCallResolution::RejectStale
    ));
    // Non-policy service `stationSvc` stays fail-closed even inside grace window.
    assert!(matches!(
        ctx.resolve_late_call_with_policy(&sess, b_station, "GetStation", &[], &empty_kw),
        LateCallResolution::RejectStale
    ));

    // Jump to Perimeter (epoch 2 -> 3) invalidates Jita invbroker and pins it to departed epoch 2.
    let jump_res = ctx
        .execute_session_transition_sync(
            Some(&mut sess),
            SessionTransitionSpec::for_kind(
                char_id,
                TransitionKind::JumpSystem {
                    solar_system_id: perimeter_sys,
                },
                "stargate_jump",
            )
            .with_emit_on_session_changed(false)
            .with_sync_local_chat(false),
        )
        .expect("jump");
    assert_eq!(jump_res.scene_epoch, 3);
    let jump_rec = ctx
        .current_transition_epoch_record(sid)
        .expect("jump epoch record");
    assert_eq!(jump_rec.departed_scene_epoch, 2);
    assert_eq!(jump_rec.entered_scene_epoch, 3);
    assert!(jump_rec.invalidated_bound_ids.contains(&b_inv));
    assert!(matches!(
        ctx.resolve_late_call_for_epoch(&sess, b_inv, 2, "GetSelfInvItem", &[], &empty_kw),
        LateCallResolution::DispatchWithBinding(ref e) if e.bound_id == b_inv
    ));
    assert!(matches!(
        ctx.resolve_late_call_for_epoch(&sess, b_inv, 1, "GetSelfInvItem", &[], &empty_kw),
        LateCallResolution::RejectStale
    ));
    assert!(matches!(
        ctx.resolve_late_call_for_epoch(&sess, b_inv, 3, "GetSelfInvItem", &[], &empty_kw),
        LateCallResolution::RejectStale
    ));
    assert_eq!(ctx.transition_epoch_records(sid).len(), 3);
}

#[test]
fn declarative_late_call_policy_table_covers_benign_rebind_and_stale_fail_closed() {
    let ctx = test_ctx();
    let user_id = 1_u64;
    let char_id = 90_000_001_i64;
    let sid = 880_003_u64;
    let jita_sys = 30_000_142_u32;
    let perimeter_sys = 30_000_144_u32;
    let jita_station = 60_003_760_i64;
    let ship_a = 1_897_400_201_i64;

    seed_ships_for_policy_tests(
        &ctx,
        char_id,
        90_000_002,
        jita_station,
        &[ship_a, 1_897_400_202, 1_897_400_203],
        1_897_400_298,
        1_897_400_299,
    );
    ctx.db
        .acquire_online_lock(user_id, sid, Some(char_id), true, ctx.now_unix())
        .expect("online lock");

    assert_eq!(
        lookup_late_call_policy("invbroker", "GetSelfInvItem"),
        LateCallPolicy::Benign(LateCallBenignResponse::DepartedBindingReadOnly)
    );
    assert_eq!(
        lookup_late_call_policy("invbroker", "GetAvailableTurretSlots"),
        LateCallPolicy::Benign(LateCallBenignResponse::DepartedBindingReadOnly)
    );
    assert_eq!(
        lookup_late_call_policy("ship", "GetDirtTimestamp"),
        LateCallPolicy::Benign(LateCallBenignResponse::DepartedBindingReadOnly)
    );
    assert_eq!(
        lookup_late_call_policy("crimewatch", "GetClientStates"),
        LateCallPolicy::Benign(LateCallBenignResponse::DepartedBindingReadOnly)
    );
    assert_eq!(
        lookup_late_call_policy("skillMgr2", "GetMySkillHandler"),
        LateCallPolicy::Benign(LateCallBenignResponse::TeardownGraceReadOnly {
            creates_temp_binding: true,
        })
    );
    for method in [
        "GetSkills",
        "GetSkillQueue",
        "GetSkillQueueAndFreePoints",
        "GetEndOfTraining",
        "GetAttributes",
        "GetFreeSkillPoints",
        "GetImplants",
        "GetBoosters",
        "GetSkillHistory",
        "GetRespecInfo",
        "GetCharacterAttributeModifiers",
        "GetSkillRequirements",
    ] {
        assert_eq!(
            lookup_late_call_policy("skillMgr2", method),
            LateCallPolicy::Benign(LateCallBenignResponse::TeardownGraceReadOnly {
                creates_temp_binding: false,
            }),
            "skillMgr2.{method} must map to TeardownGraceReadOnly"
        );
    }

    // Mutating and unknown methods always resolve fail-closed to StaleObjectGone.
    for (svc, method) in [
        ("invbroker", "Add"),
        ("invbroker", "Remove"),
        ("ship", "Undock"),
        ("ship", "AssembleShip"),
        ("crimewatch", "SetSafetyLevel"),
        ("skillMgr2", "SaveSkillQueue"),
        ("beyonce", "CmdWarpToStuff"),
        ("stationSvc", "GetStation"),
    ] {
        assert_eq!(
            lookup_late_call_policy(svc, method),
            LateCallPolicy::StaleObjectGone,
            "{svc}.{method} must fail closed to StaleObjectGone"
        );
    }

    // Verify all LateCallPolicy execution branches (`Benign(EmptyNone/EmptyList)`, `RebindToCurrentScene`, `StaleObjectGone`).
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(perimeter_sys);
    sess.station_id = None;
    sess.ship_id = Some(ship_a);

    let old_sys_entry = BoundObjectEntry {
        bound_id: 99_001,
        session_id: sid,
        user_id,
        character_id: char_id,
        service: "beyonce".to_string(),
        object_id: i64::from(jita_sys),
        oid_str: "N=888:99001".to_string(),
        ref_id: 1,
    };
    assert!(matches!(
        ctx.execute_late_call_policy(
            &sess,
            old_sys_entry.clone(),
            LateCallPolicy::Benign(LateCallBenignResponse::EmptyNone),
        ),
        LateCallResolution::RespondImmediate(PyRep::None)
    ));
    assert!(matches!(
        ctx.execute_late_call_policy(
            &sess,
            old_sys_entry.clone(),
            LateCallPolicy::Benign(LateCallBenignResponse::EmptyList),
        ),
        LateCallResolution::RespondImmediate(PyRep::List(items)) if items.is_empty()
    ));
    assert!(matches!(
        ctx.execute_late_call_policy(&sess, old_sys_entry.clone(), LateCallPolicy::StaleObjectGone),
        LateCallResolution::RejectStale
    ));
    match ctx.execute_late_call_policy(
        &sess,
        old_sys_entry,
        LateCallPolicy::RebindToCurrentScene,
    ) {
        LateCallResolution::DispatchWithBinding(rebound) => {
            assert_eq!(rebound.service, "beyonce");
            assert_eq!(rebound.object_id, i64::from(perimeter_sys));
        }
        other => panic!("expected RebindToCurrentScene for beyonce, got {other:?}"),
    }

    // Rebinding a station service while now in space fails closed (`RejectStale`).
    let old_dock_entry = BoundObjectEntry {
        bound_id: 99_002,
        session_id: sid,
        user_id,
        character_id: char_id,
        service: "stationSvc".to_string(),
        object_id: jita_station,
        oid_str: "N=888:99002".to_string(),
        ref_id: 2,
    };
    assert!(matches!(
        ctx.execute_late_call_policy(
            &sess,
            old_dock_entry,
            LateCallPolicy::RebindToCurrentScene,
        ),
        LateCallResolution::RejectStale
    ));
}

#[test]
fn predicate_and_cross_epoch_logout_relogin_displaced_rejection() {
    let ctx = test_ctx();
    let user_id = 1_u64;
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let sid = 880_004_u64;
    let jita_sys = 30_000_142_u32;
    let perimeter_sys = 30_000_144_u32;
    let jita_station = 60_003_760_i64;
    let ship_a = 1_897_400_301_i64;
    let foreign_ship = 1_897_400_398_i64;
    let non_ship = 1_897_400_399_i64;

    seed_ships_for_policy_tests(
        &ctx,
        char_id,
        other_char,
        jita_station,
        &[ship_a, 1_897_400_302, 1_897_400_303],
        foreign_ship,
        non_ship,
    );
    ctx.db
        .acquire_online_lock(user_id, sid, Some(char_id), true, ctx.now_unix())
        .expect("online lock");

    let mut sess = SessionState::new(sid);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(jita_sys);
    sess.station_id = Some(jita_station);
    sess.ship_id = Some(ship_a);

    let b_inv = ctx
        .bind_object(&sess, "invbroker", jita_station)
        .expect("bind invbroker");
    ctx.execute_session_transition_sync(
        Some(&mut sess),
        SessionTransitionSpec::for_kind(
            char_id,
            TransitionKind::Undock {
                solar_system_id: jita_sys,
            },
            "undock",
        )
        .with_emit_on_session_changed(false)
        .with_sync_local_chat(false),
    )
    .expect("undock");

    let kw = HashMap::new();
    // #1150 ship.GetDirtTimestamp ownership check denies foreign ship and non-ship item.
    assert!(ctx
        .allocate_late_call_grace_binding(
            &sess,
            "ship",
            jita_station,
            "GetDirtTimestamp",
            &[PyRep::Integer(foreign_ship)],
            &kw,
        )
        .is_none());
    assert!(ctx
        .allocate_late_call_grace_binding(
            &sess,
            "ship",
            jita_station,
            "GetDirtTimestamp",
            &[PyRep::Integer(non_ship)],
            &kw,
        )
        .is_none());
    let dirt_bid = ctx
        .allocate_late_call_grace_binding(
            &sess,
            "ship",
            jita_station,
            "GetDirtTimestamp",
            &[PyRep::Integer(ship_a)],
            &kw,
        )
        .expect("owned ship dirt grace");

    // #1209 crimewatch.GetClientStates denies non-empty args after JumpSystem.
    ctx.execute_session_transition_sync(
        Some(&mut sess),
        SessionTransitionSpec::for_kind(
            char_id,
            TransitionKind::JumpSystem {
                solar_system_id: perimeter_sys,
            },
            "stargate_jump",
        )
        .with_emit_on_session_changed(false)
        .with_sync_local_chat(false),
    )
    .expect("jump");
    // #1146 policy retains departed Jita invbroker binding across JumpSystem only for matching service & object.
    assert!(ctx.authorize_departed_object_late_call(&sess, "invbroker", b_inv, jita_station));
    assert!(!ctx.authorize_departed_object_late_call(&sess, "beyonce", b_inv, jita_station));
    assert!(ctx
        .allocate_late_call_grace_binding(
            &sess,
            "crimewatch",
            i64::from(jita_sys),
            "GetClientStates",
            &[PyRep::Integer(1)],
            &kw,
        )
        .is_none());
    assert!(ctx
        .allocate_late_call_grace_binding(
            &sess,
            "crimewatch",
            i64::from(jita_sys),
            "GetClientStates",
            &[],
            &kw,
        )
        .is_some());

    // Cross-epoch Dock back into jita_station clears departed dock grace from earlier epoch.
    ctx.execute_session_transition_sync(
        Some(&mut sess),
        SessionTransitionSpec::for_kind(
            char_id,
            TransitionKind::Dock {
                dock_id: jita_station,
                is_structure: false,
            },
            "dock",
        )
        .with_emit_on_session_changed(false)
        .with_sync_local_chat(false),
    )
    .expect("dock back");
    assert!(ctx
        .allocate_late_call_grace_binding(
            &sess,
            "ship",
            jita_station,
            "GetDirtTimestamp",
            &[PyRep::Integer(ship_a)],
            &kw,
        )
        .is_none());

    // Logout -> CharacterLogin on SAME session_id/user_id/char_id rejects all prior epoch grace!
    ctx.execute_session_transition_sync(
        Some(&mut sess),
        SessionTransitionSpec::for_kind(char_id, TransitionKind::Logout, "logout")
            .with_emit_on_session_changed(false),
    )
    .expect("logout");
    ctx.execute_session_transition_sync(
        Some(&mut sess),
        SessionTransitionSpec::for_kind(
            char_id,
            TransitionKind::CharacterLogin {
                solar_system_id: jita_sys,
                dock_id: Some((jita_station, false)),
                ship_id: Some(ship_a),
            },
            "login",
        )
        .with_emit_on_session_changed(false),
    )
    .expect("relogin");
    assert!(matches!(
        ctx.resolve_late_call_with_policy(&sess, b_inv, "GetSelfInvItem", &[], &kw),
        LateCallResolution::RejectStale
    ));
    assert!(matches!(
        ctx.resolve_late_call_with_policy(
            &sess,
            dirt_bid,
            "GetDirtTimestamp",
            &[PyRep::Integer(ship_a)],
            &kw,
        ),
        LateCallResolution::RejectStale
    ));

    // Undock again, then displace online lock -> immediate rejection across session boundary.
    ctx.execute_session_transition_sync(
        Some(&mut sess),
        SessionTransitionSpec::for_kind(
            char_id,
            TransitionKind::Undock {
                solar_system_id: jita_sys,
            },
            "undock",
        )
        .with_emit_on_session_changed(false)
        .with_sync_local_chat(false),
    )
    .expect("second undock");
    ctx.db
        .acquire_online_lock(user_id, sid + 50, Some(char_id), true, ctx.now_unix())
        .expect("displace session");
    assert!(ctx
        .allocate_late_call_grace_binding(
            &sess,
            "ship",
            jita_station,
            "GetDirtTimestamp",
            &[PyRep::Integer(ship_a)],
            &kw,
        )
        .is_none());
}

#[test]
fn safe_logoff_teardown_grace_shares_declarative_late_call_policy_table() {
    let ctx = test_ctx();
    let user_id = 1_u64;
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let sid = 880_005_u64;
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(30_000_142);
    sess.station_id = Some(60_003_760);
    ctx.db
        .acquire_online_lock(user_id, sid, Some(char_id), true, ctx.now_unix())
        .expect("online lock");

    let temp_skill_bid = ctx
        .bind_object(&sess, "skillMgr2", char_id)
        .expect("bind skillMgr2");
    let other_ship_bid = ctx
        .bind_object(&sess, "invbroker", 60_003_760)
        .expect("bind invbroker");

    // Teardown session sits at character selection (`character_id == None`).
    sess.character_id = None;
    sess.station_id = None;
    sess.solar_system_id = None;
    let kw = HashMap::new();

    // Unbound skillMgr2.GetMySkillHandler authorizes temp binding creation (`Some(true)`).
    assert_eq!(
        ctx.evaluate_safe_logoff_teardown_late_call(
            &sess,
            char_id,
            "skillMgr2",
            None,
            "GetMySkillHandler",
            &[],
            &kw,
        ),
        Some(true)
    );
    // Bound temp skillMgr2 authorizes read-only getters (`Some(false)`), rejects non-skillMgr2 / wrong char.
    assert_eq!(
        ctx.evaluate_safe_logoff_teardown_late_call(
            &sess,
            char_id,
            "",
            Some(temp_skill_bid),
            "GetSkills",
            &[],
            &kw,
        ),
        Some(false)
    );
    assert_eq!(
        ctx.evaluate_safe_logoff_teardown_late_call(
            &sess,
            other_char,
            "",
            Some(temp_skill_bid),
            "GetSkills",
            &[],
            &kw,
        ),
        None
    );
    assert_eq!(
        ctx.evaluate_safe_logoff_teardown_late_call(
            &sess,
            char_id,
            "",
            Some(other_ship_bid),
            "GetSkills",
            &[],
            &kw,
        ),
        None
    );

    // Personal vs corporate wallet balance predicate parity.
    assert_eq!(
        ctx.evaluate_safe_logoff_teardown_late_call(
            &sess,
            char_id,
            "account",
            None,
            "GetCashBalance",
            &[],
            &kw,
        ),
        Some(false)
    );
    assert_eq!(
        ctx.evaluate_safe_logoff_teardown_late_call(
            &sess,
            char_id,
            "account",
            None,
            "GetCashBalance",
            &[PyRep::Integer(1)],
            &kw,
        ),
        None
    );
    let mut corp_kw = HashMap::new();
    corp_kw.insert("accountKey".to_string(), PyRep::Integer(1000));
    assert_eq!(
        ctx.evaluate_safe_logoff_teardown_late_call(
            &sess,
            char_id,
            "account",
            None,
            "GetCashBalance",
            &[],
            &corp_kw,
        ),
        None
    );

    // Bookmark read-only teardown getters succeed; mutations fail closed.
    assert_eq!(
        ctx.evaluate_safe_logoff_teardown_late_call(
            &sess,
            char_id,
            "accessGroupBookmarkMgr",
            None,
            "GetMyActiveBookmarks",
            &[],
            &kw,
        ),
        Some(false)
    );
    assert_eq!(
        ctx.evaluate_safe_logoff_teardown_late_call(
            &sess,
            char_id,
            "bookmark",
            None,
            "GetBookmarks",
            &[],
            &kw,
        ),
        Some(false)
    );
    assert_eq!(
        ctx.evaluate_safe_logoff_teardown_late_call(
            &sess,
            char_id,
            "skillMgr2",
            None,
            "SaveSkillQueue",
            &[],
            &kw,
        ),
        None
    );

    // Teardown temp binding cleanup clears only skillMgr2 for `(sid, char_id)`.
    ctx.clear_safe_logoff_temp_bindings(sid, char_id);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(30_000_142);
    sess.station_id = Some(60_003_760);
    assert_eq!(
        ctx.verify_bound_object(&sess, temp_skill_bid).unwrap_err().code,
        "InvalidBoundObject"
    );
    assert!(ctx.verify_bound_object(&sess, other_ship_bid).is_ok());
}

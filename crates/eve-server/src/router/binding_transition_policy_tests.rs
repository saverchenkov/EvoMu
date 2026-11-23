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

#[test]
fn transition_grace_store_lock_order_matches_world_snapshot_under_concurrency() {
    let ctx = Arc::new(test_ctx());
    let user_id = 1_u64;
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let st_id = 60_003_760_i64;
    let structure_id = 1_000_000_050_001_i64;
    let jita_sys = 30_000_142_u32;
    let perimeter_sys = 30_000_144_u32;
    let ships = [980_210_001_i64, 980_210_002_i64, 980_210_003_i64];
    seed_ships_for_policy_tests(&ctx, char_id, other_char, st_id, &ships, 980_210_004, 980_210_005);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = jita_sys;
        }
    });

    let sid = 7_100_u64;
    ctx.db
        .acquire_online_lock(user_id, sid, Some(char_id), true, ctx.now_unix())
        .expect("online lock");
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(jita_sys);

    // 1. Direct lock-rank check: holding WorldRepository::inner (40), combat_ephemeral (60),
    //    or bound_objects (60) while reading `transition_grace_store` must not violate rank order.
    {
        let _guard = ReconnectHoldGuard::acquire(&ctx, char_id, sid);
        ctx.db.with_mut_snapshot(|_| {
            assert_eq!(ctx.reconnect_hold_session(char_id), Some(sid));
        });
        crate::services::combat_crimewatch::with_mut_combat_runtime_tx_locks(
            &ctx.db,
            &*ctx.combat_ephemeral,
            |_snap, _rt| {
                assert_eq!(ctx.reconnect_hold_session(char_id), Some(sid));
            },
        );
        let _bounds = ctx.bound_objects.read().unwrap();
        assert_eq!(ctx.reconnect_hold_session(char_id), Some(sid));
    }

    // 2. Two-thread AB-BA stress test:
    //    Thread A replicates `cluster_tick` / `disconnect_hold`: WorldRepository::inner (plus
    //    `combat_ephemeral`) -> `reconnect_hold_session`.
    //    Thread B replicates transition grace recording on non-station/Upwell/system bindings
    //    that hit `departed_location_kind_for_entry` -> `WorldRepository::inner`.
    let barrier = Arc::new(std::sync::Barrier::new(2));
    std::thread::scope(|s| {
        let ctx_a = Arc::clone(&ctx);
        let b_a = Arc::clone(&barrier);
        s.spawn(move || {
            b_a.wait();
            for iter in 0..120 {
                let _hold_guard = ReconnectHoldGuard::acquire(&ctx_a, char_id, sid + (iter % 2) as u64);
                ctx_a.db.with_mut_snapshot(|_| {
                    let _ = ctx_a.reconnect_hold_session(char_id);
                });
                crate::services::combat_crimewatch::with_mut_combat_runtime_tx_locks(
                    &ctx_a.db,
                    &*ctx_a.combat_ephemeral,
                    |_snap, _rt| {
                        let _ = ctx_a.reconnect_hold_session(char_id);
                    },
                );
            }
        });

        let ctx_b = Arc::clone(&ctx);
        let b_b = Arc::clone(&barrier);
        s.spawn(move || {
            b_b.wait();
            for iter in 0..120_u64 {
                let base_id = 50_000 + iter * 4;
                let entries = vec![
                    ctx_b.make_bound_object_entry(
                        sid,
                        base_id,
                        user_id,
                        char_id,
                        "invbroker",
                        ships[0],
                    ),
                    ctx_b.make_bound_object_entry(
                        sid,
                        base_id + 1,
                        user_id,
                        char_id,
                        "crimewatch",
                        i64::from(jita_sys),
                    ),
                    ctx_b.make_bound_object_entry(
                        sid,
                        base_id + 2,
                        user_id,
                        char_id,
                        "beyonce",
                        i64::from(jita_sys),
                    ),
                    ctx_b.make_bound_object_entry(
                        sid,
                        base_id + 3,
                        user_id,
                        char_id,
                        "invbroker",
                        structure_id,
                    ),
                ];
                ctx_b.record_transition_grace_bindings(&entries, Some(perimeter_sys), None);
                let _ = ctx_b.record_atomic_session_transition(AtomicSceneTransitionRecord {
                    session_id: sid,
                    user_id: Some(user_id),
                    char_id,
                    kind_label: "jump_system",
                    reason: "stargate_jump",
                    is_logout: false,
                    is_login: false,
                    prev_sys: Some(jita_sys),
                    prev_dock: None,
                    next_sys: Some(perimeter_sys),
                    next_dock: None,
                    can_record_departed: true,
                    removed_bindings: entries,
                });
            }
        });
    });

    assert!(ctx.session_scene_epoch(sid) >= 120);
    ctx.clear_session_scene_epoch(sid);
    ctx.purge_transition_grace_for_session(sid);
}

#[test]
fn cleanup_paths_and_offline_transitions_never_leak_epochs_or_cross_stamp_sessions() {
    let ctx = Arc::new(test_ctx());
    let user_id = 1_u64;
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let st_id = 60_003_760_i64;
    let jita_sys = 30_000_142_u32;
    let perimeter_sys = 30_000_144_u32;
    let ships = [980_220_001_i64, 980_220_002_i64, 980_220_003_i64];
    seed_ships_for_policy_tests(&ctx, char_id, other_char, st_id, &ships, 980_220_004, 980_220_005);

    // 1. SessionCleanupGuard::drop clears `scene_epochs` & `epoch_records` while safe logoff
    //    (`release_session_bindings`) keeps monotonicity beforehand.
    let sid_guard = 7_201_u64;
    ctx.db
        .acquire_online_lock(user_id, sid_guard, Some(char_id), true, ctx.now_unix())
        .expect("online lock for guard");
    let mut sess_guard = SessionState::new(sid_guard);
    sess_guard.user_id = Some(user_id);
    sess_guard.character_id = Some(char_id);
    sess_guard.solar_system_id = Some(jita_sys);
    sess_guard.station_id = Some(st_id);
    {
        let _cleanup = crate::router::binding::SessionCleanupGuard::new(
            Arc::clone(&ctx),
            sid_guard,
        );
        let undock = SessionTransitionSpec::for_kind(
            char_id,
            TransitionKind::Undock {
                solar_system_id: jita_sys,
            },
            "undock",
        )
        .with_emit_on_session_changed(false)
        .with_sync_local_chat(false);
        let ep = ctx
            .execute_session_transition_sync(Some(&mut sess_guard), undock)
            .expect("undock transition")
            .scene_epoch;
        assert_eq!(ep, 1);
        assert_eq!(ctx.session_scene_epoch(sid_guard), 1);
        assert!(ctx.current_transition_epoch_record(sid_guard).is_some());
    }
    assert_eq!(ctx.session_scene_epoch(sid_guard), 0);
    assert!(ctx.current_transition_epoch_record(sid_guard).is_none());

    // 2. Headless `disconnect_session` (`!sent_to_live`) clears `scene_epochs` & `epoch_records`.
    let sid_kick = 7_202_u64;
    ctx.db
        .acquire_online_lock(user_id, sid_kick, Some(char_id), true, ctx.now_unix())
        .expect("online lock for kick");
    let mut sess_kick = SessionState::new(sid_kick);
    sess_kick.user_id = Some(user_id);
    sess_kick.character_id = Some(char_id);
    sess_kick.solar_system_id = Some(jita_sys);
    sess_kick.station_id = Some(st_id);
    let jump_spec = SessionTransitionSpec::for_kind(
        char_id,
        TransitionKind::JumpSystem {
            solar_system_id: perimeter_sys,
        },
        "stargate_jump",
    )
    .with_emit_on_session_changed(false)
    .with_sync_local_chat(false);
    let ep_kick = ctx
        .execute_session_transition_sync(Some(&mut sess_kick), jump_spec)
        .expect("jump transition")
        .scene_epoch;
    assert_eq!(ep_kick, 1);
    assert!(!ctx.disconnect_session(sid_kick, "TestKick", "forced"));
    assert_eq!(ctx.session_scene_epoch(sid_kick), 0);
    assert!(ctx.current_transition_epoch_record(sid_kick).is_none());

    // 3. Offline transition (`session: None` and no online session -> `target_sid == 0`) must NEVER
    //    mutate or leak `scene_epochs[0]` / `epoch_records[0]`.
    let offline_spec = SessionTransitionSpec::for_kind(
        char_id,
        TransitionKind::DeathRespawn {
            solar_system_id: jita_sys,
            dock_id: Some((st_id, false)),
            ship_id: ships[2],
        },
        "death_respawn",
    )
    .with_emit_on_session_changed(false)
    .with_sync_local_chat(false);
    let offline_res = ctx
        .execute_session_transition_sync(None, offline_spec)
        .expect("offline death respawn");
    assert_eq!(offline_res.scene_epoch, 0);
    assert_eq!(ctx.session_scene_epoch(0), 0);
    assert!(ctx.current_transition_epoch_record(0).is_none());
    ctx.read_context_grace_store(|s| {
        if let Some(st) = s {
            assert!(!st.scene_epochs.contains_key(&0));
            assert!(!st.epoch_records.contains_key(&0));
        }
    });

    // 4. Cross-session epoch stamping & `can_record_departed` gating in `insert_departed_bindings_into_store`:
    //    Removing entries from `sid_other` during `sid_primary`'s transition must NOT stamp
    //    `sid_primary`'s departed epoch onto `sid_other`'s entries, nor write `departed_locations`
    //    when `can_record_departed == false` / offline.
    let sid_primary = 7_203_u64;
    let sid_other = 7_204_u64;
    ctx.db
        .acquire_online_lock(user_id, sid_primary, Some(char_id), true, ctx.now_unix())
        .expect("online lock for primary");
    let _ = ctx.record_atomic_session_transition(AtomicSceneTransitionRecord {
        session_id: sid_other,
        user_id: Some(99),
        char_id: other_char,
        kind_label: "undock",
        reason: "undock",
        is_logout: false,
        is_login: false,
        prev_sys: Some(jita_sys),
        prev_dock: Some(st_id),
        next_sys: Some(jita_sys),
        next_dock: None,
        can_record_departed: false,
        removed_bindings: vec![],
    });
    // Advance `sid_primary` cleanly to epoch 3 (`departed_epoch` for next transition = 3).
    for _ in 0..3 {
        let _ = ctx.record_atomic_session_transition(AtomicSceneTransitionRecord {
            session_id: sid_primary,
            user_id: Some(user_id),
            char_id,
            kind_label: "jump_system",
            reason: "stargate_jump",
            is_logout: false,
            is_login: false,
            prev_sys: Some(jita_sys),
            prev_dock: None,
            next_sys: Some(perimeter_sys),
            next_dock: None,
            can_record_departed: false,
            removed_bindings: vec![],
        });
    }
    let e_primary = ctx.make_bound_object_entry(sid_primary, 88_001, user_id, char_id, "invbroker", st_id);
    let e_other = ctx.make_bound_object_entry(sid_other, 88_002, 99, other_char, "crimewatch", st_id);
    let entered = ctx.record_atomic_session_transition(AtomicSceneTransitionRecord {
        session_id: sid_primary,
        user_id: Some(user_id),
        char_id,
        kind_label: "undock",
        reason: "undock",
        is_logout: false,
        is_login: false,
        prev_sys: Some(jita_sys),
        prev_dock: Some(st_id),
        next_sys: Some(jita_sys),
        next_dock: None,
        can_record_departed: false,
        removed_bindings: vec![e_primary, e_other],
    });
    assert_eq!(entered, 4);
    ctx.read_context_grace_store(|s| {
        let st = s.expect("grace store present");
        assert_eq!(st.entries.get(&88_001).map(|b| b.scene_epoch), Some(3));
        assert_eq!(st.entries.get(&88_002).map(|b| b.scene_epoch), Some(1));
        assert!(!st.departed_locations.contains_key(&(sid_primary, DepartedLocationKind::Dock, st_id)));
        assert!(!st.departed_locations.contains_key(&(sid_other, DepartedLocationKind::Dock, st_id)));
    });
    ctx.clear_session_scene_epoch(sid_primary);
    ctx.clear_session_scene_epoch(sid_other);
    ctx.purge_transition_grace_for_session(sid_primary);
    ctx.purge_transition_grace_for_session(sid_other);
}

#[test]
fn out_of_band_transition_followed_by_sync_session_preserves_departed_epoch_and_ttl() {
    let ctx = test_ctx();
    let user_id = 1_u64;
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let st_id = 60_003_760_i64;
    let jita_sys = 30_000_142_u32;
    let perimeter_sys = 30_000_144_u32;
    let ships = [980_230_001_i64, 980_230_002_i64, 980_230_003_i64];
    seed_ships_for_policy_tests(&ctx, char_id, other_char, st_id, &ships, 980_230_004, 980_230_005);

    let sid = 7_301_u64;
    ctx.db
        .acquire_online_lock(user_id, sid, Some(char_id), true, ctx.now_unix())
        .expect("online lock");
    let mut prev_stale = SessionState::new(sid);
    prev_stale.user_id = Some(user_id);
    prev_stale.character_id = Some(char_id);
    prev_stale.solar_system_id = Some(jita_sys);
    prev_stale.station_id = Some(st_id);
    prev_stale.ship_id = Some(ships[0]);

    // First transition establishes epoch 1 (`CharacterLogin` -> 1).
    let login_spec = SessionTransitionSpec::for_kind(
        char_id,
        TransitionKind::CharacterLogin {
            solar_system_id: jita_sys,
            dock_id: Some((st_id, false)),
            ship_id: Some(ships[0]),
        },
        "login",
    )
    .with_emit_on_session_changed(false)
    .with_sync_local_chat(false);
    let ep_login = ctx
        .execute_session_transition_sync(Some(&mut prev_stale), login_spec)
        .expect("login transition")
        .scene_epoch;
    assert_eq!(ep_login, 1);

    // Out-of-band `DeathRespawn` (`session: None`) moves online char `jita_sys / st_id -> perimeter_sys / space`,
    // advancing `scene_epoch: 1 -> 2` and stamping departed location at `departed_epoch = 1` and `T0`.
    let t0_ticks = ctx.now_machonet_wallclock_filetime_ticks();
    let respawn_spec = SessionTransitionSpec::for_kind(
        char_id,
        TransitionKind::DeathRespawn {
            solar_system_id: perimeter_sys,
            dock_id: None,
            ship_id: ships[2],
        },
        "death_respawn",
    )
    .with_emit_on_session_changed(false)
    .with_sync_local_chat(false);
    let res_oob = ctx
        .execute_session_transition_sync(None, respawn_spec)
        .expect("out-of-band death respawn");
    assert_eq!(res_oob.scene_epoch, 2);

    // Advance 12s, then run `sync_transition_grace_on_session_change(&prev_stale, &refreshed_next)`
    // just like live TCP `sync_session` does after noticing DB location changed out-of-band.
    ctx.advance_secs(12);
    let mut refreshed_next = prev_stale.clone();
    refreshed_next.solar_system_id = Some(perimeter_sys);
    refreshed_next.station_id = None;
    refreshed_next.ship_id = Some(ships[2]);
    ctx.sync_transition_grace_on_session_change(&prev_stale, &refreshed_next);

    // Departed dock & system must keep `scene_epoch == 1` (`< current_epoch == 2`) AND original `T0`.
    ctx.read_context_grace_store(|s| {
        let st = s.expect("grace store present");
        let dep_dock = st
            .departed_locations
            .get(&(sid, DepartedLocationKind::Dock, st_id))
            .expect("departed dock preserved");
        assert_eq!(dep_dock.scene_epoch, 1);
        assert_eq!(dep_dock.departed_ticks, t0_ticks);
        let dep_sys = st
            .departed_locations
            .get(&(sid, DepartedLocationKind::SolarSystem, i64::from(jita_sys)))
            .expect("departed system preserved");
        assert_eq!(dep_sys.scene_epoch, 1);
        assert_eq!(dep_sys.departed_ticks, t0_ticks);
    });

    // Late bind + epoch-pinned resolution must succeed with strict `departed_epoch (1) < current_epoch (2)`.
    let kw = HashMap::new();
    let grace_bid = ctx
        .allocate_late_call_grace_binding(
            &refreshed_next,
            "crimewatch",
            i64::from(jita_sys),
            "GetClientStates",
            &[],
            &kw,
        )
        .expect("late grace binding allocated at departed epoch 1");
    assert!(matches!(
        ctx.resolve_late_call_for_epoch(
            &refreshed_next,
            grace_bid,
            1,
            "GetClientStates",
            &[],
            &kw
        ),
        LateCallResolution::DispatchWithBinding(_)
    ));

    // Advance 19s more (`T0 + 31s` total, only 19s since `sync_session` at `T0 + 12s`): grace MUST expire!
    ctx.advance_secs(19);
    assert!(matches!(
        ctx.resolve_late_call_for_epoch(
            &refreshed_next,
            grace_bid,
            1,
            "GetClientStates",
            &[],
            &kw
        ),
        LateCallResolution::RejectStale
    ));
    assert!(ctx
        .allocate_late_call_grace_binding(
            &refreshed_next,
            "crimewatch",
            i64::from(jita_sys),
            "GetClientStates",
            &[],
            &kw,
        )
        .is_none());

    // Even a belated duplicate `sync_session` after expiry must NOT resurrect expired grace.
    ctx.advance_secs(5);
    ctx.sync_transition_grace_on_session_change(&prev_stale, &refreshed_next);
    assert!(ctx
        .allocate_late_call_grace_binding(
            &refreshed_next,
            "crimewatch",
            i64::from(jita_sys),
            "GetClientStates",
            &[],
            &kw,
        )
        .is_none());
}

#[test]
fn epoch_record_tracks_only_changed_departed_and_entered_locations() {
    let ctx = test_ctx();
    let user_id = 1_u64;
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let sid = 880_010_u64;
    let jita_sys = 30_000_142_u32;
    let perimeter_sys = 30_000_144_u32;
    let jita_station = 60_003_760_i64;
    let ship_a = 1_897_400_501_i64;

    seed_ships_for_policy_tests(
        &ctx,
        char_id,
        other_char,
        jita_station,
        &[ship_a],
        1_897_400_598,
        1_897_400_599,
    );
    ctx.db
        .acquire_online_lock(user_id, sid, Some(char_id), true, ctx.now_unix())
        .expect("online lock");

    let mut sess = SessionState::new(sid);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);

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
        .with_emit_on_session_changed(false)
        .with_sync_local_chat(false),
    )
    .expect("character login");
    let login_rec = ctx
        .current_transition_epoch_record(sid)
        .expect("login epoch record");
    assert_eq!(login_rec.departed_solar_system_id, None);
    assert_eq!(login_rec.departed_dock_id, None);
    assert_eq!(login_rec.entered_solar_system_id, Some(jita_sys));
    assert_eq!(login_rec.entered_dock_id, Some(jita_station));

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
    .expect("same-system undock");
    let undock_rec = ctx
        .current_transition_epoch_record(sid)
        .expect("undock epoch record");
    assert_eq!(undock_rec.departed_solar_system_id, None);
    assert_eq!(undock_rec.departed_dock_id, Some(jita_station));
    assert_eq!(undock_rec.entered_solar_system_id, Some(jita_sys));
    assert_eq!(undock_rec.entered_dock_id, None);

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
    .expect("same-system dock");
    let dock_rec = ctx
        .current_transition_epoch_record(sid)
        .expect("dock epoch record");
    assert_eq!(dock_rec.departed_solar_system_id, None);
    assert_eq!(dock_rec.departed_dock_id, None);
    assert_eq!(dock_rec.entered_solar_system_id, Some(jita_sys));
    assert_eq!(dock_rec.entered_dock_id, Some(jita_station));

    ctx.execute_session_transition_sync(
        Some(&mut sess),
        SessionTransitionSpec::for_kind(
            char_id,
            TransitionKind::Undock {
                solar_system_id: jita_sys,
            },
            "undock_for_jump",
        )
        .with_emit_on_session_changed(false)
        .with_sync_local_chat(false),
    )
    .expect("undock for jump");
    ctx.execute_session_transition_sync(
        Some(&mut sess),
        SessionTransitionSpec::for_kind(
            char_id,
            TransitionKind::JumpSystem {
                solar_system_id: perimeter_sys,
            },
            "jump_perimeter",
        )
        .with_emit_on_session_changed(false)
        .with_sync_local_chat(false),
    )
    .expect("jump perimeter");
    let jump_rec = ctx
        .current_transition_epoch_record(sid)
        .expect("jump epoch record");
    assert_eq!(jump_rec.departed_solar_system_id, Some(jita_sys));
    assert_eq!(jump_rec.departed_dock_id, None);
    assert_eq!(jump_rec.entered_solar_system_id, Some(perimeter_sys));
    assert_eq!(jump_rec.entered_dock_id, None);

    ctx.execute_session_transition_sync(
        Some(&mut sess),
        SessionTransitionSpec::for_kind(char_id, TransitionKind::Logout, "logout")
            .with_emit_on_session_changed(false),
    )
    .expect("logout");
    let logout_rec = ctx
        .current_transition_epoch_record(sid)
        .expect("logout epoch record");
    assert_eq!(logout_rec.departed_solar_system_id, None);
    assert_eq!(logout_rec.departed_dock_id, None);
    assert_eq!(logout_rec.entered_solar_system_id, None);
    assert_eq!(logout_rec.entered_dock_id, None);

    ctx.purge_transition_grace_for_user(user_id);
    assert!(ctx.current_transition_epoch_record(sid).is_none());
}

#[test]
fn undock_after_30s_followed_by_out_of_band_system_sync_grants_fresh_grace() {
    let ctx = test_ctx();
    let user_id = 1_u64;
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let sid = 880_011_u64;
    let jita_sys = 30_000_142_u32;
    let perimeter_sys = 30_000_144_u32;
    let jita_station = 60_003_760_i64;
    let ship_a = 1_897_400_601_i64;

    seed_ships_for_policy_tests(
        &ctx,
        char_id,
        other_char,
        jita_station,
        &[ship_a],
        1_897_400_698,
        1_897_400_699,
    );
    ctx.db
        .acquire_online_lock(user_id, sid, Some(char_id), true, ctx.now_unix())
        .expect("online lock");

    let mut sess = SessionState::new(sid);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);

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
        .with_emit_on_session_changed(false)
        .with_sync_local_chat(false),
    )
    .expect("character login");
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

    // Advance past the 30s Undock grace horizon before an out-of-band system sync occurs.
    ctx.advance_secs(45);

    let prev = sess.clone();
    sess.solar_system_id = Some(perimeter_sys);
    ctx.sync_transition_grace_on_session_change(&prev, &sess);

    let kw = HashMap::new();
    let sys_bid = ctx
        .allocate_late_call_grace_binding(
            &sess,
            "crimewatch",
            i64::from(jita_sys),
            "GetClientStates",
            &[],
            &kw,
        )
        .expect("fresh 30s Jita SolarSystem grace after Undock + 45s");

    ctx.advance_secs(20);
    assert!(matches!(
        ctx.resolve_late_call_with_policy(&sess, sys_bid, "GetClientStates", &[], &kw),
        LateCallResolution::DispatchWithBinding(entry) if entry.object_id == i64::from(jita_sys)
    ));

    ctx.advance_secs(11);
    assert!(matches!(
        ctx.resolve_late_call_with_policy(&sess, sys_bid, "GetClientStates", &[], &kw),
        LateCallResolution::RejectStale
    ));
    assert!(ctx
        .allocate_late_call_grace_binding(
            &sess,
            "crimewatch",
            i64::from(jita_sys),
            "GetClientStates",
            &[],
            &kw,
        )
        .is_none());
}

#[test]
fn return_visit_to_departed_system_and_dock_grants_fresh_grace_after_reentry() {
    let ctx = test_ctx();
    let user_id = 1_u64;
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let sid = 880_012_u64;
    let jita_sys = 30_000_142_u32;
    let perimeter_sys = 30_000_144_u32;
    let amarr_sys = 30_002_187_u32;
    let jita_station = 60_003_760_i64;
    let ship_a = 1_897_400_701_i64;

    seed_ships_for_policy_tests(
        &ctx,
        char_id,
        other_char,
        jita_station,
        &[ship_a],
        1_897_400_798,
        1_897_400_799,
    );
    ctx.db
        .acquire_online_lock(user_id, sid, Some(char_id), true, ctx.now_unix())
        .expect("online lock");

    let mut sess = SessionState::new(sid);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);

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
        .with_emit_on_session_changed(false)
        .with_sync_local_chat(false),
    )
    .expect("character login");
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
    ctx.execute_session_transition_sync(
        Some(&mut sess),
        SessionTransitionSpec::for_kind(
            char_id,
            TransitionKind::JumpSystem {
                solar_system_id: perimeter_sys,
            },
            "jump_out",
        )
        .with_emit_on_session_changed(false)
        .with_sync_local_chat(false),
    )
    .expect("jump Jita -> Perimeter");
    ctx.execute_session_transition_sync(
        Some(&mut sess),
        SessionTransitionSpec::for_kind(
            char_id,
            TransitionKind::JumpSystem {
                solar_system_id: jita_sys,
            },
            "jump_return",
        )
        .with_emit_on_session_changed(false)
        .with_sync_local_chat(false),
    )
    .expect("jump Perimeter -> Jita");

    // Advance past 30s after returning to Jita, then depart Jita via out-of-band sync.
    ctx.advance_secs(45);
    let prev = sess.clone();
    sess.solar_system_id = Some(amarr_sys);
    ctx.sync_transition_grace_on_session_change(&prev, &sess);

    let kw = HashMap::new();
    let sys_bid = ctx
        .allocate_late_call_grace_binding(
            &sess,
            "crimewatch",
            i64::from(jita_sys),
            "GetClientStates",
            &[],
            &kw,
        )
        .expect("fresh 30s Jita grace after returning to Jita and departing again");

    ctx.advance_secs(20);
    assert!(matches!(
        ctx.resolve_late_call_with_policy(&sess, sys_bid, "GetClientStates", &[], &kw),
        LateCallResolution::DispatchWithBinding(entry) if entry.object_id == i64::from(jita_sys)
    ));
    ctx.advance_secs(11);
    assert!(matches!(
        ctx.resolve_late_call_with_policy(&sess, sys_bid, "GetClientStates", &[], &kw),
        LateCallResolution::RejectStale
    ));
    assert!(ctx
        .allocate_late_call_grace_binding(
            &sess,
            "crimewatch",
            i64::from(jita_sys),
            "GetClientStates",
            &[],
            &kw,
        )
        .is_none());

    // Dock return-visit parity: Undock(Jita 4-4) -> Dock(Jita 4-4) -> >30s -> out-of-band undock.
    let dock_ctx = test_ctx();
    let dock_sid = 880_013_u64;
    seed_ships_for_policy_tests(
        &dock_ctx,
        char_id,
        other_char,
        jita_station,
        &[ship_a],
        1_897_400_798,
        1_897_400_799,
    );
    dock_ctx
        .db
        .acquire_online_lock(user_id, dock_sid, Some(char_id), true, dock_ctx.now_unix())
        .expect("online lock");

    let mut dock_sess = SessionState::new(dock_sid);
    dock_sess.user_id = Some(user_id);
    dock_sess.character_id = Some(char_id);

    dock_ctx
        .execute_session_transition_sync(
            Some(&mut dock_sess),
            SessionTransitionSpec::for_kind(
                char_id,
                TransitionKind::CharacterLogin {
                    solar_system_id: jita_sys,
                    dock_id: Some((jita_station, false)),
                    ship_id: Some(ship_a),
                },
                "login",
            )
            .with_emit_on_session_changed(false)
            .with_sync_local_chat(false),
        )
        .expect("character login");
    dock_ctx
        .execute_session_transition_sync(
            Some(&mut dock_sess),
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
    dock_ctx
        .execute_session_transition_sync(
            Some(&mut dock_sess),
            SessionTransitionSpec::for_kind(
                char_id,
                TransitionKind::Dock {
                    dock_id: jita_station,
                    is_structure: false,
                },
                "dock_return",
            )
            .with_emit_on_session_changed(false)
            .with_sync_local_chat(false),
        )
        .expect("dock back at Jita 4-4");

    dock_ctx.advance_secs(45);
    let prev_dock = dock_sess.clone();
    dock_sess.station_id = None;
    dock_ctx.sync_transition_grace_on_session_change(&prev_dock, &dock_sess);

    let dock_bid = dock_ctx
        .allocate_late_call_grace_binding(
            &dock_sess,
            "ship",
            jita_station,
            "GetDirtTimestamp",
            &[PyRep::Integer(ship_a)],
            &kw,
        )
        .expect("fresh 30s Jita 4-4 dock grace after returning and departing via sync");

    dock_ctx.advance_secs(20);
    assert!(matches!(
        dock_ctx.resolve_late_call_with_policy(
            &dock_sess,
            dock_bid,
            "GetDirtTimestamp",
            &[PyRep::Integer(ship_a)],
            &kw,
        ),
        LateCallResolution::DispatchWithBinding(entry) if entry.object_id == jita_station
    ));
    dock_ctx.advance_secs(11);
    assert!(matches!(
        dock_ctx.resolve_late_call_with_policy(
            &dock_sess,
            dock_bid,
            "GetDirtTimestamp",
            &[PyRep::Integer(ship_a)],
            &kw,
        ),
        LateCallResolution::RejectStale
    ));
    assert!(dock_ctx
        .allocate_late_call_grace_binding(
            &dock_sess,
            "ship",
            jita_station,
            "GetDirtTimestamp",
            &[PyRep::Integer(ship_a)],
            &kw,
        )
        .is_none());
}

#[test]
fn live_apply_db_false_dock_purges_departed_dock_grace_and_grants_fresh_window() {
    let ctx = test_ctx();
    let user_id = 1_u64;
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let sid = 880_014_u64;
    let jita_sys = 30_000_142_u32;
    let jita_station = 60_003_760_i64;
    let ship_a = 1_897_400_801_i64;

    seed_ships_for_policy_tests(
        &ctx,
        char_id,
        other_char,
        jita_station,
        &[ship_a],
        1_897_400_898,
        1_897_400_899,
    );
    ctx.db
        .acquire_online_lock(user_id, sid, Some(char_id), true, ctx.now_unix())
        .expect("online lock");

    let mut sess = SessionState::new(sid);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);

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
        .with_emit_on_session_changed(false)
        .with_sync_local_chat(false),
    )
    .expect("character login");
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
    assert!(
        ctx.allocate_late_call_grace_binding(
            &sess,
            "ship",
            jita_station,
            "GetDirtTimestamp",
            &[PyRep::Integer(ship_a)],
            &kw,
        )
        .is_some(),
        "departed dock grace active after undock"
    );

    // Mirror live `stationSvc.Dock` / `beyonce.CmdDock`: mutate DB first, then call `apply_db(false)`.
    ctx.advance_secs(10);
    ctx.db
        .dock_character_if_in_space(char_id, jita_sys, jita_station, false)
        .expect("dock_character_if_in_space");
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
        .with_apply_db(false)
        .with_cleanup_departing_space(false)
        .with_emit_on_session_changed(false)
        .with_sync_local_chat(false),
    )
    .expect("live apply_db(false) dock");

    // Departed-dock grace must be immediately purged while docked at Jita 4-4.
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
    ctx.read_context_grace_store(|s| {
        let st = s.expect("grace store present");
        assert!(
            !st.departed_locations
                .contains_key(&(sid, DepartedLocationKind::Dock, jita_station)),
            "apply_db(false) dock must purge departed dock location"
        );
    });
    let dock_rec = ctx
        .current_transition_epoch_record(sid)
        .expect("dock epoch record");
    assert_eq!(dock_rec.departed_dock_id, None);
    assert_eq!(dock_rec.entered_dock_id, Some(jita_station));

    // Departing again via out-of-band sync still <30s after the initial undock must grant a fresh 30s TTL.
    ctx.advance_secs(5);
    let prev = sess.clone();
    sess.station_id = None;
    ctx.sync_transition_grace_on_session_change(&prev, &sess);

    let fresh_bid = ctx
        .allocate_late_call_grace_binding(
            &sess,
            "ship",
            jita_station,
            "GetDirtTimestamp",
            &[PyRep::Integer(ship_a)],
            &kw,
        )
        .expect("fresh dock grace after live apply_db(false) dock + out-of-band departure");

    // At +20s after re-departure (+35s after initial Undock), the fresh 30s window must still hold.
    ctx.advance_secs(20);
    assert!(matches!(
        ctx.resolve_late_call_with_policy(
            &sess,
            fresh_bid,
            "GetDirtTimestamp",
            &[PyRep::Integer(ship_a)],
            &kw,
        ),
        LateCallResolution::DispatchWithBinding(entry) if entry.object_id == jita_station
    ));

    // At +31s after re-departure, the fresh window expires cleanly.
    ctx.advance_secs(11);
    assert!(matches!(
        ctx.resolve_late_call_with_policy(
            &sess,
            fresh_bid,
            "GetDirtTimestamp",
            &[PyRep::Integer(ship_a)],
            &kw,
        ),
        LateCallResolution::RejectStale
    ));
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
fn same_system_apply_db_false_dock_excludes_active_system_from_departed_locations() {
    let ctx = test_ctx();
    let user_id = 1_u64;
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let sid = 880_015_u64;
    let jita_sys = 30_000_142_u32;
    let perimeter_sys = 30_000_144_u32;
    let jita_station = 60_003_760_i64;
    let ship_a = 1_897_400_801_i64;

    seed_ships_for_policy_tests(
        &ctx,
        char_id,
        other_char,
        jita_station,
        &[ship_a],
        1_897_400_898,
        1_897_400_899,
    );
    ctx.db
        .acquire_online_lock(user_id, sid, Some(char_id), true, ctx.now_unix())
        .expect("online lock");

    let mut sess = SessionState::new(sid);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);

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
        .with_emit_on_session_changed(false)
        .with_sync_local_chat(false),
    )
    .expect("character login");
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

    // Bind a solar-system-scoped object while undocked (every live pilot holds `beyonce(sys_id)`).
    let beyonce_bid = ctx
        .bind_object(&sess, "beyonce", i64::from(jita_sys))
        .expect("bind beyonce in space");

    // Same-system live Dock (`apply_db(false)`): invalidates `beyonce(jita_sys)` while remaining in `jita_sys`.
    ctx.advance_secs(10);
    ctx.db
        .dock_character_if_in_space(char_id, jita_sys, jita_station, false)
        .expect("dock_character_if_in_space");
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
        .with_apply_db(false)
        .with_cleanup_departing_space(false)
        .with_emit_on_session_changed(false)
        .with_sync_local_chat(false),
    )
    .expect("live apply_db(false) dock");

    assert!(!ctx.bound_objects.read().unwrap().contains_key(&beyonce_bid));
    let kw = HashMap::new();
    ctx.read_context_grace_store(|s| {
        let st = s.expect("grace store present");
        assert!(
            !st.departed_locations.contains_key(&(
                sid,
                DepartedLocationKind::SolarSystem,
                i64::from(jita_sys),
            )),
            "same-system dock must never record entered solar system in departed_locations"
        );
    });
    assert!(
        ctx.allocate_late_call_grace_binding(
            &sess,
            "crimewatch",
            i64::from(jita_sys),
            "GetClientStates",
            &[],
            &kw,
        )
        .is_none(),
        "solar-system late-call grace must stay inactive while still in system"
    );

    // Out-of-band invalidation sync while docked in `jita_sys` must also filter active destination.
    let oob_beyonce = ctx
        .bind_verified_sub_object(&sess, "beyonce", i64::from(jita_sys))
        .expect("bind oob beyonce");
    ctx.invalidate_bound_objects_for_session_transition(
        char_id,
        jita_sys,
        Some(jita_station),
        "dock",
    );
    assert!(!ctx.bound_objects.read().unwrap().contains_key(&oob_beyonce));
    ctx.read_context_grace_store(|s| {
        let st = s.expect("grace store present");
        assert!(
            !st.departed_locations.contains_key(&(
                sid,
                DepartedLocationKind::SolarSystem,
                i64::from(jita_sys),
            )),
            "out-of-band dock invalidation must not record active solar system"
        );
    });

    // Departing `jita_sys` 15s after docking must grant a fresh 30s window (+35s valid, +46s expired).
    ctx.advance_secs(15);
    let prev = sess.clone();
    sess.solar_system_id = Some(perimeter_sys);
    sess.station_id = None;
    ctx.sync_transition_grace_on_session_change(&prev, &sess);

    let sys_bid = ctx
        .allocate_late_call_grace_binding(
            &sess,
            "crimewatch",
            i64::from(jita_sys),
            "GetClientStates",
            &[],
            &kw,
        )
        .expect("fresh solar-system grace after docking and out-of-band departure");

    ctx.advance_secs(20);
    assert!(matches!(
        ctx.resolve_late_call_with_policy(
            &sess,
            sys_bid,
            "GetClientStates",
            &[],
            &kw,
        ),
        LateCallResolution::DispatchWithBinding(entry) if entry.object_id == i64::from(jita_sys)
    ));

    ctx.advance_secs(11);
    assert!(matches!(
        ctx.resolve_late_call_with_policy(
            &sess,
            sys_bid,
            "GetClientStates",
            &[],
            &kw,
        ),
        LateCallResolution::RejectStale
    ));
    assert!(ctx
        .allocate_late_call_grace_binding(
            &sess,
            "crimewatch",
            i64::from(jita_sys),
            "GetClientStates",
            &[],
            &kw,
        )
        .is_none());
}

#[test]
fn transition_grace_store_is_isolated_across_independent_server_contexts() {
    let ctx_a = test_ctx();
    let ctx_b = test_ctx();
    assert_ne!(ctx_a.instance_id, ctx_b.instance_id);
    assert!(!Arc::ptr_eq(
        &ctx_a.transition_grace_store,
        &ctx_b.transition_grace_store
    ));

    let user_id = 1_u64;
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let sid = 880_099_u64;
    let jita_sys = 30_000_142_u32;
    let jita_station = 60_003_760_i64;
    let ship_a = 1_897_400_901_i64;

    for ctx in [&ctx_a, &ctx_b] {
        seed_ships_for_policy_tests(
            ctx,
            char_id,
            other_char,
            jita_station,
            &[ship_a],
            1_897_400_998,
            1_897_400_999,
        );
        ctx.db
            .acquire_online_lock(user_id, sid, Some(char_id), true, ctx.now_unix())
            .expect("online lock");
    }

    let mut sess_a = SessionState::new(sid);
    sess_a.user_id = Some(user_id);
    sess_a.character_id = Some(char_id);

    let mut sess_b = SessionState::new(sid);
    sess_b.user_id = Some(user_id);
    sess_b.character_id = Some(char_id);
    sess_b.solar_system_id = Some(jita_sys);
    sess_b.ship_id = Some(ship_a);

    ctx_a
        .execute_session_transition_sync(
            Some(&mut sess_a),
            SessionTransitionSpec::for_kind(
                char_id,
                TransitionKind::CharacterLogin {
                    solar_system_id: jita_sys,
                    dock_id: Some((jita_station, false)),
                    ship_id: Some(ship_a),
                },
                "login",
            )
            .with_emit_on_session_changed(false)
            .with_sync_local_chat(false),
        )
        .expect("ctx_a login");
    let station_ship_bid = ctx_a
        .bind_object(&sess_a, "ship", jita_station)
        .expect("ctx_a bind ship at station");
    ctx_a
        .execute_session_transition_sync(
            Some(&mut sess_a),
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
        .expect("ctx_a undock");

    let kw = HashMap::new();
    let grace_bid = ctx_a
        .allocate_late_call_grace_binding(
            &sess_a,
            "ship",
            jita_station,
            "GetDirtTimestamp",
            &[PyRep::Integer(ship_a)],
            &kw,
        )
        .expect("ctx_a departed dock grace binding");
    ctx_a.record_pending_displaced_logoff(sid, user_id, char_id);
    ctx_a.record_prelogoff_hold_session(char_id, Some((sid, user_id)));
    let hold_a = ReconnectHoldGuard::acquire(&ctx_a, char_id, sid);

    // Independent `ctx_b` using identical `sid`, `user_id`, `char_id`, and `ship_id`
    // must observe none of `ctx_a`'s grace store state.
    assert_eq!(ctx_b.session_scene_epoch(sid), 0);
    assert!(ctx_b.current_transition_epoch_record(sid).is_none());
    assert!(ctx_b.transition_epoch_records(sid).is_empty());
    assert!(matches!(
        ctx_b.resolve_late_call_with_policy(
            &sess_b,
            station_ship_bid,
            "GetDirtTimestamp",
            &[PyRep::Integer(ship_a)],
            &kw,
        ),
        LateCallResolution::RejectStale
    ));
    assert!(matches!(
        ctx_b.resolve_late_call_with_policy(
            &sess_b,
            grace_bid,
            "GetDirtTimestamp",
            &[PyRep::Integer(ship_a)],
            &kw,
        ),
        LateCallResolution::RejectStale
    ));
    assert!(ctx_b
        .allocate_late_call_grace_binding(
            &sess_b,
            "ship",
            jita_station,
            "GetDirtTimestamp",
            &[PyRep::Integer(ship_a)],
            &kw,
        )
        .is_none());
    assert_eq!(ctx_b.take_pending_displaced_logoff(sid), None);
    assert_eq!(ctx_b.prelogoff_hold_session(char_id), None);
    assert!(ctx_b.snapshot_prelogoff_hold_sessions().is_empty());
    assert_eq!(ctx_b.reconnect_hold_session(char_id), None);

    // Purges and hold mutations on `ctx_b` must never touch `ctx_a`'s isolated store.
    let _hold_b = ReconnectHoldGuard::acquire(&ctx_b, char_id, sid + 1);
    assert_eq!(ctx_b.reconnect_hold_session(char_id), Some(sid + 1));
    assert_eq!(ctx_a.reconnect_hold_session(char_id), Some(sid));
    ctx_b.clear_pending_displaced_logoffs_for_char(user_id, char_id);
    ctx_b.record_prelogoff_hold_session(char_id, None);
    ctx_b.clear_prelogoff_hold_session_if_matches(char_id, (sid, user_id));
    ctx_b.clear_session_scene_epoch(sid);
    ctx_b.purge_transition_grace_for_session(sid);
    ctx_b.purge_transition_grace_for_user(user_id);

    assert_eq!(ctx_a.session_scene_epoch(sid), 2);
    assert!(ctx_a.current_transition_epoch_record(sid).is_some());
    assert!(matches!(
        ctx_a.resolve_late_call_with_policy(
            &sess_a,
            grace_bid,
            "GetDirtTimestamp",
            &[PyRep::Integer(ship_a)],
            &kw,
        ),
        LateCallResolution::DispatchWithBinding(entry) if entry.object_id == jita_station
    ));
    assert_eq!(ctx_a.prelogoff_hold_session(char_id), Some((sid, user_id)));
    assert_eq!(
        ctx_a.take_pending_displaced_logoff(sid),
        Some((user_id, char_id))
    );
    assert_eq!(ctx_a.reconnect_hold_session(char_id), Some(sid));

    // Weak handle upgrade preserves the exact per-context store, and dropping `ctx_a`
    // releases its `ContextGraceStore` with zero global retention.
    let ctx_a_weak = ctx_a.downgrade();
    let store_weak = Arc::downgrade(&ctx_a.transition_grace_store);
    let upgraded = ctx_a_weak.upgrade().expect("upgrade ctx_a");
    assert!(Arc::ptr_eq(
        &ctx_a.transition_grace_store,
        &upgraded.transition_grace_store
    ));
    assert_eq!(upgraded.session_scene_epoch(sid), 2);
    drop(upgraded);
    drop(hold_a);
    assert_eq!(ctx_a.reconnect_hold_session(char_id), None);
    drop(ctx_a);
    assert!(ctx_a_weak.upgrade().is_none());
    assert_eq!(store_weak.strong_count(), 0);
}

#[test]
fn emit_typed_to_solar_system_in_space_stamps_epoch_and_drops_same_system_stale_epoch() {
    use crate::outbound_sequencer::{
        sequence_tick, DestinyBatch, GateNotificationOutcome, OutboundSceneScope, SessionWireState,
        TypedNotification,
    };
    use crate::router::notify_registry::{emit_typed_to_solar_system_in_space_core, OnTarget};

    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let sid = 91_101_u64;

    let uid = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).expect("demo char");
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.user_id
    });
    ctx.db
        .acquire_online_lock(uid, sid, Some(char_id), true, 1_800_000_000)
        .unwrap();

    let ep1 = ctx.record_atomic_session_transition(AtomicSceneTransitionRecord {
        session_id: sid,
        user_id: Some(uid),
        char_id,
        kind_label: "undock",
        reason: "undock",
        is_logout: false,
        is_login: false,
        prev_sys: Some(sys_id),
        prev_dock: Some(60_003_760),
        next_sys: Some(sys_id),
        next_dock: None,
        can_record_departed: false,
        removed_bindings: Vec::new(),
    });
    assert_eq!(ep1, 1);
    assert_eq!(ctx.db.session_scene_epoch(sid), 1);
    assert_eq!(
        ctx.db.active_scene_epoch_for_char_and_user(char_id, uid),
        Some(1)
    );

    emit_typed_to_solar_system_in_space_core(
        &ctx.db,
        &ctx.pending_notifications,
        &ctx.notify_tx,
        sys_id,
        OnTarget {
            scene: crate::router::notify_registry::BallparkScene::solar_system(sys_id),
            arg0: PyRep::Integer(700_001),
            arg1: PyRep::String("lost".into()),
            reason: None,
        },
    );
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(notifs.len(), 1);
    assert_eq!(
        notifs[0].scene_scope,
        Some(OutboundSceneScope::space(Some(sys_id), Some(1)))
    );

    // Same-system transition advances scene epoch 1 -> 2; pre-transition epoch-1 solar-system
    // notification must drop as stale_scene_epoch on the wire.
    let ep2 = ctx.record_atomic_session_transition(AtomicSceneTransitionRecord {
        session_id: sid,
        user_id: Some(uid),
        char_id,
        kind_label: "jump_system",
        reason: "same_sys_reentry",
        is_logout: false,
        is_login: false,
        prev_sys: Some(sys_id),
        prev_dock: None,
        next_sys: Some(sys_id),
        next_dock: None,
        can_record_departed: true,
        removed_bindings: Vec::new(),
    });
    assert_eq!(ep2, 2);
    assert_eq!(ctx.db.session_scene_epoch(sid), 2);

    let mut wire = SessionWireState::new(sid, char_id as u64).with_scene_epoch(2);
    let ep = wire.enter_space(Some(sys_id));
    assert!(sequence_tick(
        &mut wire,
        DestinyBatch::new(
            ep,
            10,
            true,
            crate::outbound_sequencer::tests::dummy_updates("SetState", 10),
        )
        .with_scene_scope(Some(OutboundSceneScope::space(Some(sys_id), Some(2)))),
    )
    .is_some());
    let notif = &notifs[0];
    match wire.gate_typed_notification(
        TypedNotification::new(notif.event.clone(), notif.args.clone())
            .with_scene_scope(notif.scene_scope),
    ) {
        GateNotificationOutcome::Dropped(ev) => {
            assert!(
                ev.reason.starts_with("stale_scene_epoch:1<2"),
                "expected stale_scene_epoch:1<2, got {}",
                ev.reason
            );
        }
        other => panic!("expected stale solar-system notif to drop at epoch 2, got {other:?}"),
    }

    ctx.clear_session_scene_epoch(sid);
    assert_eq!(ctx.db.session_scene_epoch(sid), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn module_deactivation_on_special_fx_isolates_origin_scene_after_jump_and_skips_zero_sys() {
    use crate::outbound_sequencer::OutboundSceneScope;
    use crate::router::binding_effects::special_fx_guid_for_module_sde;
    use crate::system_actor::ActiveModuleCycle;

    let ctx = test_ctx();
    ctx.wire_module_deactivation_hook();

    let sys_a = 30_000_142_u32;
    let sys_b = 30_000_144_u32;
    let pilot_user = 1_u64;
    let obs_user = 2_u64;
    let pilot_cid = 90_000_001_i64;
    let obs_cid = 90_000_002_i64;
    let pilot_sid = 881_001_u64;
    let obs_sid = 881_002_u64;
    let pilot_ship = 1_897_410_001_i64;
    let obs_ship = 1_897_410_002_i64;
    let mod_id = 1_897_410_010_i64;

    let mod_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| {
            t.group_id == 40
                && special_fx_guid_for_module_sde(&ctx.sde, t.type_id, 0, Some("shieldBoosting"))
                    .is_some_and(|(g, _)| g != "effects.Cloak")
        })
        .map(|t| t.type_id)
        .expect("shield booster type in SDE");

    ctx.db.with_mut_snapshot(|snap| {
        let base_char = snap.characters.get(&pilot_cid).expect("pilot char").clone();
        snap.characters.insert(
            pilot_cid,
            eve_db::CharacterRecord {
                solar_system_id: sys_a,
                station_id: None,
                structure_id: None,
                active_ship_id: pilot_ship,
                ..base_char.clone()
            },
        );
        snap.characters.insert(
            obs_cid,
            eve_db::CharacterRecord {
                character_id: obs_cid,
                user_id: obs_user,
                name: "Observer Pilot".to_string(),
                solar_system_id: sys_a,
                station_id: None,
                structure_id: None,
                active_ship_id: obs_ship,
                ..base_char
            },
        );
        for (sid_item, owner, sys_id) in [(pilot_ship, pilot_cid, sys_a), (obs_ship, obs_cid, sys_a)]
        {
            snap.items.insert(
                sid_item,
                InventoryItemRecord {
                    item_id: sid_item,
                    type_id: 587,
                    owner_id: owner,
                    location_id: i64::from(sys_id),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });

    ctx.db
        .acquire_online_lock(pilot_user, pilot_sid, Some(pilot_cid), true, ctx.now_unix())
        .expect("pilot online");
    ctx.db
        .acquire_online_lock(obs_user, obs_sid, Some(obs_cid), true, ctx.now_unix())
        .expect("observer online");

    let pilot_ep1 = ctx.record_atomic_session_transition(AtomicSceneTransitionRecord {
        session_id: pilot_sid,
        user_id: Some(pilot_user),
        char_id: pilot_cid,
        kind_label: "undock",
        reason: "undock",
        is_logout: false,
        is_login: false,
        prev_sys: Some(sys_a),
        prev_dock: Some(60_003_760),
        next_sys: Some(sys_a),
        next_dock: None,
        can_record_departed: false,
        removed_bindings: vec![],
    });
    assert_eq!(pilot_ep1, 1);

    let obs_ep1 = ctx.record_atomic_session_transition(AtomicSceneTransitionRecord {
        session_id: obs_sid,
        user_id: Some(obs_user),
        char_id: obs_cid,
        kind_label: "undock",
        reason: "undock",
        is_logout: false,
        is_login: false,
        prev_sys: Some(sys_a),
        prev_dock: Some(60_003_760),
        next_sys: Some(sys_a),
        next_dock: None,
        can_record_departed: false,
        removed_bindings: vec![],
    });
    assert_eq!(obs_ep1, 1);

    let sys_a_handle = ctx.systems.get_or_spawn(sys_a);
    let mk_ball = |id| eve_destiny::Ball {
        id,
        type_id: 587,
        position: glam::DVec3::ZERO,
        velocity: glam::DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    };
    sys_a_handle.try_spawn_ball_with_combat(mk_ball(pilot_ship), eve_destiny::CombatState::default());

    let mk_cycle = |m_id| ActiveModuleCycle {
        ship_id: pilot_ship,
        module_id: m_id,
        module_type_id: mod_type_id,
        owner_char_id: Some(pilot_cid),
        effect_name: "shieldBoosting".to_string(),
        duration_s: 1.0,
        repeat: 0,
        ..Default::default()
    };

    // Case (a): Module starts cycling in `sys_a` at pilot epoch 1, then pilot jumps to `sys_b`
    // (epoch 2). Deactivating in `sys_a` must NOT deliver System-B-stamped `OnSpecialFX` to pilot,
    // while `obs_cid` still in `sys_a` receives `sys_a`-scoped `OnSpecialFX` with `obs_ep1`.
    sys_a_handle
        .add_module_cycle(mk_cycle(mod_id))
        .await
        .expect("add cycle 1");
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&pilot_cid).unwrap().solar_system_id = sys_b;
        snap.items.get_mut(&pilot_ship).unwrap().location_id = i64::from(sys_b);
    });
    let pilot_ep2 = ctx.record_atomic_session_transition(AtomicSceneTransitionRecord {
        session_id: pilot_sid,
        user_id: Some(pilot_user),
        char_id: pilot_cid,
        kind_label: "jump_system",
        reason: "stargate_jump",
        is_logout: false,
        is_login: false,
        prev_sys: Some(sys_a),
        prev_dock: None,
        next_sys: Some(sys_b),
        next_dock: None,
        can_record_departed: false,
        removed_bindings: vec![],
    });
    assert_eq!(pilot_ep2, 2);
    let _ = ctx.drain_notifications_for_char(pilot_cid);
    let _ = ctx.drain_notifications_for_char(obs_cid);

    let _ = sys_a_handle.try_step_full_tick(1.5);
    let pilot_sfx_after_jump: Vec<_> = ctx
        .drain_notifications_for_char(pilot_cid)
        .into_iter()
        .filter(|n| n.event == "OnSpecialFX")
        .collect();
    assert!(
        pilot_sfx_after_jump.is_empty(),
        "jumped pilot must not receive cross-system OnSpecialFX, got {pilot_sfx_after_jump:?}"
    );
    let obs_sfx_after_jump: Vec<_> = ctx
        .drain_notifications_for_char(obs_cid)
        .into_iter()
        .filter(|n| n.event == "OnSpecialFX")
        .collect();
    assert_eq!(obs_sfx_after_jump.len(), 1);
    assert_eq!(
        obs_sfx_after_jump[0].scene_scope,
        Some(OutboundSceneScope::space(Some(sys_a), Some(obs_ep1)))
    );

    // Case (b): Pilot returns to `sys_a` (epoch 3), starts module cycle 2, then advances to
    // epoch 4 inside the SAME `sys_a`. Deactivating cycle 2 must drop pilot's stale epoch-3
    // `OnSpecialFX` while still notifying `obs_cid` in `sys_a`.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&pilot_cid).unwrap().solar_system_id = sys_a;
        snap.items.get_mut(&pilot_ship).unwrap().location_id = i64::from(sys_a);
    });
    let pilot_ep3 = ctx.record_atomic_session_transition(AtomicSceneTransitionRecord {
        session_id: pilot_sid,
        user_id: Some(pilot_user),
        char_id: pilot_cid,
        kind_label: "jump_system",
        reason: "stargate_jump",
        is_logout: false,
        is_login: false,
        prev_sys: Some(sys_b),
        prev_dock: None,
        next_sys: Some(sys_a),
        next_dock: None,
        can_record_departed: false,
        removed_bindings: vec![],
    });
    assert_eq!(pilot_ep3, 3);
    sys_a_handle
        .add_module_cycle(mk_cycle(mod_id + 1))
        .await
        .expect("add cycle 2");
    let pilot_ep4 = ctx.record_atomic_session_transition(AtomicSceneTransitionRecord {
        session_id: pilot_sid,
        user_id: Some(pilot_user),
        char_id: pilot_cid,
        kind_label: "undock",
        reason: "same_system_reentry",
        is_logout: false,
        is_login: false,
        prev_sys: Some(sys_a),
        prev_dock: Some(60_003_760),
        next_sys: Some(sys_a),
        next_dock: None,
        can_record_departed: false,
        removed_bindings: vec![],
    });
    assert_eq!(pilot_ep4, 4);
    let _ = ctx.drain_notifications_for_char(pilot_cid);
    let _ = ctx.drain_notifications_for_char(obs_cid);

    let _ = sys_a_handle.try_step_full_tick(1.5);
    let pilot_sfx_stale_epoch: Vec<_> = ctx
        .drain_notifications_for_char(pilot_cid)
        .into_iter()
        .filter(|n| n.event == "OnSpecialFX")
        .collect();
    assert!(
        pilot_sfx_stale_epoch.is_empty(),
        "stale-epoch pilot in same system must not receive OnSpecialFX, got {pilot_sfx_stale_epoch:?}"
    );
    let obs_sfx_same_sys: Vec<_> = ctx
        .drain_notifications_for_char(obs_cid)
        .into_iter()
        .filter(|n| n.event == "OnSpecialFX")
        .collect();
    assert_eq!(obs_sfx_same_sys.len(), 1);
    assert_eq!(
        obs_sfx_same_sys[0].scene_scope,
        Some(OutboundSceneScope::space(Some(sys_a), Some(obs_ep1)))
    );

    // Case (c): Deactivating a cycle whose `origin_scene_epoch` is `None` while the pilot is
    // in space in `sys_a` must omit `scene_epoch` rather than stamping the current epoch.
    let hook = ctx
        .systems
        .module_deactivation_hook
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .expect("deactivation hook");
    let _ = ctx.drain_notifications_for_char(pilot_cid);
    let _ = ctx.drain_notifications_for_char(obs_cid);
    hook(sys_a, pilot_cid, &mk_cycle(mod_id + 2));
    let pilot_sfx_unepoched: Vec<_> = ctx
        .drain_notifications_for_char(pilot_cid)
        .into_iter()
        .filter(|n| n.event == "OnSpecialFX")
        .collect();
    assert_eq!(pilot_sfx_unepoched.len(), 1);
    assert_eq!(
        pilot_sfx_unepoched[0].scene_scope,
        Some(OutboundSceneScope::space(Some(sys_a), None))
    );

    // Case (d): Deactivation hook passed `sys_id == 0` where DB also resolves `0` must skip
    // cleanly without triggering `BallparkScene` zero-system `debug_assert!` panics.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&pilot_cid).unwrap().solar_system_id = 0;
        snap.items.remove(&pilot_ship);
    });
    let _ = ctx.drain_notifications_for_char(pilot_cid);
    let mut zero_sys_cycle = mk_cycle(mod_id + 3);
    zero_sys_cycle.ship_id = pilot_ship + 99;
    hook(0, pilot_cid, &zero_sys_cycle);
    let pilot_sfx_zero_sys: Vec<_> = ctx
        .drain_notifications_for_char(pilot_cid)
        .into_iter()
        .filter(|n| n.event == "OnSpecialFX")
        .collect();
    assert!(pilot_sfx_zero_sys.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zero_system_actor_steps_and_flushes_without_panic_and_drops_ballpark_notifications() {
    use crate::system_actor::{
        flush_pending_cycle_damage_notifications_with_ballpark, flush_target_lost_notifications,
        ActiveModuleCycle, PendingCycleAction,
    };

    let ctx = test_ctx();
    let pilot_cid = 90_000_001_i64;
    let _ = ctx.drain_notifications_for_char(pilot_cid);
    let sys_zero = ctx.systems.get_or_spawn(0);

    let ship_atk = 1_897_430_001_i64;
    let ship_tgt_a = 1_897_430_002_i64;
    let ship_tgt_b = 1_897_430_003_i64;

    let mk_ball_at = |id, pos| eve_destiny::Ball {
        id,
        type_id: 587,
        position: pos,
        velocity: glam::DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    };
    let mk_combat = || eve_destiny::CombatState {
        capacitor: 500.0,
        max_capacitor: 1_000.0,
        shield_hp: 500.0,
        max_shield_hp: 500.0,
        armor_hp: 500.0,
        max_armor_hp: 500.0,
        structure_hp: 500.0,
        max_structure_hp: 500.0,
        ..Default::default()
    };

    sys_zero
        .spawn_ball_with_meta_tagged(
            mk_ball_at(ship_atk, glam::DVec3::ZERO),
            mk_combat(),
            eve_destiny::SlimItemMeta {
                char_id: Some(pilot_cid),
                owner_id: pilot_cid,
                ..Default::default()
            },
            1,
        )
        .await
        .expect("spawn attacker on zero system");
    for (idx, tgt) in [ship_tgt_a, ship_tgt_b].into_iter().enumerate() {
        sys_zero
            .spawn_ball_with_meta_tagged(
                mk_ball_at(tgt, glam::DVec3::new(10_000.0, 0.0, 0.0)),
                mk_combat(),
                eve_destiny::SlimItemMeta::default(),
                (idx as u64) + 2,
            )
            .await
            .expect("spawn target on zero system");
    }

    let mk_cycle = |module_id| ActiveModuleCycle {
        ship_id: ship_atk,
        module_id,
        module_type_id: 2048,
        owner_char_id: Some(pilot_cid),
        effect_name: "shieldBoosting".to_string(),
        duration_s: 1.0,
        repeat: 0,
        cap_need_per_cycle: 10.0,
        ..Default::default()
    };

    sys_zero
        .add_target_lock(
            ship_atk,
            eve_destiny::TargetLock {
                target_id: ship_tgt_a,
                lock_time_ms: 100.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(1_000.0),
            },
        )
        .await
        .expect("add target lock A");
    sys_zero
        .add_module_cycle(mk_cycle(1_897_431_001))
        .await
        .expect("add cycle 1");
    sys_zero
        .step_manual(1.5)
        .await
        .expect("step_manual must not panic on solar_system_id == 0");

    sys_zero
        .add_target_lock(
            ship_atk,
            eve_destiny::TargetLock {
                target_id: ship_tgt_b,
                lock_time_ms: 100.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(1_000.0),
            },
        )
        .await
        .expect("add target lock B");
    sys_zero
        .add_module_cycle(mk_cycle(1_897_431_002))
        .await
        .expect("add cycle 2");
    assert!(sys_zero.try_step_full_tick(1.5) >= 1);

    let drained = ctx.drain_notifications_for_char(pilot_cid);
    assert!(
        !drained
            .iter()
            .any(|n| crate::router::notify_registry::is_ballpark_scoped_notification(&n.event)),
        "solar_system_id == 0 actor must never emit ballpark-scoped notifications: {drained:?}"
    );

    // Direct zero-system Ballpark flush skips OnTarget/OnDamageStateChange/OnDamageMessage* without panic.
    let mut bp_zero = eve_destiny::Ballpark::new(0);
    bp_zero.add_ball(mk_ball_at(ship_atk, glam::DVec3::ZERO));
    bp_zero.ball_slim_meta.insert(
        ship_atk,
        eve_destiny::SlimItemMeta {
            char_id: Some(pilot_cid),
            owner_id: pilot_cid,
            ..Default::default()
        },
    );
    bp_zero.add_ball(mk_ball_at(ship_tgt_a, glam::DVec3::new(1_000.0, 0.0, 0.0)));
    bp_zero.combat_states.insert(ship_tgt_a, mk_combat());
    let q_p: std::sync::RwLock<HashMap<i64, Vec<crate::router::PendingMachoNotification>>> =
        std::sync::RwLock::new(HashMap::new());
    flush_target_lost_notifications(
        &bp_zero,
        &[eve_destiny::DestinyEvent::OnTarget {
            mode: "lost".into(),
            source_id: ship_atk,
            target_id: ship_tgt_a,
        }],
        Some(&ctx.db),
        Some(&q_p),
        None,
    );
    let mut actions = vec![PendingCycleAction::CycleWeaponDamage {
        attacker_char_id: Some(pilot_cid),
        attacker_ship_id: ship_atk,
        attacker_ship_type_id: 587,
        target_id: ship_tgt_a,
        target_char_id: None,
        module_type_id: 3634,
        applied_dmg: 25.0,
        notified: false,
    }];
    flush_pending_cycle_damage_notifications_with_ballpark(
        &bp_zero,
        None,
        Some(&ctx.db),
        Some(&q_p),
        None,
        &mut actions,
    );
    assert!(q_p.read().unwrap().get(&pilot_cid).is_none_or(|v| v.is_empty()));
}

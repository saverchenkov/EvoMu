//! Unit and mutation-killing tests for `router::binding`.

use crate::router::{ServerContext, SessionCleanupGuard};
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
fn transition_grace_ship_dirt_binding_lifecycle_and_security_boundaries() {
    let ctx = test_ctx();
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(
        1_800_000_000,
    )));
    let user_id = 1_u64;
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let sid = 777_150_u64;
    let sys_id = 30_000_142_u32;
    let st_id = 60_003_760_i64;
    let other_st_id = ctx
        .sde
        .stations
        .keys()
        .copied()
        .find(|&id| id != st_id)
        .expect("alternate station");
    let ship_a = 1_897_301_923_i64;
    let foreign_ship = 1_897_301_999_i64;
    let non_ship_item = 1_897_301_998_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_a;
        }
        snap.items.insert(
            ship_a,
            InventoryItemRecord {
                item_id: ship_a,
                type_id: 587,
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
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
                type_id: 34, // Tritanium mineral (non-ship category)
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
    });
    ctx.db
        .acquire_online_lock(user_id, sid, Some(char_id), false, ctx.now_unix())
        .expect("acquire online lock");

    let mut sess = SessionState::new(sid);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(1_000_009);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_a);

    // Before recording dock departure, transition grace refuses station binding.
    assert!(
        ctx.allocate_transition_grace_ship_dirt_binding(
            &sess,
            st_id,
            &[PyRep::Integer(ship_a)],
        )
        .is_none()
    );

    // Simulate departing `st_id` into space (`station_id: Some(st_id) -> None`).
    ctx.update_transition_departed_dock(&sess, Some(st_id), None);

    // 1. Owned ship dirt binding allocates grace-only bound ID (NOT inserted into active bound_objects).
    let grace_bid = ctx
        .allocate_transition_grace_ship_dirt_binding(&sess, st_id, &[PyRep::Integer(ship_a)])
        .expect("grace bid within 30s");
    assert!(
        !ctx.bound_objects.read().unwrap().contains_key(&grace_bid),
        "grace bid must stay out of active bound_objects"
    );
    assert_eq!(
        ctx.verify_bound_object(&sess, grace_bid).unwrap_err().code,
        "InvalidBoundObject"
    );

    let resolved = ctx
        .grace_bound_object_for_inflight_transition(
            &sess,
            grace_bid,
            "GetDirtTimestamp",
            &[PyRep::Integer(ship_a)],
        )
        .expect("GetDirtTimestamp grace authorized");
    assert_eq!(resolved.service, "ship");
    assert_eq!(resolved.object_id, st_id);

    // 2. Non-GetDirtTimestamp methods and foreign/non-ship/unrelated-station targets are rejected.
    for blocked in [
        "Board",
        "AssembleShip",
        "ResetDirtTimestamp",
        "Undock",
        "LeaveShip",
        "TrashItems",
        "GetSelfInvItem",
    ] {
        assert!(
            ctx.grace_bound_object_for_inflight_transition(
                &sess,
                grace_bid,
                blocked,
                &[PyRep::Integer(ship_a)],
            )
            .is_none(),
            "{blocked} must be rejected on ship transition grace"
        );
    }
    assert!(
        ctx.allocate_transition_grace_ship_dirt_binding(
            &sess,
            st_id,
            &[PyRep::Integer(foreign_ship)],
        )
        .is_none()
    );
    assert!(
        ctx.allocate_transition_grace_ship_dirt_binding(
            &sess,
            st_id,
            &[PyRep::Integer(non_ship_item)],
        )
        .is_none()
    );
    assert!(
        ctx.allocate_transition_grace_ship_dirt_binding(
            &sess,
            other_st_id,
            &[PyRep::Integer(ship_a)],
        )
        .is_none()
    );
    assert!(
        ctx.grace_bound_object_for_inflight_transition(
            &sess,
            grace_bid,
            "GetDirtTimestamp",
            &[PyRep::Integer(foreign_ship)],
        )
        .is_none()
    );

    // 3. Wrong session, wrong user, wrong character, and displaced online lock are rejected.
    let mut wrong_sid = sess.clone();
    wrong_sid.session_id = sid + 1;
    assert!(
        ctx.allocate_transition_grace_ship_dirt_binding(
            &wrong_sid,
            st_id,
            &[PyRep::Integer(ship_a)],
        )
        .is_none()
    );
    let mut wrong_user = sess.clone();
    wrong_user.user_id = Some(user_id + 1);
    assert!(
        ctx.grace_bound_object_for_inflight_transition(
            &wrong_user,
            grace_bid,
            "GetDirtTimestamp",
            &[PyRep::Integer(ship_a)],
        )
        .is_none()
    );
    let mut wrong_char = sess.clone();
    wrong_char.character_id = Some(other_char);
    assert!(
        ctx.grace_bound_object_for_inflight_transition(
            &wrong_char,
            grace_bid,
            "GetDirtTimestamp",
            &[PyRep::Integer(ship_a)],
        )
        .is_none()
    );
    assert!(
        ctx.grace_bound_object_for_inflight_transition(
            &sess,
            grace_bid,
            "GetDirtTimestamp",
            &[PyRep::Integer(ship_a)],
        )
        .is_some()
    );

    // 4. Verify 20s validity and >30s expiry, plus dock-back clearing and session purge.
    ctx.advance_secs(20);
    assert!(
        ctx.allocate_transition_grace_ship_dirt_binding(&sess, st_id, &[PyRep::Integer(ship_a)])
            .is_some()
    );
    ctx.advance_secs(12);
    assert!(
        ctx.allocate_transition_grace_ship_dirt_binding(&sess, st_id, &[PyRep::Integer(ship_a)])
            .is_none()
    );
    assert!(
        ctx.grace_bound_object_for_inflight_transition(
            &sess,
            grace_bid,
            "GetDirtTimestamp",
            &[PyRep::Integer(ship_a)],
        )
        .is_none()
    );

    ctx.update_transition_departed_dock(&sess, Some(st_id), None);
    ctx.update_transition_departed_dock(&sess, None, Some(st_id));
    assert!(
        ctx.allocate_transition_grace_ship_dirt_binding(&sess, st_id, &[PyRep::Integer(ship_a)])
            .is_none(),
        "docking back into st_id clears departed dock grace"
    );

    ctx.update_transition_departed_dock(&sess, Some(st_id), None);
    let active_bid = ctx
        .allocate_transition_grace_ship_dirt_binding(&sess, st_id, &[PyRep::Integer(ship_a)])
        .expect("fresh grace bid");

    // 5. Displaced online lock immediately rejects both allocation and in-flight grace.
    ctx.db
        .acquire_online_lock(user_id, sid + 99, Some(char_id), true, ctx.now_unix())
        .expect("displace lock");
    assert!(
        ctx.allocate_transition_grace_ship_dirt_binding(&sess, st_id, &[PyRep::Integer(ship_a)])
            .is_none()
    );
    assert!(
        ctx.grace_bound_object_for_inflight_transition(
            &sess,
            active_bid,
            "GetDirtTimestamp",
            &[PyRep::Integer(ship_a)],
        )
        .is_none()
    );

    // 6. Session purge clears grace entries on fresh session.
    let sid_clean = sid + 100;
    ctx.db
        .acquire_online_lock(user_id, sid_clean, Some(char_id), true, ctx.now_unix())
        .expect("acquire fresh lock");
    let mut sess_clean = sess.clone();
    sess_clean.session_id = sid_clean;
    ctx.update_transition_departed_dock(&sess_clean, Some(st_id), None);
    let purged_bid = ctx
        .allocate_transition_grace_ship_dirt_binding(&sess_clean, st_id, &[PyRep::Integer(ship_a)])
        .expect("fresh session grace bid");
    ctx.purge_transition_grace_for_session(sid_clean);
    assert!(
        ctx.grace_bound_object_for_inflight_transition(
            &sess_clean,
            purged_bid,
            "GetDirtTimestamp",
            &[PyRep::Integer(ship_a)],
        )
        .is_none()
    );
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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

#[test]
fn build_ship_slim_meta_populates_name_from_custom_or_sde_type() {
    let ctx = test_ctx();
    let (char_id, active_ship_id, ship_type_id) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.values().next().unwrap();
        let ship = snap.items.get(&ch.active_ship_id).unwrap();
        (ch.character_id, ch.active_ship_id, ship.type_id)
    });

    // Clear any custom name -> should fall back to SDE type name
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_custom_names.remove(&active_ship_id);
    });
    let expected_type_name = ctx.sde.types.get(&ship_type_id).map(|t| t.name.clone());
    let fallback_meta = ctx.build_ship_slim_meta(char_id, ship_type_id);
    assert_eq!(fallback_meta.name, expected_type_name);
    assert!(fallback_meta.name.is_some());

    // Set a custom name -> build_ship_slim_meta must prefer the custom ship name
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_custom_names
            .insert(active_ship_id, "USS Custom Playtest".into());
    });
    let custom_meta = ctx.build_ship_slim_meta(char_id, ship_type_id);
    assert_eq!(custom_meta.name.as_deref(), Some("USS Custom Playtest"));
}

#[test]
fn invbroker_structure_pos_trade_and_transition_invalidation_matrix_cover_w3_branches() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let other_sys = 30_000_144_u32;
    let st_id = 60_003_760_i64;
    let ship_id = 100_000_001_i64;
    let corp_id = 1_000_009_i64;
    let ally_id = 99_000_001_i64;
    let fleet_id = 777_001_i64;
    let struct_id = 1_000_000_777_001_i64;
    let space_can = 897_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(ally_id);
            ch.fleet_id = Some(fleet_id);
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = Some(struct_id);
            ch.active_ship_id = ship_id;
        }
        snap.structures.insert(
            struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Test Astrahus".into(),
                position: [0.0; 3],
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.items.insert(
            space_can,
            InventoryItemRecord {
                item_id: space_can,
                type_id: 23,
                owner_id: char_id,
                location_id: struct_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.wars.insert(
            501,
            eve_db::WarRecord {
                war_id: 501,
                aggressor_corp_id: corp_id,
                defender_corp_id: 1_000_010,
                ..Default::default()
            },
        );
    });

    let mut sess = SessionState {
        session_id: 901,
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        alliance_id: Some(ally_id),
        fleet_id: Some(fleet_id),
        solar_system_id: Some(sys_id),
        constellation_id: Some(20_000_020),
        region_id: Some(10_000_002),
        station_id: None,
        structure_id: Some(struct_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    // Bindings while docked in an Upwell structure (`struct_id`):
    let b_struct = ctx.bind_object(&sess, "structure", struct_id).unwrap();
    let b_pos = ctx.bind_object(&sess, "posMgr", struct_id).unwrap();
    let b_trade_st = ctx.bind_object(&sess, "trademgr", st_id).unwrap();
    let b_trade_reg = ctx.bind_object(&sess, "trademgr", 10_000_002).unwrap();
    let b_office = ctx.bind_object(&sess, "officeManager", struct_id).unwrap();
    let b_inv_can = ctx.bind_object(&sess, "invbroker", space_can).unwrap();
    let b_corp = ctx.bind_object(&sess, "corpRegistry", corp_id).unwrap();
    let b_ally = ctx.bind_object(&sess, "allianceRegistry", ally_id).unwrap();
    let b_fleet = ctx.bind_object(&sess, "fleetObjectHandler", fleet_id).unwrap();
    let b_war = ctx.bind_object(&sess, "warRegistry", corp_id).unwrap();
    let b_war_stat = ctx.bind_object(&sess, "warStatisticMgr", 501).unwrap();
    let b_dev = ctx.bind_object(&sess, "devIndexManager", 20_000_020).unwrap();
    let b_voucher = ctx.bind_object(&sess, "voucher", corp_id).unwrap();
    let b_contract = ctx.bind_object(&sess, "contractProxy", struct_id).unwrap();
    let b_keeper = ctx.bind_object(&sess, "keeper", sys_id as i64).unwrap();
    let b_cw_sys = ctx.bind_object(&sess, "crimewatch", sys_id as i64).unwrap();

    for bid in [
        b_struct, b_pos, b_trade_st, b_trade_reg, b_office, b_inv_can, b_corp, b_ally, b_fleet,
        b_war, b_war_stat, b_dev, b_voucher, b_contract, b_keeper, b_cw_sys,
    ] {
        assert!(ctx.verify_bound_object(&sess, bid).is_ok());
    }

    // Negative object_id is always rejected.
    assert!(ctx.bind_object(&sess, "agentMgr", -1).is_err());

    // `fleet_change` invalidates only `fleetObjectHandler` when `fleet_id` changes.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().fleet_id = None;
    });
    sess.fleet_id = None;
    ctx.invalidate_bound_objects_for_session_transition(
        char_id,
        sys_id,
        Some(struct_id),
        "fleet_change",
    );
    assert!(ctx.verify_bound_object(&sess, b_fleet).is_err());
    assert!(ctx.verify_bound_object(&sess, b_struct).is_ok());

    // `corp_change` invalidates `corpRegistry`, `allianceRegistry`, `warRegistry`, `voucher` when corp/alliance change.
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.corporation_id = 1_000_010;
        ch.alliance_id = None;
    });
    sess.corporation_id = Some(1_000_010);
    sess.alliance_id = None;
    ctx.invalidate_bound_objects_for_session_transition(
        char_id,
        sys_id,
        Some(struct_id),
        "corp_change",
    );
    assert!(ctx.verify_bound_object(&sess, b_corp).is_err());
    assert!(ctx.verify_bound_object(&sess, b_ally).is_err());
    assert!(ctx.verify_bound_object(&sess, b_war).is_err());
    assert!(ctx.verify_bound_object(&sess, b_voucher).is_err());

    // `stargate_jump` to `other_sys` invalidates system-scoped keeper/crimewatch/structure bindings.
    sess.structure_id = None;
    sess.solar_system_id = Some(other_sys);
    ctx.invalidate_bound_objects_for_session_transition(char_id, other_sys, None, "stargate_jump");
    assert!(ctx.verify_bound_object(&sess, b_keeper).is_err());
    assert!(ctx.verify_bound_object(&sess, b_cw_sys).is_err());
    assert!(ctx.verify_bound_object(&sess, b_struct).is_err());
}

#[tokio::test]
async fn session_cleanup_guard_disconnect_cleanup_and_target_painter_multiplier() {
    let ctx = Arc::new(test_ctx());
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let ship_id = 100_000_001_i64;

    // 1. `SessionCleanupGuard::set_reader_abort` aborts the reader task on drop.
    let handle = tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    });
    let mut sess = SessionState::new(4242);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    ctx.db
        .acquire_online_lock(1, 4242, Some(char_id), true, ctx.now_unix())
        .unwrap();
    {
        let mut guard = crate::router::SessionCleanupGuard::new(Arc::clone(&ctx), 4242);
        guard.set_reader_abort(handle.abort_handle());
    }
    tokio::task::yield_now().await;
    assert!(handle.is_finished());

    // 2. `effective_target_painter_multiplier` returns 1.0 with no painters and > 1.0 when painted.
    assert!((ctx.effective_target_painter_multiplier(ship_id) - 1.0).abs() < 1e-9);
    let target_tid = ctx.db.with_snapshot(|s| {
        s.items
            .get(&ship_id)
            .map(|i| i.type_id)
            .unwrap_or(587)
    });
    let mut tp_sde = (*ctx.sde).clone();
    if let Some(t) = tp_sde.types.get_mut(&target_tid) {
        t.default_attributes.insert(2114, 0.5);
    }
    let tp_ctx = ServerContext::with_sde_and_repository(Arc::new(tp_sde), Arc::clone(&ctx.db));
    let tp_tid = tp_ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 379)
        .map(|t| t.type_id)
        .unwrap_or(0);
    let mut eff_mod = DogmaItemState::from_sde(888_001, tp_tid, &tp_ctx.sde);
    eff_mod.base_attributes.insert(554, 30.0);
    eff_mod.base_attributes.insert(54, 30_000.0);
    eff_mod.base_attributes.insert(2044, 15_000.0);
    tp_ctx.projected_ewar.write().unwrap().insert(
        ship_id,
        vec![ProjectedEwarEffect {
            source_module_id: 888_001,
            source_module_type_id: tp_tid,
            charge_type_id: None,
            distance: 5_000.0,
            overloaded: false,
            effective_module: eff_mod,
        }],
    );
    let mult = tp_ctx.effective_target_painter_multiplier(ship_id);
    assert!(mult > 1.10 && mult < 1.30, "expected ~1.15x with 50% TP resist, got {mult}");

    // 3. `cleanup_disconnected_client_session` releases bindings/locks and persists in-space ball position.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let ship_ball = Ball {
        id: ship_id,
        type_id: target_tid,
        position: DVec3::new(12_345.0, -6_789.0, 42.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle.spawn_ball(ship_ball).await;
    let oid = ctx.bind_object(&sess, "ship", sys_id as i64).unwrap();
    assert!(ctx.verify_bound_object(&sess, oid).is_ok());
    ctx.cleanup_disconnected_client_session(&mut sess).await;
    assert!(ctx.verify_bound_object(&sess, oid).is_err());
    assert_eq!(
        ctx.db.with_snapshot(|s| s.item_positions.get(&ship_id).copied()),
        Some([12_345.0, -6_789.0, 42.0])
    );
}

#[test]
fn special_fx_guid_for_module_sde_covers_all_groups_effects_and_fallbacks() {
    use super::special_fx_guid_for_module_sde;
    use eve_sde::{DogmaEffectType, InvType, SdeRegistry};

    let base_ctx = test_ctx();
    let mut sde = (*base_ctx.sde).clone();
    let tpl: InvType = sde.get_type(34).unwrap().clone();

    let add_ty = |sde: &mut SdeRegistry,
                  tid: u32,
                  group_id: u32,
                  name: &str,
                  default_eff: Option<u16>,
                  eff_ids: Vec<u16>| {
        let mut t = tpl.clone();
        t.type_id = tid;
        t.group_id = group_id;
        t.name = name.to_string();
        t.default_effect_id = default_eff;
        t.effect_ids = eff_ids;
        t.default_attributes.clear();
        sde.types.insert(tid, t);
    };

    // Synthetic effects to exercise effect_id > 0 vs <= 0 (default_effect_id fallback),
    // comma-split + starts_with("effects.") filter, and non-matching fallback.
    sde.effects.insert(
        0,
        DogmaEffectType {
            effect_id: 0,
            is_offensive: true,
            guid: Some("effects.ZeroShouldNotMatch".into()),
            ..Default::default()
        },
    );
    sde.effects.insert(
        61_001,
        DogmaEffectType {
            effect_id: 61_001,
            is_offensive: true,
            guid: Some(" effects.CustomPos , effects.Secondary ".into()),
            ..Default::default()
        },
    );
    sde.effects.insert(
        61_002,
        DogmaEffectType {
            effect_id: 61_002,
            is_offensive: false,
            guid: Some("effects.FallbackDefault".into()),
            ..Default::default()
        },
    );
    sde.effects.insert(
        61_003,
        DogmaEffectType {
            effect_id: 61_003,
            is_offensive: true,
            guid: Some("custom.NotEffectsPrefix".into()),
            ..Default::default()
        },
    );

    // 1. Mining suppression by group_id, effect_id, and effect_name
    for (idx, &gid) in [54_u32, 464, 483, 737, 2004, 4138].iter().enumerate() {
        let tid = 910_000 + idx as u32;
        add_ty(&mut sde, tid, gid, "Miner", None, vec![]);
        assert_eq!(special_fx_guid_for_module_sde(&sde, tid, 61_001, Some("targetAttack")), None);
    }
    add_ty(&mut sde, 910_020, 999, "Neutral Module", None, vec![]);
    for eid in [17_i64, 67, 2726] {
        assert_eq!(special_fx_guid_for_module_sde(&sde, 910_020, eid, Some("neutral")), None);
    }
    for ename in ["miningLaser", "miningClouds", "mining"] {
        assert_eq!(special_fx_guid_for_module_sde(&sde, 910_020, 61_001, Some(ename)), None);
    }

    // 2. Cloak by group_id == 330 or effect_name containing "cloak" (case-insensitive)
    add_ty(&mut sde, 910_021, 330, "Covert Ops", None, vec![]);
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 910_021, 0, Some("neutral")),
        Some(("effects.Cloak".into(), false))
    );
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 910_020, 0, Some("ModuleCloakFX")),
        Some(("effects.Cloak".into(), false))
    );

    // 3. HybridFired (group 74, eff_name hybridFired, or group 53 with blaster/railgun name)
    add_ty(&mut sde, 910_022, 74, "Mystery Turret", None, vec![]);
    add_ty(&mut sde, 910_023, 53, "Light Neutron Blaster II", None, vec![]);
    add_ty(&mut sde, 910_024, 53, "75mm Gatling Railgun", None, vec![]);
    add_ty(&mut sde, 910_025, 53, "Mega Pulse Laser", None, vec![]);
    add_ty(&mut sde, 910_026, 55, "200mm AutoCannon", None, vec![]);

    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 910_022, 0, None),
        Some(("effects.HybridFired".into(), true))
    );
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 910_020, 0, Some("hybridFired")),
        Some(("effects.HybridFired".into(), true))
    );
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 910_023, 0, Some("neutral")),
        Some(("effects.HybridFired".into(), true))
    );
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 910_024, 0, Some("neutral")),
        Some(("effects.HybridFired".into(), true))
    );

    // 4. ProjectileFired & Laser
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 910_026, 0, None),
        Some(("effects.ProjectileFired".into(), true))
    );
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 910_020, 0, Some("projectileFired")),
        Some(("effects.ProjectileFired".into(), true))
    );
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 910_025, 0, Some("neutral")),
        Some(("effects.Laser".into(), true))
    );
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 910_020, 0, Some("targetAttack")),
        Some(("effects.Laser".into(), true))
    );

    // 5. SDE effect GUID resolution (effect_id > 0 vs == 0 vs < 0 -> default_effect_id)
    add_ty(&mut sde, 910_027, 999, "Effect Holder", Some(61_002), vec![]);
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 910_027, 61_001, Some("armorRepair")),
        Some(("effects.CustomPos".into(), true))
    );
    // effect_id == 0 must NOT read sde.effects[&0]; it uses default_effect_id (61_002)
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 910_027, 0, Some("armorRepair")),
        Some(("effects.FallbackDefault".into(), false))
    );
    // negative effect_id also falls back to default_effect_id
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 910_027, -1, Some("armorRepair")),
        Some(("effects.FallbackDefault".into(), false))
    );
    // non-"effects." prefix falls through to group/eff_name fallbacks
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 910_027, 61_003, Some("armorRepair")),
        Some(("effects.ArmorRepair".into(), false))
    );

    // 6. Group match arms
    let group_cases: &[(u32, &str, bool)] = &[
        (65, "effects.ModifyTargetSpeed", true),
        (52, "effects.WarpScramble", true),
        (201, "effects.ElectronicAttributeModifyTarget", true),
        (208, "effects.ElectronicAttributeModifyTarget", true),
        (291, "effects.ElectronicAttributeModifyTarget", true),
        (842, "effects.ElectronicAttributeModifyTarget", true),
        (379, "effects.TargetPaint", true),
        (71, "effects.EnergyDestabilization", true),
        (68, "effects.EnergyVampire", true),
        (40, "effects.ShieldBoosting", false),
        (1156, "effects.ShieldBoosting", false),
        (62, "effects.ArmorRepair", false),
        (1199, "effects.ArmorRepair", false),
        (506, "effects.MissileDeployment", true),
        (508, "effects.MissileDeployment", true),
        (511, "effects.MissileDeployment", true),
        (524, "effects.MissileDeployment", true),
        (1245, "effects.MissileDeployment", true),
    ];
    for (idx, &(gid, expected_guid, offensive)) in group_cases.iter().enumerate() {
        let tid = 910_100 + idx as u32;
        add_ty(&mut sde, tid, gid, "Generic", None, vec![]);
        assert_eq!(
            special_fx_guid_for_module_sde(&sde, tid, 0, None),
            Some((expected_guid.into(), offensive)),
            "group {gid}"
        );
    }

    // Group 46 propulsion sub-branches (AB vs MWD via eff_name / effect 6730 / name)
    add_ty(&mut sde, 910_200, 46, "1MN Afterburner", None, vec![]);
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 910_200, 0, Some("moduleBonusAfterburner")),
        Some(("effects.Afterburner".into(), false))
    );
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 910_200, 0, Some("moduleBonusMicrowarpdrive")),
        Some(("effects.MicroWarpDrive".into(), false))
    );
    add_ty(&mut sde, 910_201, 46, "1MN Prop", None, vec![6730]);
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 910_201, 0, None),
        Some(("effects.MicroWarpDrive".into(), false))
    );
    add_ty(&mut sde, 910_202, 46, "5MN MicroWarp Drive", None, vec![]);
    assert_eq!(
        special_fx_guid_for_module_sde(&sde, 910_202, 0, None),
        Some(("effects.MicroWarpDrive".into(), false))
    );

    // 7. Fallback effect_name match arms
    let name_cases: &[(&str, &str, bool)] = &[
        ("remoteWebifierFalloff", "effects.ModifyTargetSpeed", true),
        ("modifyTargetSpeed2", "effects.ModifyTargetSpeed", true),
        ("decreaseTargetSpeed", "effects.ModifyTargetSpeed", true),
        ("warpScramble", "effects.WarpScramble", true),
        ("warpScrambleForEntity", "effects.WarpScramble", true),
        ("warpScrambleBlockMWDWithNPCEffect", "effects.WarpScramble", true),
        ("remoteSensorDampFalloff", "effects.ElectronicAttributeModifyTarget", true),
        ("remoteECMFalloff", "effects.ElectronicAttributeModifyTarget", true),
        ("structureModuleEffectRemoteSensorDampener", "effects.ElectronicAttributeModifyTarget", true),
        ("remoteWeaponDisruptFalloff", "effects.ElectronicAttributeModifyTarget", true),
        ("ewTargetPaint", "effects.TargetPaint", true),
        ("remoteTargetPaintFalloff", "effects.TargetPaint", true),
        ("energyDestabilizationNew", "effects.EnergyDestabilization", true),
        ("energyNeutralizerFalloff", "effects.EnergyDestabilization", true),
        ("energyVampire", "effects.EnergyVampire", true),
        ("energyNosferatuFalloff", "effects.EnergyVampire", true),
        ("moduleBonusMicrowarpdrive", "effects.MicroWarpDrive", false),
        ("moduleBonusAfterburner", "effects.Afterburner", false),
        ("speedBoost", "effects.Afterburner", false),
        ("shieldBoosting", "effects.ShieldBoosting", false),
        ("fueledShieldBoosting", "effects.ShieldBoosting", false),
        ("armorRepair", "effects.ArmorRepair", false),
        ("fueledArmorRepair", "effects.ArmorRepair", false),
    ];
    for &(ename, expected_guid, offensive) in name_cases {
        assert_eq!(
            special_fx_guid_for_module_sde(&sde, 910_020, 0, Some(ename)),
            Some((expected_guid.into(), offensive)),
            "effect name {ename}"
        );
    }
    assert_eq!(special_fx_guid_for_module_sde(&sde, 910_020, 0, Some("completelyUnknown")), None);
}

#[test]
fn bf03_headless_disconnect_session_releases_online_lock_and_logs_off_sync() {
    let ctx = test_ctx();
    let user_id = 1_u64;
    let char_id = 90_000_001_i64;
    let sid = 777_001_u64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = 30_000_142;
        }
    });
    ctx.db
        .acquire_online_lock(user_id, sid, Some(char_id), false, ctx.now_unix())
        .expect("acquire online lock");
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnAccountChange",
        "charid",
        vec![eve_proto::PyRep::Integer(1)],
    );

    // No closer registered and no Tokio reactor running -> headless/admin disconnect must still
    // release online lock and purge notifications, while preserving persisted ship state untouched
    // without inventing a `(0, 0, 0)` position or registering a phantom hold when no live ball exists.
    ctx.disconnect_session(sid, "EndAllGameSessions", "Headless admin session kill");

    assert!(
        ctx.db.get_online_session(user_id).is_none(),
        "headless disconnect_session must release online lock (#851)"
    );
    assert!(
        ctx.drain_notifications_for_char(char_id).is_empty(),
        "headless disconnect_session must run logoff_character_sync and clear pending notifications (#881)"
    );
    assert!(
        !crate::sp6_lifecycle::has_active_disconnected_ship(&ctx.db, char_id),
        "headless disconnect_session without live ball must not fabricate an in-space ship hold"
    );
}

#[tokio::test]
async fn bf03_tokio_headless_disconnect_session_spawns_hold_and_logs_off_sync() {
    let ctx = test_ctx();
    let user_id = 1_u64;
    let char_id = 90_000_001_i64;
    let sid = 777_002_u64;

    let ship_id = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = 30_000_142;
        ch.active_ship_id
    });
    let live_pos = glam::DVec3::new(15_000_000_000.0, 250.0, -400.0);
    let live_vel = glam::DVec3::new(42.0, -3.5, 1.0);
    ctx.systems
        .get_or_spawn(30_000_142)
        .try_spawn_ball_with_meta(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 606,
                position: live_pos,
                velocity: live_vel,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, 606),
            eve_destiny::SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: char_id,
                char_id: Some(char_id),
                corp_id: 1_000_009,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
                ..Default::default()
            },
        );
    ctx.db
        .acquire_online_lock(user_id, sid, Some(char_id), false, ctx.now_unix())
        .expect("acquire online lock");

    ctx.disconnect_session(sid, "EndAllGameSessions", "Headless Tokio session kill");

    assert!(ctx.db.get_online_session(user_id).is_none());
    assert!(crate::sp6_lifecycle::has_active_disconnected_ship(&ctx.db, char_id));
    let sys_handle = ctx
        .systems
        .get_handle(30_000_142)
        .expect("Tokio disconnect_session must spawn SystemActor for active in-space ship hold");
    assert!(
        sys_handle.get_ball_position_sync(ship_id).is_some(),
        "Tokio disconnect_session must materialize the held ship ball in Ballpark (#851)"
    );
}

#[test]
fn transition_grace_retains_inflight_invbroker_metadata_and_enforces_expiry_and_purges() {
    let ctx = test_ctx();
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(
        1_800_000_000,
    )));
    let user_id = 1_u64;
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let sid = 777_146_u64;
    let sys_from = 30_000_144_u32;
    let sys_to = 30_000_142_u32;
    let ship_a = 1_897_301_923_i64;
    let ship_b = 1_897_147_421_i64;
    let ship_c = 1_897_147_422_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_from;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_b;
        }
        for (item_id, type_id) in [(ship_a, 587_u32), (ship_b, 606_u32), (ship_c, 587_u32)] {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id,
                    owner_id: char_id,
                    location_id: i64::from(sys_from),
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });
    ctx.db
        .acquire_online_lock(user_id, sid, Some(char_id), false, ctx.now_unix())
        .expect("acquire online lock");
    let _ = ctx.drain_notifications_for_char(char_id);

    let mut sess = SessionState::new(sid);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(1_000_009);
    sess.solar_system_id = Some(sys_from);
    sess.ship_id = Some(ship_b);

    let b_prev_ship = ctx
        .bind_object(&sess, "invbroker", ship_a)
        .expect("bind prior ship invbroker in sys_from");
    let prev_oid = ctx
        .verify_bound_object(&sess, b_prev_ship)
        .expect("verify b_prev_ship")
        .oid_str;
    let b_active_ship = ctx
        .bind_object(&sess, "invbroker", ship_b)
        .expect("bind active ship invbroker");
    let b_beyonce = ctx
        .bind_object(&sess, "beyonce", i64::from(sys_from))
        .expect("bind beyonce");

    // Cross-system /moveme invalidates remote ship_a and beyonce while preserving active ship_b.
    sess.solar_system_id = Some(sys_to);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_to;
        }
        if let Some(sb) = snap.items.get_mut(&ship_b) {
            sb.location_id = i64::from(sys_to);
        }
    });
    ctx.invalidate_bound_objects_for_session_transition(char_id, sys_to, None, "slash_teleport");

    assert_eq!(
        ctx.verify_bound_object(&sess, b_prev_ship).unwrap_err().code,
        "InvalidBoundObject"
    );
    assert_eq!(
        ctx.verify_bound_object(&sess, b_beyonce).unwrap_err().code,
        "InvalidBoundObject"
    );
    assert!(ctx.verify_bound_object(&sess, b_active_ship).is_ok());
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(notifs.iter().any(|n| {
        n.event == "OnMachoObjectDisconnect"
            && n.args.first() == Some(&PyRep::ascii_bytes(prev_oid.clone()))
    }));

    // In-flight read-only metadata calls on invalidated invbroker stay authorized inside 30s grace.
    let grace_self = ctx
        .grace_bound_object_for_inflight_invbroker(&sess, b_prev_ship, "GetSelfInvItem")
        .expect("GetSelfInvItem grace within 30s");
    assert_eq!(grace_self.object_id, ship_a);
    assert!(
        ctx.grace_bound_object_for_inflight_invbroker(&sess, b_prev_ship, "GetAvailableTurretSlots")
            .is_some()
    );
    assert!(ctx.has_transition_grace_invbroker_binding(&sess, b_prev_ship, ship_a));

    // Mutating / container listing / non-invbroker bindings must NEVER enter grace.
    for blocked in ["List", "ListByFlags", "Add", "MultiAdd", "Remove"] {
        assert!(
            ctx.grace_bound_object_for_inflight_invbroker(&sess, b_prev_ship, blocked)
                .is_none(),
            "{blocked} must remain rejected"
        );
    }
    assert!(
        ctx.grace_bound_object_for_inflight_invbroker(&sess, b_beyonce, "GetSelfInvItem")
            .is_none()
    );
    assert!(!ctx.has_transition_grace_invbroker_binding(&sess, b_prev_ship, ship_b));

    // Wrong session, wrong user, or wrong character is rejected.
    let mut wrong_sid = sess.clone();
    wrong_sid.session_id = sid + 1;
    assert!(
        ctx.grace_bound_object_for_inflight_invbroker(&wrong_sid, b_prev_ship, "GetSelfInvItem")
            .is_none()
    );
    let mut wrong_user = sess.clone();
    wrong_user.user_id = Some(user_id + 99);
    assert!(
        ctx.grace_bound_object_for_inflight_invbroker(&wrong_user, b_prev_ship, "GetSelfInvItem")
            .is_none()
    );
    let mut wrong_char = sess.clone();
    wrong_char.character_id = Some(other_char);
    assert!(
        ctx.grace_bound_object_for_inflight_invbroker(&wrong_char, b_prev_ship, "GetSelfInvItem")
            .is_none()
    );

    // Grace remains valid at 20s and expires after 31s.
    ctx.advance_secs(20);
    assert!(
        ctx.grace_bound_object_for_inflight_invbroker(&sess, b_prev_ship, "GetSelfInvItem")
            .is_some()
    );
    ctx.advance_secs(11);
    assert!(
        ctx.grace_bound_object_for_inflight_invbroker(&sess, b_prev_ship, "GetSelfInvItem")
            .is_none()
    );

    // Explicit client release, logout transition, release_session_bindings, and
    // release_user_bindings each purge grace entries immediately.
    sess.solar_system_id = Some(sys_from);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_from;
        }
    });
    let b_c1 = ctx.bind_object(&sess, "invbroker", ship_c).expect("bind ship_c");
    let c1_oid = ctx.verify_bound_object(&sess, b_c1).unwrap().oid_str;
    ctx.invalidate_bound_objects_for_session_transition(char_id, sys_to, None, "slash_teleport");
    assert!(
        ctx.grace_bound_object_for_inflight_invbroker(&sess, b_c1, "GetSelfInvItem")
            .is_some()
    );
    ctx.release_bound_objects_from_pyrep(sid, &PyRep::List(vec![PyRep::ascii_bytes(c1_oid)]));
    assert!(
        ctx.grace_bound_object_for_inflight_invbroker(&sess, b_c1, "GetSelfInvItem")
            .is_none()
    );

    let b_c2 = ctx.bind_object(&sess, "invbroker", ship_c).expect("bind ship_c #2");
    ctx.invalidate_bound_objects_for_session_transition(char_id, sys_to, None, "slash_teleport");
    assert!(
        ctx.grace_bound_object_for_inflight_invbroker(&sess, b_c2, "GetSelfInvItem")
            .is_some()
    );
    ctx.invalidate_bound_objects_for_session_transition(char_id, sys_to, None, "logout");
    assert!(
        ctx.grace_bound_object_for_inflight_invbroker(&sess, b_c2, "GetSelfInvItem")
            .is_none()
    );

    let b_c3 = ctx.bind_object(&sess, "invbroker", ship_c).expect("bind ship_c #3");
    ctx.invalidate_bound_objects_for_session_transition(char_id, sys_to, None, "slash_teleport");
    ctx.release_session_bindings(sid);
    assert!(
        ctx.grace_bound_object_for_inflight_invbroker(&sess, b_c3, "GetSelfInvItem")
            .is_none()
    );

    let b_c4 = ctx.bind_object(&sess, "invbroker", ship_c).expect("bind ship_c #4");
    ctx.invalidate_bound_objects_for_session_transition(char_id, sys_to, None, "slash_teleport");
    ctx.release_user_bindings(user_id);
    assert!(
        ctx.grace_bound_object_for_inflight_invbroker(&sess, b_c4, "GetSelfInvItem")
            .is_none()
    );

    // Per-session cap (256) evicts oldest grace entries cleanly across batches.
    let mut batch1 = Vec::new();
    for _ in 0..150 {
        batch1.push(ctx.bind_object(&sess, "invbroker", ship_c).expect("batch1 bind"));
    }
    ctx.invalidate_bound_objects_for_session_transition(char_id, sys_to, None, "slash_teleport");
    ctx.advance_secs(1);
    let mut batch2 = Vec::new();
    for _ in 0..150 {
        batch2.push(ctx.bind_object(&sess, "invbroker", ship_c).expect("batch2 bind"));
    }
    ctx.invalidate_bound_objects_for_session_transition(char_id, sys_to, None, "slash_teleport");
    assert!(
        ctx.grace_bound_object_for_inflight_invbroker(&sess, batch1[0], "GetSelfInvItem")
            .is_none(),
        "oldest grace entries above 256 per-session cap must be evicted"
    );
    assert!(
        ctx.grace_bound_object_for_inflight_invbroker(&sess, batch2[149], "GetSelfInvItem")
            .is_some(),
        "newest grace entries within 256 per-session cap must remain"
    );

    // Online-session displacement immediately blocks grace access even before 30s expires.
    ctx.db
        .acquire_online_lock(user_id, sid + 50, Some(char_id), true, ctx.now_unix())
        .expect("displace online lock");
    assert!(
        ctx.grace_bound_object_for_inflight_invbroker(&sess, batch2[149], "GetSelfInvItem")
            .is_none(),
        "displaced session must lose transition grace immediately"
    );
}

#[test]
fn transition_grace_crimewatch_binding_lifecycle_and_security_boundaries() {
    let ctx = test_ctx();
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(
        1_800_000_000,
    )));
    let user_id = 1_u64;
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let sid = 777_209_u64;
    let jita_sys = 30_000_142_u32;
    let perimeter_sys = 30_000_144_u32;
    let amarr_sys = 30_002_187_u32;
    let st_id = 60_003_760_i64;
    let ship_a = ctx
        .db
        .get_character(char_id)
        .expect("demo character")
        .active_ship_id
        .max(1_897_301_923);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = jita_sys;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_a;
        }
        snap.items
            .entry(ship_a)
            .or_insert_with(|| InventoryItemRecord {
                item_id: ship_a,
                type_id: 587,
                owner_id: char_id,
                location_id: i64::from(jita_sys),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            });
    });
    ctx.db
        .acquire_online_lock(user_id, sid, Some(char_id), true, ctx.now_unix())
        .expect("acquire online lock");

    let mut sess = SessionState::new(sid);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(1_000_009);
    sess.solar_system_id = Some(jita_sys);
    sess.ship_id = Some(ship_a);

    // Before recording any departure, crimewatch grace refuses unvisited solar systems.
    assert!(
        ctx.allocate_transition_grace_crimewatch_binding(&sess, i64::from(perimeter_sys), &[])
            .is_none()
    );

    // 1. Overlapping /tr simulation: Perimeter (30000144) -> Jita (30000142) records departed solar system
    //    even if crimewatch(30000144) was never bound prior to departure (#1209, #1211).
    ctx.update_transition_departed_locations(
        &sess,
        Some(perimeter_sys),
        None,
        Some(jita_sys),
        None,
    );

    let grace_bid = ctx
        .allocate_transition_grace_crimewatch_binding(&sess, i64::from(perimeter_sys), &[])
        .expect("crimewatch grace allocated within 30s");
    assert!(
        !ctx.bound_objects.read().unwrap().contains_key(&grace_bid),
        "crimewatch grace bid must remain out of active bound_objects"
    );
    assert_eq!(
        ctx.verify_bound_object(&sess, grace_bid).unwrap_err().code,
        "InvalidBoundObject"
    );
    assert_eq!(
        ctx.transition_grace_service_for_bound_id(&sess, grace_bid).as_deref(),
        Some("crimewatch")
    );
    let resolved = ctx
        .grace_bound_object_for_inflight_transition(&sess, grace_bid, "GetClientStates", &[])
        .expect("GetClientStates grace authorized");
    assert_eq!(resolved.service, "crimewatch");
    assert_eq!(resolved.object_id, i64::from(perimeter_sys));

    // Disjoint boundary preservation: departed SolarSystem does NOT grant ship.GetDirtTimestamp dock grace.
    assert!(
        ctx.allocate_transition_grace_ship_dirt_binding(
            &sess,
            i64::from(perimeter_sys),
            &[PyRep::Integer(ship_a)],
        )
        .is_none(),
        "departed solar system must not grant ship dock grace"
    );

    // Non-empty args and non-GetClientStates crimewatch/other methods must be rejected.
    assert!(
        ctx.allocate_transition_grace_crimewatch_binding(
            &sess,
            i64::from(perimeter_sys),
            &[PyRep::Integer(1)],
        )
        .is_none()
    );
    assert!(
        ctx.grace_bound_object_for_inflight_transition(
            &sess,
            grace_bid,
            "GetClientStates",
            &[PyRep::Integer(1)],
        )
        .is_none()
    );
    for blocked in [
        "SetSafetyLevel",
        "StartDuelChallenge",
        "RespondToDuelChallenge",
        "ApplyTagsForSecStatus",
        "GetMySecurityStatus",
        "GetCharacterSecurityStatus",
        "GetSecurityStatusTransactions",
        "GetDirtTimestamp",
        "GetSelfInvItem",
    ] {
        assert!(
            ctx.grace_bound_object_for_inflight_transition(&sess, grace_bid, blocked, &[])
                .is_none(),
            "{blocked} must be rejected on crimewatch transition grace"
        );
    }
    assert!(
        ctx.allocate_transition_grace_crimewatch_binding(&sess, i64::from(amarr_sys), &[])
            .is_none(),
        "unvisited solar system must be denied"
    );

    // Wrong session/user/char are rejected.
    let mut wrong_sid = sess.clone();
    wrong_sid.session_id = sid + 1;
    assert!(
        ctx.allocate_transition_grace_crimewatch_binding(
            &wrong_sid,
            i64::from(perimeter_sys),
            &[],
        )
        .is_none()
    );
    let mut wrong_user = sess.clone();
    wrong_user.user_id = Some(user_id + 1);
    assert!(
        ctx.grace_bound_object_for_inflight_transition(&wrong_user, grace_bid, "GetClientStates", &[])
            .is_none()
    );
    let mut wrong_char = sess.clone();
    wrong_char.character_id = Some(other_char);
    assert!(
        ctx.grace_bound_object_for_inflight_transition(&wrong_char, grace_bid, "GetClientStates", &[])
            .is_none()
    );

    // 2. TTL boundary: valid at 20s, expired past 30s.
    ctx.advance_secs(20);
    assert!(
        ctx.allocate_transition_grace_crimewatch_binding(&sess, i64::from(perimeter_sys), &[])
            .is_some()
    );
    ctx.advance_secs(12);
    assert!(
        ctx.allocate_transition_grace_crimewatch_binding(&sess, i64::from(perimeter_sys), &[])
            .is_none()
    );
    assert!(
        ctx.grace_bound_object_for_inflight_transition(&sess, grace_bid, "GetClientStates", &[])
            .is_none()
    );

    // 3. Undocking from st_id also authorizes crimewatch grace against departed dock st_id,
    //    and returning to a previously departed location clears that location's grace.
    ctx.update_transition_departed_locations(&sess, Some(jita_sys), Some(st_id), Some(jita_sys), None);
    assert!(
        ctx.allocate_transition_grace_crimewatch_binding(&sess, st_id, &[])
            .is_some(),
        "departed dock should authorize crimewatch.GetClientStates grace"
    );
    ctx.update_transition_departed_locations(&sess, Some(jita_sys), None, Some(perimeter_sys), None);
    ctx.update_transition_departed_locations(&sess, Some(perimeter_sys), None, Some(jita_sys), None);
    assert!(
        ctx.allocate_transition_grace_crimewatch_binding(&sess, i64::from(jita_sys), &[])
            .is_none(),
        "returning to jita_sys clears jita_sys from departed locations"
    );
    assert!(
        ctx.allocate_transition_grace_crimewatch_binding(&sess, i64::from(perimeter_sys), &[])
            .is_some(),
        "perimeter_sys departed in return jump remains valid"
    );

    // 4. Evicting active crimewatch(jita_sys) & beyonce(jita_sys) records both bound-entry and system grace,
    //    while client release / logout / displaced lock purges cleanly.
    let active_cw_bid = ctx
        .bind_object(&sess, "crimewatch", i64::from(jita_sys))
        .expect("bind active crimewatch to jita");
    let active_cw_oid = ctx.verify_bound_object(&sess, active_cw_bid).unwrap().oid_str;
    ctx.invalidate_bound_objects_for_session_transition(
        char_id,
        perimeter_sys,
        None,
        "slash_teleport",
    );
    assert!(
        ctx.grace_bound_object_for_inflight_transition(&sess, active_cw_bid, "GetClientStates", &[])
            .is_some(),
        "evicted crimewatch binding retained for GetClientStates grace"
    );
    assert!(
        ctx.allocate_transition_grace_crimewatch_binding(&sess, i64::from(jita_sys), &[])
            .is_some(),
        "evicted crimewatch system also records departed solar system"
    );
    ctx.release_bound_objects_from_pyrep(sid, &PyRep::List(vec![PyRep::ascii_bytes(active_cw_oid)]));
    assert!(
        ctx.grace_bound_object_for_inflight_transition(&sess, active_cw_bid, "GetClientStates", &[])
            .is_none()
    );

    sess.solar_system_id = Some(jita_sys);
    ctx.invalidate_bound_objects_for_session_transition(char_id, perimeter_sys, None, "logout");
    assert!(
        ctx.allocate_transition_grace_crimewatch_binding(&sess, i64::from(jita_sys), &[])
            .is_none(),
        "logout must purge departed location grace"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dispatch_rpc_tr_overlapping_transition_serves_crimewatch_get_client_states_via_grace() {
    let ctx = test_ctx();
    let user_id = 1_u64;
    let char_id = 90_000_001_i64;
    let sid = 777_211_u64;
    let jita_sys = 30_000_142_u32;
    let perimeter_sys = 30_000_144_u32;
    let amarr_sys = 30_002_187_u32;
    let station_jita = 60_003_760_i64;
    let kw = std::collections::HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = jita_sys;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    let ship_id = ctx.db.get_character(char_id).unwrap().active_ship_id;
    ctx.db
        .acquire_online_lock(user_id, sid, Some(char_id), true, ctx.now_unix())
        .expect("acquire online lock");

    let mut sess = SessionState::new(sid);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(1_000_009);
    sess.solar_system_id = Some(jita_sys);
    sess.ship_id = (ship_id > 0).then_some(ship_id);
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;

    // 1. Overlapping /tr sequence matching #1209 / #1211:
    //    /tr me 30000144 (never binding crimewatch in 30000144), followed immediately by /tr me 60003760.
    ctx.dispatch_rpc(
        &mut sess,
        "slash",
        None,
        "SlashCmd",
        &[PyRep::String("/tr me 30000144".into())],
        &kw,
        Some("machoNet:26000"),
    )
    .await
    .expect("/tr me 30000144 succeeds");
    assert_eq!(sess.solar_system_id, Some(perimeter_sys));

    ctx.dispatch_rpc(
        &mut sess,
        "slash",
        None,
        "SlashCmd",
        &[PyRep::String(format!("/tr me {station_jita}"))],
        &kw,
        Some("machoNet:26000"),
    )
    .await
    .expect("/tr me 60003760 succeeds");
    assert_eq!(sess.solar_system_id, Some(jita_sys));
    assert_eq!(sess.station_id, None);

    // 2. Delayed client crimewatchSvc.ProcessSessionChange / OnSessionChanged calls
    //    eveMoniker.CharGetCrimewatchLocation().GetClientStates() targeting departed Perimeter (30000144, 5).
    let bind_res = ctx
        .dispatch_rpc(
            &mut sess,
            "crimewatch",
            None,
            "MachoBindObject",
            &[
                PyRep::Tuple(vec![
                    PyRep::Integer(i64::from(perimeter_sys)),
                    PyRep::Integer(5),
                ]),
                PyRep::Tuple(vec![
                    PyRep::String("GetClientStates".into()),
                    PyRep::Tuple(Vec::new()),
                    PyRep::Dict(Vec::new()),
                ]),
            ],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("delayed crimewatch.MachoBindObject((30000144, 5), GetClientStates) must succeed via grace");

    let PyRep::Tuple(pair) = bind_res else {
        panic!("expected MachoBindObject (oid_rep, retval) tuple");
    };
    assert_eq!(pair.len(), 2);
    let PyRep::Tuple(states) = &pair[1] else {
        panic!("expected 4-tuple from GetClientStates, got {:?}", pair[1]);
    };
    assert_eq!(states.len(), 4);

    let PyRep::SubStruct(inner) = &pair[0] else {
        panic!("expected SubStruct OID ref, got {:?}", pair[0]);
    };
    let PyRep::SubStream(sub) = inner.as_ref() else {
        panic!("expected SubStream inside OID SubStruct");
    };
    let PyRep::Tuple(oid_pair) = sub.as_ref() else {
        panic!("expected (oid_str, ref_id) tuple");
    };
    let oid_str = oid_pair[0].as_str().expect("oid_str");
    let grace_bid: u64 = oid_str
        .rsplit_once(':')
        .expect("N=node:bid")
        .1
        .parse()
        .expect("numeric grace bid");
    assert!(
        !ctx.bound_objects.read().unwrap().contains_key(&grace_bid),
        "grace bound_id must stay out of active bound_objects"
    );

    // 3. Direct ObjectCall(grace_bid, "GetClientStates") with wire service="machoNet" succeeds too.
    let follow_res = ctx
        .dispatch_rpc(
            &mut sess,
            "machoNet",
            Some(grace_bid),
            "GetClientStates",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("follow-up GetClientStates on grace_bid must succeed");
    let PyRep::Tuple(follow_states) = follow_res else {
        panic!("expected 4-tuple from ObjectCall GetClientStates");
    };
    assert_eq!(follow_states.len(), 4);

    // 4. Mutating crimewatch calls and unvisited solar systems are strictly denied.
    let mutating_bind_err = ctx
        .dispatch_rpc(
            &mut sess,
            "crimewatch",
            None,
            "MachoBindObject",
            &[
                PyRep::Tuple(vec![
                    PyRep::Integer(i64::from(perimeter_sys)),
                    PyRep::Integer(5),
                ]),
                PyRep::Tuple(vec![
                    PyRep::String("SetSafetyLevel".into()),
                    PyRep::Tuple(vec![PyRep::Integer(0)]),
                    PyRep::Dict(Vec::new()),
                ]),
            ],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect_err("SetSafetyLevel must not be authorized via crimewatch grace bind");
    assert_eq!(mutating_bind_err.code, super::ERR_BIND_AUTH_DENIED);

    let mutating_obj_err = ctx
        .dispatch_rpc(
            &mut sess,
            "machoNet",
            Some(grace_bid),
            "SetSafetyLevel",
            &[PyRep::Integer(0)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect_err("SetSafetyLevel must be rejected on grace bound_id");
    assert_eq!(mutating_obj_err.code, "InvalidBoundObject");

    let unvisited_err = ctx
        .dispatch_rpc(
            &mut sess,
            "crimewatch",
            None,
            "MachoBindObject",
            &[
                PyRep::Tuple(vec![
                    PyRep::Integer(i64::from(amarr_sys)),
                    PyRep::Integer(5),
                ]),
                PyRep::Tuple(vec![
                    PyRep::String("GetClientStates".into()),
                    PyRep::Tuple(Vec::new()),
                    PyRep::Dict(Vec::new()),
                ]),
            ],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect_err("unvisited system Amarr must remain denied");
    assert_eq!(unvisited_err.code, super::ERR_BIND_AUTH_DENIED);
}

#[test]
fn notification_sub_object_binding_isolates_quota_and_caps_offline_without_evicting_services() {
    let ctx = test_ctx();
    let trade_service = "trademgr";
    let partner_id = 90_000_002_i64;
    let partner_uid = 2_u64;
    let partner_sid = 88_100_u64;
    let station_id = 60_003_760_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&partner_id) {
            ch.user_id = partner_uid;
            ch.station_id = Some(station_id);
        }
    });
    ctx.db
        .acquire_online_lock(partner_uid, partner_sid, Some(partner_id), false, ctx.now_unix())
        .unwrap();

    let partner_sess = SessionState {
        session_id: partner_sid,
        user_id: Some(partner_uid),
        character_id: Some(partner_id),
        station_id: Some(station_id),
        ..Default::default()
    };

    // Fill partner session up to MAX_BOUND_OBJECTS_PER_SESSION with normal bindings.
    let mut normal_bids = Vec::new();
    for _ in 0..super::MAX_BOUND_OBJECTS_PER_SESSION {
        let bid = ctx
            .bind_verified_sub_object(&partner_sess, "invbroker", station_id)
            .expect("bind normal invbroker");
        normal_bids.push(bid);
    }

    // Pushing notification-scoped `trademgr` sub-objects must NOT evict any existing bindings.
    let mut notif_bids = Vec::new();
    for idx in 0..super::MAX_NOTIFICATION_SUB_OBJECTS_PER_TARGET_SERVICE {
        let (bid, rep) = ctx
            .bind_sub_object_for_notification(partner_id, None, trade_service, 70_000 + idx as i64)
            .expect("bind notification sub-object within dedicated quota");
        assert!(matches!(rep, PyRep::SubStruct(_)));
        notif_bids.push(bid);
    }

    let quota_err = ctx
        .bind_sub_object_for_notification(partner_id, None, trade_service, 79_999)
        .expect_err("exceeding per-(session, service) notification quota must fail");
    assert_eq!(quota_err.code, "TooManyBoundSubObjects");

    ctx.bound_objects
        .read()
        .unwrap()
        .get(&normal_bids[0])
        .expect("earliest normal bound object must NOT be evicted by notification sub-objects");
    for bid in &normal_bids {
        assert!(ctx.bound_objects.read().unwrap().contains_key(bid));
    }

    // Binding another normal object evicts only the oldest normal object, leaving notification sub-objects intact.
    let extra_normal = ctx
        .bind_verified_sub_object(&partner_sess, "invbroker", station_id)
        .expect("bind extra normal object");
    {
        let map = ctx.bound_objects.read().unwrap();
        assert!(!map.contains_key(&normal_bids[0]));
        assert!(map.contains_key(&extra_normal));
        for bid in &notif_bids {
            assert!(map.contains_key(bid));
        }
    }

    // release_bound_objects_for_object clears notification-pushed handles and restores quota.
    let released = ctx.release_bound_objects_for_object(trade_service, 70_000);
    assert_eq!(released, 1);
    let (rebound, _) = ctx
        .bind_sub_object_for_notification(partner_id, None, trade_service, 70_000)
        .expect("rebind after release_bound_objects_for_object");
    assert!(ctx.bound_objects.read().unwrap().contains_key(&rebound));

    // Offline target with fallback=None must fail cleanly without creating session_id=0 entries.
    let offline_char = 90_000_001_i64;
    let offline_err = ctx
        .bind_sub_object_for_notification(offline_char, None, trade_service, 81_000)
        .expect_err("offline character without fallback must fail");
    assert_eq!(offline_err.code, "CharacterOffline");
    assert_eq!(
        ctx.bound_objects
            .read()
            .unwrap()
            .values()
            .filter(|e| e.session_id == 0)
            .count(),
        0
    );

    // Explicit synthetic fallback with session_id=0 is still strictly bounded.
    let synth_uid = ctx.db.with_snapshot(|s| s.characters[&offline_char].user_id);
    let synth_sess = SessionState {
        session_id: 0,
        user_id: Some(synth_uid),
        character_id: Some(offline_char),
        ..Default::default()
    };
    for idx in 0..super::MAX_NOTIFICATION_SUB_OBJECTS_PER_TARGET_SERVICE {
        ctx.bind_sub_object_for_notification(
            offline_char,
            Some(&synth_sess),
            trade_service,
            82_000 + idx as i64,
        )
        .expect("synthetic session_id=0 binding within cap");
    }
    let synth_err = ctx
        .bind_sub_object_for_notification(offline_char, Some(&synth_sess), trade_service, 89_999)
        .expect_err("synthetic session_id=0 cap must fail cleanly");
    assert_eq!(synth_err.code, "TooManyBoundSubObjects");
}

#[tokio::test]
async fn cleanup_disconnected_client_session_preserves_notifications_when_live_session_owns_char() {
    let ctx = test_ctx();
    let uid = 1_u64;
    let char_id = 90_000_001_i64;
    let s1_id = 91_101_u64;
    let s2_id = 91_102_u64;

    ctx.db
        .acquire_online_lock(uid, s1_id, Some(char_id), true, ctx.now_unix())
        .expect("s1 acquires initial online lock");
    ctx.db
        .acquire_online_lock(uid, s2_id, Some(char_id), true, ctx.now_unix())
        .expect("s2 displaces s1 and owns char_id");

    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnAccountChange",
        "charid",
        vec![
            PyRep::ascii_bytes("cash"),
            PyRep::Integer(1000),
            PyRep::Real(42.0),
        ],
    );

    let mut displaced_s1 = SessionState::new(s1_id);
    displaced_s1.user_id = Some(uid);
    displaced_s1.character_id = Some(char_id);
    ctx.cleanup_disconnected_client_session(&mut displaced_s1)
        .await;

    let remaining = ctx.drain_notifications_for_char(char_id);
    assert_eq!(
        remaining.len(),
        1,
        "cleanup of displaced s1 must preserve live s2 pending notifications"
    );
    assert_eq!(remaining[0].event, "OnAccountChange");
}

#[tokio::test]
async fn session_cleanup_guard_drop_runs_deferred_logoff_after_live_displacement() {
    let ctx = Arc::new(test_ctx());
    let uid = 1_u64;
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let sys_id = 30_000_142_u32;
    let s1_id = 91_201_u64;
    let s2_id = 91_202_u64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = i64::from(sys_id);
        }
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.spawn_ball(eve_destiny::Ball {
        id: ship_id,
        type_id: 606,
        position: eve_destiny::DVec3::new(100_000.0, 0.0, 0.0),
        velocity: eve_destiny::DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    })
    .await
    .expect("spawn ball");

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    ctx.register_session_closer(s1_id, tx);
    ctx.db
        .acquire_online_lock(uid, s1_id, Some(char_id), true, ctx.now_unix())
        .expect("s1 acquires online lock");
    let displaced = ctx
        .db
        .acquire_online_lock(uid, s2_id, None, true, ctx.now_unix())
        .expect("s2 displaces s1")
        .expect("displaced record");
    ctx.disconnect_displaced_session(s2_id, &displaced);
    assert_eq!(rx.try_recv().ok().map(|(c, _)| c), Some("ACL_DISPLACED".into()));
    assert!(
        !crate::sp6_lifecycle::has_active_disconnected_ship(&ctx.db, char_id),
        "disconnect_displaced_session must defer logoff while live closer received ACL_DISPLACED"
    );

    // Simulate S1 panic/abort unwinding SessionCleanupGuard without cleanup_disconnected_client_session.
    let guard = SessionCleanupGuard::new(Arc::clone(&ctx), s1_id);
    drop(guard);

    assert!(
        crate::sp6_lifecycle::has_active_disconnected_ship(&ctx.db, char_id),
        "SessionCleanupGuard::drop must execute deferred logoff_character_sync after live displacement"
    );

    // Normal cleanup clears the obligation so a subsequent guard drop never re-logs off a later session.
    ctx.db.with_mut_snapshot(|snap| {
        snap.sp6_lifecycle.disconnected_ships.remove(&char_id);
    });
    let s3_id = 91_203_u64;
    let s1_b_id = 91_204_u64;
    let (tx3, _rx3) = tokio::sync::mpsc::unbounded_channel();
    ctx.register_session_closer(s1_b_id, tx3);
    ctx.db
        .acquire_online_lock(uid, s1_b_id, Some(char_id), true, ctx.now_unix())
        .expect("s1_b acquires online lock");
    let displaced2 = ctx
        .db
        .acquire_online_lock(uid, s3_id, None, true, ctx.now_unix())
        .expect("s3 displaces s1_b")
        .expect("displaced2");
    ctx.disconnect_displaced_session(s3_id, &displaced2);

    let guard_normal = SessionCleanupGuard::new(Arc::clone(&ctx), s1_b_id);
    let mut sess1 = SessionState::new(s1_b_id);
    sess1.user_id = Some(uid);
    sess1.character_id = Some(char_id);
    ctx.cleanup_disconnected_client_session(&mut sess1).await;
    assert!(crate::sp6_lifecycle::has_active_disconnected_ship(&ctx.db, char_id));
    ctx.db.with_mut_snapshot(|snap| {
        snap.sp6_lifecycle.disconnected_ships.remove(&char_id);
    });
    drop(guard_normal);
    assert!(
        !crate::sp6_lifecycle::has_active_disconnected_ship(&ctx.db, char_id),
        "drop after cleanup_disconnected_client_session must not double-trigger logoff"
    );
}

#[tokio::test]
async fn disconnect_displaced_session_headless_runs_logoff_and_clears_pending_obligation() {
    let ctx = Arc::new(test_ctx());
    let uid = 1_u64;
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let sys_id = 30_000_142_u32;
    let s1_id = 91_211_u64;
    let s2_id = 91_212_u64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = i64::from(sys_id);
        }
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.spawn_ball(eve_destiny::Ball {
        id: ship_id,
        type_id: 606,
        position: eve_destiny::DVec3::new(100_000.0, 0.0, 0.0),
        velocity: eve_destiny::DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    })
    .await
    .expect("spawn ball");

    ctx.db
        .acquire_online_lock(uid, s1_id, Some(char_id), true, ctx.now_unix())
        .expect("s1 acquires online lock");
    let displaced = ctx
        .db
        .acquire_online_lock(uid, s2_id, None, true, ctx.now_unix())
        .expect("s2 displaces s1")
        .expect("displaced record");

    // No live closer is registered for s1 (!sent_to_live).
    ctx.disconnect_displaced_session(s2_id, &displaced);

    assert!(
        crate::sp6_lifecycle::has_active_disconnected_ship(&ctx.db, char_id),
        "headless displacement (!sent_to_live) must synchronously trigger logoff_character_sync"
    );
    assert_eq!(
        ctx.take_pending_displaced_logoff(s1_id),
        None,
        "headless displacement (!sent_to_live) must consume and clear pending_displaced_logoffs"
    );
}

#[tokio::test]
async fn disconnect_displaced_session_records_pending_logoff_before_concurrent_guard_unwind() {
    let ctx = Arc::new(test_ctx());
    let uid = 1_u64;
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let sys_id = 30_000_142_u32;
    let s1_id = 91_221_u64;
    let s2_id = 91_222_u64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = i64::from(sys_id);
        }
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.spawn_ball(eve_destiny::Ball {
        id: ship_id,
        type_id: 606,
        position: eve_destiny::DVec3::new(100_000.0, 0.0, 0.0),
        velocity: eve_destiny::DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    })
    .await
    .expect("spawn ball");

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    ctx.register_session_closer(s1_id, tx);
    ctx.db
        .acquire_online_lock(uid, s1_id, Some(char_id), true, ctx.now_unix())
        .expect("s1 acquires online lock");

    let ctx_task = Arc::clone(&ctx);
    let unwinder = tokio::spawn(async move {
        let guard = SessionCleanupGuard::new(ctx_task, s1_id);
        let msg = rx.recv().await.expect("close signal");
        assert_eq!(msg.0, "ACL_DISPLACED");
        drop(guard);
    });
    tokio::task::yield_now().await;

    let displaced = ctx
        .db
        .acquire_online_lock(uid, s2_id, None, true, ctx.now_unix())
        .expect("s2 displaces s1")
        .expect("displaced record");
    ctx.disconnect_displaced_session(s2_id, &displaced);
    unwinder.await.expect("unwinder task ok");

    assert!(
        crate::sp6_lifecycle::has_active_disconnected_ship(&ctx.db, char_id),
        "fast-unwinding guard woken immediately by ACL_DISPLACED must see pre-recorded pending logoff"
    );
    assert_eq!(
        ctx.take_pending_displaced_logoff(s1_id),
        None,
        "unwound guard must consume pending_displaced_logoffs entry cleanly"
    );
}

#[tokio::test]
async fn displaced_docked_s1_delayed_cleanup_after_s2_relocation_does_not_overwrite_location() {
    let ctx = Arc::new(test_ctx());
    let uid = 1_u64;
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let struct_a = 1_000_000_050_001_i64;
    let struct_b = 1_000_000_050_002_i64;
    let s1_id = 91_231_u64;
    let s2_id = 91_232_u64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = uid;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = Some(struct_a);
        }
    });
    ctx.db
        .acquire_online_lock(uid, s1_id, Some(char_id), true, ctx.now_unix())
        .expect("S1 lock");

    let mut sess_s1 = SessionState::new(s1_id);
    sess_s1.user_id = Some(uid);
    sess_s1.character_id = Some(char_id);
    sess_s1.solar_system_id = Some(sys_id);
    sess_s1.structure_id = Some(struct_a);

    // S2 displaces S1, selects char_id (clearing pending_displaced_logoffs), moves char_id to struct_b, and logs off.
    let displaced = ctx
        .db
        .acquire_online_lock(uid, s2_id, None, true, ctx.now_unix())
        .expect("S2 displaces S1")
        .expect("displaced S1");
    ctx.disconnect_displaced_session(s2_id, &displaced);
    ctx.db
        .acquire_online_lock(uid, s2_id, Some(char_id), false, ctx.now_unix())
        .expect("S2 selects char_id");
    ctx.clear_pending_displaced_logoffs_for_char(uid, char_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.structure_id = Some(struct_b);
            ch.station_id = None;
        }
    });
    let mut sess_s2 = SessionState::new(s2_id);
    sess_s2.user_id = Some(uid);
    sess_s2.character_id = Some(char_id);
    sess_s2.solar_system_id = Some(sys_id);
    sess_s2.structure_id = Some(struct_b);
    let emitted = ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnAccountChange",
        "charid",
        vec![],
    );
    assert_eq!(emitted.len(), 1, "online S2 must produce 1 notification");
    ctx.cleanup_disconnected_client_session(&mut sess_s2).await;

    // Re-queue a post-logoff notification (e.g. shutdown/maintenance notice) after S2 logs off;
    // delayed S1 cleanup must recognize its pre-release displacement revocation (`BF-R6-01`)
    // and skip BOTH `logoff_character_sync` and `clear_notifications_for_char`.
    ctx.restore_pending_notifications(emitted);
    assert!(
        ctx.latest_notification_seq_for_char(char_id) > 0,
        "precondition: post-S2 notification must be queued before delayed S1 cleanup"
    );

    // Delayed cleanup of displaced S1 (still carrying stale `structure_id = struct_a`) must NOT overwrite struct_b.
    ctx.cleanup_disconnected_client_session(&mut sess_s1).await;
    ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        assert_eq!(
            ch.structure_id,
            Some(struct_b),
            "displaced S1 delayed cleanup must not overwrite S2's docked structure_id back to struct_a"
        );
    });
    assert_eq!(
        ctx.drain_notifications_for_char(char_id).len(),
        1,
        "delayed cleanup of displaced S1 after S2 logoff must skip clear_notifications_for_char and logoff_character_sync"
    );
}

#[cfg(test)]
#[path = "binding_auth_unit_tests.rs"]
mod binding_auth_unit_tests;

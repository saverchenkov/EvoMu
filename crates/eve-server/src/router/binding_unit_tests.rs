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

#[cfg(test)]
#[path = "binding_auth_unit_tests.rs"]
mod binding_auth_unit_tests;

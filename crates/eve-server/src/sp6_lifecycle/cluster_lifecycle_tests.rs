//! Unit tests for space object persistence reload, logoff/disconnect ticks, Abyssal collapse, and cluster tick.

use super::{
    collapse_stranded_abyssal_if_needed_sync, on_advance_secs, populate_persisted_space_objects,
    seed_persisted_abandoned_drones, tick_background_cluster,
};
use crate::router::ServerContext;
use crate::services::combat_crimewatch::with_combat_runtime;
use crate::sp6_lifecycle::{
    record_anchor_meta_in_snap, record_wreck_or_container_meta_in_snap, with_mut_sp6_state,
    with_sp6_state, AbyssalPersistedSession, DisconnectedShipEntry, SafeLogoffEntry,
    SelfDestructPersistedEntry,
};
use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_sde::SdeRegistry;
use glam::DVec3;
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
fn populate_persisted_space_objects_restores_structures_bubbles_wrecks_and_disconnected_ships() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let now = ctx.now_unix();
    let structure_id = 1_050_000_001;
    let fresh_wreck = 1_050_000_010;
    let expired_wreck = 1_050_000_011;
    let fresh_jetcan = 1_050_000_012;
    let expired_jetcan = 1_050_000_013;
    let fresh_corpse = 1_050_000_014;
    let active_bubble = 1_050_000_015;
    let pending_bubble = 1_050_000_016;
    let expired_bubble = 1_050_000_017;
    let abandoned_drone = 1_050_000_020;
    let active_disc_ship = 1_050_000_030;
    let expired_disc_ship = 1_050_000_031;

    let mwd_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 361)
        .map(|t| t.type_id)
        .unwrap_or(22_782);

    with_mut_sp6_state(&ctx.db, |st, snap| {
        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                type_id: 35832,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                name: "Test Astrahus".into(),
                position: [50_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 500_000.0,
                armor_hp: 500_000.0,
                hull_hp: 500_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );

        let items_to_insert = [
            (fresh_wreck, 26557, 90_000_001, InventoryFlag::None),
            (expired_wreck, 26557, 90_000_001, InventoryFlag::None),
            (fresh_jetcan, 23, 90_000_001, InventoryFlag::None),
            (expired_jetcan, 23, 90_000_001, InventoryFlag::None),
            (fresh_corpse, 25, 90_000_001, InventoryFlag::None),
            (active_bubble, mwd_tid, 90_000_001, InventoryFlag::None),
            (pending_bubble, mwd_tid, 90_000_001, InventoryFlag::None),
            (expired_bubble, mwd_tid, 90_000_001, InventoryFlag::None),
            (abandoned_drone, 2454, 90_000_001, InventoryFlag::None),
            (active_disc_ship, 587, 90_000_001, InventoryFlag::None),
            (expired_disc_ship, 587, 90_000_002, InventoryFlag::None),
        ];
        for (iid, tid, owner, flag) in items_to_insert {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: owner,
                    location_id: sys_id as i64,
                    flag,
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
            snap.item_positions.insert(iid, [1_000.0, 2_000.0, 3_000.0]);
        }

        // Write into st directly (since with_mut_sp6_state overwrites snap.sp6_lifecycle with st)
        let _ = snap;
        st.space_object_meta.entry(fresh_wreck).or_default().created_unix = Some(now.saturating_sub(100));
        st.space_object_meta.entry(expired_wreck).or_default().created_unix = Some(now.saturating_sub(8_000));
        st.space_object_meta.entry(fresh_jetcan).or_default().created_unix = Some(now.saturating_sub(100));
        st.space_object_meta.entry(expired_jetcan).or_default().created_unix = Some(now.saturating_sub(4_000));
        st.space_object_meta.entry(fresh_corpse).or_default().created_unix = Some(now.saturating_sub(100));

        let m_act = st.space_object_meta.entry(active_bubble).or_default();
        m_act.anchor_start_unix = Some(now.saturating_sub(120));
        m_act.anchoring_delay_secs = 60.0;
        m_act.bubble_radius_m = Some(20_000.0);

        let m_pend = st.space_object_meta.entry(pending_bubble).or_default();
        m_pend.anchor_start_unix = Some(now.saturating_sub(10));
        m_pend.anchoring_delay_secs = 60.0;
        m_pend.bubble_radius_m = Some(20_000.0);

        let m_exp = st.space_object_meta.entry(expired_bubble).or_default();
        m_exp.anchor_start_unix = Some(now.saturating_sub(300_000));
        m_exp.anchoring_delay_secs = 60.0;

        st.disconnected_ships.insert(
            90_000_001,
            DisconnectedShipEntry {
                char_id: 90_000_001,
                ship_id: active_disc_ship,
                ship_type_id: 587,
                solar_system_id: sys_id,
                last_position: [0.0, 0.0, 0.0],
                emergency_warp_dest: Some([10_000.0, 0.0, 0.0]),
                disconnected_at_unix: now.saturating_sub(10),
                remove_at_unix: now + 50,
                removed_from_space: false,
                pre_warp_position: None,
            },
        );
        st.disconnected_ships.insert(
            90_000_002,
            DisconnectedShipEntry {
                char_id: 90_000_002,
                ship_id: expired_disc_ship,
                ship_type_id: 587,
                solar_system_id: sys_id,
                last_position: [500.0, 0.0, 0.0],
                emergency_warp_dest: Some([25_000.0, 0.0, 0.0]),
                disconnected_at_unix: now.saturating_sub(100),
                remove_at_unix: now,
                removed_from_space: false,
                pre_warp_position: None,
            },
        );
    });

    let mut bp = eve_destiny::Ballpark::new(sys_id);
    let pending = populate_persisted_space_objects(&mut bp, sys_id, &ctx.sde, &ctx.db);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, pending_bubble);

    assert!(bp.balls.contains_key(&structure_id));
    assert!(bp.balls.contains_key(&fresh_wreck));
    assert!(!bp.balls.contains_key(&expired_wreck));
    assert!(bp.balls.contains_key(&fresh_jetcan));
    assert!(!bp.balls.contains_key(&expired_jetcan));
    assert!(bp.balls.contains_key(&fresh_corpse));
    assert!(bp.balls.contains_key(&active_bubble));
    assert!(!bp.balls.contains_key(&expired_bubble));
    assert!(bp.balls.contains_key(&abandoned_drone));
    assert!(bp.balls.contains_key(&active_disc_ship));
    assert!(!bp.balls.contains_key(&expired_disc_ship));

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&expired_wreck));
        assert!(!snap.items.contains_key(&expired_jetcan));
        assert!(!snap.items.contains_key(&expired_bubble));
    });
    with_sp6_state(&ctx.db, |st| {
        assert!(st.disconnected_ships[&90_000_002].removed_from_space);
    });

    // Also test seed_persisted_abandoned_drones for both drones (cat 18) and fighters (cat 87)
    let abandoned_fighter = 1_050_000_040;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            abandoned_fighter,
            InventoryItemRecord {
                item_id: abandoned_fighter,
                type_id: 23057,
                owner_id: 90_000_001,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 9,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    seed_persisted_abandoned_drones(&ctx);
    with_combat_runtime(&ctx, |rt| {
        assert!(rt.active_drones_in_space[&abandoned_drone].is_abandoned);
    });
    assert!(ctx.active_fighters.read().unwrap()[&abandoned_fighter].is_abandoned);
}

#[tokio::test]
async fn on_advance_secs_safe_logoff_disconnect_warp_self_destruct_and_abyssal_collapse() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let base_now = ctx.now_unix();
    let safe_ship = 1_060_000_001;
    let abort_ship = 1_060_000_002;
    let warp_ship = 1_060_000_003;
    let sd_ship = 1_060_000_004;

    ctx.db.with_mut_snapshot(|snap| {
        for (sid, owner) in [
            (safe_ship, 90_000_001),
            (abort_ship, 90_000_002),
            (warp_ship, 90_000_002),
            (sd_ship, 90_000_002),
        ] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
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
        snap.zarzakh_gate_locks.insert(90_000_001, (500_000_001, base_now));
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = sd_ship;
        }
        record_wreck_or_container_meta_in_snap(snap, 999, base_now, false);
        record_anchor_meta_in_snap(snap, 998, base_now, 5.0, Some(20_000.0));
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (idx, sid) in [safe_ship, abort_ship, warp_ship, sd_ship].into_iter().enumerate() {
        sys_handle.try_spawn_ball_with_combat(
            Ball {
                id: sid,
                type_id: 587,
                position: DVec3::new((idx as f64 + 1.0) * 10_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_100_000.0,
                radius: 40.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        );
    }

    // 90_000_002 has an active weapons timer -> safe logoff aborts; 90_000_001 completes safe logoff
    super::super::record_npc_aggression_timer(&ctx, 90_000_002, base_now);

    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.safe_logoffs.insert(
            90_000_001,
            SafeLogoffEntry {
                char_id: 90_000_001,
                ship_id: safe_ship,
                solar_system_id: sys_id,
                started_unix: base_now,
                complete_at_unix: base_now + 10,
                completed: false,
            },
        );
        st.safe_logoffs.insert(
            90_000_002,
            SafeLogoffEntry {
                char_id: 90_000_002,
                ship_id: abort_ship,
                solar_system_id: sys_id,
                started_unix: base_now,
                complete_at_unix: base_now + 10,
                completed: false,
            },
        );
        st.disconnected_ships.insert(
            90_000_002,
            DisconnectedShipEntry {
                char_id: 90_000_002,
                ship_id: warp_ship,
                ship_type_id: 587,
                solar_system_id: sys_id,
                last_position: [30_000.0, 0.0, 0.0],
                emergency_warp_dest: Some([500_000_000.0, 0.0, 0.0]),
                disconnected_at_unix: base_now,
                remove_at_unix: base_now + 60,
                removed_from_space: false,
                pre_warp_position: None,
            },
        );
        st.self_destructs.insert(
            sd_ship,
            SelfDestructPersistedEntry {
                ship_id: sd_ship,
                char_id: 90_000_002,
                solar_system_id: sys_id,
                started_unix: base_now,
                detonate_at_unix: base_now + 10,
            },
        );
    });

    ctx.advance_secs(12);
    on_advance_secs(&ctx, base_now, 12);

    with_sp6_state(&ctx.db, |st| {
        assert!(st.safe_logoffs[&90_000_001].completed);
        assert!(!st.safe_logoffs.contains_key(&90_000_002));
        assert!(!st.self_destructs.contains_key(&sd_ship));
    });
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&sd_ship)));
    assert!(ctx.db.with_snapshot(|s| s.zarzakh_gate_locks.is_empty()));

    // Abyssal session collapse + full background cluster tick
    let ab_now = ctx.now_unix();
    let ab_ship = 1_060_000_099;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ab_ship,
            InventoryItemRecord {
                item_id: ab_ship,
                type_id: 587,
                owner_id: 90_000_001,
                location_id: 32_000_001,
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
        if let Some(c) = snap.characters.get_mut(&90_000_001) {
            c.solar_system_id = 32_000_001;
            c.station_id = None;
            c.structure_id = None;
            c.active_ship_id = ab_ship;
        }
    });
    with_mut_sp6_state(&ctx.db, |st, _| {
        st.abyssal_sessions.insert(
            90_000_001,
            AbyssalPersistedSession {
                char_id: 90_000_001,
                ship_id: ab_ship,
                pocket_system_id: 32_000_001,
                origin_system_id: sys_id,
                entered_unix: ab_now.saturating_sub(1_200),
                collapse_at_unix: ab_now,
            },
        );
    });
    collapse_stranded_abyssal_if_needed_sync(&ctx, 90_000_001, ab_now);
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&ab_ship)));

    tick_background_cluster(&ctx).await;
}

#[tokio::test]
async fn restarted_structures_belt_asteroids_wrecks_drones_and_specialized_npcs_encode_canonical_flags_and_slim_meta() {
    let ctx = test_ctx();
    let perim_sys = 30_000_144;

    // 1. Populated SDE asteroid belts + persisted Upwell structure are `MovementMode::Rigid` static balls,
    //    keeping `has_dynamic_balls() == false` so empty systems hibernate at LOD 0.
    let structure_id = 1_070_000_001;
    with_mut_sp6_state(&ctx.db, |_, snap| {
        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                type_id: 35_832,
                owner_corp_id: 98_000_001,
                solar_system_id: perim_sys,
                name: "Perimeter Keep-Hibernating Astrahus".into(),
                position: [120_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 500_000.0,
                armor_hp: 500_000.0,
                hull_hp: 500_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });

    let mut hib_bp = eve_destiny::Ballpark::new(perim_sys);
    crate::system_actor::populate_system_celestials(&mut hib_bp, perim_sys, &ctx.sde);
    let belt_asts =
        crate::system_actor::populate_asteroid_belt_entities(&mut hib_bp, perim_sys, &ctx.sde);
    assert!(!belt_asts.is_empty(), "Perimeter has SDE belt asteroids");
    let _ = populate_persisted_space_objects(&mut hib_bp, perim_sys, &ctx.sde, &ctx.db);

    assert_eq!(hib_bp.balls[&structure_id].mode, MovementMode::Rigid);
    assert!(hib_bp.is_static_ball(structure_id));
    assert!(hib_bp.combat_states.contains_key(&structure_id));
    for ast in &belt_asts {
        assert_eq!(hib_bp.balls[&ast.asteroid_id].mode, MovementMode::Rigid);
        assert!(hib_bp.is_static_ball(ast.asteroid_id));
    }
    assert!(
        !hib_bp.has_dynamic_balls(),
        "empty system with celestials, belt asteroids, and restarted Upwell structures must stay at LOD 0"
    );

    // 2. Persisted wrecks and jetcans are pinned non-free (`!BALL_FLAG_IS_FREE`), whereas abandoned drones stay free.
    let now = ctx.now_unix();
    let wreck_id = 1_070_000_010;
    let jetcan_id = 1_070_000_011;
    let drone_id = 1_070_000_012;
    with_mut_sp6_state(&ctx.db, |st, snap| {
        for (iid, tid) in [(wreck_id, 26_557), (jetcan_id, 23), (drone_id, 2_454)] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: 90_000_001,
                    location_id: perim_sys as i64,
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
            snap.item_positions.insert(iid, [2_000.0, 0.0, 0.0]);
        }
        st.space_object_meta.entry(wreck_id).or_default().created_unix = Some(now);
        st.space_object_meta.entry(jetcan_id).or_default().created_unix = Some(now);
    });
    let _ = populate_persisted_space_objects(&mut hib_bp, perim_sys, &ctx.sde, &ctx.db);
    assert!(!hib_bp.is_ball_free(wreck_id));
    assert!(!hib_bp.is_ball_free(jetcan_id));
    assert!(hib_bp.is_ball_free(drone_id));

    let raw = eve_destiny::wire::encode_add_balls_with_park(
        1,
        1,
        &[&hib_bp.balls[&wreck_id], &hib_bp.balls[&jetcan_id], &hib_bp.balls[&drone_id]],
        Some(&hib_bp),
    );
    let (_, _, dec) = eve_destiny::wire::decode_add_balls(&raw).expect("decode persisted balls");
    assert_eq!(dec[0].flags & eve_destiny::wire::BALL_FLAG_IS_FREE, 0);
    assert_eq!(dec[1].flags & eve_destiny::wire::BALL_FLAG_IS_FREE, 0);
    assert_ne!(dec[2].flags & eve_destiny::wire::BALL_FLAG_IS_FREE, 0);

    // 3. Player launched drone (`DroneController::launch_drones`) encodes `categoryID == 18`, SDE `groupID`,
    //    `ownerID == char_id`, and `corpID == pilot_corp_id`.
    let char_id = 90_000_001;
    let pilot_corp = ctx.db.get_character(char_id).unwrap().corporation_id;
    let ship_id = 1_070_000_050;
    let bay_drone_id = 1_070_000_051;
    let hobgoblin_tid = 2_454u32;
    let expected_drone_grp = ctx.sde.types.get(&hobgoblin_tid).map(|t| t.group_id).unwrap_or(100);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 645, // Dominix (ample drone bandwidth)
                owner_id: char_id,
                location_id: perim_sys as i64,
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
        snap.items.insert(
            bay_drone_id,
            InventoryItemRecord {
                item_id: bay_drone_id,
                type_id: hobgoblin_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
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
    });
    let sys_h = ctx.systems.get_or_spawn(perim_sys);
    sys_h
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 645,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 140.0,
            inertia: 5.0,
            mass: 97_000_000.0,
            radius: 300.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();

    let mut dc = crate::combat::drones_salvage_tractor::DroneController::new();
    let launched = dc
        .launch_drones(&ctx, perim_sys, char_id, ship_id, &[bay_drone_id])
        .await
        .expect("launch drone");
    let launched_bid = launched[0];
    let drone_slim = sys_h
        .get_ball_slim_item(launched_bid)
        .await
        .unwrap()
        .expect("drone slim item");
    assert_eq!(drone_slim.dict_get("categoryID"), Some(&eve_proto::PyRep::Integer(18)));
    assert_eq!(
        drone_slim.dict_get("groupID"),
        Some(&eve_proto::PyRep::Integer(expected_drone_grp as i64))
    );
    assert_eq!(drone_slim.dict_get("ownerID"), Some(&eve_proto::PyRep::Integer(char_id)));
    assert_eq!(drone_slim.dict_get("corpID"), Some(&eve_proto::PyRep::Integer(pilot_corp)));

    // 4. Specialized NPC (`spawn_specialized_npc_entity`) encodes `categoryID == 11`, SDE `groupID`,
    //    faction `ownerID`, and V24 `hostile_response_threshold` / `friendly_response_threshold`.
    let npc_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 11)
        .map(|t| t.type_id)
        .unwrap_or(10_029);
    let expected_npc_grp = ctx.sde.types.get(&npc_tid).map(|t| t.group_id).unwrap_or(546);
    let npc_bid = crate::services::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_entity(
        &ctx,
        perim_sys,
        npc_tid,
        "",
        1,
        DVec3::new(15_000.0, 0.0, 0.0),
    )
    .await;
    let npc_slim = sys_h
        .get_ball_slim_item(npc_bid)
        .await
        .unwrap()
        .expect("npc slim item");
    assert_eq!(npc_slim.dict_get("categoryID"), Some(&eve_proto::PyRep::Integer(11)));
    assert_eq!(
        npc_slim.dict_get("groupID"),
        Some(&eve_proto::PyRep::Integer(expected_npc_grp as i64))
    );
    let npc_owner = npc_slim
        .dict_get("ownerID")
        .and_then(|r| r.as_i64())
        .unwrap_or(0);
    assert!(npc_owner >= 500_000, "NPC ownerID should be a valid NPC faction, got {npc_owner}");
    assert!(npc_slim.dict_get("hostile_response_threshold").is_some());
    assert!(npc_slim.dict_get("friendly_response_threshold").is_some());
}

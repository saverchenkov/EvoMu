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

#[test]
fn cosmic_anomaly_and_signature_sweep_respects_exemptions_and_exact_expiry() {
    use eve_db::{CosmicAnomalyRecord, CosmicSignatureRecord};

    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let now = 1_794_300_000_u64;

    let mk_anom = |id: &str, typ: &str, created: u64, life: u64, cleared: bool| CosmicAnomalyRecord {
        anomaly_id: id.into(),
        solar_system_id: sys_id,
        name: id.into(),
        anomaly_type: typ.into(),
        position: [0.0; 3],
        signal_strength: 1.0,
        ore_types: Vec::new(),
        remaining_volume: 100.0,
        created_unix: created,
        lifetime_seconds: life,
        cleared,
    };
    let mk_sig = |id: &str, grp: &str, created: u64, life: u64, collapsed: bool| CosmicSignatureRecord {
        signature_id: id.into(),
        solar_system_id: sys_id,
        sig_group: grp.into(),
        type_name: grp.into(),
        position: [0.0; 3],
        signal_strength: 0.25,
        wormhole_type_id: None,
        destination_system_id: None,
        created_unix: created,
        lifetime_seconds: life,
        max_mass_kg: 0.0,
        remaining_mass_kg: 0.0,
        max_jump_mass_kg: 0.0,
        collapsed,
    };

    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies.insert(
            sys_id,
            vec![
                mk_anom("A-ICE", "IceBelt", now - 1000, 10, false),       // exempt type
                mk_anom("A-ORE", "OreAnomaly", now - 1000, 10, false),    // exempt type
                mk_anom("A-ZERO", "Combat", now - 1000, 0, false),        // lifetime 0 kept
                mk_anom("A-ALIVE", "Combat", now - 9, 10, false),         // now < created+life (9 < 10) kept
                mk_anom("A-EXACT", "Combat", now - 10, 10, false),        // now == created+life swept!
                mk_anom("A-CLEARED", "Combat", now - 10, 10, true),       // cleared swept silently
            ],
        );
        snap.cosmic_signatures.insert(
            sys_id,
            vec![
                mk_sig("S-WH", "Wormhole", now - 1000, 10, false),        // exempt case-insensitive wormhole
                mk_sig("S-ICE", "IceBelt", now - 1000, 10, false),        // exempt
                mk_sig("S-OA", "OreAnomaly", now - 1000, 10, false),      // exempt
                mk_sig("S-ORE", "Ore", now - 1000, 10, false),            // exempt
                mk_sig("S-GAS", "Gas", now - 1000, 10, false),            // exempt
                mk_sig("S-ZERO", "Data", now - 1000, 0, false),           // lifetime 0 kept
                mk_sig("S-ALIVE", "Data", now - 9, 10, false),            // now < created+life kept
                mk_sig("S-EXACT", "Combat", now - 10, 10, false),         // now == created+life swept!
                mk_sig("S-COL", "Relic", now - 10, 10, true),             // collapsed swept silently
            ],
        );
    });

    super::sweep_expired_cosmic_anomalies_and_signatures(&ctx, now);

    ctx.db.with_snapshot(|snap| {
        let a_ids: Vec<&str> = snap.cosmic_anomalies[&sys_id]
            .iter()
            .map(|a| a.anomaly_id.as_str())
            .collect();
        assert_eq!(a_ids, vec!["A-ICE", "A-ORE", "A-ZERO", "A-ALIVE"]);

        let s_ids: Vec<&str> = snap.cosmic_signatures[&sys_id]
            .iter()
            .map(|s| s.signature_id.as_str())
            .collect();
        assert_eq!(
            s_ids,
            vec!["S-WH", "S-ICE", "S-OA", "S-ORE", "S-GAS", "S-ZERO", "S-ALIVE"]
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_advance_secs_internal_sweeps_crimewatch_timers_abyssal_traces_and_ticks() {
    use crate::combat::CharacterCrimeWatchState;
    use crate::services::pve_special_sites::AbyssalTraceRecord;
    use eve_db::KillRightRecord;
    use crate::services::pve_special_sites_abyssal::with_abyssal_state;
    use eve_db::PersistedAggressionRecord;

    let ctx = test_ctx();
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_776_300_000)));
    let now = ctx.now_unix();
    let p_weap = 91_800_001_i64;
    let p_pvp = 91_800_002_i64;
    let p_susp = 91_800_003_i64;
    let p_crim = 91_800_004_i64;
    let p_concord_keep = 91_800_005_i64;
    let p_alive = 91_800_006_i64;
    let p_dis = 91_800_007_i64;
    let p_npc = 91_800_008_i64;

    ctx.db.with_mut_snapshot(|snap| {
        // DB crimewatch_states + crimewatch_aggressions + kill_rights
        snap.crimewatch_states.insert(
            p_weap,
            CharacterCrimeWatchState {
                character_id: p_weap,
                weapon_timer_until_unix: now,
                ..Default::default()
            },
        );
        snap.crimewatch_states.insert(
            p_pvp,
            CharacterCrimeWatchState {
                character_id: p_pvp,
                pvp_timer_until_unix: now,
                ..Default::default()
            },
        );
        snap.crimewatch_states.insert(
            p_susp,
            CharacterCrimeWatchState {
                character_id: p_susp,
                suspect_flag_until_unix: now,
                ..Default::default()
            },
        );
        snap.crimewatch_states.insert(
            p_crim,
            CharacterCrimeWatchState {
                character_id: p_crim,
                criminal_flag_until_unix: now,
                ..Default::default()
            },
        );
        snap.crimewatch_states.insert(
            p_alive,
            CharacterCrimeWatchState {
                character_id: p_alive,
                weapon_timer_until_unix: now + 1,
                ..Default::default()
            },
        );
        snap.crimewatch_aggressions.insert(
            (p_weap, p_pvp),
            PersistedAggressionRecord {
                aggressor_char_id: p_weap,
                victim_char_id: p_pvp,
                system_id: 30_000_142,
                started_unix: now.saturating_sub(100),
                expires_unix: now, // boundary `<= now`
                is_legal: false,
                is_pod: false,
            },
        );
        snap.kill_rights.insert(
            77_001,
            KillRightRecord {
                kill_right_id: 77_001,
                owner_id: p_weap,
                target_id: p_crim,
                activation_fee: eve_db::IskCents(0),
                is_active_suspect: false,
            },
        );
        snap.kill_rights.insert(
            77_002,
            KillRightRecord {
                kill_right_id: 77_002,
                owner_id: p_weap,
                target_id: p_alive,
                activation_fee: eve_db::IskCents(0),
                is_active_suspect: false,
            },
        );
    });

    ctx.crimewatch.with_mut_crimewatch(|cw| {
        for (pid, st) in [
            (
                p_weap,
                CharacterCrimeWatchState {
                    character_id: p_weap,
                    weapon_timer_until_unix: now,
                    ..Default::default()
                },
            ),
            (
                p_pvp,
                CharacterCrimeWatchState {
                    character_id: p_pvp,
                    pvp_timer_until_unix: now,
                    ..Default::default()
                },
            ),
            (
                p_susp,
                CharacterCrimeWatchState {
                    character_id: p_susp,
                    suspect_flag_until_unix: now,
                    ..Default::default()
                },
            ),
            (
                p_crim,
                CharacterCrimeWatchState {
                    character_id: p_crim,
                    criminal_flag_until_unix: now,
                    ..Default::default()
                },
            ),
            (
                p_concord_keep,
                CharacterCrimeWatchState {
                    character_id: p_concord_keep,
                    criminal_flag_until_unix: now,
                    pending_concord_arrival: Some((30_000_142, now + 30, 555, p_pvp)),
                    ..Default::default()
                },
            ),
            (
                p_alive,
                CharacterCrimeWatchState {
                    character_id: p_alive,
                    weapon_timer_until_unix: now + 1,
                    pvp_timer_until_unix: now + 1,
                    suspect_flag_until_unix: now + 1,
                    criminal_flag_until_unix: now + 1,
                    pending_concord_arrival: None,
                },
            ),
        ] {
            cw.states.insert(pid, st);
        }
        cw.aggression_mgr.records.insert(
            (p_weap, p_pvp),
            PersistedAggressionRecord {
                aggressor_char_id: p_weap,
                victim_char_id: p_pvp,
                system_id: 30_000_142,
                started_unix: now.saturating_sub(10),
                expires_unix: now,
                is_legal: false,
                is_pod: false,
            },
        );
        cw.aggression_mgr.records.insert(
            (p_alive, p_pvp),
            PersistedAggressionRecord {
                aggressor_char_id: p_alive,
                victim_char_id: p_pvp,
                system_id: 30_000_142,
                started_unix: now.saturating_sub(10),
                expires_unix: now + 1,
                is_legal: false,
                is_pod: false,
            },
        );
    });

    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.disapproval_flags.insert(p_dis, now);
        rt.disapproval_flags.insert(p_alive, now + 1);
        rt.npc_timers.insert(p_npc, now);
        rt.npc_timers.insert(p_alive, now + 1);
        rt.duel_challenges.insert((p_weap, p_pvp), now);
        rt.duel_challenges.insert((p_alive, p_pvp), now + 1);
        rt.kill_right_expires_unix.insert(77_001, now);
        rt.kill_right_restricted_to.insert(77_001, p_crim);
        rt.kill_right_expires_unix.insert(77_002, now + 1);
        rt.limited_engagements
            .entry(p_weap)
            .or_default()
            .insert(p_pvp, now);
        rt.limited_engagements
            .entry(p_alive)
            .or_default()
            .insert(p_pvp, now + 1);
        rt.limited_engagements
            .entry(p_alive)
            .or_default()
            .insert(p_crim, now);
    });

    with_abyssal_state(&ctx, |ab| {
        for (tid, exp) in [(88_001, 0), (88_002, now + 1), (88_003, now)] {
            ab.traces.insert(
                tid,
                AbyssalTraceRecord {
                    trace_ball_id: tid,
                    char_id: p_weap,
                    filament_type_id: 47762,
                    origin_solar_system_id: 30_000_142,
                    position: [0.0; 3],
                    created_unix: now.saturating_sub(60),
                    expires_at_sec: exp,
                    max_ships: 1,
                    entered_char_ids: Vec::new(),
                    pocket_id: None,
                    fleet_id: None,
                },
            );
        }
    });

    super::on_advance_secs_internal(&ctx, now.saturating_sub(1), 1, false, false);

    ctx.crimewatch.with_crimewatch(|cw| {
        assert!(!cw.states.contains_key(&p_weap));
        assert!(!cw.states.contains_key(&p_pvp));
        assert!(!cw.states.contains_key(&p_susp));
        assert!(!cw.states.contains_key(&p_crim));
        assert!(cw.states.contains_key(&p_concord_keep));
        assert!(cw.states.contains_key(&p_alive));
        assert!(!cw.aggression_mgr.records.contains_key(&(p_weap, p_pvp)));
        assert!(cw.aggression_mgr.records.contains_key(&(p_alive, p_pvp)));
    });
    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.disapproval_flags.contains_key(&p_dis));
        assert!(rt.disapproval_flags.contains_key(&p_alive));
        assert!(!rt.npc_timers.contains_key(&p_npc));
        assert!(rt.npc_timers.contains_key(&p_alive));
        assert!(!rt.duel_challenges.contains_key(&(p_weap, p_pvp)));
        assert!(rt.duel_challenges.contains_key(&(p_alive, p_pvp)));
        assert!(!rt.kill_right_expires_unix.contains_key(&77_001));
        assert!(!rt.kill_right_restricted_to.contains_key(&77_001));
        assert!(rt.kill_right_expires_unix.contains_key(&77_002));
        assert!(!rt.limited_engagements.contains_key(&p_weap));
        assert!(rt.limited_engagements[&p_alive].contains_key(&p_pvp));
        assert!(!rt.limited_engagements[&p_alive].contains_key(&p_crim));
    });
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.kill_rights.contains_key(&77_001));
        assert!(snap.kill_rights.contains_key(&77_002));
    });
    with_abyssal_state(&ctx, |ab| {
        assert!(ab.traces.contains_key(&88_001), "expires_at_sec==0 trace kept");
        assert!(ab.traces.contains_key(&88_002), "unexpired trace kept");
        assert!(!ab.traces.contains_key(&88_003), "exact boundary trace swept");
    });

    // `tick_background_cluster_with_step(0)` is a no-op; `>= 60s` with `pve_special_sites.initialized` persists DB timestamp
    ctx.cluster_tick_last_unix
        .store(0, std::sync::atomic::Ordering::Relaxed);
    super::tick_background_cluster_with_step(&ctx, 0).await;
    assert_eq!(
        ctx.cluster_tick_last_unix
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.pve_special_sites.initialized = true;
        snap.service_state_blobs
            .insert("cluster_tick_last_unix".into(), now.saturating_sub(65).to_string());
    });
    ctx.cluster_tick_persisted_unix
        .store(now.saturating_sub(65), std::sync::atomic::Ordering::Relaxed);
    super::tick_background_cluster_with_step(&ctx, 5).await;
    assert!(
        ctx.cluster_tick_last_unix
            .load(std::sync::atomic::Ordering::Relaxed)
            >= now
    );
    assert!(
        ctx.cluster_tick_persisted_unix
            .load(std::sync::atomic::Ordering::Relaxed)
            >= now
    );

    // `step_active_systems_physics_tick` runs cleanly when dedicated physics ticker is inactive
    let _ = ctx.systems.get_or_spawn(30_000_142);
    super::step_active_systems_physics_tick(&ctx, 1).await;

    // Abyssal Void Space collapse path (`collapse_stranded_abyssal_if_needed_sync`)
    let void_char = 91_800_020_i64;
    let void_ship = 101_800_020_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = void_char;
        ch.solar_system_id = crate::services::abyssal_void_mgr::VOID_SPACE_SYSTEM_ID;
        ch.active_ship_id = void_ship;
        snap.characters.insert(void_char, ch);
        snap.items.insert(
            void_ship,
            InventoryItemRecord {
                item_id: void_ship,
                type_id: 587,
                owner_id: void_char,
                location_id: crate::services::abyssal_void_mgr::VOID_SPACE_SYSTEM_ID as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    collapse_stranded_abyssal_if_needed_sync(&ctx, void_char, now);
    ctx.db.with_snapshot(|snap| {
        assert_ne!(
            snap.characters[&void_char].solar_system_id,
            crate::services::abyssal_void_mgr::VOID_SPACE_SYSTEM_ID
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sp6_lifecycle_hp_persistence_unification_filament_gates_logoff_and_reconnect() {
    use super::super::{
        apply_persisted_hp_to_combat_state, clear_ship_combat_damage_in_snap,
        handle_character_logoff_or_disconnect, maybe_abort_safe_logoff_on_action,
        persist_ship_combat_hp_batch_inner, read_unified_ship_damage_state,
        reconnect_character_in_space_sync, require_ship_ready_for_filament_jump, PersistedCombatHp,
    };

    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 91_810_001_i64;
    let docked_char = 91_810_002_i64;
    let ship_id = 101_810_001_i64;
    let ghost_ship_id = 101_810_999_i64;
    let wreck_non_free = 101_810_010_i64;
    let drone_free = 101_810_011_i64;
    let struct_id = 1_066_810_001_i64;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch.clone());

        let mut dch = ch;
        dch.character_id = docked_char;
        dch.station_id = Some(60_003_760);
        snap.characters.insert(docked_char, dch);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(ship_id, [15_000.0, 0.0, 0.0]);

        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: 98_810_001,
                solar_system_id: sys_id,
                name: "PersistedCitadel".into(),
                position: [5_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 50,
                shield_hp: 1000.0,
                armor_hp: 1000.0,
                hull_hp: 1000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.items.insert(
            wreck_non_free,
            InventoryItemRecord {
                item_id: wreck_non_free,
                type_id: 26557,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(wreck_non_free, [1_000.0, 0.0, 0.0]);
        snap.items.insert(
            drone_free,
            InventoryItemRecord {
                item_id: drone_free,
                type_id: 2454,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(drone_free, [2_000.0, 0.0, 0.0]);
    });

    // 1. `populate_persisted_space_objects`: non-drone is marked non-free, drone stays free, structure SlimItemMeta fields exact
    let mut bp = eve_destiny::Ballpark::new(sys_id);
    let _ = populate_persisted_space_objects(&mut bp, sys_id, &ctx.sde, &ctx.db);
    assert!(bp.non_free_ball_ids.contains(&wreck_non_free));
    assert!(!bp.non_free_ball_ids.contains(&drone_free));
    let st_meta = bp.ball_slim_meta(struct_id).expect("structure slim meta");
    assert_eq!(st_meta.owner_id, 98_810_001);
    assert_eq!(st_meta.char_id, None);
    assert_eq!(st_meta.corp_id, 98_810_001);
    assert_eq!(st_meta.alliance_id, -1);

    // 2. `persist_ship_combat_hp_batch_inner` + `with_sp6_state` + `clear_ship_combat_damage_in_snap`
    let mut dmg_cs = CombatState::default();
    dmg_cs.max_shield_hp = 500.0;
    dmg_cs.shield_hp = 400.0;
    dmg_cs.max_armor_hp = 400.0;
    dmg_cs.armor_hp = 300.0;
    dmg_cs.max_structure_hp = 300.0;
    dmg_cs.structure_hp = 200.0;

    // require_persisted_entity=true ignores ghost_ship_id
    persist_ship_combat_hp_batch_inner(&ctx.db, [(ghost_ship_id, &dmg_cs)], true);
    assert!(!with_sp6_state(&ctx.db, |s| s.ship_combat_hp.contains_key(&ghost_ship_id)));

    // require_persisted_entity=false writes ghost_ship_id too; ship_id updates both sp6 & item dynamic_attributes & bumps version
    persist_ship_combat_hp_batch_inner(
        &ctx.db,
        [(ship_id, &dmg_cs), (ghost_ship_id, &dmg_cs)],
        false,
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.sp6_lifecycle.ship_combat_hp[&ship_id].shield_hp, 400.0);
        assert_eq!(snap.items[&ship_id].version, 2);
        assert_eq!(
            snap.items[&ship_id]
                .dynamic_attributes
                .get(&eve_sde::attr_ids::ARMOR_DAMAGE)
                .copied(),
            Some(100.0)
        );
        assert_eq!(
            snap.items[&ship_id]
                .dynamic_attributes
                .get(&eve_sde::attr_ids::DAMAGE)
                .copied(),
            Some(100.0)
        );
        assert!(snap.sp6_lifecycle.ship_combat_hp.contains_key(&ghost_ship_id));
    });

    // No-op repeat MUST NOT bump item version again
    persist_ship_combat_hp_batch_inner(&ctx.db, [(ship_id, &dmg_cs)], true);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&ship_id].version, 2);
    });

    // Zero damage removes DAMAGE & ARMOR_DAMAGE and bumps version; zero max_*_hp sets 0.0 damage
    let mut full_cs = CombatState::default();
    full_cs.max_shield_hp = 500.0;
    full_cs.shield_hp = 500.0;
    full_cs.max_armor_hp = 400.0;
    full_cs.armor_hp = 400.0;
    full_cs.max_structure_hp = 300.0;
    full_cs.structure_hp = 300.0;
    persist_ship_combat_hp_batch_inner(&ctx.db, [(ship_id, &full_cs)], true);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&ship_id].version, 3);
        assert!(!snap.items[&ship_id]
            .dynamic_attributes
            .contains_key(&eve_sde::attr_ids::ARMOR_DAMAGE));
        assert!(!snap.items[&ship_id]
            .dynamic_attributes
            .contains_key(&eve_sde::attr_ids::DAMAGE));
    });

    // `read_unified_ship_damage_state` + `apply_persisted_hp_to_combat_state` + `clear_ship_combat_damage_in_snap`
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h.try_spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(15_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState {
            shield_hp: 450.0,
            max_shield_hp: 500.0, // live shield dmg = 50
            armor_hp: 380.0,
            max_armor_hp: 400.0,  // live armor dmg = 20
            structure_hp: 290.0,
            max_structure_hp: 300.0, // live hull dmg = 10
            ..CombatState::default()
        },
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.sp6_lifecycle.ship_combat_hp.insert(
            ship_id,
            PersistedCombatHp {
                shield_hp: 420.0, // persisted shield dmg = 80 > live 50
                armor_hp: 350.0,  // persisted armor dmg = 50 > live 20
                hull_hp: 250.0,   // persisted hull dmg = 50 > live 10
            },
        );
        snap.items
            .get_mut(&ship_id)
            .unwrap()
            .dynamic_attributes
            .insert(eve_sde::attr_ids::ARMOR_DAMAGE, 120.0); // dyn armor dmg = 120 highest!
        snap.items
            .get_mut(&ship_id)
            .unwrap()
            .dynamic_attributes
            .insert(eve_sde::attr_ids::DAMAGE, 90.0); // dyn hull dmg = 90 highest!
    });
    ctx.db.with_snapshot(|snap| {
        let u = read_unified_ship_damage_state(&ctx, snap, ship_id, 587, Some((300.0, 400.0, 500.0)));
        assert!((u.shield_damage - 80.0).abs() < 1e-6);
        assert!((u.shield_hp - 420.0).abs() < 1e-6);
        assert!((u.armor_damage - 120.0).abs() < 1e-6);
        assert!((u.hull_damage - 90.0).abs() < 1e-6);

        let zero_u = read_unified_ship_damage_state(&ctx, snap, ghost_ship_id, 587, Some((0.0, 0.0, 0.0)));
        assert_eq!(zero_u.shield_damage, 0.0);
        assert_eq!(zero_u.armor_damage, 0.0);
        assert_eq!(zero_u.hull_damage, 0.0);
    });

    // apply_persisted_hp_to_combat_state respects persisted hull_hp > 0 AND dynamic_attributes fallback
    let mut restored_cs = full_cs.clone();
    apply_persisted_hp_to_combat_state(&ctx, ship_id, &mut restored_cs);
    assert_eq!(restored_cs.shield_hp, 420.0);
    assert_eq!(restored_cs.armor_hp, 350.0);
    assert_eq!(restored_cs.structure_hp, 250.0);

    // Clear removes both dynamic_attributes and persisted HP entry
    ctx.db.with_mut_snapshot(|snap| {
        clear_ship_combat_damage_in_snap(snap, ship_id);
        assert!(!snap.sp6_lifecycle.ship_combat_hp.contains_key(&ship_id));
        assert!(!snap.items[&ship_id]
            .dynamic_attributes
            .contains_key(&eve_sde::attr_ids::ARMOR_DAMAGE));
        assert!(!snap.items[&ship_id]
            .dynamic_attributes
            .contains_key(&eve_sde::attr_ids::DAMAGE));
        assert_eq!(snap.items[&ship_id].version, 4);

        // Dynamic attribute fallback when persisted entry is cleared
        snap.items
            .get_mut(&ship_id)
            .unwrap()
            .dynamic_attributes
            .insert(eve_sde::attr_ids::ARMOR_DAMAGE, 40.0);
        snap.items
            .get_mut(&ship_id)
            .unwrap()
            .dynamic_attributes
            .insert(eve_sde::attr_ids::DAMAGE, 30.0);
    });
    let mut fallback_cs = full_cs.clone();
    apply_persisted_hp_to_combat_state(&ctx, ship_id, &mut fallback_cs);
    assert_eq!(fallback_cs.armor_hp, 360.0);
    assert_eq!(fallback_cs.structure_hp, 270.0);

    // 3. `require_ship_ready_for_filament_jump`: weapons timer, lance debuff, warp scramble
    assert!(require_ship_ready_for_filament_jump(&ctx, char_id, ship_id, sys_id).is_ok());
    ctx.crimewatch.with_mut_crimewatch(|cw| {
        cw.states.insert(
            char_id,
            crate::combat::CharacterCrimeWatchState {
                character_id: char_id,
                weapon_timer_until_unix: now + 30,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        require_ship_ready_for_filament_jump(&ctx, char_id, ship_id, sys_id)
            .unwrap_err()
            .code,
        "WeaponsTimerActive"
    );
    ctx.crimewatch.with_mut_crimewatch(|cw| cw.states.clear());
    {
        let mut bp_w = sys_h.ballpark.write().unwrap();
        bp_w.combat_states.get_mut(&ship_id).unwrap().lance_disrupted_remaining_s = 12.0;
    }
    assert_eq!(
        require_ship_ready_for_filament_jump(&ctx, char_id, ship_id, sys_id)
            .unwrap_err()
            .code,
        "DisruptiveLanceDebuffActive"
    );
    {
        let mut bp_w = sys_h.ballpark.write().unwrap();
        let cs = bp_w.combat_states.get_mut(&ship_id).unwrap();
        cs.lance_disrupted_remaining_s = 0.0;
        cs.warp_scramble_strength = 2;
    }
    assert_eq!(
        require_ship_ready_for_filament_jump(&ctx, char_id, ship_id, sys_id)
            .unwrap_err()
            .code,
        "WarpScrambled"
    );
    {
        let mut bp_w = sys_h.ballpark.write().unwrap();
        bp_w.combat_states.get_mut(&ship_id).unwrap().warp_scramble_strength = 0;
    }

    // 4. `maybe_abort_safe_logoff_on_action`, `handle_character_logoff_or_disconnect`, `reconnect_character_in_space_sync`
    assert!(!maybe_abort_safe_logoff_on_action(&ctx, char_id, "ModuleActivated"));
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.safe_logoffs.insert(
            char_id,
            SafeLogoffEntry {
                char_id,
                ship_id,
                solar_system_id: sys_id,
                started_unix: now,
                complete_at_unix: now + 30,
                completed: true,
            },
        );
    });
    assert!(!maybe_abort_safe_logoff_on_action(&ctx, char_id, "ModuleActivated"));
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.safe_logoffs.insert(
            char_id,
            SafeLogoffEntry {
                char_id,
                ship_id,
                solar_system_id: sys_id,
                started_unix: now,
                complete_at_unix: now + 30,
                completed: false,
            },
        );
    });
    assert!(maybe_abort_safe_logoff_on_action(&ctx, char_id, "ModuleActivated"));

    assert!(handle_character_logoff_or_disconnect(&ctx, docked_char).await.is_none());
    let disc = handle_character_logoff_or_disconnect(&ctx, char_id)
        .await
        .expect("undocked char creates DisconnectedShipEntry");
    assert_eq!(disc.char_id, char_id);
    assert_eq!(disc.ship_id, ship_id);

    // Move live ship >10km from pre_warp_position and test reconnect initiates return warp
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        if let Some(d) = st.disconnected_ships.get_mut(&char_id) {
            d.pre_warp_position = Some([0.0, 0.0, 0.0]);
        }
    });
    let rec = reconnect_character_in_space_sync(&ctx, char_id).expect("reconnect in space");
    assert_eq!(rec.0, sys_id);
    assert_eq!(rec.1, ship_id);
    assert!(matches!(
        sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.mode,
        MovementMode::Warp { .. }
    ));
}

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::cluster_macho_misc_services::with_misc_state;
use eve_destiny::{Ball, MovementMode};
use glam::DVec3;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_shipcaster_ops_starter_eligibility_vectors_proximity_and_jump() {
    let ctx = test_ctx();
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let cid = 90_000_001;
    let now = ctx.now_unix();

    // Give rookie pilot an active frigate in Jita (30000142)
    let ship_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let sid = snap.next_entity_id;
        snap.items.insert(
            sid,
            eve_db::InventoryItemRecord {
                item_id: sid,
                type_id: 587, // Rifter (Frigate)
                owner_id: cid,
                location_id: 30_000_142,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.create_date_unix = now.saturating_sub(86_400); // 1 day old rookie
            ch.active_ship_id = sid;
            ch.solar_system_id = 30_000_142;
        }
        sid
    });
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(30_000_142);

    // 1. Rookie eligibility checks
    assert!(check_starter_shipcaster_eligibility(&ctx, &sess, cid).is_ok());

    // Already used check
    with_misc_state(&ctx, |st| {
        st.used_starter_shipcaster_chars.insert(cid);
    });
    assert!(check_starter_shipcaster_eligibility(&ctx, &sess, cid).is_err());
    with_misc_state(&ctx, |st| {
        st.used_starter_shipcaster_chars.remove(&cid);
    });

    // Too old check
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.create_date_unix = now.saturating_sub(STARTER_SHIPCASTER_MAX_ROOKIE_AGE_SECS + 100);
        }
    });
    assert!(check_starter_shipcaster_eligibility(&ctx, &sess, cid).is_err());
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.create_date_unix = now.saturating_sub(86_400);
        }
    });

    // Capsule check
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&ship_id) {
            it.type_id = 670;
        }
    });
    assert!(check_starter_shipcaster_eligibility(&ctx, &sess, cid).is_err());
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&ship_id) {
            it.type_id = 587;
        }
    });

    // 2. Launch vector and arrival position math
    let v_same = calculate_shipcaster_launch_vector(
        &ctx.sde,
        30_000_142,
        DVec3::ZERO,
        30_000_142,
        DVec3::new(0.0, 1000.0, 0.0),
    );
    assert!((v_same.y - 1.0).abs() < 1e-6);

    let v_inter = calculate_shipcaster_launch_vector(
        &ctx.sde,
        30_000_142,
        DVec3::ZERO,
        30_002_187,
        DVec3::ZERO,
    );
    assert!((v_inter.length() - 1.0).abs() < 1e-6);

    let arr = calculate_shipcaster_arrival_position(
        DVec3::new(100.0, 0.0, 0.0),
        DVec3::X,
        2_500.0,
    );
    assert_eq!(arr, DVec3::new(2_600.0, 0.0, 0.0));

    // 3. Proximity, alignment FX, and execute_shipcaster_jump
    let jita_sys = ctx.systems.get_or_spawn(30_000_142);
    assert!(check_proximity(&jita_sys, ship_id, 0, 5000.0, "caster").await.is_err());
    assert!(check_proximity(&jita_sys, ship_id, 777_001, 5000.0, "caster").await.is_err());

    let _ = jita_sys
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = jita_sys
        .spawn_ball(Ball {
            id: 777_001,
            type_id: 75900,
            position: DVec3::new(1000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1.0e9,
            radius: 1000.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await;
    assert!(check_proximity(&jita_sys, ship_id, 777_001, 5000.0, "caster").await.is_ok());
    assert!(check_proximity(&jita_sys, ship_id, 777_001, 500.0, "caster").await.is_err());

    apply_shipcaster_beacon_alignment_and_spool(&jita_sys, ship_id, 777_001, DVec3::X)
        .await
        .unwrap();
    execute_shipcaster_jump(
        &ctx,
        &mut sess,
        cid,
        ship_id,
        30_000_142,
        30_000_144,
        DVec3::new(5000.0, 0.0, 0.0),
        DVec3::X,
    )
    .await
    .unwrap();
    assert_eq!(sess.solar_system_id, Some(30_000_144));

    // 4. on_landing_pad_ball_destroyed
    on_landing_pad_ball_destroyed(&ctx, -999_999).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_shipcaster_ops_starter_eligibility_negative_and_boundary_cases() {
    let ctx = test_ctx();
    ctx.set_clock_paused(true);
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let cid = 90_000_001;
    let now = ctx.now_unix();

    // Unknown character (create_date_unix == 0)
    let err_missing = check_starter_shipcaster_eligibility(&ctx, &sess, 99_999_999).unwrap_err();
    assert_eq!(err_missing.code, "CharacterTooOldForStarterShipcaster");

    // Seed frigate and exact rookie boundary age
    let freighter_type_id = ctx
        .sde
        .types
        .values()
        .filter(|t| is_capital_or_freighter_group(t.group_id))
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .expect("capital/freighter type in SDE");

    let ship_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let sid = snap.next_entity_id;
        snap.items.insert(
            sid,
            eve_db::InventoryItemRecord {
                item_id: sid,
                type_id: 587,
                owner_id: cid,
                location_id: 30_000_142,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.create_date_unix = now.saturating_sub(STARTER_SHIPCASTER_MAX_ROOKIE_AGE_SECS);
            ch.active_ship_id = sid;
            ch.solar_system_id = 30_000_142;
        }
        sid
    });
    sess.ship_id = Some(ship_id);

    // Exact 30-day age boundary is allowed
    assert!(check_starter_shipcaster_eligibility(&ctx, &sess, cid).is_ok());

    // 1 second past 30-day age boundary fails
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.create_date_unix = now.saturating_sub(STARTER_SHIPCASTER_MAX_ROOKIE_AGE_SECS + 1);
        }
    });
    assert_eq!(
        check_starter_shipcaster_eligibility(&ctx, &sess, cid)
            .unwrap_err()
            .code,
        "CharacterTooOldForStarterShipcaster"
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.create_date_unix = now.saturating_sub(1_000);
        }
    });

    // Crimewatch weapons, PvP, and criminal timers each block eligibility
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(cid).weapon_timer_until_unix = now + 30;
    }
    assert_eq!(
        check_starter_shipcaster_eligibility(&ctx, &sess, cid)
            .unwrap_err()
            .code,
        "WeaponsTimerActive"
    );
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(cid);
        st.weapon_timer_until_unix = 0;
        st.pvp_timer_until_unix = now + 30;
    }
    assert_eq!(
        check_starter_shipcaster_eligibility(&ctx, &sess, cid)
            .unwrap_err()
            .code,
        "WeaponsTimerActive"
    );
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(cid);
        st.pvp_timer_until_unix = 0;
        st.criminal_flag_until_unix = now + 30;
    }
    assert_eq!(
        check_starter_shipcaster_eligibility(&ctx, &sess, cid)
            .unwrap_err()
            .code,
        "WeaponsTimerActive"
    );
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(cid).criminal_flag_until_unix = 0;
    }

    // No active ship (session.ship_id = None, ch.active_ship_id = 0)
    sess.ship_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.active_ship_id = 0;
        }
    });
    assert_eq!(
        check_starter_shipcaster_eligibility(&ctx, &sess, cid)
            .unwrap_err()
            .code,
        "CustomError"
    );
    sess.ship_id = Some(ship_id);

    // Golden Pod capsule (33328)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&ship_id) {
            it.type_id = 33328;
        }
    });
    assert_eq!(
        check_starter_shipcaster_eligibility(&ctx, &sess, cid)
            .unwrap_err()
            .code,
        "CapsuleCannotJumpShipcaster"
    );

    // Capital / freighter ship
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&ship_id) {
            it.type_id = freighter_type_id;
        }
    });
    assert_eq!(
        check_starter_shipcaster_eligibility(&ctx, &sess, cid)
            .unwrap_err()
            .code,
        "CapitalShipCannotJumpShipcaster"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_shipcaster_ops_starter_notifications_and_pad_destruction_promotion() {
    use crate::router::cache_notifications::MAX_PENDING_NOTIFICATIONS_PER_CHAR;
    use crate::services::cluster_macho_misc_services::{LandingPadRecord, LauncherRecord};

    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let uid = ctx.db.with_snapshot(|s| s.characters[&cid].user_id);
    let now = ctx.now_unix();

    // pending_notifications == None is a clean no-op
    emit_shipcaster_target_state_changed_core(&ctx.db, None, &ctx.notify_tx, 30_000_142, 555);

    // No online recipients in system is a no-op
    emit_shipcaster_target_state_changed_core(
        &ctx.db,
        Some(&ctx.pending_notifications),
        &ctx.notify_tx,
        30_000_142,
        555,
    );
    assert!(ctx.pending_notifications.read().unwrap().is_empty());

    // Register online session for cid in 30_000_142
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&cid) {
            ch.solar_system_id = 30_000_142;
        }
    });
    ctx.db
        .acquire_online_lock(uid, 999, Some(cid), true, now)
        .unwrap();
    let mut rx = ctx.notify_tx.subscribe();

    emit_shipcaster_target_state_changed_core(
        &ctx.db,
        Some(&ctx.pending_notifications),
        &ctx.notify_tx,
        30_000_142,
        777,
    );
    let (rx_cid, rx_note) = rx.try_recv().expect("broadcast notification");
    assert_eq!(rx_cid, cid);
    assert_eq!(rx_note.event, "OnShipcasterTargetStateChanged");
    assert_eq!(rx_note.args, vec![PyRep::Integer(777)]);

    // Queue cap eviction at MAX_PENDING_NOTIFICATIONS_PER_CHAR
    for i in 0..(MAX_PENDING_NOTIFICATIONS_PER_CHAR + 2) {
        emit_shipcaster_target_state_changed_core(
            &ctx.db,
            Some(&ctx.pending_notifications),
            &ctx.notify_tx,
            30_000_142,
            1_000 + i as i64,
        );
    }
    assert_eq!(
        ctx.pending_notifications.read().unwrap()[&cid].len(),
        MAX_PENDING_NOTIFICATIONS_PER_CHAR
    );
    ctx.pending_notifications.write().unwrap().clear();

    // Pad destruction promotes next queued built, non-disrupted pad and notifies both systems
    let pad_destroyed_id = 880_001_i64;
    let pad_disrupted_id = 880_002_i64;
    let pad_queued_id = 880_003_i64;
    let launcher_id = 880_100_i64;
    let pad_sys = 30_000_144_u32;
    let war_hq_sys = 30_000_142_u32;

    let _ = ctx
        .systems
        .get_or_spawn(pad_sys)
        .spawn_ball(Ball {
            id: pad_destroyed_id,
            type_id: 75928,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1e9,
            radius: 1000.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await;

    with_misc_state(&ctx, |st| {
        st.landing_pads.push(LandingPadRecord {
            item_id: pad_destroyed_id,
            type_id: 75928,
            built_type_id: 75928,
            solar_system_id: pad_sys,
            faction_id: 500_999,
            is_built: true,
            link_timestamp: now as i64,
            is_disrupted: false,
            position: DVec3::ZERO,
            required_materials: Default::default(),
            contributed_materials: Default::default(),
        });
        st.landing_pads.push(LandingPadRecord {
            item_id: pad_disrupted_id,
            type_id: 75928,
            built_type_id: 75928,
            solar_system_id: pad_sys,
            faction_id: 500_999,
            is_built: true,
            link_timestamp: 0,
            is_disrupted: true,
            position: DVec3::X * 10_000.0,
            required_materials: Default::default(),
            contributed_materials: Default::default(),
        });
        st.landing_pads.push(LandingPadRecord {
            item_id: pad_queued_id,
            type_id: 75928,
            built_type_id: 75928,
            solar_system_id: pad_sys,
            faction_id: 500_999,
            is_built: true,
            link_timestamp: 0,
            is_disrupted: false,
            position: DVec3::Y * 10_000.0,
            required_materials: Default::default(),
            contributed_materials: Default::default(),
        });
        st.launchers.push(LauncherRecord {
            item_id: launcher_id,
            type_id: 75900,
            solar_system_id: war_hq_sys,
            faction_id: 500_999,
            linked_pad_ids: vec![pad_destroyed_id],
            position: DVec3::ZERO,
        });
    });

    on_landing_pad_ball_destroyed(&ctx, pad_destroyed_id).await;

    with_misc_state(&ctx, |st| {
        assert!(!st.landing_pads.iter().any(|p| p.item_id == pad_destroyed_id));
        let promoted = st
            .landing_pads
            .iter()
            .find(|p| p.item_id == pad_queued_id)
            .expect("queued pad remains");
        assert!(promoted.link_timestamp > 0);
        let launcher = st
            .launchers
            .iter()
            .find(|l| l.item_id == launcher_id)
            .expect("launcher exists");
        assert_eq!(launcher.linked_pad_ids, vec![pad_queued_id]);
    });
    assert!(ctx
        .systems
        .get_or_spawn(pad_sys)
        .get_ball_snapshot(pad_destroyed_id)
        .await
        .unwrap()
        .is_none());
    let pending = ctx.pending_notifications.read().unwrap();
    assert!(pending[&cid]
        .iter()
        .any(|n| n.event == "OnShipcasterTargetStateChanged" && n.args == vec![PyRep::Integer(launcher_id)]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_shipcaster_ops_starter_prevalidation_and_jump_state_preservation() {
    let ctx = test_ctx();
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_100_000);
    let cid = 90_000_001_i64;
    let now = ctx.now_unix();

    // Vector fallbacks
    assert_eq!(
        calculate_shipcaster_launch_vector(&ctx.sde, 30_000_142, DVec3::ZERO, 30_000_142, DVec3::ZERO),
        DVec3::X
    );
    assert_eq!(
        calculate_shipcaster_launch_vector(&ctx.sde, 999_999_991, DVec3::ZERO, 999_999_992, DVec3::ZERO),
        DVec3::X
    );

    let (ship_id, mod_id) = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 2;
        let sid = snap.next_entity_id - 1;
        let mid = snap.next_entity_id;
        snap.items.insert(
            sid,
            eve_db::InventoryItemRecord {
                item_id: sid,
                type_id: 587,
                owner_id: cid,
                location_id: 30_100_000,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            mid,
            eve_db::InventoryItemRecord {
                item_id: mid,
                type_id: 486,
                owner_id: cid,
                location_id: sid,
                flag: eve_db::InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.active_ship_id = sid;
            ch.solar_system_id = 30_100_000;
        }
        snap.zarzakh_gate_locks.insert(cid, (50_000_999, now + 600));
        (sid, mid)
    });
    sess.ship_id = Some(ship_id);
    sess.station_id = Some(60_003_760);
    sess.structure_id = Some(1_000_000_111);

    let origin_sys = ctx.systems.get_or_spawn(30_100_000);
    // ShipNotInSpace before spawning ball
    assert_eq!(
        check_proximity(&origin_sys, ship_id, 777_001, 5_000.0, "caster")
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );
    assert_eq!(
        prevalidate_shipcaster_jump(&ctx, cid, ship_id, 30_100_000)
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    // Spawn ship as capsule first -> CapsuleCannotJumpShipcaster
    let _ = origin_sys
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 670,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    // TargetNotInSpace when ship IS in space but target is missing
    assert_eq!(
        check_proximity(&origin_sys, ship_id, 777_888, 5_000.0, "caster")
            .await
            .unwrap_err()
            .code,
        "TargetNotInSpace"
    );
    assert_eq!(
        prevalidate_shipcaster_jump(&ctx, cid, ship_id, 30_100_000)
            .await
            .unwrap_err()
            .code,
        "CapsuleCannotJumpShipcaster"
    );

    // Respawn as frigate (587) with combat state
    let _ = origin_sys.remove_ball(ship_id).await;
    let mut init_cs = ctx.build_ship_combat_state(cid, ship_id, 587);
    init_cs.shield_hp = 100.0;
    init_cs.armor_hp = 80.0;
    init_cs.structure_hp = 60.0;
    init_cs.capacitor = 45.0;
    init_cs.warp_scramble_strength = 2;
    let _ = origin_sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            init_cs,
        )
        .await;

    // Weapon timer check
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(cid).weapon_timer_until_unix = now + 30;
    }
    assert_eq!(
        prevalidate_shipcaster_jump(&ctx, cid, ship_id, 30_100_000)
            .await
            .unwrap_err()
            .code,
        "WeaponsTimerActive"
    );
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(cid).weapon_timer_until_unix = 0;
    }
    {
        let mut bp = origin_sys.ballpark.write().unwrap();
        bp.combat_states.get_mut(&ship_id).unwrap().warp_scramble_strength = 0;
    }

    // Navigation lock check (lance firing lock)
    let db_key = std::sync::Arc::as_ptr(&ctx.db) as usize;
    {
        let mut tst = ctx.tactical_state.write().unwrap();
        tst.lance_caster_lock_until.insert((db_key, ship_id), now + 60);
    }
    assert_eq!(
        prevalidate_shipcaster_jump(&ctx, cid, ship_id, 30_100_000)
            .await
            .unwrap_err()
            .code,
        "LanceFiringLockActive"
    );
    {
        let mut tst = ctx.tactical_state.write().unwrap();
        tst.lance_caster_lock_until.remove(&(db_key, ship_id));
    }

    // ShipInWarp check
    let _ = origin_sys
        .set_movement_mode(
            ship_id,
            MovementMode::new_warp(DVec3::ZERO, DVec3::new(100_000.0, 0.0, 0.0), 3.0),
        )
        .await;
    assert_eq!(
        prevalidate_shipcaster_jump(&ctx, cid, ship_id, 30_100_000)
            .await
            .unwrap_err()
            .code,
        "ShipInWarp"
    );
    let _ = origin_sys.remove_ball(ship_id).await;
    let mut stop_cs = ctx.build_ship_combat_state(cid, ship_id, 587);
    stop_cs.shield_hp = 100.0;
    stop_cs.armor_hp = 80.0;
    stop_cs.structure_hp = 60.0;
    stop_cs.capacitor = 45.0;
    stop_cs.warp_scramble_strength = 2;
    let _ = origin_sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            stop_cs,
        )
        .await;

    // WarpScrambled check
    assert_eq!(
        prevalidate_shipcaster_jump(&ctx, cid, ship_id, 30_100_000)
            .await
            .unwrap_err()
            .code,
        "WarpScrambled"
    );
    assert!(origin_sys.try_update_combat_state(ship_id, |cs| cs.warp_scramble_strength = 0));

    // Seed saved heat & module damage
    if let Ok(mut fit) = ctx.get_or_rebuild_ship_fitting(cid, ship_id) {
        fit.rack_heat = [0.42, 0.15, 0.0];
        if let Some(m) = fit.modules.get_mut(&mod_id) {
            m.damage_hp = 12.5;
        }
        ctx.store_ship_fitting(ship_id, fit);
    }

    // Execute jump from Zarzakh (30_100_000) to Jita (30_000_142)
    execute_shipcaster_jump(
        &ctx,
        &mut sess,
        cid,
        ship_id,
        30_100_000,
        30_000_142,
        DVec3::new(1000.0, 2000.0, 3000.0),
        DVec3::Y,
    )
    .await
    .unwrap();

    assert_eq!(sess.solar_system_id, Some(30_000_142));
    assert_eq!(sess.station_id, None);
    assert_eq!(sess.structure_id, None);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.zarzakh_gate_locks.contains_key(&cid));
        assert_eq!(snap.items[&ship_id].location_id, 30_000_142);
        assert_eq!(
            snap.items[&mod_id]
                .dynamic_attributes
                .get(&eve_sde::attr_ids::DAMAGE)
                .copied(),
            Some(12.5)
        );
    });
    let dest_handle = ctx.systems.get_or_spawn(30_000_142);
    let (_, arrived_ball) = dest_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert_eq!(arrived_ball.position, DVec3::new(1000.0, 2000.0, 3000.0));
    let arrived_cs = dest_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(arrived_cs.is_cloaked);
    assert_eq!(arrived_cs.gate_cloak_remaining_ticks, 60);
    assert_eq!(arrived_cs.undock_invulnerable_remaining_ticks, 60);
    assert!((arrived_cs.shield_hp - 100.0_f64.min(arrived_cs.max_shield_hp)).abs() < 1e-6);

    // Immediately jumping again hits SessionChangeInProgress
    assert_eq!(
        prevalidate_shipcaster_jump(&ctx, cid, ship_id, 30_000_142)
            .await
            .unwrap_err()
            .code,
        "SessionChangeInProgress"
    );
}

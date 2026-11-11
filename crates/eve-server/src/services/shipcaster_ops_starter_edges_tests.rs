use super::*;
use crate::router::{PendingMachoNotification, ServerContext};
use crate::services::cluster_macho_misc_services::{
    LandingPadRecord, LauncherRecord, with_misc_state,
};
use crate::services::space_destiny::record_session_change_timer;
use crate::services::tactical_modules::db_ptr_key;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystem};
use glam::DVec3;
use std::collections::BTreeMap;
use std::sync::Arc;

fn build_shipcaster_ctx() -> ServerContext {
    let mut sde = SdeRegistry::with_canonical_seed();
    let base_type = sde.types.values().next().unwrap().clone();
    for (sid, name, pos) in [
        (30_000_142_u32, "Jita", [0.0, 0.0, 0.0]),
        (30_002_187_u32, "Amarr", [10.0, 2.0, -1.0]),
        (30_100_000_u32, "Zarzakh", [-5.0, 0.0, 5.0]),
    ] {
        sde.solar_systems.insert(
            sid,
            SolarSystem {
                solar_system_id: sid,
                constellation_id: 20_000_020,
                region_id: 10_000_002,
                name: name.into(),
                security_status: 0.9,
                radius: 1.0e13,
                galactic_pos_ly: pos,
                planet_ids: vec![],
                stations: vec![],
                wormhole_class_id: None,
            },
        );
    }
    for (tid, gid, cat, name) in [
        (587_u32, 25_u32, 6_u32, "Rifter"),
        (670, 29, 6, "Capsule"),
        (33328, 29, 6, "Capsule - Genolution"),
        (19720, 485, 6, "Revelation"),
        (20185, 513, 6, "Charon"),
        (2046, 324, 7, "Damage Control II"),
    ] {
        let entry = sde.types.entry(tid).or_insert_with(|| {
            let mut t = base_type.clone();
            t.type_id = tid;
            t.group_id = gid;
            t.category_id = cat;
            t.name = name.into();
            t
        });
        if tid == 587 {
            entry.default_attributes.insert(eve_sde::attr_ids::SHIELD_CAPACITY, 500.0);
            entry.default_attributes.insert(eve_sde::attr_ids::ARMOR_HP, 400.0);
            entry.default_attributes.insert(9, 350.0);
            entry.default_attributes.insert(eve_sde::attr_ids::CAPACITOR_CAPACITY, 250.0);
        }
    }
    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(Arc::new(sde), db)
}

fn spawn_char_with_ship(
    ctx: &ServerContext,
    cid: i64,
    sys_id: u32,
    ship_id: i64,
    ship_type_id: u32,
    create_date_unix: u64,
) -> u64 {
    let uname = format!("sc_user_{cid}");
    let uid = ctx.db.provision_player_account(&uname, "pw123");
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = cid;
        ch.user_id = uid;
        ch.name = format!("Pilot {cid}");
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        ch.create_date_unix = create_date_unix;
        ch.balance = IskCents(0);
        snap.characters.insert(cid, ch);
        if ship_id > 0 {
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: ship_type_id,
                    owner_id: cid,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..InventoryItemRecord::default()
                },
            );
        }
    });
    uid
}

fn make_ball(id: i64, type_id: u32, pos: DVec3, mode: MovementMode) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 3.0,
        mass: 1.0e6,
        radius: 40.0,
        speed_fraction: 0.0,
        mode,
    }
}

#[test]
fn starter_shipcaster_eligibility_covers_all_rejection_and_success_branches() {
    let ctx = build_shipcaster_ctx();
    let now = ctx.now_unix();
    let cid = 92_000_001;
    let uid = spawn_char_with_ship(&ctx, cid, 30_000_142, 92_100_001, 587, now.saturating_sub(100));
    let mut sess = SessionState::default();
    sess.session_id = 900_001;
    sess.user_id = Some(uid);
    sess.character_id = Some(cid);
    sess.solar_system_id = Some(30_000_142);

    with_misc_state(&ctx, |st| {
        st.used_starter_shipcaster_chars.insert(cid);
    });
    assert!(matches!(
        check_starter_shipcaster_eligibility(&ctx, &sess, cid),
        Err(ref e) if e.code == "StarterShipcasterAlreadyUsed"
    ));
    with_misc_state(&ctx, |st| {
        st.used_starter_shipcaster_chars.remove(&cid);
    });

    // create_date_unix == 0
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&cid).unwrap().create_date_unix = 0;
    });
    assert!(check_starter_shipcaster_eligibility(&ctx, &sess, cid).is_err());

    // age > 30 days
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&cid).unwrap().create_date_unix = now.saturating_sub(35 * 86_400);
    });
    assert!(check_starter_shipcaster_eligibility(&ctx, &sess, cid).is_err());

    // Reset to young char
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&cid).unwrap().create_date_unix = now.saturating_sub(600);
    });

    // Crimewatch timers: weapon, pvp, criminal
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(cid).weapon_timer_until_unix = now + 60;
    }
    assert!(check_starter_shipcaster_eligibility(&ctx, &sess, cid).is_err());
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(cid).weapon_timer_until_unix = 0;
        cw.get_or_create(cid).pvp_timer_until_unix = now + 60;
    }
    assert!(check_starter_shipcaster_eligibility(&ctx, &sess, cid).is_err());
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(cid).pvp_timer_until_unix = 0;
        cw.get_or_create(cid).criminal_flag_until_unix = now + 60;
    }
    assert!(check_starter_shipcaster_eligibility(&ctx, &sess, cid).is_err());
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(cid).criminal_flag_until_unix = 0;
    }

    // active_ship_id <= 0
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&cid).unwrap().active_ship_id = 0;
    });
    assert!(check_starter_shipcaster_eligibility(&ctx, &sess, cid).is_err());

    // Capsules (670 and 33328)
    for cap_type in [670_u32, 33328] {
        ctx.db.with_mut_snapshot(|s| {
            s.characters.get_mut(&cid).unwrap().active_ship_id = 92_100_001;
            s.items.get_mut(&92_100_001).unwrap().type_id = cap_type;
        });
        assert!(check_starter_shipcaster_eligibility(&ctx, &sess, cid).is_err());
    }

    // Capital / freighter groups (485 dreadnought, 513 freighter)
    for cap_type in [19720_u32, 20185] {
        ctx.db.with_mut_snapshot(|s| {
            s.items.get_mut(&92_100_001).unwrap().type_id = cap_type;
        });
        assert!(check_starter_shipcaster_eligibility(&ctx, &sess, cid).is_err());
    }

    // Valid subcap
    ctx.db.with_mut_snapshot(|s| {
        s.items.get_mut(&92_100_001).unwrap().type_id = 587;
    });
    assert!(check_starter_shipcaster_eligibility(&ctx, &sess, cid).is_ok());
}

#[test]
fn emit_shipcaster_target_state_changed_core_handles_none_empty_and_overflow() {
    let ctx = build_shipcaster_ctx();
    let sys_id = 30_000_142;
    let c_online = 92_000_101;
    let c_offline = 92_000_102;
    let uid_online = spawn_char_with_ship(&ctx, c_online, sys_id, 92_100_101, 587, 100);
    let _ = spawn_char_with_ship(&ctx, c_offline, 30_002_187, 92_100_102, 587, 100);

    emit_shipcaster_target_state_changed_core(&ctx.db, None, &ctx.notify_tx, sys_id, 555_001);

    emit_shipcaster_target_state_changed_core(
        &ctx.db,
        Some(&ctx.pending_notifications),
        &ctx.notify_tx,
        sys_id,
        555_001,
    );
    assert!(ctx.drain_notifications_for_char(c_online).is_empty());

    let _ = ctx.db.acquire_online_lock_with_computer_hash(
        uid_online,
        999_001,
        Some(c_online),
        None,
        true,
        ctx.now_unix(),
    );

    {
        let mut map = ctx.pending_notifications.write().unwrap();
        let q = map.entry(c_online).or_default();
        for i in 0..crate::router::cache_notifications::MAX_PENDING_NOTIFICATIONS_PER_CHAR {
            let args = vec![PyRep::Integer(i as i64)];
            let (pkt, marsh) = ServerContext::build_macho_notification_packet(
                c_online,
                Some(uid_online),
                "OnSlimItemChange",
                "solarsystemid",
                args.clone(),
            );
            q.push(PendingMachoNotification { seq_id: 0,
                recipient_char_id: c_online,
                event: "OnSlimItemChange".into(),
                id_type: "solarsystemid".into(),
                args,
                packet: pkt,
                marshaled_packet: marsh,
                created_filetime: 0,
            });
        }
    }

    emit_shipcaster_target_state_changed_core(
        &ctx.db,
        Some(&ctx.pending_notifications),
        &ctx.notify_tx,
        sys_id,
        555_999,
    );
    let drained = ctx.drain_notifications_for_char(c_online);
    assert_eq!(
        drained.len(),
        crate::router::cache_notifications::MAX_PENDING_NOTIFICATIONS_PER_CHAR
    );
    let last = drained.last().unwrap();
    assert_eq!(last.event, "OnShipcasterTargetStateChanged");
    assert_eq!(last.args, vec![PyRep::Integer(555_999)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn landing_pad_destroyed_clears_link_and_promotes_queued_built_pad() {
    let ctx = build_shipcaster_ctx();
    let pad_destroyed_id = 104_001_001;
    let pad_replacement_id = 104_001_002;
    let launcher_id = 104_002_001;
    let sys_launcher = 30_000_142;
    let sys_pad = 30_002_187;
    let watcher = 92_000_201;
    let uid_w = spawn_char_with_ship(&ctx, watcher, sys_launcher, 92_100_201, 587, 100);
    let _ = ctx.db.acquire_online_lock_with_computer_hash(
        uid_w,
        999_002,
        Some(watcher),
        None,
        true,
        ctx.now_unix(),
    );

    // Missing pad returns cleanly without changes
    on_landing_pad_ball_destroyed(&ctx, 999_888_777).await;

    with_misc_state(&ctx, |st| {
        st.landing_pads.clear();
        st.launchers.clear();
        st.landing_pads.push(LandingPadRecord {
            item_id: pad_destroyed_id,
            type_id: 75928,
            built_type_id: 75928,
            solar_system_id: sys_pad,
            faction_id: 500_001,
            is_built: true,
            link_timestamp: 1_700_000_000,
            is_disrupted: false,
            position: DVec3::ZERO,
            required_materials: BTreeMap::new(),
            contributed_materials: BTreeMap::new(),
        });
        st.landing_pads.push(LandingPadRecord {
            item_id: pad_replacement_id,
            type_id: 75928,
            built_type_id: 75928,
            solar_system_id: sys_pad,
            faction_id: 500_001,
            is_built: true,
            link_timestamp: 0,
            is_disrupted: false,
            position: DVec3::new(1000.0, 0.0, 0.0),
            required_materials: BTreeMap::new(),
            contributed_materials: BTreeMap::new(),
        });
        st.launchers.push(LauncherRecord {
            item_id: launcher_id,
            type_id: 75936,
            solar_system_id: sys_launcher,
            faction_id: 500_001,
            linked_pad_ids: vec![pad_destroyed_id],
            position: DVec3::ZERO,
        });
    });

    on_landing_pad_ball_destroyed_core(
        &ctx.cluster_misc_state,
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        Some(&ctx.pending_notifications),
        &ctx.notify_tx,
        pad_destroyed_id,
    )
    .await;

    with_misc_state(&ctx, |st| {
        assert!(!st.landing_pads.iter().any(|p| p.item_id == pad_destroyed_id));
        let repl = st
            .landing_pads
            .iter()
            .find(|p| p.item_id == pad_replacement_id)
            .expect("replacement pad remains");
        assert!(repl.link_timestamp > 0, "queued built pad must be promoted and linked");
        let l = st.launchers.iter().find(|l| l.item_id == launcher_id).unwrap();
        assert_eq!(l.linked_pad_ids, vec![pad_replacement_id]);
    });

    let notifs = ctx.drain_notifications_for_char(watcher);
    assert_eq!(notifs.len(), 1);
    assert_eq!(notifs[0].event, "OnShipcasterTargetStateChanged");
    assert_eq!(notifs[0].args, vec![PyRep::Integer(launcher_id)]);
}

#[test]
fn calculate_shipcaster_launch_vector_and_arrival_position_edges() {
    let ctx = build_shipcaster_ctx();
    // Unknown system -> DVec3::X
    assert_eq!(
        calculate_shipcaster_launch_vector(&ctx.sde, 99_999_999, DVec3::ZERO, 30_002_187, DVec3::ZERO),
        DVec3::X
    );
    // Same system (zero diff) -> DVec3::X
    assert_eq!(
        calculate_shipcaster_launch_vector(&ctx.sde, 30_000_142, DVec3::ZERO, 30_000_142, DVec3::ZERO),
        DVec3::X
    );
    // Same system (non-zero diff) -> local normalized vector
    let same = calculate_shipcaster_launch_vector(
        &ctx.sde,
        30_000_142,
        DVec3::ZERO,
        30_000_142,
        DVec3::new(0.0, 500.0, 0.0),
    );
    assert!((same - DVec3::Y).length() < 1e-9);
    // Distinct valid systems -> unit normalized galactic direction
    let v = calculate_shipcaster_launch_vector(
        &ctx.sde,
        30_000_142,
        DVec3::ZERO,
        30_002_187,
        DVec3::ZERO,
    );
    assert!((v.length() - 1.0).abs() < 1e-9);
    assert!(v.x > 0.0 && v.y > 0.0 && v.z < 0.0);

    let arr = calculate_shipcaster_arrival_position(DVec3::new(100.0, 0.0, 0.0), DVec3::X, 2500.0);
    assert_eq!(arr, DVec3::new(2600.0, 0.0, 0.0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prevalidate_shipcaster_jump_and_proximity_cover_blocking_conditions() {
    let ctx = build_shipcaster_ctx();
    let sys_id = 30_000_142;
    let cid = 92_000_301;
    let ship_id = 92_100_301;
    let gate_id = 92_200_301;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    spawn_char_with_ship(&ctx, cid, sys_id, ship_id, 587, 100);

    // 1. ShipNotInSpace
    assert!(matches!(
        prevalidate_shipcaster_jump(&ctx, cid, ship_id, sys_id).await,
        Err(ref e) if e.code == "ShipNotInSpace"
    ));

    // Proximity errors
    assert!(matches!(
        check_proximity(&sys_handle, ship_id, 0, 2500.0, "shipcaster").await,
        Err(ref e) if e.code == "TargetNotFound"
    ));
    assert!(matches!(
        check_proximity(&sys_handle, ship_id, gate_id, 2500.0, "shipcaster").await,
        Err(ref e) if e.code == "ShipNotInSpace"
    ));

    let _ = sys_handle
        .spawn_ball(make_ball(ship_id, 587, DVec3::ZERO, MovementMode::Stop))
        .await;
    assert!(matches!(
        check_proximity(&sys_handle, ship_id, gate_id, 2500.0, "shipcaster").await,
        Err(ref e) if e.code == "TargetNotInSpace"
    ));
    let _ = sys_handle
        .spawn_ball(make_ball(gate_id, 75936, DVec3::new(10_000.0, 0.0, 0.0), MovementMode::Stop))
        .await;
    assert!(matches!(
        check_proximity(&sys_handle, ship_id, gate_id, 2500.0, "shipcaster").await,
        Err(ref e) if e.code == "OutsideOperationalRange"
    ));

    // 2. Capsules
    for cap in [670_u32, 33328] {
        ctx.db.with_mut_snapshot(|s| s.items.get_mut(&ship_id).unwrap().type_id = cap);
        let _ = sys_handle.remove_ball(ship_id).await;
        let _ = sys_handle
            .spawn_ball(make_ball(ship_id, cap, DVec3::ZERO, MovementMode::Stop))
            .await;
        assert!(prevalidate_shipcaster_jump(&ctx, cid, ship_id, sys_id).await.is_err());
    }
    ctx.db.with_mut_snapshot(|s| s.items.get_mut(&ship_id).unwrap().type_id = 587);
    let _ = sys_handle.remove_ball(ship_id).await;
    let _ = sys_handle
        .spawn_ball(make_ball(ship_id, 587, DVec3::ZERO, MovementMode::Stop))
        .await;

    // 3. Weapon timer
    let now = ctx.now_unix();
    ctx.crimewatch.write().unwrap().get_or_create(cid).weapon_timer_until_unix = now + 60;
    assert!(prevalidate_shipcaster_jump(&ctx, cid, ship_id, sys_id).await.is_err());
    ctx.crimewatch.write().unwrap().get_or_create(cid).weapon_timer_until_unix = 0;

    // 4. SessionChangeInProgress
    record_session_change_timer(&ctx, cid, now, sys_id, 1, "shipcaster_test");
    assert!(prevalidate_shipcaster_jump(&ctx, cid, ship_id, sys_id).await.is_err());
    ctx.session_change_timers.lock().unwrap().remove(&cid);

    // 5. Navigation lock (tactical_state.lance_caster_lock_until)
    let k = db_ptr_key(&ctx.db);
    ctx.tactical_state
        .write()
        .unwrap()
        .lance_caster_lock_until
        .insert((k, ship_id), now + 60);
    assert!(prevalidate_shipcaster_jump(&ctx, cid, ship_id, sys_id).await.is_err());
    ctx.tactical_state
        .write()
        .unwrap()
        .lance_caster_lock_until
        .remove(&(k, ship_id));

    // 6. MovementMode::Warp
    let _ = sys_handle.remove_ball(ship_id).await;
    let _ = sys_handle
        .spawn_ball(make_ball(
            ship_id,
            587,
            DVec3::ZERO,
            MovementMode::new_warp(DVec3::ZERO, DVec3::new(1.0e9, 0.0, 0.0), 3.0),
        ))
        .await;
    assert!(prevalidate_shipcaster_jump(&ctx, cid, ship_id, sys_id).await.is_err());

    let _ = sys_handle.remove_ball(ship_id).await;
    let _ = sys_handle
        .spawn_ball(make_ball(ship_id, 587, DVec3::ZERO, MovementMode::Stop))
        .await;

    // 7. CombatState warp_scramble_strength > 0
    let mut cs = CombatState::default();
    cs.shield_hp = 500.0;
    cs.max_shield_hp = 500.0;
    cs.armor_hp = 400.0;
    cs.max_armor_hp = 400.0;
    cs.structure_hp = 300.0;
    cs.max_structure_hp = 300.0;
    cs.warp_scramble_strength = 2;
    let _ = sys_handle.set_combat_state(ship_id, cs.clone()).await;
    assert!(prevalidate_shipcaster_jump(&ctx, cid, ship_id, sys_id).await.is_err());

    cs.warp_scramble_strength = 0;
    let _ = sys_handle.set_combat_state(ship_id, cs).await;

    // 8. Valid
    assert!(prevalidate_shipcaster_jump(&ctx, cid, ship_id, sys_id).await.is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_shipcaster_jump_preserves_heat_combat_state_and_clears_zarzakh_lock() {
    let ctx = build_shipcaster_ctx();
    let origin_sys = 30_100_000_u32;
    let dest_sys = 30_002_187_u32;
    let cid = 92_000_401;
    let ship_id = 92_100_401;
    let mod_id = 92_200_401;

    let uid = spawn_char_with_ship(&ctx, cid, origin_sys, ship_id, 587, 100);
    let mod_type_id = ctx
        .sde
        .types
        .get(&2889)
        .map(|t| t.type_id)
        .unwrap_or(2046);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: mod_type_id,
                owner_id: cid,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..InventoryItemRecord::default()
            },
        );
        snap.zarzakh_gate_locks.insert(cid, (50_001_234, ctx.now_unix() + 3600));
    });

    let sys_orig = ctx.systems.get_or_spawn(origin_sys);
    let _ = sys_orig
        .spawn_ball(make_ball(ship_id, 587, DVec3::new(100.0, 0.0, 0.0), MovementMode::Stop))
        .await;

    let mut cs = CombatState::default();
    cs.shield_hp = 123.0;
    cs.max_shield_hp = 500.0;
    cs.armor_hp = 234.0;
    cs.max_armor_hp = 400.0;
    cs.structure_hp = 345.0;
    cs.max_structure_hp = 350.0;
    cs.capacitor = 77.0;
    cs.max_capacitor = 250.0;
    let _ = sys_orig.set_combat_state(ship_id, cs).await;

    let mut had_mod_dmg = false;
    {
        let mut fit = ctx.get_or_rebuild_ship_fitting(cid, ship_id).unwrap();
        if let Some(m) = fit.modules.get_mut(&mod_id) {
            m.damage_hp = 8.5;
            had_mod_dmg = true;
        }
        fit.rack_heat = [12.0, 0.0, 0.0];
        ctx.store_ship_fitting(ship_id, fit);
    }

    let mut sess = SessionState::default();
    sess.session_id = 999_401;
    sess.user_id = Some(uid);
    sess.character_id = Some(cid);
    sess.solar_system_id = Some(origin_sys);
    sess.ship_id = Some(ship_id);

    let arrival = DVec3::new(50_000.0, 0.0, 0.0);
    let launch_vec = DVec3::X;
    execute_shipcaster_jump(
        &ctx,
        &mut sess,
        cid,
        ship_id,
        origin_sys,
        dest_sys,
        arrival,
        launch_vec,
    )
    .await
    .expect("jump from Zarzakh to Amarr should succeed");

    assert_eq!(sess.solar_system_id, Some(dest_sys));
    assert!(!ctx.db.with_snapshot(|s| s.zarzakh_gate_locks.contains_key(&cid)));

    let sys_dest = ctx.systems.get_or_spawn(dest_sys);
    let dest_cs = sys_dest
        .get_combat_state(ship_id)
        .await
        .unwrap()
        .expect("combat state restored in dest");
    assert!((dest_cs.shield_hp - 123.0).abs() < 1e-6);
    assert!((dest_cs.armor_hp - 234.0).abs() < 1e-6);
    assert!((dest_cs.capacitor - 77.0).abs() < 1e-6);

    let preserved_fit = ctx.ship_fittings.read().unwrap().get(&ship_id).cloned().unwrap();
    assert!((preserved_fit.rack_heat[0] - 12.0).abs() < 1e-6);
    if had_mod_dmg {
        let item_dmg = ctx.db.with_snapshot(|s| {
            s.items[&mod_id]
                .dynamic_attributes
                .get(&eve_sde::attr_ids::DAMAGE)
                .copied()
        });
        assert_eq!(item_dmg, Some(8.5));
    }
}

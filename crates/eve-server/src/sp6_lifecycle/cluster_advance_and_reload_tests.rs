//! Semantic unit tests split from `downtime_cleanup_tests`.

use super::*;
use crate::services::combat_crimewatch::with_mut_combat_runtime;
use crate::sp6_lifecycle::with_sp6_state;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn cluster_advance_handles_safe_logoff_abort_scrambled_warp_bounty_concord_and_void() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let base_now = ctx.now_unix();
    let ship1 = 900_210_001;
    let ship2 = 900_210_002;
    let void_sys = crate::services::abyssal_void_mgr::VOID_SPACE_SYSTEM_ID;

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, sid, s) in [(90_000_001i64, ship1, sys_id), (90_000_002i64, ship2, sys_id)] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: cid,
                    location_id: s as i64,
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
            snap.item_positions.insert(sid, [0.0, 0.0, 0.0]);
            if let Some(c) = snap.characters.get_mut(&cid) {
                c.active_ship_id = sid;
                c.solar_system_id = s;
                c.station_id = None;
                c.structure_id = None;
            }
        }
    });

    let handle = ctx.systems.get_or_spawn(sys_id);
    for (cid, sid) in [(90_000_001i64, ship1), (90_000_002i64, ship2)] {
        handle.try_spawn_ball_with_combat(
            Ball {
                id: sid,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_100_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(cid, sid, 587),
        );
    }
    let _ = handle.set_warp_scramble_strength(ship2, 2).await;

    with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.insert((sys_id, 90_000_001), 50_000.0);
        rt.bounty_tick_next_flush_unix.insert((sys_id, 90_000_001), base_now + 5);
    });
    if let Ok(mut cw) = ctx.crimewatch.write() {
        let st = cw.states.entry(90_000_002).or_default();
        st.pending_concord_arrival = Some((sys_id, base_now + 5, 90_000_001, ship1));
    }

    crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st, _| {
        st.self_destructs.insert(
            ship1,
            eve_db::SelfDestructPersistedEntry {
                ship_id: ship1,
                char_id: 90_000_001,
                solar_system_id: sys_id,
                started_unix: base_now,
                detonate_at_unix: base_now + 60,
            },
        );
        st.safe_logoffs.insert(
            90_000_001,
            crate::sp6_lifecycle::SafeLogoffEntry {
                char_id: 90_000_001,
                ship_id: ship1,
                solar_system_id: sys_id,
                started_unix: base_now,
                complete_at_unix: base_now + 20,
                completed: false,
            },
        );
        st.disconnected_ships.insert(
            90_000_002,
            crate::sp6_lifecycle::DisconnectedShipEntry {
                char_id: 90_000_002,
                ship_id: ship2,
                ship_type_id: 587,
                solar_system_id: sys_id,
                last_position: [0.0, 0.0, 0.0],
                emergency_warp_dest: Some([1_000_000.0, 0.0, 0.0]),
                disconnected_at_unix: base_now,
                remove_at_unix: base_now + 60,
                removed_from_space: false,
                pre_warp_position: None,
            },
        );
    });

    ctx.advance_secs(20);
    crate::sp6_lifecycle::on_advance_secs(&ctx, base_now, 20);

    crate::sp6_lifecycle::with_sp6_state(&ctx.db, |st| {
        assert!(st.safe_logoffs.get(&90_000_001).is_some_and(|e| e.completed));
        assert!(st.disconnected_ships[&90_000_002].emergency_warp_dest.is_none());
    });

    // Stranded pilot in Void Space evacuates back to origin solar system; Abyssal Space collapses ship
    let void_ship = 900_210_003;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            void_ship,
            InventoryItemRecord {
                item_id: void_ship,
                type_id: 587,
                owner_id: 90_000_002,
                location_id: void_sys as i64,
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
        if let Some(c) = snap.characters.get_mut(&90_000_002) {
            c.active_ship_id = void_ship;
            c.solar_system_id = void_sys;
            c.station_id = None;
            c.structure_id = None;
        }
    });
    crate::sp6_lifecycle::cluster_tick::collapse_stranded_abyssal_if_needed_sync(
        &ctx,
        90_000_002,
        ctx.now_unix(),
    );
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&90_000_002].solar_system_id),
        30_000_142
    );

    let abyss_sys = crate::services::abyssal_void_mgr::ABYSSAL_SPACE_SYSTEM_ID;
    let abyss_ship = 900_210_004;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            abyss_ship,
            InventoryItemRecord {
                item_id: abyss_ship,
                type_id: 587,
                owner_id: 90_000_002,
                location_id: abyss_sys as i64,
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
        if let Some(c) = snap.characters.get_mut(&90_000_002) {
            c.active_ship_id = abyss_ship;
            c.solar_system_id = abyss_sys;
            c.station_id = None;
            c.structure_id = None;
        }
    });
    crate::sp6_lifecycle::cluster_tick::collapse_stranded_abyssal_if_needed_sync(
        &ctx,
        90_000_002,
        ctx.now_unix(),
    );
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&abyss_ship)));
}

#[tokio::test]
async fn reload_space_objects_and_advance_disconnected_warp_expiry_and_self_destruct() {
    use crate::sp6_lifecycle::{
        on_advance_secs, populate_persisted_space_objects, with_mut_sp6_state,
        DisconnectedShipEntry, PersistedCombatHp, PersistedSpaceObjectMeta,
        SelfDestructPersistedEntry,
    };
    use eve_destiny::Ballpark;

    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let now = ctx.now_unix();
    let mwd_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 361)
        .map(|t| t.type_id)
        .unwrap_or(12198);

    let custom_hp_ship = 900_220_001;
    let anchoring_mwd_id = 900_220_002;
    let disc_expired_ship = 900_220_003;
    let disc_warping_ship = 900_220_004;
    let self_destruct_ship = 900_220_006;

    ctx.db.with_mut_snapshot(|snap| {
        for (item_id, type_id, owner_id, pos) in [
            (custom_hp_ship, 587, 90_000_001, [1_000.0, 0.0, 0.0]),
            (anchoring_mwd_id, mwd_type_id, 90_000_001, [25_000.0, 0.0, 0.0]),
            (disc_expired_ship, 587, 90_000_002, [1_000.0, 0.0, 0.0]),
            (disc_warping_ship, 587, 90_000_001, [4_000.0, 0.0, 0.0]),
            (self_destruct_ship, 587, 90_000_001, [500_000.0, 1_000.0, 0.0]),
        ] {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id, type_id, owner_id, location_id: sys_id as i64,
                    flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1,
                    ..Default::default()
                },
            );
            snap.item_positions.insert(item_id, pos);
        }
        for (cid, sid) in [(90_000_002, disc_expired_ship), (90_000_001, disc_warping_ship)] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.active_ship_id = sid;
                ch.solar_system_id = sys_id;
                ch.station_id = None;
                ch.structure_id = None;
            }
        }
    });

    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.ship_combat_hp.insert(disc_warping_ship, PersistedCombatHp { shield_hp: 123.0, armor_hp: 234.0, hull_hp: 345.0 });
        st.space_object_meta.insert(anchoring_mwd_id, PersistedSpaceObjectMeta {
            created_unix: Some(now), anchor_start_unix: Some(now), anchoring_delay_secs: 90.0,
            anchor_completed: false, bubble_radius_m: Some(18_500.0), ..Default::default()
        });
        st.disconnected_ships.insert(90_000_002, DisconnectedShipEntry {
            char_id: 90_000_002, ship_id: disc_expired_ship, ship_type_id: 587, solar_system_id: sys_id,
            disconnected_at_unix: now.saturating_sub(120), remove_at_unix: now,
            emergency_warp_dest: Some([50_000.0, 0.0, 0.0]), last_position: [1_000.0, 0.0, 0.0],
            removed_from_space: false, pre_warp_position: None,
        });
        st.disconnected_ships.insert(90_000_001, DisconnectedShipEntry {
            char_id: 90_000_001, ship_id: disc_warping_ship, ship_type_id: 587, solar_system_id: sys_id,
            disconnected_at_unix: now, remove_at_unix: now + 90,
            emergency_warp_dest: Some([5_500.0, 0.0, 0.0]), last_position: [4_000.0, 0.0, 0.0],
            removed_from_space: false, pre_warp_position: Some([4_000.0, 0.0, 0.0]),
        });
    });

    let mut ballpark = Ballpark::new(sys_id);
    let pending = populate_persisted_space_objects(&mut ballpark, sys_id, &ctx.sde, &ctx.db);
    assert!(pending.iter().any(|p| p.id == anchoring_mwd_id));
    assert!(ballpark.balls.contains_key(&disc_warping_ship));
    assert!(!ballpark.balls.contains_key(&disc_expired_ship));
    let cs = ballpark.combat_states.get(&disc_warping_ship).expect("ship combat hp");
    assert_eq!(cs.shield_hp, 123.0);
    assert_eq!(cs.armor_hp, 234.0);

    let _sys = ctx.systems.get_or_spawn(sys_id);
    with_mut_sp6_state(&ctx.db, |st, snap| {
        snap.items.insert(
            disc_expired_ship,
            InventoryItemRecord {
                item_id: disc_expired_ship, type_id: 587, owner_id: 90_000_002,
                location_id: sys_id as i64, flag: InventoryFlag::None, quantity: 1,
                singleton: true, version: 1, ..Default::default()
            },
        );
        st.disconnected_ships.insert(90_000_002, DisconnectedShipEntry {
            char_id: 90_000_002, ship_id: disc_expired_ship, ship_type_id: 587, solar_system_id: sys_id,
            disconnected_at_unix: now.saturating_sub(115), remove_at_unix: now + 5,
            emergency_warp_dest: None, last_position: [1_000.0, 0.0, 0.0],
            removed_from_space: false, pre_warp_position: None,
        });
        st.self_destructs.insert(self_destruct_ship, SelfDestructPersistedEntry {
            ship_id: self_destruct_ship, char_id: 90_000_001, solar_system_id: sys_id,
            started_unix: now.saturating_sub(120), detonate_at_unix: now + 5,
        });
    });
    ctx.db.clock().advance_secs(10);
    on_advance_secs(&ctx, now, 10);
    with_sp6_state(&ctx.db, |st| {
        assert!(st.disconnected_ships[&90_000_002].removed_from_space);
        assert!(st.disconnected_ships[&90_000_001].emergency_warp_dest.is_none());
    });
}

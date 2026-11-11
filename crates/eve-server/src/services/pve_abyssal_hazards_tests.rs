//! Unit and integration tests for `pve_abyssal_hazards`.

use super::*;
use crate::router::ServerContext;
use crate::services::abyssal_void_mgr::VOID_SPACE_SYSTEM_ID;
use crate::services::pve_special_sites::{with_abyssal_state, AbyssalTraceRecord, AbyssalWeatherKind};
use eve_db::clock::VirtualServerClock;
use eve_db::{
    FleetRecord, InventoryFlag, InventoryItemRecord, WorldRepository,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystemId, TypeId};
use glam::DVec3;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn build_ctx() -> ServerContext {
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_400_000)));
    ctx
}

fn find_ship_of_group(sde: &SdeRegistry, group_id: u32, fallback: TypeId) -> TypeId {
    sde.types
        .values()
        .find(|t| t.published && t.group_id == group_id && !t.name.contains("Blueprint"))
        .map(|t| t.type_id)
        .unwrap_or(fallback)
}

#[test]
fn test_abyssal_policy_hull_capacity_weather_math_and_dogma() {
    let ctx = build_ctx();

    // High-sec Abyssal tier / Suspect policy across security bands
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.9, 0), Ok(false));
    assert_eq!(
        evaluate_highsec_abyssal_tier_policy(0.9, 1)
            .unwrap_err()
            .code,
        "FilamentDisallowedInHighSec"
    );
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.8, 3), Ok(false));
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.8, 4), Ok(true));
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.7, 4), Ok(false));
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.7, 5), Ok(true));
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.6, 5), Ok(false));
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.6, 6), Ok(true));
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.5, 6), Ok(false));

    // Hull group & fleet capacity checks
    let frigate_tid = find_ship_of_group(&ctx.sde, 25, 587);
    let destroyer_tid = find_ship_of_group(&ctx.sde, 420, 16242);
    let cruiser_tid = find_ship_of_group(&ctx.sde, 26, 24690);
    let battleship_tid = find_ship_of_group(&ctx.sde, 27, 24692);
    ctx.db.with_mut_snapshot(|snap| {
        for (iid, tid) in [
            (101_i64, frigate_tid),
            (102, destroyer_tid),
            (103, cruiser_tid),
            (104, battleship_tid),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: 1,
                    location_id: 30_000_142,
                    flag: InventoryFlag::None,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });
    assert_eq!(validate_abyssal_hull_and_capacity(&ctx, 101).unwrap().2, 3);
    assert_eq!(validate_abyssal_hull_and_capacity(&ctx, 102).unwrap().2, 2);
    assert_eq!(validate_abyssal_hull_and_capacity(&ctx, 103).unwrap().2, 1);
    assert_eq!(
        validate_abyssal_hull_and_capacity(&ctx, 104)
            .unwrap_err()
            .code,
        "DisallowedShipType"
    );

    // Weather roll, combat/nav modifiers & dogma save/restore across all 5 weather types
    let p_low = roll_abyssal_weather_penalty_pct(&ctx, 1);
    assert!(p_low == 30.0 || p_low == 50.0);
    let p_high = roll_abyssal_weather_penalty_pct(&ctx, 5);
    assert!(p_high == 50.0 || p_high == 70.0);

    for weather in [
        AbyssalWeatherKind::Electrical,
        AbyssalWeatherKind::Dark,
        AbyssalWeatherKind::Exotic,
        AbyssalWeatherKind::Firestorm,
        AbyssalWeatherKind::Gamma,
    ] {
        let mut cs = CombatState::default();
        let mut max_v = 200.0;
        let mut opt_r = 10_000.0;
        apply_abyssal_weather_to_combat_and_nav(weather, 50.0, 50.0, &mut cs, &mut max_v, &mut opt_r);
        let mut attrs = HashMap::from([
            (271, 1.0), (267, 0.5), (113, 0.67), (55, 250_000.0),
            (37, 250.0), (54, 15_000.0), (158, 10_000.0),
            (273, 0.6), (269, 0.65), (109, 0.67), (564, 250.0),
            (265, 1_500.0), (274, 0.8), (270, 0.65), (110, 0.67),
            (263, 1_500.0), (272, 0.5), (268, 0.9), (111, 0.67),
        ]);
        apply_abyssal_weather_to_ship_attrs(weather, 50.0, 50.0, &mut attrs);
        assert!(attrs.values().any(|&v| v > 0.0));
    }

    // RPC restriction & local channel checks
    assert!(is_abyssal_system(32_000_001));
    assert!(!is_abyssal_system(30_000_142));
    assert!(is_abyssal_local_channel_disabled("solarsystemid2:32000001", None));

    let mut session = SessionState::default();
    session.solar_system_id = Some(30_000_142);
    assert!(check_abyssal_in_pocket_rpc_restrictions(&session, "map", "GetStation", &[]).is_none());

    session.solar_system_id = Some(32_000_001);
    assert!(check_abyssal_in_pocket_rpc_restrictions(&session, "map", "GetStation", &[])
        .unwrap()
        .is_err());
    assert_eq!(
        check_abyssal_in_pocket_rpc_restrictions(&session, "scanMgr", "DirectionalScan", &[])
            .unwrap()
            .unwrap_err()
            .code,
        "CannotDscanInThisSystem"
    );
    assert_eq!(
        check_abyssal_in_pocket_rpc_restrictions(
            &session,
            "LSC",
            "JoinChannel",
            &[PyRep::String("local".into())],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "LocalChannelDisabledInAbyssalSpace"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_abyssal_proximity_filament_consumption_and_spatial_clouds() {
    let ctx = build_ctx();
    let sys_id: SolarSystemId = 30_000_142;
    let char_id: i64 = 90_300_001;
    let ship_id: i64 = 1_200_001;
    let fil_id: i64 = 1_200_002;
    let cruiser_tid = find_ship_of_group(&ctx.sde, 26, 24690);
    let frigate_tid = find_ship_of_group(&ctx.sde, 25, 587);

    // Deadspace & 1,000km proximity checks
    assert_eq!(
        check_not_in_deadspace_pocket(&ctx, 32_000_001, DVec3::ZERO)
            .await
            .unwrap_err()
            .code,
        "ShipInDungeon"
    );

    // Near in-space restricted object ball (trace / deployable) rejected within 1,000km
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: 700_001,
            type_id: ABYSSAL_TRACE_TYPE_ID,
            position: DVec3::new(10_000_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 150.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        check_filament_1000km_proximity(&ctx, sys_id, DVec3::new(10_200_000.0, 0.0, 0.0))
            .await
            .unwrap_err()
            .code,
        "ProximityRestriction"
    );

    // Near existing Abyssal trace rejected
    with_abyssal_state(&ctx, |st| {
        st.traces.insert(
            800_001,
            AbyssalTraceRecord {
                trace_ball_id: 800_001,
                char_id,
                filament_type_id: 56131,
                origin_solar_system_id: sys_id,
                position: [50_000_000.0, 0.0, 0.0],
                created_unix: 1_776_400_000,
                expires_at_sec: 1_776_401_200,
                max_ships: 1,
                entered_char_ids: vec![],
                pocket_id: None,
                fleet_id: None,
            },
        );
    });
    assert_eq!(
        check_filament_1000km_proximity(&ctx, sys_id, DVec3::new(50_500_000.0, 0.0, 0.0))
            .await
            .unwrap_err()
            .code,
        "ProximityRestriction"
    );

    // Filament consumption: ungrouped frigate fails with NotInFleet, cruiser succeeds
    ctx.db.provision_gm_account("fil_pilot", "pw", char_id, "FilPilot");
    let safe_pos = [200_000_000.0, 200_000_000.0, 200_000_000.0];
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: frigate_tid,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions.insert(ship_id, safe_pos);
        snap.items.insert(
            fil_id,
            InventoryItemRecord {
                item_id: fil_id,
                type_id: 56131, // Tranquil Electrical Filament (T0)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        validate_and_consume_abyssal_filaments(&ctx, char_id, ship_id, sys_id, 0)
            .await
            .unwrap_err()
            .code,
        "MissingRequiredArgument"
    );
    assert_eq!(
        validate_and_consume_abyssal_filaments(&ctx, char_id, ship_id, sys_id, fil_id)
            .await
            .unwrap_err()
            .code,
        "NotInFleet"
    );

    // Switch to Cruiser in fleet -> consumes filament
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = cruiser_tid;
        }
        snap.fleets.insert(
            1,
            FleetRecord {
                fleet_id: 1,
                boss_id: char_id,
                members: vec![char_id],
                ..Default::default()
            },
        );
    });
    let ConsumedAbyssalFilament {
        filament_type_id: consumed_tid,
        tier,
        opener_fleet_id: fid_out,
        ..
    } = validate_and_consume_abyssal_filaments(&ctx, char_id, ship_id, sys_id, fil_id)
        .await
        .expect("cruiser T0 filament succeeds");
    assert_eq!(consumed_tid, 56131);
    assert_eq!(tier, 0);
    assert_eq!(fid_out, Some(1));

    // Spatial hazard clouds evaluation
    assert!(
        evaluate_abyssal_spatial_clouds_for_ship(&ctx, VOID_SPACE_SYSTEM_ID, ship_id)
            .await
            .is_none()
    );
    let ab_sys: SolarSystemId = 32_000_009;
    let ab_handle = ctx.systems.get_or_spawn(ab_sys);
    let _ = ab_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: cruiser_tid,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 10_000_000.0,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await;
    for (idx, cloud_tid, expected_label) in [
        (1_i64, TACHYON_CLOUD_TYPE_ID, "TachyonCloud"),
        (2_i64, BIOLUMINESCENT_CLOUD_TYPE_ID, "BioluminescentCloud"),
        (3_i64, FILAMENT_CLOUD_SMALL_TYPE_ID, "FilamentCloud"),
    ] {
        let _ = ab_handle
            .spawn_ball(Ball {
                id: 1_897_000_000 + idx,
                type_id: cloud_tid,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1.0,
                radius: 15_000.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
        let lbl = evaluate_abyssal_spatial_clouds_for_ship(&ctx, ab_sys, ship_id).await;
        assert_eq!(lbl, Some(expected_label));
        let _ = ab_handle.remove_ball(1_897_000_000 + idx).await;
    }
    assert!(evaluate_abyssal_spatial_clouds_for_ship(&ctx, ab_sys, ship_id)
        .await
        .is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abyssal_hazards_home_fallback_deadspace_prox_dark_turret_and_pvp_cloud_exclusions() {
    let (ctx, _, char_id, ship_id, sys_id) = crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session();
    let corp_id = 98_000_077_i64;
    let ab_sys = 32_000_004_u32;
    let now = ctx.now_unix();
    let ship_tid = find_ship_of_group(&ctx.sde, 26, 621);

    // resolve_character_home_station_and_system fallback via corp HQ station & missing station solar_system_id fallback
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.home_station_id = 0;
            ch.solar_system_id = sys_id;
        }
        let mut corp = snap.corporations.entry(corp_id).or_default().clone();
        corp.corporation_id = corp_id;
        corp.hq_station_id = ctx.sde.stations.keys().next().copied();
        snap.corporations.insert(corp_id, corp);
    });
    let resolved = ctx.db.with_snapshot(|snap| resolve_character_home_station_and_system(snap, &ctx.sde, char_id));
    assert!(resolved.0 > 0 && resolved.1 > 0);

    // RPC restriction channel forms solarsystemid2_ / solarsystemid_ and List args
    let mut sess = SessionState::default();
    sess.solar_system_id = Some(ab_sys);
    assert!(check_abyssal_in_pocket_rpc_restrictions(&sess, "LSC", "JoinChannel", &[PyRep::List(vec![PyRep::String("solarsystemid2_32000004".into())])]).is_some());
    assert!(is_abyssal_local_channel_disabled("solarsystemid_1", Some(ab_sys)));

    // Deadspace proximity check via exploration registry + acceleration gate (group_id == 366)
    crate::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.entry(sys_id).or_default().push(crate::services::pve_exploration::PveSiteInstance {
            site_id: "TEST-DED".into(),
            solar_system_id: sys_id,
            dungeon_name: "Test DED".into(),
            site_category: "Combat".into(),
            ded_rating: Some(4),
            position: [10_000.0, 0.0, 0.0],
            gate_ball_id: Some(7_000_001),
            total_waves: 1,
            created_unix: now,
            ..Default::default()
        });
    });
    assert_eq!(check_not_in_deadspace_pocket(&ctx, sys_id, DVec3::new(10_500.0, 0.0, 0.0)).await.unwrap_err().code, "ShipInDungeon");
    crate::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.remove(&sys_id);
    });
    if let Some(&gate_tid) = ctx.sde.types.values().find(|t| t.group_id == 366).map(|t| &t.type_id) {
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_h.spawn_ball(Ball {
            id: 7_000_002, type_id: gate_tid, position: DVec3::new(20_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO, max_velocity: 0.0, inertia: 1.0, mass: 1.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        }).await;
        assert_eq!(check_not_in_deadspace_pocket(&ctx, sys_id, DVec3::new(20_500.0, 0.0, 0.0)).await.unwrap_err().code, "ShipInDungeon");
    }

    // 1000km proximity against UpwellStructure + AbyssalTrace
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(7_000_010, eve_db::UpwellStructureRecord {
            structure_id: 7_000_010,
            type_id: 35_832,
            owner_corp_id: corp_id,
            solar_system_id: sys_id,
            name: "NearFort".into(),
            position: [50_000_000.0, 0.0, 0.0],
            state: "ShieldVulnerable".into(),
            ..Default::default()
        });
    });
    assert_eq!(check_filament_1000km_proximity(&ctx, sys_id, DVec3::new(50_000_500.0, 0.0, 0.0)).await.unwrap_err().code, "ProximityRestriction");

    // dark_weather_midpocket_turret_mult active Dark pocket + already_applied branch
    let turret_type = ctx.sde.types.values().find(|t| t.group_id == 53).unwrap();
    let mod_id = 96_000_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mod_id, InventoryItemRecord {
            item_id: mod_id, type_id: turret_type.type_id, owner_id: char_id, location_id: ship_id,
            flag: InventoryFlag::HiSlot0, quantity: 1, ..Default::default()
        });
    });
    let mk_room = |idx| crate::services::pve_special_sites::AbyssalRoomRecord {
        room_index: idx, dungeon_id: 0, dungeon_name: format!("R{idx}"), npc_ball_ids: vec![], cache_item_id: 0, cleared: false, gate_unlocked_at_sec: None,
    };
    crate::services::pve_special_sites::with_abyssal_state(&ctx, |reg| {
        let mut p = crate::services::pve_special_sites::AbyssalPocketSession {
            pocket_id: 88,
            trace_ball_id: 0,
            character_id: char_id,
            ship_id,
            fleet_ship_ids: vec![ship_id],
            origin_solar_system_id: sys_id,
            origin_position: [0.0; 3],
            grid_offset: [0.0; 3],
            abyssal_solar_system_id: ab_sys,
            filament_type_id: 47_762,
            tier: 1,
            weather: AbyssalWeatherKind::Dark,
            weather_penalty_pct: 50.0,
            weather_bonus_pct: 25.0,
            npc_difficulty_mult: 1.0,
            started_at_sec: now,
            collapse_at_sec: now + 1_200,
            current_room: 1,
            completed: false,
            collapsed_destroyed: false,
            ship_room: Default::default(),
            max_ships: 1,
            rooms: [mk_room(1), mk_room(2), mk_room(3)],
        };
        p.completed = false;
        reg.pockets.insert(char_id, p);
    });
    let mult = dark_weather_midpocket_turret_mult(&ctx, char_id, ab_sys, false, mod_id, turret_type);
    assert!((mult - 0.5).abs() < 1e-6);
    assert_eq!(dark_weather_midpocket_turret_mult(&ctx, char_id, ab_sys, true, mod_id, turret_type), 1.0);
    assert_eq!(dark_weather_midpocket_turret_mult(&ctx, char_id, sys_id, false, mod_id, turret_type), 1.0);

    // Void Space exclusion + missing snap.items fallback in evaluate_abyssal_spatial_clouds_for_ship
    let ab_h = ctx.systems.get_or_spawn(ab_sys);
    let _ = ab_h.spawn_ball(Ball {
        id: ship_id, type_id: ship_tid, position: DVec3::ZERO, velocity: DVec3::ZERO,
        max_velocity: 300.0, inertia: 1.0, mass: 1e6, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;
    // active_ship_id fallback when ship_id missing from snap.items
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = ship_id;
    });
    assert!(evaluate_abyssal_spatial_clouds_for_ship(&ctx, ab_sys, ship_id).await.is_none());

    crate::services::pve_special_sites::with_abyssal_state(&ctx, |reg| {
        reg.void_space_sessions.insert(char_id, crate::services::pve_special_sites::VoidSpaceSessionRecord {
            char_id, ship_id, origin_solar_system_id: sys_id, origin_position: [0.0; 3], target_id: 1, entered_unix: now, expires_at_sec: now + 600, ..Default::default()
        });
    });
    assert!(evaluate_abyssal_spatial_clouds_for_ship(&ctx, ab_sys, ship_id).await.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abyssal_hazards_weather_backup_sentinel_and_cloud_char_fallback_cov() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(eve_db::WorldRepository::with_demo_seed()),
    );
    let char_id = 90_000_781;
    let ship_id = 1_000_781;
    let turret_a = 1_000_782;
    let turret_b = 1_000_783;
    let ab_sys = 32_000_005;
    let now = ctx.now_unix();
    let turret_tid = ctx.sde.types.values().find(|t| t.group_id == 53).unwrap().type_id;

    let sys_handle = ctx.systems.get_or_spawn(ab_sys);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 588,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1.0e6,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = sys_handle
        .spawn_ball(Ball {
            id: 1_000_784,
            type_id: TACHYON_CLOUD_TYPE_ID,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1.0,
            radius: 15_000.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = char_id;
        ch.active_ship_id = ship_id;
        ch.solar_system_id = ab_sys;
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 588,
                owner_id: char_id,
                location_id: ab_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                ..Default::default()
            },
        );

        let mut mod_a = InventoryItemRecord {
            item_id: turret_a,
            type_id: turret_tid,
            owner_id: char_id,
            location_id: ship_id,
            flag: InventoryFlag::HiSlot0,
            quantity: 1,
            ..Default::default()
        };
        mod_a.dynamic_attributes.insert(9954, -1.0e300);
        mod_a.dynamic_attributes.insert(9958, -1.0e300);
        snap.items.insert(turret_a, mod_a);

        let mut mod_b = InventoryItemRecord {
            item_id: turret_b,
            type_id: turret_tid,
            owner_id: char_id,
            location_id: ship_id,
            flag: InventoryFlag::HiSlot1,
            quantity: 1,
            ..Default::default()
        };
        mod_b.dynamic_attributes.insert(9954, 15_000.0);
        mod_b.dynamic_attributes.insert(9958, 12_000.0);
        snap.items.insert(turret_b, mod_b);
    });

    let mut test_attrs = HashMap::from([(37, 300.0), (54, 15_000.0), (158, 12_000.0)]);
    apply_abyssal_weather_to_ship_attrs(
        AbyssalWeatherKind::Dark,
        30.0,
        20.0,
        &mut test_attrs,
    );
    assert!((test_attrs[&37] - 360.0).abs() < 1e-6);

    // Remove ship_id from snap.items -> hits snap.characters active_ship_id fallback (859-864)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&ship_id);
    });
    let mk_room = |idx| crate::services::pve_special_sites::AbyssalRoomRecord {
        room_index: idx,
        dungeon_id: 0,
        dungeon_name: format!("R{idx}"),
        npc_ball_ids: vec![],
        cache_item_id: 0,
        cleared: false,
        gate_unlocked_at_sec: None,
    };
    crate::services::pve_special_sites::with_abyssal_state(&ctx, |reg| {
        reg.pockets.insert(
            char_id,
            crate::services::pve_special_sites::AbyssalPocketSession {
                pocket_id: 999_781,
                trace_ball_id: 0,
                character_id: char_id,
                ship_id,
                fleet_ship_ids: vec![ship_id],
                origin_solar_system_id: 30_000_142,
                origin_position: [0.0; 3],
                grid_offset: [0.0; 3],
                abyssal_solar_system_id: ab_sys,
                filament_type_id: 47_762,
                tier: 1,
                weather: AbyssalWeatherKind::Dark,
                weather_penalty_pct: 50.0,
                weather_bonus_pct: 25.0,
                npc_difficulty_mult: 1.0,
                started_at_sec: now,
                collapse_at_sec: now + 1_200,
                current_room: 1,
                completed: false,
                collapsed_destroyed: false,
                ship_room: Default::default(),
                max_ships: 1,
                rooms: [mk_room(1), mk_room(2), mk_room(3)],
            },
        );
    });
    let cloud = evaluate_abyssal_spatial_clouds_for_ship(&ctx, ab_sys, ship_id).await;
    assert_eq!(cloud, Some("TachyonCloud"));
}

#[cfg(test)]
#[path = "pve_abyssal_hazards_extra_unit_tests.rs"]
mod pve_abyssal_hazards_extra_unit_tests;

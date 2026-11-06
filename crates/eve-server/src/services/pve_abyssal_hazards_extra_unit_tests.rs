//! Deep branch-coverage and mutation-killing unit tests for `pve_abyssal_hazards`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_abyssal_hazards::*;
use crate::services::pve_exploration::{with_pve_exploration_reg, PveSiteInstance};
use crate::services::pve_special_sites::{
    with_pve_special_reg, AbyssalPocketSession, AbyssalRoomRecord, VoidSpaceSessionRecord,
};
use crate::services::pve_special_sites_abyssal::AbyssalWeatherKind;
use eve_db::clock::VirtualServerClock;
use eve_db::{
    CorporationRecord, FleetRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord,
    WorldRepository,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystemId, TypeId};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn build_ctx() -> ServerContext {
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_500_000)));
    ctx
}

fn find_ship_of_group(sde: &SdeRegistry, group_id: u32, fallback: TypeId) -> TypeId {
    sde.types
        .values()
        .find(|t| t.published && t.group_id == group_id && !t.name.contains("Blueprint"))
        .map(|t| t.type_id)
        .unwrap_or(fallback)
}

fn check_pocket_rpc_code(
    session: &SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<String> {
    check_abyssal_in_pocket_rpc_restrictions(session, service, method, args)
        .and_then(|res| res.err().map(|e| e.code))
}

#[test]
fn home_station_resolution_hull_groups_highsec_tier_boundaries_and_rpc_restrictions() {
    let ctx = build_ctx();
    let valid_station_id = *ctx.sde.stations.keys().min().unwrap();
    let expected_sys = ctx.sde.stations[&valid_station_id].solar_system_id;
    let c_char = 930_001_i64;
    let c_corp_hq = 930_002_i64;
    let corp_id = 98_930_001_i64;

    ctx.db.provision_gm_account("hs1", "pw", c_char, "HomePilot1");
    ctx.db.provision_gm_account("hs2", "pw", c_corp_hq, "HomePilot2");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&c_char) {
            ch.home_station_id = valid_station_id;
        }
        if let Some(ch2) = snap.characters.get_mut(&c_corp_hq) {
            ch2.home_station_id = -1;
            ch2.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                hq_station_id: Some(valid_station_id),
                ..Default::default()
            },
        );
    });

    ctx.db.with_snapshot(|snap| {
        let (st1, sys1, const1, reg1) =
            resolve_character_home_station_and_system(snap, &ctx.sde, c_char);
        assert_eq!(st1, valid_station_id);
        assert_eq!(sys1, expected_sys);
        assert!(const1 > 0 && reg1 > 0);

        let (st2, sys2, _, _) =
            resolve_character_home_station_and_system(snap, &ctx.sde, c_corp_hq);
        assert_eq!(st2, valid_station_id);
        assert_eq!(sys2, expected_sys);

        // Unknown character falls back to min SDE station
        let (st3, sys3, _, _) =
            resolve_character_home_station_and_system(snap, &ctx.sde, 999_999_999);
        assert_eq!(st3, valid_station_id);
        assert_eq!(sys3, expected_sys);
    });

    // Exact security boundaries in evaluate_highsec_abyssal_tier_policy
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.85, 0), Ok(false));
    assert_eq!(
        evaluate_highsec_abyssal_tier_policy(0.85, 1)
            .unwrap_err()
            .code,
        "FilamentDisallowedInHighSec"
    );
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.849, 3), Ok(false));
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.849, 4), Ok(true));
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.75, 3), Ok(false));
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.75, 4), Ok(true));
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.749, 4), Ok(false));
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.749, 5), Ok(true));
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.65, 4), Ok(false));
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.65, 5), Ok(true));
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.649, 5), Ok(false));
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.649, 6), Ok(true));
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.55, 5), Ok(false));
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.55, 6), Ok(true));
    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.549, 6), Ok(false));

    // Abyssal system and local channel helper boundaries
    assert!(!is_abyssal_system(31_999_999));
    assert!(is_abyssal_system(32_000_000));
    assert!(is_abyssal_system(32_999_999));
    assert!(!is_abyssal_system(33_000_000));

    assert!(is_abyssal_local_channel_disabled("solarsystemid2_32000050", None));
    assert!(is_abyssal_local_channel_disabled("solarsystemid_32000050", None));
    assert!(!is_abyssal_local_channel_disabled("solarsystemid2:30000142", None));
    assert!(is_abyssal_local_channel_disabled("LOCAL", Some(32_000_001)));
    assert!(!is_abyssal_local_channel_disabled("corp", Some(32_000_001)));

    let mut sess = SessionState::default();
    sess.solar_system_id = Some(32_000_005);
    assert_eq!(
        check_pocket_rpc_code(&sess, "map", "GetSolarSystem", &[]).as_deref(),
        Some("MapDisabledInAbyssalSpace")
    );
    assert_eq!(
        check_pocket_rpc_code(&sess, "scanSvc", "ConeScan", &[]).as_deref(),
        Some("CannotDscanInThisSystem")
    );
    assert_eq!(
        check_pocket_rpc_code(
            &sess,
            "lsc",
            "SendMessage",
            &[PyRep::List(vec![PyRep::String("local".into())])]
        )
        .as_deref(),
        Some("LocalChannelDisabledInAbyssalSpace")
    );
    assert_eq!(
        check_pocket_rpc_code(
            &sess,
            "XmppChatMgr",
            "AddCharacterToChannel",
            &[PyRep::Tuple(vec![PyRep::String("solarsystemid_32000005".into())])]
        )
        .as_deref(),
        Some("LocalChannelDisabledInAbyssalSpace")
    );
    assert!(
        check_pocket_rpc_code(&sess, "LSC", "SendMessage", &[PyRep::String("fleet".into())])
            .is_none()
    );
}

#[test]
fn turret_detection_dark_weather_midpocket_mult_and_cloud_radius_modifiers() {
    let ctx = build_ctx();
    let char_id = 931_001_i64;
    let ship_id = 931_100_i64;
    let turret_id = 931_101_i64;
    let ab_sys: SolarSystemId = 32_000_010;

    let cruiser_tid = find_ship_of_group(&ctx.sde, 26, 24690);
    let turret_tid = ctx
        .sde
        .types
        .values()
        .find(|t| matches!(t.group_id, 53 | 55 | 74))
        .map(|t| t.type_id)
        .unwrap();
    let turret_sde = ctx.sde.types.get(&turret_tid).unwrap().clone();
    assert!(is_gunnery_turret_type(&turret_sde));

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: cruiser_tid,
                owner_id: char_id,
                location_id: ab_sys as i64,
                flag: InventoryFlag::None,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            turret_id,
            InventoryItemRecord {
                item_id: turret_id,
                type_id: turret_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    // Missile launcher or K-space returns 1.0
    assert_eq!(
        dark_weather_midpocket_turret_mult(&ctx, char_id, ab_sys, true, turret_id, &turret_sde),
        1.0
    );
    assert_eq!(
        dark_weather_midpocket_turret_mult(&ctx, char_id, 30_000_142, false, turret_id, &turret_sde),
        1.0
    );

    // Seed active Dark pocket with 50% penalty -> midpocket mult == 0.5 before dogma apply, 1.0 after dogma apply
    with_pve_special_reg(&ctx, |reg| {
        let empty_room = || AbyssalRoomRecord {
            room_index: 1,
            dungeon_id: 0,
            dungeon_name: String::new(),
            npc_ball_ids: Vec::new(),
            cache_item_id: 0,
            cleared: false,
            gate_unlocked_at_sec: None,
        };
        reg.abyssal_pockets.insert(
            char_id,
            AbyssalPocketSession {
                pocket_id: 7,
                character_id: char_id,
                ship_id,
                origin_solar_system_id: 30_000_142,
                abyssal_solar_system_id: ab_sys,
                origin_position: [0.0; 3],
                grid_offset: [0.0; 3],
                filament_type_id: 56132,
                tier: 3,
                weather: AbyssalWeatherKind::Dark,
                weather_penalty_pct: 50.0,
                weather_bonus_pct: 50.0,
                npc_difficulty_mult: 1.0,
                current_room: 1,
                rooms: [empty_room(), empty_room(), empty_room()],
                started_at_sec: 1_776_500_000,
                collapse_at_sec: 1_776_501_200,
                saved_ship_attrs: HashMap::new(),
                completed: false,
                collapsed_destroyed: false,
                trace_ball_id: 1,
                fleet_ship_ids: vec![ship_id],
                ship_room: HashMap::new(),
                max_ships: 1,
            },
        );
    });

    assert!(
        (dark_weather_midpocket_turret_mult(&ctx, char_id, ab_sys, false, turret_id, &turret_sde)
            - 0.5)
            .abs()
            < 1e-6
    );

    let saved = apply_abyssal_weather_dogma_modifiers_to_ship(
        &ctx,
        ship_id,
        AbyssalWeatherKind::Dark,
        50.0,
        50.0,
    );
    // Second application uses backup_key 9954/9958 without compounding
    let _ = apply_abyssal_weather_dogma_modifiers_to_ship(
        &ctx,
        ship_id,
        AbyssalWeatherKind::Dark,
        50.0,
        50.0,
    );
    assert_eq!(
        dark_weather_midpocket_turret_mult(&ctx, char_id, ab_sys, false, turret_id, &turret_sde),
        1.0
    );
    restore_abyssal_weather_dogma_modifiers(&ctx, ship_id, &saved);
    ctx.db.with_snapshot(|snap| {
        let t = &snap.items[&turret_id];
        assert!(!t.dynamic_attributes.contains_key(&9954));
        assert!(!t.dynamic_attributes.contains_key(&9958));
    });

    // Cloud radius & modifier coverage across all cloud types
    for (ctid, exp_rad, exp_lbl) in [
        (BIOLUMINESCENT_CLOUD_SMALL_TYPE_ID, 5_000.0, Some("BioluminescentCloud")),
        (FILAMENT_CLOUD_SMALL_TYPE_ID, 5_000.0, Some("FilamentCloud")),
        (TACHYON_CLOUD_TYPE_ID, 15_000.0, Some("TachyonCloud")),
        (BIOLUMINESCENT_CLOUD_TYPE_ID, 15_000.0, Some("BioluminescentCloud")),
        (FILAMENT_CLOUD_TYPE_ID, 15_000.0, Some("FilamentCloud")),
        (PROVING_TACHYON_CLOUD_TYPE_ID, 40_000.0, Some("TachyonCloud")),
        (BIOLUMINESCENT_CLOUD_LARGE_TYPE_ID, 40_000.0, Some("BioluminescentCloud")),
        (FILAMENT_CLOUD_LARGE_TYPE_ID, 40_000.0, Some("FilamentCloud")),
    ] {
        assert!(is_spatial_hazard_cloud_type(ctid));
        assert_eq!(spatial_cloud_radius_m(None, ctid, 1_000.0), exp_rad);
        assert_eq!(cloud_modifiers_for_type(Some(ctid)).5, exp_lbl);
    }
    assert!(!is_spatial_hazard_cloud_type(34));
    assert_eq!(spatial_cloud_radius_m(None, 34, 25_000.0), 25_000.0);
    assert_eq!(cloud_modifiers_for_type(None).5, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deadspace_gate_proximity_station_structure_checks_and_filament_error_branches() {
    let ctx = build_ctx();
    let sys_id: SolarSystemId = 30_000_142;
    let char_id = 932_001_i64;
    let ship_id = 932_100_i64;
    let frigate_tid = find_ship_of_group(&ctx.sde, 25, 587);

    // 1. Deadspace exploration site gate within 250 km rejects check_not_in_deadspace_pocket
    with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system.insert(
            sys_id,
            vec![PveSiteInstance {
                site_id: "DED-1".into(),
                solar_system_id: sys_id,
                site_category: "Combat".into(),
                gate_ball_id: Some(777_001),
                position: [50_000_000.0, 0.0, 0.0],
                ..Default::default()
            }],
        );
    });
    assert_eq!(
        check_not_in_deadspace_pocket(&ctx, sys_id, DVec3::new(50_100_000.0, 0.0, 0.0))
            .await
            .unwrap_err()
            .code,
        "ShipInDungeon"
    );

    // 2. Acceleration gate ball (group 366) in space within 250 km rejects check_not_in_deadspace_pocket
    let accel_gate_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 366)
        .map(|t| t.type_id)
        .unwrap();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: 777_002,
            type_id: accel_gate_tid,
            position: DVec3::new(80_000_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1e6,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        check_not_in_deadspace_pocket(&ctx, sys_id, DVec3::new(80_100_000.0, 0.0, 0.0))
            .await
            .unwrap_err()
            .code,
        "ShipInDungeon"
    );
    let _ = sys_handle.remove_ball(777_002).await;

    // 3. Station, stargate, and Upwell structure 1,000 km proximity checks
    let st_pos = ctx.sde.solar_systems[&sys_id].stations[0].position;
    assert_eq!(
        check_filament_1000km_proximity(&ctx, sys_id, st_pos + DVec3::new(10_000.0, 0.0, 0.0))
            .await
            .unwrap_err()
            .code,
        "ProximityRestriction"
    );
    let jump_pos = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == sys_id)
        .unwrap()
        .position;
    assert_eq!(
        check_filament_1000km_proximity(&ctx, sys_id, jump_pos + DVec3::new(50_000.0, 0.0, 0.0))
            .await
            .unwrap_err()
            .code,
        "ProximityRestriction"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            1_000_000_932_001,
            UpwellStructureRecord {
                structure_id: 1_000_000_932_001,
                type_id: 35_832,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                name: "Near Struct".into(),
                position: [120_000_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
    assert_eq!(
        check_filament_1000km_proximity(&ctx, sys_id, DVec3::new(120_500_000.0, 0.0, 0.0))
            .await
            .unwrap_err()
            .code,
        "ProximityRestriction"
    );

    // 4. validate_and_consume_abyssal_filaments: cloaked, tethered, insufficient quantity, invalid filament, stack decrement
    let safe_pos = DVec3::new(400_000_000.0, 400_000_000.0, 400_000_000.0);
    ctx.db.provision_gm_account("fil2", "pw", char_id, "FilPilot2");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
        }
        snap.fleets.insert(
            77,
            FleetRecord {
                fleet_id: 77,
                boss_id: char_id,
                members: vec![char_id],
                ..Default::default()
            },
        );
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
    });
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: frigate_tid,
                position: safe_pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1e6,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                is_cloaked: true,
                ..CombatState::default()
            },
        )
        .await;
    assert_eq!(
        validate_and_consume_abyssal_filaments(&ctx, char_id, ship_id, sys_id, 56131)
            .await
            .unwrap_err()
            .code,
        "ShipIsCloaked"
    );

    sys_handle.try_update_combat_state(ship_id, |cs| {
        cs.is_cloaked = false;
        cs.is_tethered = true;
    });
    assert_eq!(
        validate_and_consume_abyssal_filaments(&ctx, char_id, ship_id, sys_id, 56131)
            .await
            .unwrap_err()
            .code,
        "ShipIsTethered"
    );

    sys_handle.try_update_combat_state(ship_id, |cs| {
        cs.is_tethered = false;
    });
    // Missing item in cargo -> ItemNotFound
    assert_eq!(
        validate_and_consume_abyssal_filaments(&ctx, char_id, ship_id, sys_id, 56131)
            .await
            .unwrap_err()
            .code,
        "ItemNotFound"
    );

    let fil_item_id = 932_200_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fil_item_id,
            InventoryItemRecord {
                item_id: fil_item_id,
                type_id: 56131,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2, // Frigate requires 3!
                ..Default::default()
            },
        );
    });
    assert_eq!(
        validate_and_consume_abyssal_filaments(&ctx, char_id, ship_id, sys_id, fil_item_id)
            .await
            .unwrap_err()
            .code,
        "InsufficientFilamentQuantity"
    );

    // Non-filament type -> InvalidAbyssalFilament
    ctx.db.with_mut_snapshot(|snap| {
        let it = snap.items.get_mut(&fil_item_id).unwrap();
        it.type_id = 34; // Tritanium
        it.quantity = 5;
    });
    assert_eq!(
        validate_and_consume_abyssal_filaments(&ctx, char_id, ship_id, sys_id, fil_item_id)
            .await
            .unwrap_err()
            .code,
        "InvalidAbyssalFilament"
    );

    // Valid T0 filament stack of 5 -> consumes 3 for Frigate and leaves 2
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&fil_item_id).unwrap().type_id = 56131;
    });
    let (_, _, _, max_s, _, _, _) =
        validate_and_consume_abyssal_filaments(&ctx, char_id, ship_id, sys_id, fil_item_id)
            .await
            .unwrap();
    assert_eq!(max_s, 3);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&fil_item_id].quantity, 2);
    });

    // Excluded void-space session ship returns None in evaluate_abyssal_spatial_clouds_for_ship
    with_pve_special_reg(&ctx, |reg| {
        reg.void_space_sessions.insert(
            char_id,
            VoidSpaceSessionRecord {
                char_id,
                ship_id,
                origin_solar_system_id: sys_id,
                origin_position: [0.0; 3],
                target_id: 0,
                entered_unix: 1_776_500_000,
                expires_at_sec: 1_776_501_200,
            },
        );
    });
    assert!(
        evaluate_abyssal_spatial_clouds_for_ship(&ctx, 32_000_010, ship_id)
            .await
            .is_none()
    );
}

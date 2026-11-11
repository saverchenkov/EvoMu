//! Deep branch-coverage and mutation-killing unit tests for `pve_special_sites_abyssal`,
//! `pve_abyssal_hazards`, and `pve_abyssal_lifecycle`.

use crate::router::ServerContext;
use crate::services::abyssal_void_mgr::VOID_SPACE_SYSTEM_ID;
use crate::services::pve_abyssal_hazards::{
    apply_abyssal_weather_to_combat_and_nav, apply_abyssal_weather_to_ship_attrs,
    evaluate_abyssal_spatial_clouds_for_ship,
    evaluate_highsec_abyssal_tier_policy, roll_abyssal_weather_penalty_pct, spatial_cloud_radius_m,
    BIOLUMINESCENT_CLOUD_LARGE_TYPE_ID, BIOLUMINESCENT_CLOUD_SMALL_TYPE_ID,
    BIOLUMINESCENT_CLOUD_TYPE_ID, FILAMENT_CLOUD_LARGE_TYPE_ID, FILAMENT_CLOUD_SMALL_TYPE_ID,
    FILAMENT_CLOUD_TYPE_ID, PROVING_TACHYON_CLOUD_TYPE_ID, TACHYON_CLOUD_TYPE_ID,
};
use crate::services::pve_special_sites::AbyssalWeatherKind;
use crate::services::pve_special_sites_abyssal::{
    abyssal_mutaplasmid_for_tier, abyssal_room_gate_type, classify_abyssal_filament,
};
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_sde::{SdeRegistry, SolarSystemId, TypeId};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

pub(super) fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

pub(super) fn new_ctx() -> ServerContext {
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_785_000_000)));
    ctx
}

pub(super) fn make_item(
    item_id: i64,
    type_id: TypeId,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton: quantity == 1 && flag != InventoryFlag::Cargo,
        version: 1,
        ..Default::default()
    }
}

pub(super) fn make_ball(id: i64, type_id: TypeId, position: DVec3, radius: f64) -> Ball {
    Ball {
        id,
        type_id,
        position,
        velocity: DVec3::ZERO,
        max_velocity: 200.0,
        inertia: 0.6,
        mass: 10_000_000.0,
        radius,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

#[test]
fn deep_classify_filament_all_tiers_mutaplasmid_gate_and_highsec_policy() {
    let cases: &[(TypeId, u8, AbyssalWeatherKind)] = &[
        (56131, 0, AbyssalWeatherKind::Electrical),
        (56132, 0, AbyssalWeatherKind::Dark),
        (56133, 0, AbyssalWeatherKind::Exotic),
        (56134, 0, AbyssalWeatherKind::Firestorm),
        (56136, 0, AbyssalWeatherKind::Gamma),
        (47761, 1, AbyssalWeatherKind::Exotic),
        (47762, 1, AbyssalWeatherKind::Dark),
        (47763, 1, AbyssalWeatherKind::Firestorm),
        (47764, 1, AbyssalWeatherKind::Gamma),
        (47765, 1, AbyssalWeatherKind::Electrical),
        (47888, 2, AbyssalWeatherKind::Exotic),
        (47892, 2, AbyssalWeatherKind::Dark),
        (47896, 2, AbyssalWeatherKind::Firestorm),
        (47900, 2, AbyssalWeatherKind::Gamma),
        (47904, 2, AbyssalWeatherKind::Electrical),
        (47889, 3, AbyssalWeatherKind::Exotic),
        (47893, 3, AbyssalWeatherKind::Dark),
        (47897, 3, AbyssalWeatherKind::Firestorm),
        (47901, 3, AbyssalWeatherKind::Gamma),
        (47905, 3, AbyssalWeatherKind::Electrical),
        (47890, 4, AbyssalWeatherKind::Exotic),
        (47894, 4, AbyssalWeatherKind::Dark),
        (47898, 4, AbyssalWeatherKind::Firestorm),
        (47902, 4, AbyssalWeatherKind::Gamma),
        (47906, 4, AbyssalWeatherKind::Electrical),
        (47891, 5, AbyssalWeatherKind::Exotic),
        (47895, 5, AbyssalWeatherKind::Dark),
        (47899, 5, AbyssalWeatherKind::Firestorm),
        (47903, 5, AbyssalWeatherKind::Gamma),
        (47907, 5, AbyssalWeatherKind::Electrical),
        (56139, 6, AbyssalWeatherKind::Electrical),
        (56140, 6, AbyssalWeatherKind::Dark),
        (56141, 6, AbyssalWeatherKind::Exotic),
        (56142, 6, AbyssalWeatherKind::Firestorm),
        (56143, 6, AbyssalWeatherKind::Gamma),
    ];
    for &(tid, expected_tier, expected_weather) in cases {
        assert_eq!(
            classify_abyssal_filament(tid),
            Some((expected_tier, expected_weather)),
            "filament {tid} mismatch"
        );
    }
    for invalid in [0, 47760, 47770, 56130, 56135, 56137, 56138, 56144] {
        assert_eq!(classify_abyssal_filament(invalid), None);
    }

    assert_eq!(abyssal_mutaplasmid_for_tier(0), 47_699);
    assert_eq!(abyssal_mutaplasmid_for_tier(2), 47_699);
    assert_eq!(abyssal_mutaplasmid_for_tier(3), 47_701);
    assert_eq!(abyssal_mutaplasmid_for_tier(4), 47_701);
    assert_eq!(abyssal_mutaplasmid_for_tier(5), 47_700);
    assert_eq!(abyssal_mutaplasmid_for_tier(6), 47_700);

    assert_eq!(abyssal_room_gate_type(1), 47_685);
    assert_eq!(abyssal_room_gate_type(2), 47_685);
    assert_eq!(abyssal_room_gate_type(3), 47_686);

    let ctx = new_ctx();
    for t in 0..=3 {
        let p = roll_abyssal_weather_penalty_pct(&ctx, t);
        assert!(p == 30.0 || p == 50.0, "tier {t} rolled {p}");
    }
    for t in 4..=6 {
        let p = roll_abyssal_weather_penalty_pct(&ctx, t);
        assert!(p == 50.0 || p == 70.0, "tier {t} rolled {p}");
    }

    assert_eq!(evaluate_highsec_abyssal_tier_policy(0.85, 0).unwrap(), false);
    assert_eq!(
        evaluate_highsec_abyssal_tier_policy(0.85, 1).unwrap_err().code,
        "FilamentDisallowedInHighSec"
    );
    for (sec, tier, expected) in [
        (0.84, 3, false),
        (0.84, 4, true),
        (0.75, 3, false),
        (0.75, 4, true),
        (0.74, 4, false),
        (0.65, 4, false),
        (0.65, 5, true),
        (0.64, 5, false),
        (0.55, 5, false),
        (0.55, 6, true),
        (0.54, 6, false),
    ] {
        assert_eq!(evaluate_highsec_abyssal_tier_policy(sec, tier).unwrap(), expected);
    }
}

#[test]
fn deep_weather_combat_nav_and_ship_dogma_modifiers_and_restore_all_weathers() {
    let ctx = new_ctx();
    let char_id = 90_301_001;
    let ship_id = 1_301_000_001;
    let turret_id = 1_301_000_002;
    let cruiser_tid = ctx.sde.types.values().find(|t| t.group_id == 26).unwrap().type_id;
    let turret_tid = ctx
        .sde
        .types
        .values()
        .find(|t| matches!(t.group_id, 53 | 55 | 74))
        .unwrap()
        .type_id;

    let mut cs = CombatState::new();
    cs.max_shield_hp = 1000.0;
    cs.shield_hp = 1000.0;
    cs.max_armor_hp = 2000.0;
    cs.armor_hp = 2000.0;
    cs.max_structure_hp = 1500.0;
    cs.structure_hp = 1500.0;
    cs.shield_resonance = [0.5, 0.5, 0.5, 0.5];
    cs.armor_resonance = [0.4, 0.4, 0.4, 0.4];
    cs.structure_resonance = [0.6, 0.6, 0.6, 0.6];
    cs.capacitor_recharge_time_ms = 10_000.0;
    let (mut vel, mut opt) = (200.0, 10_000.0);

    apply_abyssal_weather_to_combat_and_nav(AbyssalWeatherKind::Electrical, 50.0, 50.0, &mut cs, &mut vel, &mut opt);
    assert!((cs.shield_resonance[0] - 0.75).abs() < 1e-9);
    assert!((cs.armor_resonance[0] - 0.60).abs() < 1e-9);
    assert!((cs.structure_resonance[0] - 0.90).abs() < 1e-9);
    assert!((cs.capacitor_recharge_time_ms - 5_000.0).abs() < 1e-9);

    apply_abyssal_weather_to_combat_and_nav(AbyssalWeatherKind::Dark, 95.0, 50.0, &mut cs, &mut vel, &mut opt);
    assert!((vel - 300.0).abs() < 1e-9);
    assert!((opt - 1_000.0).abs() < 1e-9);

    apply_abyssal_weather_to_combat_and_nav(AbyssalWeatherKind::Exotic, 50.0, 50.0, &mut cs, &mut vel, &mut opt);
    assert!((cs.shield_resonance[2] - 0.75).abs() < 1e-9);
    assert!((cs.armor_resonance[2] - 0.60).abs() < 1e-9);
    assert!((cs.structure_resonance[2] - 0.90).abs() < 1e-9);

    apply_abyssal_weather_to_combat_and_nav(AbyssalWeatherKind::Firestorm, 50.0, 50.0, &mut cs, &mut vel, &mut opt);
    assert!((cs.max_armor_hp - 3000.0).abs() < 1e-9);
    assert!((cs.armor_hp - 3000.0).abs() < 1e-9);
    assert!((cs.shield_resonance[1] - 0.75).abs() < 1e-9);

    apply_abyssal_weather_to_combat_and_nav(AbyssalWeatherKind::Gamma, 50.0, 50.0, &mut cs, &mut vel, &mut opt);
    assert!((cs.max_shield_hp - 1500.0).abs() < 1e-9);
    assert!((cs.shield_hp - 1500.0).abs() < 1e-9);
    assert!((cs.shield_resonance[3] - 0.75).abs() < 1e-9);

    ctx.db.with_mut_snapshot(|snap| {
        let mut sh = make_item(ship_id, cruiser_tid, char_id, 32_000_010, InventoryFlag::None, 1);
        sh.dynamic_attributes = HashMap::from([(37, 240.0), (54, 20_000.0)]);
        snap.items.insert(ship_id, sh);
        let mut tur = make_item(turret_id, turret_tid, char_id, ship_id, InventoryFlag::HiSlot0, 1);
        tur.dynamic_attributes = HashMap::from([(54, 12_000.0), (158, 8_000.0)]);
        snap.items.insert(turret_id, tur);
    });

    for weather in [
        AbyssalWeatherKind::Electrical,
        AbyssalWeatherKind::Exotic,
        AbyssalWeatherKind::Firestorm,
        AbyssalWeatherKind::Gamma,
        AbyssalWeatherKind::Dark,
    ] {
        let mut attrs = HashMap::from([
            (37, 240.0), (54, 20_000.0), (158, 10_000.0),
            (271, 1.0), (267, 0.5), (113, 0.67), (55, 250_000.0),
            (273, 0.6), (269, 0.65), (109, 0.67), (564, 250.0),
            (265, 1_500.0), (274, 0.8), (270, 0.65), (110, 0.67),
            (263, 1_500.0), (272, 0.5), (268, 0.9), (111, 0.67),
        ]);
        apply_abyssal_weather_to_ship_attrs(weather, 50.0, 50.0, &mut attrs);
        if weather == AbyssalWeatherKind::Dark {
            assert!((attrs[&37] - 360.0).abs() < 1e-6);
            assert!((attrs[&54] - 10_000.0).abs() < 1e-6);
            assert!((attrs[&158] - 5_000.0).abs() < 1e-6);
        }
    }

    ctx.db.with_snapshot(|snap| {
        let eff_attr = |id: i64, aid: u16| {
            let it = &snap.items[&id];
            it.dynamic_attributes.get(&aid).copied().unwrap_or_else(|| {
                ctx.sde.types.get(&it.type_id).and_then(|t| t.default_attributes.get(&aid).copied()).unwrap_or(0.0)
            })
        };
        assert!((eff_attr(ship_id, 37) - 240.0).abs() < 1e-6);
        assert!((eff_attr(ship_id, 54) - 20_000.0).abs() < 1e-6);
        assert!((eff_attr(turret_id, 54) - 12_000.0).abs() < 1e-6);
        assert!((eff_attr(turret_id, 158) - 8_000.0).abs() < 1e-6);
        let tur = &snap.items[&turret_id];
        assert!(!tur.dynamic_attributes.contains_key(&9954));
        assert!(!tur.dynamic_attributes.contains_key(&9958));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deep_spatial_hazard_clouds_radius_and_ballpark_modifiers() {
    let ctx = new_ctx();
    for (tid, fb, expected) in [
        (BIOLUMINESCENT_CLOUD_SMALL_TYPE_ID, 0.0, 5_000.0),
        (FILAMENT_CLOUD_SMALL_TYPE_ID, 0.0, 5_000.0),
        (TACHYON_CLOUD_TYPE_ID, 0.0, 15_000.0),
        (BIOLUMINESCENT_CLOUD_TYPE_ID, 0.0, 15_000.0),
        (FILAMENT_CLOUD_TYPE_ID, 0.0, 15_000.0),
        (PROVING_TACHYON_CLOUD_TYPE_ID, 0.0, 40_000.0),
        (BIOLUMINESCENT_CLOUD_LARGE_TYPE_ID, 0.0, 40_000.0),
        (FILAMENT_CLOUD_LARGE_TYPE_ID, 0.0, 40_000.0),
        (999_999, 22_000.0, 22_000.0),
    ] {
        assert_eq!(spatial_cloud_radius_m(None, tid, fb), expected);
    }

    let ab_sys: SolarSystemId = 32_000_111;
    let char_id: i64 = 90_302_001;
    let ship_id: i64 = 1_302_000_001;
    let cruiser_tid = ctx.sde.types.values().find(|t| t.group_id == 26).unwrap().type_id;
    ctx.db.provision_gm_account("cloud_pilot", "pw", char_id, "CloudPilot");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = ab_sys;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            make_item(ship_id, cruiser_tid, char_id, ab_sys as i64, InventoryFlag::None, 1),
        );
    });

    assert_eq!(
        evaluate_abyssal_spatial_clouds_for_ship(&ctx, VOID_SPACE_SYSTEM_ID, ship_id).await,
        None
    );

    let sys_handle = ctx.systems.get_or_spawn(ab_sys);
    let base_cs = ctx.build_ship_combat_state(char_id, ship_id, cruiser_tid);
    let base_sig = base_cs.signature_radius;
    let _ = sys_handle
        .spawn_ball_with_combat(make_ball(ship_id, cruiser_tid, DVec3::ZERO, 120.0), base_cs)
        .await;

    let cloud_id = 1_890_000_001;
    let _ = sys_handle
        .spawn_ball(make_ball(cloud_id, TACHYON_CLOUD_TYPE_ID, DVec3::new(1_000.0, 0.0, 0.0), 15_000.0))
        .await;
    assert_eq!(
        evaluate_abyssal_spatial_clouds_for_ship(&ctx, ab_sys, ship_id).await,
        Some("TachyonCloud")
    );
    let cs_tach = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((cs_tach.signature_radius - base_sig * 4.0).abs() < 1e-3);

    let _ = sys_handle.remove_ball(cloud_id).await;
    let _ = sys_handle
        .spawn_ball(make_ball(cloud_id, FILAMENT_CLOUD_TYPE_ID, DVec3::new(1_000.0, 0.0, 0.0), 15_000.0))
        .await;
    assert_eq!(
        evaluate_abyssal_spatial_clouds_for_ship(&ctx, ab_sys, ship_id).await,
        Some("FilamentCloud")
    );
    let cs_fil = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((cs_fil.shield_boost_multiplier - 0.6).abs() < 1e-6);

    let _ = sys_handle.remove_ball(cloud_id).await;
    assert_eq!(
        evaluate_abyssal_spatial_clouds_for_ship(&ctx, ab_sys, ship_id).await,
        None
    );
    let cs_clear = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((cs_clear.shield_boost_multiplier - 1.0).abs() < 1e-6);
}

#[tokio::test]
async fn mutants_cov_abyssal_state_room_unlock_pocket_entry_and_deployment() {
    use crate::services::pve_special_sites_abyssal::*;
    use eve_db::{AbyssalPocketSession, AbyssalRoomRecord, AbyssalTraceRecord, InventoryFlag};
    use eve_destiny::{Ball, Ballpark, CombatState, DestinyEvent, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;

    let ctx = new_ctx();
    let char_a = 90_309_001i64;
    let char_b = 90_309_002i64;
    let char_c = 90_309_003i64;
    let ship_a = 1_309_000_001i64;
    let ship_b = 1_309_000_002i64;
    let ship_c = 1_309_000_003i64;
    let filament_a = 1_309_000_010i64;
    let cruiser_tid = ctx.sde.types.values().find(|t| t.group_id == 26).unwrap().type_id;
    let origin_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.solar_system_id >= 30_000_000 && s.solar_system_id < 31_000_000 && s.security_status < 0.5)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_505);
    let far_pos = DVec3::new(9_000_000_000.0, 9_000_000_000.0, 9_000_000_000.0);

    // 1. next_abyssal_seq, read_abyssal_state, with_abyssal_state, with_abyssal_state_db
    let s1 = with_abyssal_state_db(&ctx.db, |st| next_abyssal_seq(st));
    let s2 = with_abyssal_state(&ctx, |st| next_abyssal_seq(st));
    assert!(s1 >= 1);
    assert_eq!(s2, s1 + 1);
    assert_eq!(read_abyssal_state(&ctx, |st| st.next_seq), s2);

    // 2. rpc_err via handle_abyssal_entrance_deployment guards (docked + missing arg)
    let mut docked_sess = SessionState {
        user_id: Some(char_a as u64),
        character_id: Some(char_a),
        solar_system_id: Some(origin_sys),
        station_id: Some(60_003_760),
        ship_id: Some(ship_a),
        ..Default::default()
    };
    let docked_err = handle_abyssal_entrance_deployment(&ctx, &mut docked_sess, &[PyRep::Integer(filament_a)])
        .await
        .unwrap_err();
    assert_eq!(docked_err.code, "ShipDocked");
    assert!(!docked_err.message.is_empty());

    // 3. Seed ships + filament -> handle_abyssal_entrance_deployment happy path
    for cid in [char_a, char_b, char_c] {
        ctx.db.provision_gm_account(
            &format!("ab_dep_{cid}"),
            "pw",
            cid,
            &format!("AbyssPilot_{cid}"),
        );
    }
    ctx.db.with_mut_snapshot(|snap| {
        for (cid, sid) in [(char_a, ship_a), (char_b, ship_b), (char_c, ship_c)] {
            let ch = snap.characters.get_mut(&cid).expect("provisioned char");
            ch.solar_system_id = origin_sys;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = sid;
            snap.items.insert(sid, make_item(sid, cruiser_tid, cid, i64::from(origin_sys), InventoryFlag::None, 1));
            snap.item_positions.insert(sid, [far_pos.x, far_pos.y, far_pos.z]);
        }
        snap.items.insert(filament_a, make_item(filament_a, 56131, char_a, ship_a, InventoryFlag::Cargo, 1));
    });

    let origin_handle = ctx.systems.get_or_spawn(origin_sys);
    let _ = origin_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_a,
                type_id: cruiser_tid,
                position: far_pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 1000.0,
                max_shield_hp: 1000.0,
                armor_hp: 1000.0,
                max_armor_hp: 1000.0,
                structure_hp: 1000.0,
                max_structure_hp: 1000.0,
                capacitor: 1000.0,
                max_capacitor: 1000.0,
                ..Default::default()
            },
        )
        .await;

    let mut space_sess_a = SessionState {
        user_id: Some(char_a as u64),
        character_id: Some(char_a),
        solar_system_id: Some(origin_sys),
        ship_id: Some(ship_a),
        ..Default::default()
    };
    let arg_err = handle_abyssal_entrance_deployment(&ctx, &mut space_sess_a, &[PyRep::Integer(0)])
        .await
        .unwrap_err();
    assert_eq!(arg_err.code, "MissingRequiredArgument");

    let deploy_ok = handle_abyssal_entrance_deployment(&ctx, &mut space_sess_a, &[PyRep::Integer(filament_a)])
        .await
        .expect("deployment ok");
    let trace_id = deploy_ok.dict_get("traceBallID").and_then(PyRep::as_i64).unwrap();
    assert!(trace_id > 1_897_000_000);
    assert_eq!(deploy_ok.dict_get("solarSystemID").and_then(PyRep::as_i64), Some(i64::from(origin_sys)));
    assert_eq!(deploy_ok.dict_get("filamentTypeID").and_then(PyRep::as_i64), Some(56131));
    assert_eq!(deploy_ok.dict_get("tier").and_then(PyRep::as_i64), Some(0));
    assert_eq!(deploy_ok.dict_get("maxShips").and_then(PyRep::as_i64), Some(1));
    let trace_ball = origin_handle.get_ball_snapshot(trace_id).await.unwrap().expect("trace ball spawned").1;
    assert!((trace_ball.position - (far_pos + DVec3::new(500.0, 0.0, 0.0))).length() < 1e-6);
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&filament_a)));

    // 4. enter_abyssal_pocket_with_trace: trace_ball_id == Some(0) ignored, tier diff_mult, pocket_grid_offset, and fleet trace reuse
    let (pid_a, npcs_a) = enter_abyssal_pocket_with_trace(&ctx, &mut space_sess_a, char_a, 56131, 0, origin_sys, Some(0)).await;
    assert!(pid_a > 1);
    assert!(!npcs_a.is_empty());
    let ab_sys_a = read_abyssal_state(&ctx, |st| st.pockets[&char_a].abyssal_solar_system_id);
    assert_eq!(space_sess_a.solar_system_id, Some(ab_sys_a));
    let npc_hp_t0 = ctx
        .systems
        .get_or_spawn(ab_sys_a)
        .get_combat_state(npcs_a[0])
        .await
        .unwrap()
        .unwrap()
        .max_shield_hp;

    // Higher tier (tier 3) increases npc_difficulty_mult = 1.0 + 3 * 1.25 = 4.75 vs 1.0
    let mut space_sess_c = SessionState {
        user_id: Some(char_c as u64),
        character_id: Some(char_c),
        solar_system_id: Some(origin_sys),
        ship_id: Some(ship_c),
        ..Default::default()
    };
    let (pid_c, npcs_c) = enter_abyssal_pocket_with_trace(&ctx, &mut space_sess_c, char_c, 47905, 3, origin_sys, None).await;
    assert_ne!(pid_c, pid_a);
    let ab_sys_c = read_abyssal_state(&ctx, |st| st.pockets[&char_c].abyssal_solar_system_id);
    let npc_hp_t3 = ctx
        .systems
        .get_or_spawn(ab_sys_c)
        .get_combat_state(npcs_c[0])
        .await
        .unwrap()
        .unwrap()
        .max_shield_hp;
    assert!(npc_hp_t3 > npc_hp_t0 * 2.0, "tier 3 NPC HP {npc_hp_t3} must reflect difficulty mult over tier 0 {npc_hp_t0}");

    // pocket_grid_offset returns exact pocket offset or [0;3] for unknown
    assert_eq!(pocket_grid_offset(&ctx, 9_999_999), [0.0, 0.0, 0.0]);
    let expected_off_c = read_abyssal_state(&ctx, |st| st.pockets[&char_c].grid_offset);
    assert_eq!(pocket_grid_offset(&ctx, pid_c), expected_off_c);

    // Join second char via valid trace_id -> leader enters with trace_id, then char_b joins SAME pocket via trace_id
    let trace_fleet = trace_id + 50;
    with_abyssal_state(&ctx, |st| {
        st.traces.insert(
            trace_fleet,
            AbyssalTraceRecord {
                trace_ball_id: trace_fleet,
                char_id: char_b,
                filament_type_id: 56131,
                origin_solar_system_id: origin_sys,
                position: [0.0; 3],
                created_unix: ctx.now_unix(),
                expires_at_sec: ctx.now_unix() + 1800,
                max_ships: 3,
                entered_char_ids: Vec::new(),
                pocket_id: None,
                fleet_id: None,
            },
        );
    });
    let char_b2 = 90_309_004i64;
    let ship_b2 = 1_309_000_004i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&char_b].clone();
        ch.character_id = char_b2;
        ch.user_id = char_b2 as u64;
        ch.active_ship_id = ship_b2;
        snap.characters.insert(char_b2, ch);
        snap.items.insert(ship_b2, make_item(ship_b2, cruiser_tid, char_b2, i64::from(origin_sys), InventoryFlag::None, 1));
    });
    let mut space_sess_b = SessionState {
        user_id: Some(char_b as u64),
        character_id: Some(char_b),
        solar_system_id: Some(origin_sys),
        ship_id: Some(ship_b),
        ..Default::default()
    };
    let mut space_sess_b2 = SessionState {
        user_id: Some(char_b2 as u64),
        character_id: Some(char_b2),
        solar_system_id: Some(origin_sys),
        ship_id: Some(ship_b2),
        ..Default::default()
    };
    let (pid_b1, _) = enter_abyssal_pocket_with_trace(&ctx, &mut space_sess_b, char_b, 56131, 0, origin_sys, Some(trace_fleet)).await;
    let (pid_b2, _) = enter_abyssal_pocket_with_trace(&ctx, &mut space_sess_b2, char_b2, 56131, 0, origin_sys, Some(trace_fleet)).await;
    assert_eq!(pid_b1, pid_b2);
    assert!(read_abyssal_state(&ctx, |st| st.pockets[&char_b2].fleet_ship_ids.contains(&ship_b)));
    assert!(read_abyssal_state(&ctx, |st| st.pockets[&char_b2].fleet_ship_ids.contains(&ship_b2)));

    // 5. ensure_abyssal_room_balls_for_ctx: missing gate respawned for active pocket, ignored when completed/collapsed
    let gate_c = 1_895_000_000 + pid_c * 10 + 1;
    let ab_handle_c = ctx.systems.get_or_spawn(ab_sys_c);
    let _ = ab_handle_c.remove_ball(gate_c).await;
    assert!(ab_handle_c.get_ball_snapshot(gate_c).await.unwrap().is_none());
    ensure_abyssal_room_balls_for_ctx(&ctx, char_c).await;
    assert!(ab_handle_c.get_ball_snapshot(gate_c).await.unwrap().is_some());

    // Mark completed -> delete gate again -> ensure_abyssal_room_balls_for_ctx does NOT respawn
    with_abyssal_state(&ctx, |st| st.pockets.get_mut(&char_c).unwrap().completed = true);
    let _ = ab_handle_c.remove_ball(gate_c).await;
    ensure_abyssal_room_balls_for_ctx(&ctx, char_c).await;
    assert!(ab_handle_c.get_ball_snapshot(gate_c).await.unwrap().is_none());

    // 6. on_abyssal_room_npc_destroyed_in_ballpark & on_abyssal_room_npc_destroyed_core
    let ab_sys_bp = 32_991_001u32;
    let ab_sys_core = 32_991_002u32;
    let pid_bp = 699_001i64;
    let pid_core = 699_002i64;
    let pid_completed = 699_003i64;
    let mk_rooms = |pid: i64, n1: i64, n2: i64| -> [AbyssalRoomRecord; 3] {
        [
            AbyssalRoomRecord {
                room_index: 1,
                dungeon_id: 6046,
                dungeon_name: "R1".into(),
                npc_ball_ids: vec![n1, n2],
                cache_item_id: 1_896_000_000 + pid * 10 + 1,
                cleared: false,
                gate_unlocked_at_sec: None,
            },
            AbyssalRoomRecord {
                room_index: 2,
                dungeon_id: 6047,
                dungeon_name: "R2".into(),
                npc_ball_ids: vec![n1 + 10],
                cache_item_id: 1_896_000_000 + pid * 10 + 2,
                cleared: false,
                gate_unlocked_at_sec: None,
            },
            AbyssalRoomRecord {
                room_index: 3,
                dungeon_id: 6048,
                dungeon_name: "R3".into(),
                npc_ball_ids: Vec::new(),
                cache_item_id: 1_896_000_000 + pid * 10 + 3,
                cleared: false,
                gate_unlocked_at_sec: None,
            },
        ]
    };
    with_abyssal_state(&ctx, |st| {
        st.pockets.insert(
            90_309_101,
            AbyssalPocketSession {
                pocket_id: pid_bp,
                character_id: 90_309_101,
                ship_id: 1_309_100_001,
                origin_solar_system_id: origin_sys,
                abyssal_solar_system_id: ab_sys_bp,
                origin_position: [0.0; 3],
                grid_offset: [0.0; 3],
                filament_type_id: 56131,
                tier: 0,
                weather: AbyssalWeatherKind::Electrical,
                weather_penalty_pct: 30.0,
                weather_bonus_pct: 50.0,
                npc_difficulty_mult: 1.0,
                current_room: 1,
                rooms: mk_rooms(pid_bp, 71_001, 71_002),
                started_at_sec: 1_800_000_000,
                collapse_at_sec: 1_800_001_200,
                completed: false,
                collapsed_destroyed: false,
                trace_ball_id: 0,
                fleet_ship_ids: vec![1_309_100_001],
                ship_room: HashMap::new(),
                max_ships: 1,
            },
        );
        st.pockets.insert(
            90_309_102,
            AbyssalPocketSession {
                pocket_id: pid_core,
                character_id: 90_309_102,
                ship_id: 1_309_100_002,
                origin_solar_system_id: origin_sys,
                abyssal_solar_system_id: ab_sys_core,
                origin_position: [0.0; 3],
                grid_offset: [0.0; 3],
                filament_type_id: 56131,
                tier: 0,
                weather: AbyssalWeatherKind::Electrical,
                weather_penalty_pct: 30.0,
                weather_bonus_pct: 50.0,
                npc_difficulty_mult: 1.0,
                current_room: 1,
                rooms: mk_rooms(pid_core, 72_001, 72_002),
                started_at_sec: 1_800_000_000,
                collapse_at_sec: 1_800_001_200,
                completed: false,
                collapsed_destroyed: false,
                trace_ball_id: 0,
                fleet_ship_ids: vec![1_309_100_002],
                ship_room: HashMap::new(),
                max_ships: 1,
            },
        );
        st.pockets.insert(
            90_309_103,
            AbyssalPocketSession {
                pocket_id: pid_completed,
                character_id: 90_309_103,
                ship_id: 1_309_100_003,
                origin_solar_system_id: origin_sys,
                abyssal_solar_system_id: ab_sys_bp,
                origin_position: [0.0; 3],
                grid_offset: [0.0; 3],
                filament_type_id: 56131,
                tier: 0,
                weather: AbyssalWeatherKind::Electrical,
                weather_penalty_pct: 30.0,
                weather_bonus_pct: 50.0,
                npc_difficulty_mult: 1.0,
                current_room: 1,
                rooms: mk_rooms(pid_completed, 73_001, 73_002),
                started_at_sec: 1_800_000_000,
                collapse_at_sec: 1_800_001_200,
                completed: true,
                collapsed_destroyed: false,
                trace_ball_id: 0,
                fleet_ship_ids: vec![1_309_100_003],
                ship_room: HashMap::new(),
                max_ships: 1,
            },
        );
    });

    // Ballpark variant: non-abyssal system false, completed pocket NPC false, partial NPC false, final NPC true + gate event
    let mut bp = Ballpark::new(ab_sys_bp);
    let gate_bp = 1_895_000_000 + pid_bp * 10 + 1;
    let _ = bp.add_ball(make_ball(gate_bp, 47685, DVec3::ZERO, 500.0));
    let _ = bp.add_ball(make_ball(71_001, 48090, DVec3::new(1000.0, 0.0, 0.0), 40.0));
    let _ = bp.add_ball(make_ball(71_002, 48090, DVec3::new(2000.0, 0.0, 0.0), 40.0));
    assert!(!on_abyssal_room_npc_destroyed_in_ballpark(&mut bp, &ctx.db, 31_999_999, 71_001));
    assert!(!on_abyssal_room_npc_destroyed_in_ballpark(&mut bp, &ctx.db, ab_sys_bp, 73_001));
    assert!(!on_abyssal_room_npc_destroyed_in_ballpark(&mut bp, &ctx.db, ab_sys_bp, 71_001));
    let _ = bp.remove_ball(71_001);
    assert!(on_abyssal_room_npc_destroyed_in_ballpark(&mut bp, &ctx.db, ab_sys_bp, 71_002));
    assert!(read_abyssal_state(&ctx, |st| st.pockets[&90_309_101].rooms[0].cleared));
    assert!(read_abyssal_state(&ctx, |st| st.pockets[&90_309_101].rooms[0].gate_unlocked_at_sec.is_some()));
    assert!(!read_abyssal_state(&ctx, |st| st.pockets[&90_309_102].rooms[0].cleared));
    let events_bp = std::mem::take(&mut bp.pending_events);
    assert!(events_bp.iter().any(|ev| matches!(ev, DestinyEvent::SetBallInteractive { ball_id, is_interactive: true } if *ball_id == gate_bp)));
    assert!(events_bp.iter().any(|ev| matches!(ev, DestinyEvent::OnClientControllerEvent { ball_id, event_name, value: 1, .. } if *ball_id == gate_bp && event_name == "GateActivate")));

    // Core async variant: non-abyssal false, partial NPC false, final NPC true + gate unlock
    let core_handle = ctx.systems.get_or_spawn(ab_sys_core);
    let obs_core = 72_099i64;
    let gate_core = 1_895_000_000 + pid_core * 10 + 1;
    let _ = core_handle.spawn_ball(make_ball(obs_core, 587, DVec3::ZERO, 40.0)).await;
    let _ = core_handle.spawn_ball(make_ball(gate_core, 47685, DVec3::ZERO, 500.0)).await;
    let _ = core_handle.spawn_ball(make_ball(72_001, 48090, DVec3::new(1000.0, 0.0, 0.0), 40.0)).await;
    let _ = core_handle.spawn_ball(make_ball(72_002, 48090, DVec3::new(2000.0, 0.0, 0.0), 40.0)).await;
    assert!(!on_abyssal_room_npc_destroyed_core(&ctx.systems, &ctx.db, 33_000_000, 72_001).await);
    assert!(!on_abyssal_room_npc_destroyed_core(&ctx.systems, &ctx.db, ab_sys_core, 72_001).await);
    let _ = core_handle.remove_ball(72_001).await;
    assert!(on_abyssal_room_npc_destroyed_core(&ctx.systems, &ctx.db, ab_sys_core, 72_002).await);
    assert!(read_abyssal_state(&ctx, |st| st.pockets[&90_309_102].rooms[0].cleared));
    let (_, core_events, _) = core_handle.drain_observer_updates(obs_core).await.unwrap();
    assert!(core_events.iter().any(|ev| matches!(ev, DestinyEvent::OnClientControllerEvent { ball_id, event_name, value: 1, .. } if *ball_id == gate_core && event_name == "GateActivate")));
}


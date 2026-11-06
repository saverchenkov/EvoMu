//! Integration tests for Capital, Pochven, Wormhole Escalations, and Combat parity items:
//! - `feat:navigation.pochven-capital-cyno-restrictions`
//! - `feat:pve.wormhole-capital-escalations`
//! - `feat:pve.wormhole-site-capital-escalation-cap`
//! - `feat:combat.pochven-loot-drop`
//! - `feat:combat.insurance-void-and-expiry`

use crate::common::*;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::combat::death_pipeline::execute_death_pipeline;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    with_combat_runtime,
};
use eve_server::services::pve_exploration::ensure_system_pve_sites_seeded;
use eve_server::sp6_lifecycle::cleanup_npc_wrecks_and_reset_missions_on_downtime;
use glam::DVec3;
use std::collections::HashMap;

fn make_sess(char_id: i64, ship_id: i64, sys_id: u32, station_id: Option<i64>) -> SessionState {
    let mut sess = SessionState::default();
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    sess.station_id = station_id;
    sess
}

async fn seed_ship_in_space(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) {
    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().cloned().expect("seeded character");
        let ch = snap.characters.entry(char_id).or_insert_with(|| {
            let mut c = base_ch;
            c.character_id = char_id;
            c
        });
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        ch.station_id = None;
        ch.structure_id = None;
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(ship_id, [pos.x, pos.y, pos.z]);
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let cs = ctx.build_type_combat_state(ship_type_id);
    let _ = sys_handle
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 120.0,
                inertia: 2.0,
                mass: 1_000_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
            SlimItemMeta {
                group_id: ctx.sde.types.get(&ship_type_id).map(|t| t.group_id).unwrap_or(6),
                category_id: 6,
                owner_id: char_id,
                char_id: Some(char_id),
                corp_id: 1_000_009,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
                ..Default::default()
            },
        )
        .await;
}

#[tokio::test]
async fn test_pochven_capital_and_cyno_restrictions() {
    // parity: feat:navigation.pochven-capital-cyno-restrictions
    eve_testkit::attest(&["feat:navigation.pochven-capital-cyno-restrictions"]);

    let sde = match eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()) {
        Ok(reg) => std::sync::Arc::new(reg),
        Err(_) => std::sync::Arc::new(eve_sde::SdeRegistry::with_canonical_seed()),
    };
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db)
        .with_seeded_rng(eve_server::rng::TEST_MODE_DEFAULT_SEED);
    pin_clock(&ctx);
    let char_id = 90_000_001i64;
    let dread_id = 104_000_001i64;
    let cyno_mod_id = 104_000_002i64;
    let pochven_sys = 30_000_157u32; // Otela in Pochven (region 10_000_070)
    let lowsec_sys = 30_000_186u32; // Oijanen low-sec (sec 0.352, 3.063 LY from Otela)

    seed_ship_in_space(&ctx, char_id, dread_id, 19_720, pochven_sys, DVec3::ZERO).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cyno_mod_id,
            InventoryItemRecord {
                item_id: cyno_mod_id,
                type_id: 21_096, // Cynosural Field Generator I
                owner_id: char_id,
                location_id: dread_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        let char_skills = snap.skills.entry(char_id).or_default();
        char_skills.insert(
            21_603, // Cynosural Field Theory
            TrainedSkillRecord {
                type_id: 21_603,
                level: 5,
                skill_points: 1_280_000.0,
            },
        );
        char_skills.insert(
            3_456, // Jump Drive Operation
            TrainedSkillRecord {
                type_id: 3_456,
                level: 5,
                skill_points: 1_280_000.0,
            },
        );
    });
    ctx.invalidate_ship_fitting(dread_id);

    let mut sess = make_sess(char_id, dread_id, pochven_sys, None);

    // 1. Region-wide cyno jammer stops lighting a cyno in Pochven
    let cyno_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(cyno_mod_id), PyRep::String("effects.CynosuralField".into())],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("Cyno activation in Pochven must be blocked by region-wide cyno jammer");
    assert!(
        cyno_err.code.contains("Cyno") || cyno_err.message.to_lowercase().contains("pochven") || cyno_err.message.to_lowercase().contains("jam"),
        "Unexpected cyno jammer error: {cyno_err:?}"
    );

    // 2. Capital ships cannot use Pochven stargates
    let gate_id = ctx
        .sde
        .stargates()
        .values()
        .find(|g| g.from_solar_system_id == pochven_sys)
        .map(|g| g.stargate_id)
        .unwrap_or(50_000_001);
    let gate_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdStargateJump",
            &[PyRep::Integer(gate_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("Capital ship must not be able to use Pochven stargates");
    assert_eq!(gate_err.code, "CapitalShipCannotJumpStargate");

    // 3. Capital manufacturing jobs in Pochven are refused
    let bp_id = 104_000_010i64;
    let pochven_station = ctx
        .sde
        .solar_systems
        .get(&pochven_sys)
        .and_then(|s| s.stations.first().map(|st| st.station_id))
        .unwrap_or(60_003_760);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(pochven_station);
        }
        snap.items.insert(
            bp_id,
            InventoryItemRecord {
                item_id: bp_id,
                type_id: 19_721, // Revelation Blueprint
                owner_id: char_id,
                location_id: pochven_station,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let mut docked_sess = make_sess(char_id, dread_id, pochven_sys, Some(pochven_station));
    let ind_err = ctx
        .dispatch_rpc(
            &mut docked_sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(bp_id)),
                ("facilityID", PyRep::Integer(pochven_station)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
            ])],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("Capital manufacturing in Pochven must be refused");
    assert_eq!(ind_err.code, "CapitalConstructionForbiddenInPochven");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
        }
    });

    // 4. Capitals already inside Pochven may jump out to an active low-sec cyno beacon
    let cyno_char_id = 90_000_002i64;
    let cyno_ship_id = 104_000_020i64;
    let cyno_mod_id = 104_000_021i64;
    let ozone_id = 104_000_022i64;
    let fuel_id = 104_000_023i64;
    seed_ship_in_space(&ctx, cyno_char_id, cyno_ship_id, 11_965, lowsec_sys, DVec3::new(10_000.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(cyno_char_id).or_default().insert(
            21_603,
            eve_db::TrainedSkillRecord { type_id: 21_603, level: 5, skill_points: 256_000.0 },
        );
        snap.items.insert(
            cyno_mod_id,
            InventoryItemRecord {
                item_id: cyno_mod_id,
                type_id: 21_096, // Cynosural Field Generator I
                owner_id: cyno_char_id,
                location_id: cyno_ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            ozone_id,
            InventoryItemRecord {
                item_id: ozone_id,
                type_id: 16_273, // Liquid Ozone
                owner_id: cyno_char_id,
                location_id: cyno_ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            fuel_id,
            InventoryItemRecord {
                item_id: fuel_id,
                type_id: 16_274, // Helium Isotopes
                owner_id: char_id,
                location_id: dread_id,
                flag: InventoryFlag::Cargo,
                quantity: 10_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.insert(
            888_001,
            eve_db::FleetRecord {
                fleet_id: 888_001,
                boss_id: char_id,
                members: vec![char_id, cyno_char_id],
                ..Default::default()
            },
        );
    });
    let mut cyno_sess = make_sess(cyno_char_id, cyno_ship_id, lowsec_sys, None);
    let _ = ctx
        .dispatch_rpc(
            &mut cyno_sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(cyno_mod_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("Activate low-sec cyno beacon");
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        None,
        "CmdJumpToFleetModuleBeacon",
        &[PyRep::Integer(cyno_char_id), PyRep::Integer(i64::from(lowsec_sys))],
        &HashMap::new(),
        None,
    )
    .await
    .expect("jump out of Pochven must succeed");
    assert_eq!(sess.solar_system_id, Some(lowsec_sys), "Capital jumping out of Pochven must arrive in low-sec");
    let fuel_after = ctx.db.with_snapshot(|s| s.items.get(&fuel_id).map(|i| i.quantity).unwrap_or(0));
    assert!(fuel_after < 10_000, "Jumping out of Pochven must consume isotopes (remaining: {fuel_after})");

    // 5. Negative: jumping INTO Pochven from low-sec must fail with CannotJumpToPochven
    ctx.advance_secs(3_600);
    let pochven_cyno_char = 90_000_003i64;
    let pochven_cyno_ship = 104_000_030i64;
    seed_ship_in_space(&ctx, pochven_cyno_char, pochven_cyno_ship, 587, pochven_sys, DVec3::new(50_000.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(f) = s.fleets.get_mut(&888_001) {
            f.members.push(pochven_cyno_char);
        }
    });
    let err_into_pochven = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdJumpToFleetModuleBeacon",
            &[PyRep::Integer(pochven_cyno_char), PyRep::Integer(i64::from(pochven_sys))],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("Jumping into Pochven must be refused with CannotJumpToPochven");
    assert_eq!(err_into_pochven.code, "CannotJumpToPochven");
}

#[tokio::test]
async fn test_wormhole_c5_c6_capital_escalations_and_cap() {

    let sde = match eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()) {
        Ok(reg) => std::sync::Arc::new(reg),
        Err(_) => std::sync::Arc::new(eve_sde::SdeRegistry::with_canonical_seed()),
    };
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db)
        .with_seeded_rng(eve_server::rng::TEST_MODE_DEFAULT_SEED);
    pin_clock(&ctx);
    let char_id = 90_000_001i64;
    let dread_id = 104_000_101i64;
    let carrier_id = 104_000_102i64;
    let c5_sys = 31_002_001u32; // C5 wormhole system
    let c6_sys = 31_002_450u32; // C6 wormhole system

    ensure_system_pve_sites_seeded(&ctx, c5_sys).await;
    ensure_system_pve_sites_seeded(&ctx, c6_sys).await;

    // 1. C5 Dreadnought warp-in -> spawns 3 Upgraded Avengers (37472) with 30,000m sig, web, scram, neut
    let site_id_c5 = format!("PVE-ANOM-{c5_sys}-1");
    let c5_site_pos = eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system
            .get(&c5_sys)
            .and_then(|l| l.iter().find(|s| s.site_id == site_id_c5))
            .map(|s| DVec3::from_array(s.position))
            .unwrap_or(DVec3::ZERO)
    });
    seed_ship_in_space(&ctx, char_id, dread_id, 19_720, c5_sys, c5_site_pos + DVec3::new(5_000.0, 0.0, 0.0)).await;
    let mut sess = make_sess(char_id, dread_id, c5_sys, None);

    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "EnterCombatAnomaly",
            &[PyRep::String(site_id_c5.clone())],
            &HashMap::new(),
            None,
        )
        .await
        .expect("EnterCombatAnomaly C5 #1");

    let avengers_1: Vec<(i64, f64, f64, f64, f64)> = {
        let sys_handle = ctx.systems.get_or_spawn(c5_sys);
        let bp = sys_handle.ballpark.read().unwrap();
        with_combat_runtime(&ctx, |rt| {
            rt.npc_entities
                .values()
                .filter(|n| n.solar_system_id == c5_sys && n.type_id == 37_472)
                .map(|n| {
                    let sig = bp.combat_states.get(&n.ball_id).map(|cs| cs.signature_radius).unwrap_or(0.0);
                    (n.ball_id, sig, n.web_speed_factor, n.scram_strength, n.neut_amount)
                })
                .collect()
        })
    };
    assert_eq!(avengers_1.len(), 3, "C5 Dreadnought escalation must spawn 3 Upgraded Avengers");
    for (_, sig, web, scram, neut) in &avengers_1 {
        assert_eq!(*sig, 30_000.0, "Upgraded Avenger must have 30,000m signature radius");
        assert!(*web < 0.0, "Upgraded Avenger must have stasis webifier");
        assert!(*scram >= 2.0, "Upgraded Avenger must have warp scrambler");
        assert!(*neut >= 500.0, "Upgraded Avenger must have energy neutralizer");
    }

    // 2. Second Dreadnought trigger -> spawns 3 more (total 6); Third Dreadnought trigger -> spawns 0 (cap = 2 per class)
    let dread_id_2 = 104_000_103i64;
    let char_id_2 = char_id + 1;
    seed_ship_in_space(&ctx, char_id_2, dread_id_2, 19_720, c5_sys, c5_site_pos + DVec3::new(5_000.0, 0.0, 0.0)).await;
    let mut sess2 = make_sess(char_id_2, dread_id_2, c5_sys, None);
    let _ = ctx
        .dispatch_rpc(
            &mut sess2,
            "scanSvc",
            None,
            "EnterCombatAnomaly",
            &[PyRep::String(site_id_c5.clone())],
            &HashMap::new(),
            None,
        )
        .await
        .expect("EnterCombatAnomaly C5 #2");
    let count_after_2 = with_combat_runtime(&ctx, |rt| {
        rt.npc_entities.values().filter(|n| n.solar_system_id == c5_sys && n.type_id == 37_472).count()
    });
    assert_eq!(count_after_2, 6, "Second Dreadnought trigger in C5 must spawn 3 more Upgraded Avengers");

    let dread_id_3 = 104_000_104i64;
    let char_id_3 = char_id + 2;
    seed_ship_in_space(&ctx, char_id_3, dread_id_3, 19_720, c5_sys, c5_site_pos + DVec3::new(5_000.0, 0.0, 0.0)).await;
    let mut sess3 = make_sess(char_id_3, dread_id_3, c5_sys, None);
    let _ = ctx
        .dispatch_rpc(
            &mut sess3,
            "scanSvc",
            None,
            "EnterCombatAnomaly",
            &[PyRep::String(site_id_c5.clone())],
            &HashMap::new(),
            None,
        )
        .await
        .expect("EnterCombatAnomaly C5 #3");
    let count_after_3 = with_combat_runtime(&ctx, |rt| {
        rt.npc_entities.values().filter(|n| n.solar_system_id == c5_sys && n.type_id == 37_472).count()
    });
    assert_eq!(count_after_3, 6, "Third Dreadnought trigger in same C5 site must spawn nothing");

    // 3. Carrier trigger in same C5 site still succeeds (independent 2-trigger cap per capital class)
    let carrier_char_id = char_id + 3;
    seed_ship_in_space(&ctx, carrier_char_id, carrier_id, 23_757, c5_sys, c5_site_pos + DVec3::new(5_000.0, 0.0, 0.0)).await;
    let mut carrier_sess = make_sess(carrier_char_id, carrier_id, c5_sys, None);
    let _ = ctx
        .dispatch_rpc(
            &mut carrier_sess,
            "scanSvc",
            None,
            "EnterCombatAnomaly",
            &[PyRep::String(site_id_c5.clone())],
            &HashMap::new(),
            None,
        )
        .await
        .expect("EnterCombatAnomaly C5 Carrier #1");
    let count_with_carrier = with_combat_runtime(&ctx, |rt| {
        rt.npc_entities.values().filter(|n| n.solar_system_id == c5_sys && n.type_id == 37_472).count()
    });
    assert_eq!(count_with_carrier, 9, "Carrier trigger in C5 site must spawn 3 Upgraded Avengers");

    // 4. Moving capital > 100 km from warp-in despawns its escalation NPCs
    {
        let sys_handle = ctx.systems.get_or_spawn(c5_sys);
        let mut bp = sys_handle.ballpark.write().unwrap();
        if let Some(b) = bp.balls.get_mut(&carrier_id) {
            b.position = c5_site_pos + DVec3::new(150_000.0, 0.0, 0.0); // 150 km away from site
        }
    }
    eve_server::services::wormhole_pve_escalations::step_wormhole_pve_system(&ctx, c5_sys, 1.0).await;
    let count_after_despawn = with_combat_runtime(&ctx, |rt| {
        rt.npc_entities.values().filter(|n| n.solar_system_id == c5_sys && n.type_id == 37_472).count()
    });
    assert_eq!(count_after_despawn, 6, "Escalation NPCs must despawn when capital moves > 100 km from warp-in");

    // 5. Downtime resets per-site escalation counters so waves regenerate after downtime
    let _ = cleanup_npc_wrecks_and_reset_missions_on_downtime(&ctx);
    let _ = eve_server::services::wormhole_pve_escalations::process_wormhole_sites_on_downtime(&ctx);
    let dread_id_4 = 104_000_105i64;
    seed_ship_in_space(&ctx, char_id, dread_id_4, 19_720, c5_sys, c5_site_pos + DVec3::new(5_000.0, 0.0, 0.0)).await;
    let mut sess4 = make_sess(char_id, dread_id_4, c5_sys, None);
    let _ = ctx
        .dispatch_rpc(
            &mut sess4,
            "scanSvc",
            None,
            "EnterCombatAnomaly",
            &[PyRep::String(site_id_c5)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("EnterCombatAnomaly C5 post-downtime");
    let count_post_downtime = with_combat_runtime(&ctx, |rt| {
        rt.npc_entities.values().filter(|n| n.solar_system_id == c5_sys && n.type_id == 37_472).count()
    });
    assert_eq!(count_post_downtime, 9, "Downtime must reset C5 capital escalation counters");

    // 6. C6 Dreadnought warp-in -> spawns 4 Upgraded Avengers
    let dread_c6_id = 104_000_106i64;
    let site_id_c6 = format!("PVE-ANOM-{c6_sys}-1");
    let c6_site_pos = eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system
            .get(&c6_sys)
            .and_then(|l| l.iter().find(|s| s.site_id == site_id_c6))
            .map(|s| DVec3::from_array(s.position))
            .unwrap_or(DVec3::ZERO)
    });
    seed_ship_in_space(&ctx, char_id, dread_c6_id, 19_720, c6_sys, c6_site_pos + DVec3::new(5_000.0, 0.0, 0.0)).await;
    let mut sess_c6 = make_sess(char_id, dread_c6_id, c6_sys, None);
    let _ = ctx
        .dispatch_rpc(
            &mut sess_c6,
            "scanSvc",
            None,
            "EnterCombatAnomaly",
            &[PyRep::String(site_id_c6)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("EnterCombatAnomaly C6");
    let c6_avengers = with_combat_runtime(&ctx, |rt| {
        rt.npc_entities.values().filter(|n| n.solar_system_id == c6_sys && n.type_id == 37_472).count()
    });
    assert_eq!(c6_avengers, 4, "C6 Dreadnought escalation must spawn 4 Upgraded Avengers");
}

#[tokio::test]
async fn test_pochven_60pct_loot_drop_vs_highsec_50pct() {
    let sde = match eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()) {
        Ok(reg) => std::sync::Arc::new(reg),
        Err(_) => std::sync::Arc::new(eve_sde::SdeRegistry::with_canonical_seed()),
    };
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db)
        .with_seeded_rng(eve_server::rng::TEST_MODE_DEFAULT_SEED);
    pin_clock(&ctx);
    let killer_id = 90_000_001i64;
    let victim_pochven_id = 90_000_002i64;
    let victim_highsec_id = 90_000_003i64;
    let ship_pochven_id = 104_000_201i64;
    let ship_highsec_id = 104_000_202i64;
    let pochven_sys = 30_000_157u32;
    let highsec_sys = 30_000_142u32;

    seed_ship_in_space(&ctx, victim_pochven_id, ship_pochven_id, 587, pochven_sys, DVec3::ZERO).await;
    seed_ship_in_space(&ctx, victim_highsec_id, ship_highsec_id, 587, highsec_sys, DVec3::ZERO).await;

    ctx.db.with_mut_snapshot(|snap| {
        for i in 1..=50 {
            snap.items.insert(
                300_000_000 + i,
                InventoryItemRecord {
                    item_id: 300_000_000 + i,
                    type_id: 34,
                    owner_id: victim_pochven_id,
                    location_id: ship_pochven_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 100,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                300_001_000 + i,
                InventoryItemRecord {
                    item_id: 300_001_000 + i,
                    type_id: 34,
                    owner_id: victim_highsec_id,
                    location_id: ship_highsec_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 100,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let death_pochven = execute_death_pipeline(
        &ctx,
        pochven_sys,
        victim_pochven_id,
        ship_pochven_id,
        killer_id,
        DVec3::ZERO,
        42_424,
    )
    .await
    .expect("Pochven death pipeline");

    let death_highsec = execute_death_pipeline(
        &ctx,
        highsec_sys,
        victim_highsec_id,
        ship_highsec_id,
        killer_id,
        DVec3::ZERO,
        42_424,
    )
    .await
    .expect("Highsec death pipeline");

    let (pochven_dropped, highsec_dropped) = ctx.db.with_snapshot(|snap| {
        let p = snap.items.values().filter(|it| it.location_id == death_pochven.wreck_item_id).count();
        let h = snap.items.values().filter(|it| it.location_id == death_highsec.wreck_item_id).count();
        (p, h)
    });
    assert!(
        pochven_dropped > highsec_dropped && pochven_dropped >= 25,
        "Pochven 60% loot drop ({pochven_dropped}/50) must exceed Highsec 50% loot drop ({highsec_dropped}/50)"
    );
}


pub(crate) mod crimewatch_sec_status_loss_transactions {
//! Integration tests for CrimeWatch, NPC Navy/Hauler, ServerOps Downtime, and OF-19.

use crate::common::*;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::living_world::NpcArchetype;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{dispatch_combat_rpc, evaluate_faction_police_response};
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;
use std::collections::HashMap;

fn make_sess(char_id: i64, ship_id: i64, sys_id: u32, station_id: Option<i64>) -> SessionState {
    SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        ship_id: Some(ship_id),
        solar_system_id: Some(sys_id),
        station_id,
        ..Default::default()
    }
}

async fn seed_ship_in_space(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::from([(2, 1.0)]),
                ..Default::default()
            },
        );
        snap.item_positions.insert(ship_id, [pos.x, pos.y, pos.z]);
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let cs = ctx.build_type_combat_state(ship_type_id);
    let _ = sys_handle
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
            SlimItemMeta {
                group_id: ctx.sde.types.get(&ship_type_id).map(|t| t.group_id).unwrap_or(25),
                category_id: 6,
                owner_id: char_id,
                char_id: Some(char_id),
                corp_id: 1_000_009,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
                ..Default::default()
            },
        )
        .await;
}

#[tokio::test]
async fn test_crimewatch_sec_status_loss_transactions_and_kill_rights() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let aggressor_id = 90_000_001i64;
    let victim_id = 90_000_002i64;
    let buyer_id = 90_000_003i64;
    let agg_ship_id = 105_000_001i64;
    let vic_pod_id = 105_000_002i64;
    let buyer_ship_id = 105_000_003i64;
    let highsec_sys = 30_000_142u32;

    seed_ship_in_space(&ctx, aggressor_id, agg_ship_id, 587, highsec_sys, DVec3::ZERO).await;
    seed_ship_in_space(&ctx, victim_id, vic_pod_id, 670, highsec_sys, DVec3::new(1_000.0, 0.0, 0.0)).await;
    seed_ship_in_space(&ctx, buyer_id, buyer_ship_id, 587, highsec_sys, DVec3::new(2_000.0, 0.0, 0.0)).await;

    let sec_before = ctx.db.with_snapshot(|s| s.characters.get(&aggressor_id).map(|c| c.security_status).unwrap_or(0.0));

    // Fit weapon + ammo on aggressor ship and set Safety Red so dogmaIM.Activate triggers criminal aggression
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            105_000_010,
            InventoryItemRecord {
                item_id: 105_000_010,
                type_id: 562, // Light Electron Blaster I
                owner_id: aggressor_id,
                location_id: agg_ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::from([(2, 1.0)]),
                ..Default::default()
            },
        );
        snap.items.insert(
            105_000_011,
            InventoryItemRecord {
                item_id: 105_000_011,
                type_id: 215, // Antimatter Charge S
                owner_id: aggressor_id,
                location_id: agg_ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(agg_ship_id);

    // Lock pod, set low HP so 1 shot destroys it, set Safety Red, and fire weapon via dogmaIM.Activate
    {
        let sys_handle = ctx.systems.get_or_spawn(highsec_sys);
        let mut bp = sys_handle.ballpark.write().unwrap();
        if let Some(cs) = bp.combat_states.get_mut(&vic_pod_id) {
            cs.shield_hp = 0.0;
            cs.armor_hp = 0.0;
            cs.structure_hp = 1.0;
        }
    }
    let mut agg_sess = make_sess(aggressor_id, agg_ship_id, highsec_sys, None);
    let _ = dispatch_combat_rpc(
        &ctx,
        &mut agg_sess,
        "crimewatch",
        "SetSafetyLevel",
        &[PyRep::Integer(0)],
        &HashMap::new(),
    )
    .await;
    let _ = dispatch_combat_rpc(
        &ctx,
        &mut agg_sess,
        "dogmaIM",
        "AddTarget",
        &[PyRep::Integer(vic_pod_id)],
        &HashMap::new(),
    )
    .await
    .expect("AddTarget pod");
    let sys_h = ctx.systems.get_or_spawn(highsec_sys);
    for _ in 0..60 {
        let _ = sys_h.step_manual(1.0).await;
    }
    let _ = dispatch_combat_rpc(
        &ctx,
        &mut agg_sess,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(105_000_010),
            PyRep::String("effects.HybridTurret".into()),
            PyRep::Integer(vic_pod_id),
        ],
        &HashMap::new(),
    )
    .await
    .expect("Activate weapon on pod");
    ctx.advance_secs(30);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let sec_after = ctx.db.with_snapshot(|s| s.characters.get(&aggressor_id).map(|c| c.security_status).unwrap_or(0.0));
    assert!(sec_after < sec_before, "Criminal pod kill must reduce aggressor security status ({sec_after} < {sec_before})");

    // Verify GetSecurityStatusTransactions returns a rowset with the recorded change
    let tx_rep = dispatch_combat_rpc(
        &ctx,
        &mut agg_sess,
        "crimewatch",
        "GetSecurityStatusTransactions",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("GetSecurityStatusTransactions");
    assert!(!matches!(tx_rep, PyRep::None), "GetSecurityStatusTransactions must return a rowset");
    if let PyRep::List(rows) = &tx_rep {
        assert!(!rows.is_empty(), "GetSecurityStatusTransactions must contain at least one transaction");
    }

    // Verify victim earned a 30-day expiring kill right on aggressor
    let (kr_id, kr_exp) = ctx.db.with_snapshot(|s| {
        let kr = s.kill_rights.values().find(|kr| kr.owner_id == victim_id && kr.target_id == aggressor_id)?;
        let exp = s.combat_crimewatch_state.kill_right_expires_unix.get(&kr.kill_right_id).copied().unwrap_or(0);
        Some((kr.kill_right_id, exp))
    }).expect("Victim must earn a kill right on criminal aggressor");
    assert!(kr_exp >= ctx.now_unix() + 30 * 86_400 - 120, "Kill right must carry a 30-day expiration timestamp");

    // Victim lists kill right for sale, cancels it, re-lists it, and third party buys/activates it
    let mut vic_sess = make_sess(victim_id, vic_pod_id, highsec_sys, None);
    let _ = dispatch_combat_rpc(
        &ctx,
        &mut vic_sess,
        "bountyProxy",
        "SellKillRight",
        &[PyRep::Integer(kr_id), PyRep::Real(5_000_000.0)],
        &HashMap::new(),
    )
    .await
    .expect("SellKillRight");

    let _ = dispatch_combat_rpc(
        &ctx,
        &mut vic_sess,
        "bountyProxy",
        "CancelSellKillRight",
        &[PyRep::Integer(kr_id)],
        &HashMap::new(),
    )
    .await
    .expect("CancelSellKillRight");

    let _ = dispatch_combat_rpc(
        &ctx,
        &mut vic_sess,
        "bountyProxy",
        "SellKillRight",
        &[PyRep::Integer(kr_id), PyRep::Real(5_000_000.0)],
        &HashMap::new(),
    )
    .await
    .expect("Re-SellKillRight");

    ctx.db.with_mut_snapshot(|s| {
        if let Some(mut ch) = s.characters.get(&aggressor_id).cloned() {
            ch.character_id = buyer_id;
            ch.name = "KillRight Buyer".into();
            ch.active_ship_id = buyer_ship_id;
            ch.balance = IskCents::from_isk(50_000_000);
            s.characters.insert(buyer_id, ch);
        }
    });
    let mut buyer_sess = make_sess(buyer_id, buyer_ship_id, highsec_sys, None);
    let _ = dispatch_combat_rpc(
        &ctx,
        &mut buyer_sess,
        "killRightMgr",
        "BuyKillRight",
        &[PyRep::Integer(kr_id), PyRep::Real(5_000_000.0)],
        &HashMap::new(),
    )
    .await
    .expect("BuyKillRight");
    let (is_active, has_buyer_j, has_seller_j) = ctx.db.with_snapshot(|s| (
        s.kill_rights.get(&kr_id).map(|kr| kr.is_active_suspect),
        s.wallet_journal.iter().any(|j| j.character_id == buyer_id && j.entry_type == "KillRightFee" && j.amount.0 < 0),
        s.wallet_journal.iter().any(|j| j.character_id == victim_id && j.entry_type == "KillRightFee" && j.amount.0 > 0),
    ));
    assert_eq!(is_active, Some(true), "BuyKillRight must activate suspect flag on target");
    assert!(has_buyer_j && has_seller_j, "BuyKillRight must append KillRightFee wallet journal entries for buyer and seller");
}

#[tokio::test]
async fn test_crimewatch_sec_status_gain_caps_and_tag_tier_bands() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let char_id = 90_000_001i64;
    let ship_id = 105_000_101i64;
    let sec_office_svc = ctx.sde.station_services.values().find(|s| s.name == "Security Office").map(|s| s.service_id).unwrap_or(26);
    let (lowsec_station, lowsec_sys) = ctx.sde.stations.values().find(|st| {
        (matches!(st.owner_corporation_id, 1_000_125 | 1_000_137) || st.owner_faction_id == Some(500_006))
            && ctx.sde.solar_systems.get(&st.solar_system_id).is_some_and(|s| s.security_status > 0.0 && s.security_status < 0.45)
            && ctx.sde.station_operations.get(&st.operation_id).is_some_and(|op| op.services.contains(&sec_office_svc))
    }).map(|st| (st.station_id, st.solar_system_id)).expect("lowsec CONCORD Security Office station");

    seed_ship_in_space(&ctx, char_id, ship_id, 587, lowsec_sys, DVec3::ZERO).await;

    // 1. Tag tier bands: Trainer (35661) only works in [-10.0, -8.0); refused at -6.0
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.security_status = -6.0;
        }
        s.items.insert(
            750_101,
            InventoryItemRecord {
                item_id: 750_101,
                type_id: 35_661, // Trainer (-10 to -8)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        s.items.insert(
            750_102,
            InventoryItemRecord {
                item_id: 750_102,
                type_id: 33141, // Negotiator (-2 to 0)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let sec_office_st = ctx.sde.stations.values().filter_map(|st| {
        let sec = ctx.sde.solar_systems.get(&st.solar_system_id)?.security_status;
        (0.0 < sec && sec < 0.45 && (st.owner_corporation_id == 1_000_125 || st.owner_corporation_id == 1_000_137)
            && eve_server::services::station_structure::npc_station_offers_service(&ctx.sde, st.station_id, 27))
            .then_some(st.station_id)
    }).min().unwrap_or(lowsec_station);
    let mut sess = make_sess(char_id, ship_id, lowsec_sys, Some(sec_office_st));
    let wrong_band_err = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "crimewatch",
        "ApplyTagsForSecStatus",
        &[PyRep::Real(-9.5), PyRep::Dict(vec![(PyRep::Integer(33138), PyRep::Integer(1))])],
        &HashMap::new(),
    )
    .await
    .expect_err("Trainer tag must be refused at -6.0 sec status");
    assert!(!wrong_band_err.code.is_empty());

    // 2. Negotiator tag (33141) at -0.3 raises security status to 0.0 (capped at 0.0 per V24)
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) { ch.security_status = -0.3; ch.station_id = sess.station_id; }
        if let Some(sh) = s.items.get_mut(&ship_id) { sh.location_id = sec_office_st; }
    });
    let _ = dispatch_combat_rpc(&ctx, &mut sess, "crimewatch", "ApplyTagsForSecStatus", &[PyRep::Real(0.2), PyRep::Dict(vec![(PyRep::Integer(33141), PyRep::Integer(2))])], &HashMap::new())
        .await
        .expect("Negotiator tag at -0.3");
    let sec_post_tag = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.security_status).unwrap_or(-1.0));
    assert!((sec_post_tag - 0.0).abs() < 1e-6, "Clone soldier tags must raise security status from -0.3 to 0.0 (got {sec_post_tag})");

    // 3. Real belt, mission, and incursion NPC kills through live combat/death pipeline cap at +5.0
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.security_status = 4.9999;
        }
    });
    for (idx, archetype) in ["Belt", "Mission", "Incursion"].iter().enumerate() {
        let npc_pos = DVec3::new(10_000.0 + (idx as f64) * 2_000.0, 0.0, 0.0);
        let npc_id = eve_server::services::combat_crimewatch::spawn_specialized_npc_entity(
            &ctx,
            lowsec_sys,
            16_934, // Corpus Prophet (431,250 ISK SDE bounty)
            archetype,
            1,
            npc_pos,
        )
        .await;
        eve_server::services::combat_crimewatch::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
            &ctx,
            lowsec_sys,
            char_id,
            npc_id,
            16_934,
            npc_pos,
            ctx.now_unix(),
        )
        .await;
    }
    let sec_at_cap = ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.security_status).unwrap_or(0.0));
    assert_eq!(sec_at_cap, 5.0, "Belt, mission, and incursion NPC kills must raise to +5.0 and stop at +5.0");

    // 4. Verify live server CrimeWatch security status clamping at -10.0 on illegal capsule kill
    let pod_victim_char = 90_000_002i64;
    let pod_ship_id = 105_000_199i64;
    seed_ship_in_space(&ctx, pod_victim_char, pod_ship_id, 670, lowsec_sys, DVec3::new(3_000.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.security_status = -9.5;
        }
    });
    eve_server::services::combat_crimewatch::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
        &ctx,
        lowsec_sys,
        char_id,
        pod_ship_id,
        670,
        DVec3::new(3_000.0, 0.0, 0.0),
        ctx.now_unix(),
    )
    .await;
    let (sec_clamped_low, low_pod_kr) = ctx.db.with_snapshot(|s| (
        s.characters.get(&char_id).map(|c| c.security_status).unwrap_or(0.0),
        s.kill_rights.values().any(|kr| kr.owner_id == pod_victim_char && kr.target_id == char_id),
    ));
    assert_eq!(sec_clamped_low, -10.0, "Illegal capsule kill below -8.0 must clamp character security_status at -10.0");
    assert!(low_pod_kr, "Illegal Lowsec capsule kill must grant a 30-day Kill Right to the podded victim");
}

#[tokio::test]
async fn test_npc_navy_police_hauler_convoy_and_serverops_downtime() {
    let ctx = ServerContext::new();
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_700_045_970))); // 10:59:30 UTC
    let char_id = 90_000_001i64;
    let ship_id = 105_000_201i64;
    let highsec_sys = 30_000_142u32;

    seed_ship_in_space(&ctx, char_id, ship_id, 587, highsec_sys, DVec3::ZERO).await;
    tick_background_cluster(&ctx).await; // Seed pre-downtime day index at 10:59:30 UTC

    // 1. Faction Police / Navy NPC response against outlaw (-5.5 sec status) in high-sec
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.security_status = -5.5;
        }
    });
    let (attacked, can_dock, cloak_broken) = evaluate_faction_police_response(&ctx, highsec_sys, char_id, ship_id).await;
    assert_eq!(attacked, true, "Faction Police must engage outlaw pilot in high-sec");
    assert_eq!(can_dock, false, "Faction Police must refuse docking to outlaw pilot");
    assert_eq!(cloak_broken, true, "Faction Police must break cloak on outlaw pilot");

    // 2. NPC Hauler Convoy traveling gates via living_world
    ctx.living_world.seed_default_convoys();
    ctx.living_world.materialize_system_lod2(highsec_sys, &ctx.systems, &ctx.sde).await;
    let convoy_steps = ctx.living_world.step_all_hauler_convoys(&ctx.systems, &ctx.db, &ctx.sde).await;
    assert!(convoy_steps.iter().all(|(_, r)| r.is_ok()), "step_all_hauler_convoys must succeed");
    let hauler_count = ctx
        .living_world
        .list_convoys()
        .iter()
        .filter(|c| c.archetype == NpcArchetype::HaulerConvoy && !c.destroyed)
        .count();
    assert!(hauler_count >= 1, "Active HaulerConvoy must be tracked in living_world (got {hauler_count})");

    // 3. ServerOps Downtime & Daily Jobs across 11:00 UTC: real NPC wreck cleanup & accepted mission reset
    let npc_id = 1_880_000_099i64;
    let npc_pos = DVec3::new(25_000.0, 0.0, 0.0);
    let sys = ctx.systems.get_or_spawn(highsec_sys);
    let npc_ball = eve_server::services::pve_incursion_sites::make_site_npc_ball(npc_id, 24_000, npc_pos);
    let _ = sys.spawn_ball_with_combat(npc_ball, ctx.build_type_combat_state(24_000)).await;
    eve_server::services::combat_crimewatch::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
        &ctx, highsec_sys, char_id, npc_id, 24_000, npc_pos, ctx.now_unix(),
    )
    .await;
    let expired_wreck_id = eve_server::sp6_lifecycle::with_sp6_state(&ctx.db, |st| {
        st.space_object_meta.iter().find(|(_, m)| m.is_npc_wreck).map(|(&id, _)| id)
    })
    .expect("Death pipeline must spawn an NPC wreck with is_npc_wreck = true");
    ctx.db.with_mut_snapshot(|s| {
        s.active_missions.insert((char_id, 3_008_416), eve_db::ActiveMissionRecord {
            mission_id: 1, character_id: char_id, agent_id: 3_008_416, solar_system_id: highsec_sys,
            state: "Accepted".into(), current_room_id: 2, npcs_remaining: 1,
            spawned_npc_ball_ids: vec![1_880_000_098], bookmark_id: None,
        });
    });

    ctx.advance_secs(60); // 11:00:30 UTC (only 60s elapsed, so 2h decay has not fired)
    tick_background_cluster(&ctx).await;
    let wreck_in_meta = eve_server::sp6_lifecycle::with_sp6_state(&ctx.db, |st| {
        st.space_object_meta.contains_key(&expired_wreck_id)
    });
    let (wreck_in_items, mission_room, mission_npcs) = ctx.db.with_snapshot(|s| {
        let m = s.active_missions.get(&(char_id, 3_008_416)).unwrap();
        (s.items.contains_key(&expired_wreck_id), m.current_room_id, m.npcs_remaining)
    });
    assert!(!wreck_in_items && !wreck_in_meta, "11:00 UTC downtime must delete NPC wreck");
    assert_eq!((mission_room, mission_npcs), (1, 4), "11:00 UTC downtime must reset accepted mission waves");
}

#[tokio::test]
async fn test_capital_construction_restrictions_and_insurance_contract_lifecycle() {
    let sde = match eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()) {
        Ok(reg) => std::sync::Arc::new(reg),
        Err(_) => std::sync::Arc::new(eve_sde::SdeRegistry::with_canonical_seed()),
    };
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db)
        .with_seeded_rng(eve_server::rng::TEST_MODE_DEFAULT_SEED);
    pin_clock(&ctx);
    let char_id = 90_000_001i64;
    let ship_id = 104_000_301i64;
    let highsec_sys = 30_000_142u32;
    let jita_station = 60_003_760i64;

    seed_ship_in_space(&ctx, char_id, ship_id, 587, highsec_sys, DVec3::ZERO).await;

    // 1. Capital manufacturing job in high-sec NPC station is refused
    let bp_id = 104_000_302i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(jita_station);
        }
        snap.items.insert(
            bp_id,
            InventoryItemRecord {
                item_id: bp_id,
                type_id: 19_721, // Revelation Blueprint
                owner_id: char_id,
                location_id: jita_station,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let mut sess = make_sess(char_id, ship_id, highsec_sys, Some(jita_station));
    let cap_err = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(bp_id)),
                ("facilityID", PyRep::Integer(jita_station)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
            ])],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("Capital ship manufacturing in high-sec NPC station must be refused");
    assert_eq!(cap_err.code, "CapitalConstructionForbiddenInHighSec");

    // 1b. Supercapital manufacturing in low-sec NPC station is refused
    let super_bp_id = 104_000_303i64;
    let lowsec_sys = 30_002_537u32;
    let lowsec_station = ctx
        .sde
        .solar_systems
        .get(&lowsec_sys)
        .and_then(|s| s.stations.first().map(|st| st.station_id))
        .unwrap_or(60_003_760);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(lowsec_station);
        }
        snap.items.insert(
            super_bp_id,
            InventoryItemRecord {
                item_id: super_bp_id,
                type_id: 23_918, // Nyx Blueprint
                owner_id: char_id,
                location_id: lowsec_station,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let mut low_sess = make_sess(char_id, ship_id, lowsec_sys, Some(lowsec_station));
    let super_err = ctx
        .dispatch_rpc(
            &mut low_sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(super_bp_id)),
                ("facilityID", PyRep::Integer(lowsec_station)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
            ])],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("Supercapital manufacturing in low-sec NPC station must be refused");
    assert_eq!(super_err.code, "SupercapitalRequiresSovereignNullSec");

    // 2. Insurance contract via real InsureShip RPC: 12-week expiry -> 40% fallback + EVE-mail, and CONCORD criminal voiding
    let cost_isk = 1.0_f64; // Platinum fraction (1.0)
    let _ = ctx.systems.get_or_spawn(highsec_sys).remove_ball(ship_id).await;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c) = s.characters.get_mut(&char_id) {
            c.station_id = Some(jita_station);
        }
        if let Some(it) = s.items.get_mut(&ship_id) {
            it.location_id = jita_station;
        }
    });
    sess.station_id = Some(jita_station);
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "insuranceSvc",
            None,
            "InsureShip",
            &[PyRep::Integer(ship_id), PyRep::Real(cost_isk), PyRep::Bool(false), PyRep::Bool(true)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("InsureShip");
    let full_payout = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.insurance_contracts.get(&ship_id).map(|c| c.payout_isk).unwrap_or(0.0)
    });
    assert!(full_payout > 0.0);

    // Advance past 12 weeks (7,257,600s) -> expired contract falls back to 40% payout and sends EVE-mail
    ctx.advance_secs(12 * 7 * 86_400 + 60);
    let mails_before = ctx.db.with_snapshot(|s| s.mails.len());
    let payout = eve_server::services::combat_crimewatch::process_ship_insurance_payout(&ctx, char_id, ship_id, 587, false);
    assert!(payout > 0.0 && payout < full_payout, "Expired insurance must fall back to 40% payout");
    let has_mail = ctx.db.with_snapshot(|s| {
        s.mails.len() > mails_before && s.mails.iter().any(|m| m.recipient_id == char_id && m.subject.contains("Insurance Payout"))
    });
    assert!(has_mail, "Insurance payout must send an EVE-mail");

    // CONCORD criminal death, self-destruct, and repackaging all void active insurance contracts (0.0 payout)
    let ship_id_2 = 104_000_304i64;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c) = s.characters.get_mut(&char_id) { c.station_id = Some(jita_station); }
        s.items.insert(ship_id_2, InventoryItemRecord { item_id: ship_id_2, type_id: 587, owner_id: char_id, location_id: jita_station, flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default() });
    });
    sess.station_id = Some(jita_station);
    let _ = ctx.dispatch_rpc(&mut sess, "insuranceSvc", None, "InsureShip", &[PyRep::Integer(ship_id_2), PyRep::Real(cost_isk), PyRep::Bool(false), PyRep::Bool(true)], &HashMap::new(), None).await.expect("InsureShip ship 2");
    seed_ship_in_space(&ctx, char_id, ship_id_2, 587, highsec_sys, DVec3::ZERO).await;
    sess.station_id = None;
    assert_eq!(eve_server::services::combat_crimewatch::process_ship_insurance_payout(&ctx, char_id, ship_id_2, 587, true), 0.0);

    let ship_sd = 104_000_305i64;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c) = s.characters.get_mut(&char_id) { c.station_id = Some(jita_station); }
        s.items.insert(ship_sd, InventoryItemRecord { item_id: ship_sd, type_id: 587, owner_id: char_id, location_id: jita_station, flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default() });
    });
    sess.station_id = Some(jita_station);
    let _ = ctx.dispatch_rpc(&mut sess, "insuranceSvc", None, "InsureShip", &[PyRep::Integer(ship_sd), PyRep::Real(cost_isk), PyRep::Bool(false), PyRep::Bool(true)], &HashMap::new(), None).await.expect("InsureShip self-destruct");
    seed_ship_in_space(&ctx, char_id, ship_sd, 587, highsec_sys, DVec3::new(500.0, 0.0, 0.0)).await;
    sess.station_id = None;
    let sd_res = eve_server::combat::death_pipeline::execute_death_pipeline_sync_inner(&ctx, highsec_sys, char_id, ship_sd, char_id, DVec3::ZERO, 7, true).expect("Self-destruct death");
    assert_eq!(sd_res.insurance_payout_isk, 0.0, "Self-destruct must void ship insurance payout");

    let ship_rep = 104_000_306i64;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) { ch.station_id = Some(jita_station); ch.active_ship_id = ship_id; }
        s.items.insert(ship_rep, InventoryItemRecord { item_id: ship_rep, type_id: 587, owner_id: char_id, location_id: jita_station, flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default() });
    });
    let mut rep_sess = make_sess(char_id, ship_id, highsec_sys, Some(jita_station));
    let _ = ctx.dispatch_rpc(&mut rep_sess, "insuranceSvc", None, "InsureShip", &[PyRep::Integer(ship_rep), PyRep::Real(cost_isk), PyRep::Bool(false), PyRep::Bool(true)], &HashMap::new(), None).await.expect("InsureShip repackage");
    let _ = ctx.dispatch_rpc(&mut rep_sess, "repackagingSvc", None, "RepackageItems", &[PyRep::List(vec![PyRep::Integer(ship_rep)])], &HashMap::new(), None).await.expect("RepackageItems");
    assert!(!eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| rt.insurance_contracts.contains_key(&ship_rep)), "RepackageItems must void active ship insurance contract");
}

}

pub(crate) mod doomsday_bosonic_cone_gtfo_reaper {
//! Integration tests for Bosonic/GTFO/Reaper Doomsdays, Ice Refinery Heist,
//! and Deadspace Pocket Rules:
//! - `feat:modules.doomsday-bosonic-field`
//! - `feat:modules.doomsday-gtfo-reaper`
//! - `feat:pve.insurgency-ice-heist`
//! - `feat:pve.deadspace-pocket-rules`

use crate::common::*;
use eve_db::{InventoryFlag, InventoryItemRecord, PersistedInsurgencyCampaign};
use eve_destiny::{MovementMode, WarpPhase};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::pve_dungeon_runtime::{
    with_dungeon_runtime_store, DungeonRuntimeInstance, DungeonRuntimeObject, DungeonRuntimeRoom,
};
use eve_server::services::pve_incursion_sites::make_site_npc_ball;
use glam::DVec3;
use std::collections::HashMap;

fn make_sess(char_id: i64, ship_id: i64, sys_id: u32, station_id: Option<i64>) -> SessionState {
    SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        ship_id: Some(ship_id),
        solar_system_id: Some(sys_id),
        station_id,
        ..Default::default()
    }
}

async fn call_svc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: Vec<PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, &args, &HashMap::new(), None)
        .await
}

fn py_keyval_to_map(rep: &PyRep) -> HashMap<String, PyRep> {
    let mut out = HashMap::new();
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

async fn seed_ship_in_space(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) {
    ctx.db.with_mut_snapshot(|snap| {
        if !snap.characters.contains_key(&char_id) {
            let mut template = snap.characters.get(&90_001).or_else(|| snap.characters.values().next()).cloned().unwrap();
            template.character_id = char_id;
            template.name = format!("Pilot{char_id}");
            template.war_faction_id = None;
            template.loyalty_points.clear();
            snap.characters.insert(char_id, template);
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::new(),
                ..Default::default()
            },
        );
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            make_site_npc_ball(ship_id, ship_type_id, pos),
            ctx.build_type_combat_state(ship_type_id),
        )
        .await;
}

#[tokio::test]
async fn test_doomsday_bosonic_cone_and_gtfo_reaper() {
    // parity: feat:modules.doomsday-bosonic-field, feat:modules.doomsday-gtfo-reaper
    eve_testkit::attest(&[
        "feat:modules.doomsday-bosonic-field",
        "feat:modules.doomsday-gtfo-reaper",
    ]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let highsec_sys = 30_000_142; // Jita (0.95 highsec)
    let lowsec_sys = 30_002_537; // Amamake (0.44 lowsec)
    let nullsec_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < 0.0 && s.solar_system_id < 31_000_000 && s.region_id != 10_000_070)
        .map(|s| s.solar_system_id)
        .expect("SDE must contain a K-space nullsec system");
    let char_id = 90_001;
    let titan_id = 88_100_001;
    let dd_mod_id = 88_100_002; // 40633 Bosonic Field Generator (doomsdayConeDOT)
    let gtfo_mod_id = 88_100_003; // 40634 Gravitational Transportation Field Oscillator (doomsdayHOG)
    let reaper_mod_id = 88_100_004; // 40632 'Divine Harvest' Electromagnetic Reaper (doomsdaySlash)
    let helium_id = 88_100_005;
    let stront_id = 88_100_006;

    let slash_vec = |mid: i64, x: f64, y: f64, z: f64| {
        vec![PyRep::Integer(mid), PyRep::Real(x), PyRep::Real(y), PyRep::Real(z)]
    };

    // 1. Negative: non-Titan hull refused
    seed_ship_in_space(&ctx, char_id, titan_id, 587, nullsec_sys, DVec3::ZERO).await;
    ctx.db.with_mut_snapshot(|snap| {
        for (mid, tid, flg) in [
            (dd_mod_id, 40_633, InventoryFlag::HiSlot0),
            (gtfo_mod_id, 40_634, InventoryFlag::HiSlot1),
            (reaper_mod_id, 40_632, InventoryFlag::HiSlot2),
        ] {
            snap.items.insert(mid, InventoryItemRecord {
                item_id: mid, type_id: tid, owner_id: char_id, location_id: titan_id,
                flag: flg, quantity: 1, singleton: true, ..Default::default()
            });
            snap.module_online_states.insert(mid, true);
        }
    });
    let mut sess = make_sess(char_id, titan_id, nullsec_sys, None);
    let err = call_svc(&ctx, &mut sess, "superWeaponMgr", "ActivateSlashModule", slash_vec(dd_mod_id, 1.0, 0.0, 0.0)).await.unwrap_err();
    assert_eq!(err.code, "ShipCannotActivateSuperweapon");

    // 2. Negative: BOTH Highsec (0.95) AND Lowsec (0.44) Empire space refused (`disallowInEmpireSpace`)
    for emp_sys in [highsec_sys, lowsec_sys] {
        seed_ship_in_space(&ctx, char_id, titan_id, 11_567, emp_sys, DVec3::ZERO).await;
        let mut emp_sess = make_sess(char_id, titan_id, emp_sys, None);
        let emp_err = call_svc(&ctx, &mut emp_sess, "superWeaponMgr", "ActivateSlashModule", slash_vec(dd_mod_id, 1.0, 0.0, 0.0)).await.unwrap_err();
        assert_eq!(emp_err.code, "CannotActivateInEmpireSpace");
    }

    // 3. Negative: docked, cloaked, warping, and missing-fuel checks in Nullsec
    seed_ship_in_space(&ctx, char_id, titan_id, 11_567, nullsec_sys, DVec3::ZERO).await;
    ctx.db.with_mut_snapshot(|s| { if let Some(c) = s.characters.get_mut(&char_id) { c.station_id = Some(60_003_760); } });
    let mut docked_sess = make_sess(char_id, titan_id, nullsec_sys, Some(60_003_760));
    let docked_err = call_svc(&ctx, &mut docked_sess, "superWeaponMgr", "ActivateSlashModule", slash_vec(dd_mod_id, 1.0, 0.0, 0.0)).await.unwrap_err();
    assert_eq!(docked_err.code, "CantDoThatWhileDocked");
    ctx.db.with_mut_snapshot(|s| { if let Some(c) = s.characters.get_mut(&char_id) { c.station_id = None; } });

    let sys = ctx.systems.get_or_spawn(nullsec_sys);
    let mut t_cs = sys.get_combat_state(titan_id).await.unwrap().unwrap();
    t_cs.is_cloaked = true;
    let _ = sys.set_combat_state(titan_id, t_cs.clone()).await;
    let cloak_err = call_svc(&ctx, &mut sess, "superWeaponMgr", "ActivateSlashModule", slash_vec(dd_mod_id, 1.0, 0.0, 0.0)).await.unwrap_err();
    assert_eq!(cloak_err.code, "CantActivateWhileCloaked");
    t_cs.is_cloaked = false;
    let _ = sys.set_combat_state(titan_id, t_cs).await;

    let _ = sys.set_movement_mode(titan_id, MovementMode::Warp {
        origin: DVec3::ZERO, destination: DVec3::new(1.0e7, 0.0, 0.0), warp_speed_au_s: 1.5,
        total_distance: 1.0e7, phase: WarpPhase::Accelerating, elapsed_warp_time: 0.0,
    }).await;
    let warp_err = call_svc(&ctx, &mut sess, "superWeaponMgr", "ActivateSlashModule", slash_vec(dd_mod_id, 1.0, 0.0, 0.0)).await.unwrap_err();
    assert_eq!(warp_err.code, "CantActivateInWarp");
    let _ = sys.set_movement_mode(titan_id, MovementMode::Stop).await;

    let err_fuel = call_svc(&ctx, &mut sess, "superWeaponMgr", "ActivateSlashModule", slash_vec(dd_mod_id, 1.0, 0.0, 0.0)).await.unwrap_err();
    assert_eq!(err_fuel.code, "InsufficientFuel");

    // 4. Seed SDE Strontium Clathrates (16275) & Helium Isotopes (16274) + targets
    ctx.db.with_mut_snapshot(|snap| {
        for (fid, tid, qty) in [(stront_id, 16_275, 10_000), (helium_id, 16_274, 150_000)] {
            snap.items.insert(fid, InventoryItemRecord {
                item_id: fid, type_id: tid, owner_id: char_id, location_id: titan_id,
                flag: InventoryFlag::Cargo, quantity: qty, ..Default::default()
            });
        }
    });

    let neut_ship_id = 88_100_009;
    let cone_frig_id = 88_100_010;
    let cone_cap_id = 88_100_017;
    let cone_miss_id = 88_100_011;
    let cone_tethered_id = 88_100_014;
    let gtfo_hit_id = 88_100_012;
    let gtfo_cap_id = 88_100_018;
    let gtfo_tethered_id = 88_100_015;
    let gtfo_struct_id = 88_100_016;
    let reaper_hit_id = 88_100_013;

    seed_ship_in_space(&ctx, 90_011, neut_ship_id, 638, nullsec_sys, DVec3::new(0.0, 8_000.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_002, cone_frig_id, 587, nullsec_sys, DVec3::new(20_000.0, 0.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_012, cone_cap_id, 19_720, nullsec_sys, DVec3::new(22_000.0, 0.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_003, cone_miss_id, 587, nullsec_sys, DVec3::new(0.0, 20_000.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_006, cone_tethered_id, 587, nullsec_sys, DVec3::new(15_000.0, 0.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_004, gtfo_hit_id, 587, nullsec_sys, DVec3::new(-40_000.0, 0.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_013, gtfo_cap_id, 19_720, nullsec_sys, DVec3::new(-39_000.0, 0.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_007, gtfo_tethered_id, 587, nullsec_sys, DVec3::new(-38_000.0, 0.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_005, reaper_hit_id, 638, nullsec_sys, DVec3::new(0.0, -20_000.0, 18_000.0)).await;

    for tid in [cone_frig_id, cone_cap_id] {
        let mut cs = sys.get_combat_state(tid).await.unwrap().unwrap();
        cs.shield_hp = 500_000.0;
        cs.max_shield_hp = 500_000.0;
        cs.shield_resonance = [1.0, 1.0, 1.0, 1.0];
        let _ = sys.set_combat_state(tid, cs).await;
    }
    for tid in [cone_tethered_id, gtfo_tethered_id] {
        let mut cs = sys.get_combat_state(tid).await.unwrap().unwrap();
        cs.is_tethered = true;
        let _ = sys.set_combat_state(tid, cs).await;
    }
    let mut struct_ball = make_site_npc_ball(gtfo_struct_id, 35_832, DVec3::new(-37_000.0, 0.0, 0.0));
    struct_ball.max_velocity = 0.0;
    let _ = sys.spawn_ball(struct_ball).await;

    // 5. Activate Bosonic Field Generator -> 10km 35k GJ neut pulse, 10s warmup (0 dmg at t=0), then sig-scaled DoT
    assert!(sys.get_combat_state(neut_ship_id).await.unwrap().unwrap().capacitor > 0.0);
    let pre_tether_cs = sys.get_combat_state(cone_tethered_id).await.unwrap().unwrap();
    let cone_res = call_svc(
        &ctx,
        &mut sess,
        "superWeaponMgr",
        "ActivateSlashModule",
        vec![PyRep::Integer(dd_mod_id), PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
    )
    .await
    .unwrap();
    let cone_kv = py_keyval_to_map(&cone_res);
    assert_eq!(cone_kv.get("mode").and_then(|v| v.as_str()), Some("Cone"));
    assert_eq!(cone_kv.get("status").and_then(|v| v.as_str()), Some("WarmingUp"));
    assert_eq!(cone_kv.get("fuelConsumed").and_then(|v| v.as_i64()), Some(2_000));
    assert_eq!(sys.get_combat_state(neut_ship_id).await.unwrap().unwrap().capacitor, 0.0, "10km superweapon neut pulse must drain nearby ship capacitor");
    assert_eq!(sys.get_combat_state(cone_frig_id).await.unwrap().unwrap().shield_hp, 500_000.0, "No Bosonic damage before 10s warmup");

    let refire_err = call_svc(
        &ctx,
        &mut sess,
        "superWeaponMgr",
        "ActivateSlashModule",
        vec![PyRep::Integer(dd_mod_id), PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
    )
    .await
    .unwrap_err();
    assert_eq!(refire_err.code, "ModuleInReactivationDelay");
    assert_eq!(sys.get_ball_snapshot(titan_id).await.unwrap().unwrap().1.max_velocity, 0.0);
    assert!(sys.get_combat_state(titan_id).await.unwrap().unwrap().disallow_assistance);

    ctx.advance_secs(10);
    let _ = sys.tick().await;
    let frig_dmg = 500_000.0 - sys.get_combat_state(cone_frig_id).await.unwrap().unwrap().shield_hp;
    let cap_dmg = 500_000.0 - sys.get_combat_state(cone_cap_id).await.unwrap().unwrap().shield_hp;
    assert!(frig_dmg > 0.0 && cap_dmg > frig_dmg * 5.0, "Bosonic damage must scale with target signatureRadius (cap={cap_dmg}, frig={frig_dmg})");
    assert_eq!(sys.get_combat_state(cone_tethered_id).await.unwrap().unwrap().shield_hp, pre_tether_cs.shield_hp);
    ctx.advance_secs(20);
    let _ = sys.tick().await;

    // 6. GTFO: range/NaN checks, 15s spool delay, subcapital-only filter (Dreadnought/tethered/structure excluded), and 60s warp suppression
    let oor_err = call_svc(&ctx, &mut sess, "superWeaponMgr", "ActivateSinglePointTargetedModule", slash_vec(gtfo_mod_id, 900_000.0, 0.0, 0.0)).await.unwrap_err();
    assert_eq!(oor_err.code, "TargetOutOfRange");
    let nan_err = call_svc(&ctx, &mut sess, "superWeaponMgr", "ActivateSinglePointTargetedModule", slash_vec(gtfo_mod_id, f64::NAN, 0.0, 0.0)).await.unwrap_err();
    assert_eq!(nan_err.code, "InvalidTargetPoint");
    let gtfo_res = call_svc(&ctx, &mut sess, "superWeaponMgr", "ActivateSinglePointTargetedModule", slash_vec(gtfo_mod_id, -35_000.0, 0.0, 0.0)).await.unwrap();
    let gtfo_kv = py_keyval_to_map(&gtfo_res);
    assert_eq!(gtfo_kv.get("status").and_then(|v| v.as_str()), Some("WarmingUp"));
    assert!(sys.get_ball_snapshot(gtfo_hit_id).await.unwrap().unwrap().1.position.distance(DVec3::new(-40_000.0, 0.0, 0.0)) < 1.0);

    ctx.advance_secs(15);
    let _ = sys.tick().await;
    assert!(sys.get_ball_snapshot(gtfo_hit_id).await.unwrap().unwrap().1.position.distance(DVec3::new(-40_000.0, 0.0, 0.0)) > 500_000.0);
    assert!(sys.get_combat_state(gtfo_hit_id).await.unwrap().unwrap().warp_scramble_strength >= 25, "GTFO must apply 60s warp suppression");
    assert!(sys.get_ball_snapshot(gtfo_cap_id).await.unwrap().unwrap().1.position.distance(DVec3::new(-39_000.0, 0.0, 0.0)) < 1.0, "Capital ships must not be teleported by GTFO");
    assert!(sys.get_ball_snapshot(gtfo_tethered_id).await.unwrap().unwrap().1.position.distance(DVec3::new(-38_000.0, 0.0, 0.0)) < 1.0);
    assert!(sys.get_ball_snapshot(gtfo_struct_id).await.unwrap().unwrap().1.position.distance(DVec3::new(-37_000.0, 0.0, 0.0)) < 1.0);

    ctx.advance_secs(61);
    let _ = sys.tick().await;
    assert_eq!(sys.get_combat_state(gtfo_hit_id).await.unwrap().unwrap().warp_scramble_strength, 0, "GTFO warp suppression must expire after 60s");

    // 7. Reaper (40632): >100km arc cap rejected, then 10s warmup + 10s progressive sweep
    let arc_err = call_svc(
        &ctx,
        &mut sess,
        "superWeaponMgr",
        "ActivateSlashModule",
        vec![
            PyRep::Integer(reaper_mod_id),
            PyRep::Real(0.0), PyRep::Real(-20_000.0), PyRep::Real(-60_000.0),
            PyRep::Real(0.0), PyRep::Real(-20_000.0), PyRep::Real(60_000.0),
        ],
    )
    .await
    .unwrap_err();
    assert_eq!(arc_err.code, "SweepArcTooLong");

    let pre_reaper_hp = sys.get_combat_state(reaper_hit_id).await.unwrap().unwrap().shield_hp;
    let reaper_res = call_svc(
        &ctx,
        &mut sess,
        "superWeaponMgr",
        "ActivateSlashModule",
        vec![
            PyRep::Integer(reaper_mod_id),
            PyRep::Real(0.0), PyRep::Real(-20_000.0), PyRep::Real(0.0),
            PyRep::Real(0.0), PyRep::Real(-20_000.0), PyRep::Real(20_000.0),
        ],
    )
    .await
    .unwrap();
    let reaper_kv = py_keyval_to_map(&reaper_res);
    assert_eq!(reaper_kv.get("fuelConsumed").and_then(|v| v.as_i64()), Some(50_000));
    assert_eq!(sys.get_combat_state(reaper_hit_id).await.unwrap().unwrap().shield_hp, pre_reaper_hp, "Reaper must not deal instant damage before warmup");

    ctx.advance_secs(12);
    let _ = sys.tick().await;
    assert_eq!(
        sys.get_combat_state(reaper_hit_id).await.unwrap().unwrap().shield_hp,
        pre_reaper_hp,
        "Target near end of Reaper arc (z=18km) must not be hit during early sweep ticks (z=0..6km)"
    );
    ctx.advance_secs(8);
    let _ = sys.tick().await;
    assert!(sys.get_combat_state(reaper_hit_id).await.unwrap().is_none_or(|c| c.shield_hp < pre_reaper_hp));

    ctx.advance_secs(10);
    let _ = sys.tick().await;
    assert!(sys.get_ball_snapshot(titan_id).await.unwrap().unwrap().1.max_velocity > 0.0);
    assert!(!sys.get_combat_state(titan_id).await.unwrap().unwrap().disallow_assistance);

    // 8. Negative abuse checks: offline module & unfitted cargo module refused
    ctx.db.with_mut_snapshot(|snap| { snap.module_online_states.insert(reaper_mod_id, false); });
    let offline_err = call_svc(&ctx, &mut sess, "superWeaponMgr", "ActivateSlashModule", vec![PyRep::Integer(reaper_mod_id), PyRep::Real(0.0), PyRep::Real(-20_000.0), PyRep::Real(0.0)]).await.unwrap_err();
    assert_eq!(offline_err.code, "ModuleNotOnline");

    ctx.db.with_mut_snapshot(|snap| { if let Some(m) = snap.items.get_mut(&reaper_mod_id) { m.flag = InventoryFlag::Cargo; } });
    let cargo_err = call_svc(&ctx, &mut sess, "superWeaponMgr", "ActivateSlashModule", vec![PyRep::Integer(reaper_mod_id), PyRep::Real(0.0), PyRep::Real(-20_000.0), PyRep::Real(0.0)]).await.unwrap_err();
    assert_eq!(cargo_err.code, "ModuleNotFitted");
}

#[tokio::test]
async fn test_insurgency_ice_heist_lifecycle() {
    // parity: feat:pve.insurgency-ice-heist
    eve_testkit::attest(&["feat:pve.insurgency-ice-heist"]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let sys_id = 30_002_537;
    let char_id = 90_001;
    let ship_id = 88_200_001;
    let bystander_id = 90_009;
    let bystander_ship = 88_200_009;
    seed_ship_in_space(&ctx, char_id, ship_id, 587, sys_id, DVec3::new(155_000.0, 0.0, 150_000.0)).await;
    seed_ship_in_space(&ctx, bystander_id, bystander_ship, 587, sys_id, DVec3::new(155_000.0, 0.0, 152_000.0)).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(500_001); // Caldari State Protectorate
        }
        snap.insurgency_runtime.campaigns.insert(
            1,
            PersistedInsurgencyCampaign {
                campaign_id: 1,
                pirate_faction_id: 500_010,
                target_faction_id: 500_001,
                fob_solar_system_id: sys_id,
                systems: vec![sys_id],
                created_at_unix: 0,
                forecasting_end_unix: 0,
                active_end_unix: 100_000,
                winddown_end_unix: 200_000,
                corruption_points: HashMap::from([(sys_id, 0)]),
                suppression_points: HashMap::new(),
            },
        );
    });
    let mut sess = make_sess(char_id, ship_id, sys_id, None);
    sess.role = 1 << 29; // ROLE_QA without ROLE_GM_MASK

    // 0. Negative: subrole ROLE_QA without ROLE_GM_MASK is refused on slash.SlashCmd with InsufficientRole
    let slash_err = call_svc(&ctx, &mut sess, "slash", "SlashCmd", vec![PyRep::String("/heal".into())])
        .await
        .unwrap_err();
    assert_eq!(slash_err.code, "InsufficientRole");

    // 1. Negative: corruption stage < 3 does not spawn Ice Heist on 1 Hz tick
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys.tick().await;
    let PyRep::List(pre_list) = call_svc(&ctx, &mut sess, "dungeonInstanceCacheMgr", "GetPirateInsurgencyIceHeistInstances", vec![])
        .await
        .unwrap()
    else {
        panic!("expected List");
    };
    assert!(pre_list.is_empty(), "Ice Heist must not spawn below corruption stage 3");

    // 2. Advance corruption to stage 3 (600 pts) via live `add_system_corruption` and verify 1 Hz tick materializes balls
    eve_server::services::insurgency_services::add_system_corruption(&ctx, 1, sys_id, 600);
    let _ = sys.tick().await;
    let PyRep::List(active_list) = call_svc(&ctx, &mut sess, "dungeonInstanceCacheMgr", "GetPirateInsurgencyIceHeistInstances", vec![])
        .await
        .unwrap()
    else {
        panic!("expected List");
    };
    assert_eq!(active_list.len(), 1);
    let inst_id = active_list[0].dict_get("instanceID").and_then(|v| v.as_i64()).unwrap();

    let tanker_id = 1_896_000_000 + inst_id * 10 + 1;
    let guard_id = 1_896_000_000 + inst_id * 10 + 2;
    let tanker_cs = sys.get_combat_state(tanker_id).await.unwrap().expect("Tanker ball must materialize in space");
    assert!(tanker_cs.warp_core_strength >= 100 && tanker_cs.is_web_immune);
    assert!(sys.get_ball_snapshot(guard_id).await.unwrap().is_some(), "Thunderchild guard (79986) must materialize in space");

    // Spawn a pirate cruiser (621) and pirate capsule (670) within 25km of Thunderchild guard (150_000, 0, 190_000)
    let pirate_char_id = 90_002;
    let pirate_ship_id = 88_200_002;
    let pod_char_id = 90_014;
    let pirate_pod_id = 88_200_014;
    seed_ship_in_space(&ctx, pirate_char_id, pirate_ship_id, 621, sys_id, DVec3::new(150_000.0, 0.0, 195_000.0)).await;
    seed_ship_in_space(&ctx, pod_char_id, pirate_pod_id, 670, sys_id, DVec3::new(150_000.0, 0.0, 196_000.0)).await;
    ctx.db.with_mut_snapshot(|snap| {
        for pid in [pirate_char_id, pod_char_id] {
            if let Some(ch) = snap.characters.get_mut(&pid) {
                ch.war_faction_id = Some(500_010);
            }
        }
    });
    let pre_pirate_hp = sys.get_combat_state(pirate_ship_id).await.unwrap().unwrap().shield_hp;

    // 3. Verify 1 tick deals weapon damage (NOT instant 1-tick deletion), then 80 ticks destroy pirate cruiser while sparing capsule
    let _ = sys.tick().await;
    let after_one_cs = sys.get_combat_state(pirate_ship_id).await.unwrap().expect("Thunderchild guard must apply weapon DPS, not 1-tick instant deletion");
    assert!(after_one_cs.shield_hp < pre_pirate_hp);

    for _ in 0..80 {
        let _ = sys.tick().await;
    }
    assert!(sys.get_ball_snapshot(pirate_ship_id).await.unwrap().is_none(), "Thunderchild guard weapon DPS must destroy nearby pirate ship");
    assert!(sys.get_ball_snapshot(pirate_pod_id).await.unwrap().is_some(), "Thunderchild guard must not execute capsules (670)");

    let PyRep::List(post_list) = call_svc(&ctx, &mut sess, "dungeonInstanceCacheMgr", "GetPirateInsurgencyIceHeistInstances", vec![])
        .await
        .unwrap()
    else {
        panic!("expected List");
    };
    assert_eq!(post_list[0].dict_get("completed"), Some(&PyRep::Bool(true)));
    assert_eq!(post_list[0].dict_get("empireTankersSaved"), Some(&PyRep::Integer(10)));

    let (militia_lp, bystander_lp) = ctx.db.with_snapshot(|snap| {
        (
            snap.characters.get(&char_id).and_then(|c| c.loyalty_points.get(&1_000_180).copied()).unwrap_or(0),
            snap.characters.get(&bystander_id).map(|c| c.loyalty_points.values().sum::<i64>()).unwrap_or(0),
        )
    });
    assert_eq!(militia_lp, 150_000, "Winning militia escort must earn 150,000 LP");
    assert_eq!(bystander_lp, 0, "Unaligned bystander must not earn Ice Heist LP");

    // 4. Verify 4h respawn cooldown blocks new Ice Heist on 1 Hz tick until 14,400s elapse
    let _ = sys.tick().await;
    let PyRep::List(cd_list) = call_svc(&ctx, &mut sess, "dungeonInstanceCacheMgr", "GetPirateInsurgencyIceHeistInstances", vec![])
        .await
        .unwrap()
    else {
        panic!("expected List");
    };
    assert_eq!(cd_list.len(), 1, "4-hour cooldown must block immediate Ice Heist respawn");
    ctx.advance_secs(14_401);
    let _ = sys.tick().await;
    let PyRep::List(respawn_list) = call_svc(&ctx, &mut sess, "dungeonInstanceCacheMgr", "GetPirateInsurgencyIceHeistInstances", vec![])
        .await
        .unwrap()
    else {
        panic!("expected List");
    };
    assert_eq!(respawn_list.len(), 2, "New Ice Heist instance must spawn after 4h cooldown expires");
}

#[tokio::test]
async fn test_deadspace_pocket_rules_and_mwd_disallowance() {
    // parity: feat:pve.deadspace-pocket-rules
    eve_testkit::attest(&["feat:pve.deadspace-pocket-rules"]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let sys_id = 30_000_142;
    let char_id = 90_001;
    let ship_id = 88_300_001;
    let mwd_id = 88_300_002;
    let gate1_id = 88_300_010;
    let gate2_id = 88_300_011;
    let room2_obj_id = 88_300_012;

    let entry_gate_pos = [500_000.0, 0.0, 500_000.0];
    let room1_dest = [600_000.0, 0.0, 600_000.0];
    let room2_dest = [800_000.0, 0.0, 800_000.0];

    seed_ship_in_space(&ctx, char_id, ship_id, 587, sys_id, DVec3::from_array(entry_gate_pos)).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mwd_id,
            InventoryItemRecord {
                item_id: mwd_id,
                type_id: 434, // 5MN Microwarpdrive I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(
            777,
            DungeonRuntimeInstance {
                instance_id: 777,
                dungeon_id: 9001,
                dungeon_name: "Gated Deadspace Pocket".into(),
                archetype: "Combat".into(),
                category: "Combat".into(),
                solar_system_id: sys_id,
                owner_char_id: char_id,
                base_position: entry_gate_pos,
                entry_gate_ball_id: Some(gate1_id),
                entry_gate_pos: Some(entry_gate_pos),
                entry_gate_locked: false,
                active_objective_id: 0,
                completed_task_ids: Vec::new(),
                last_completed_task_id: None,
                current_room_index: 0,
                disallow_mwd: false,
                completed: false,
                despawned: false,
                created_unix: 0,
                rooms: vec![
                    DungeonRuntimeRoom {
                        room_index: 1,
                        room_id: 1,
                        room_name: "Room 1".into(),
                        position: room1_dest,
                        gate_destination_pos: room1_dest,
                        gate_ball_id: Some(gate2_id),
                        gate_locked: false,
                        disallow_mwd: Some(false),
                        gated: true,
                        cleared: true,
                        spawned: true,
                        npc_groups: Vec::new(),
                        objects: Vec::new(),
                        triggers: Vec::new(),
                    },
                    DungeonRuntimeRoom {
                        room_index: 2,
                        room_id: 2,
                        room_name: "Room 2 (No MWD)".into(),
                        position: room2_dest,
                        gate_destination_pos: room2_dest,
                        gate_ball_id: None,
                        gate_locked: false,
                        disallow_mwd: Some(true),
                        gated: true,
                        cleared: false,
                        spawned: true,
                        npc_groups: Vec::new(),
                        objects: vec![DungeonRuntimeObject {
                            ball_id: room2_obj_id,
                            type_id: 23,
                            name: "Overseer Structure".into(),
                            position: room2_dest,
                            radius: 100.0,
                            is_container: true,
                            spawned: true,
                            loot_items: Vec::new(),
                        }],
                        triggers: Vec::new(),
                    },
                ],
            },
        );
    });

    let mut sess = make_sess(char_id, ship_id, sys_id, None);
    sess.role = u64::MAX;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let mut r2_ball = make_site_npc_ball(room2_obj_id, 23, DVec3::from_array(room2_dest));
    r2_ball.mode = MovementMode::Rigid;
    let _ = sys.spawn_ball(r2_ball).await;

    // 1. Negative: direct warp into gated Room 2 refused
    let err_warp = call_svc(
        &ctx,
        &mut sess,
        "beyonce",
        "CmdWarpToStuff",
        vec![PyRep::String("item".into()), PyRep::Integer(room2_obj_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_warp.code, "WarpBlockedByDeadspacePocket");

    // 2. ActivateAccelerationGate into Room 1 -> lands at room1_dest, and MWD activation succeeds
    call_svc(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", vec![PyRep::Integer(gate1_id)])
        .await
        .unwrap();
    let (_, b1) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(b1.position.distance(DVec3::from_array(room1_dest)) < 5_000.0);

    call_svc(&ctx, &mut sess, "dogmaIM", "Activate", vec![PyRep::Integer(mwd_id), PyRep::String("online".into())])
        .await
        .expect("MWD online must succeed in Room 1");
    call_svc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        vec![PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into())],
    )
    .await
    .expect("MWD propulsion activation must succeed in MWD-allowed Room 1");
    let _ = call_svc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Deactivate",
        vec![PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into())],
    )
    .await;

    // 3. Move to gate2 and activate into MWD-disallowed Room 2
    let mut b_at_g2 = b1.clone();
    b_at_g2.position = DVec3::new(room1_dest[0] + 5_000.0, room1_dest[1], room1_dest[2]);
    let _ = sys.spawn_ball(b_at_g2).await;
    call_svc(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", vec![PyRep::Integer(gate2_id)])
        .await
        .unwrap();

    // 4. Negative: MWD propulsion activation refused in Room 2 where disallow_mwd == true
    let err_mwd = call_svc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        vec![PyRep::Integer(mwd_id), PyRep::String("moduleBonusMicrowarpdrive".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err_mwd.code, "MicroWarpdriveDisallowedInDeadspace");
}

}

pub(crate) mod pirate_fob_spawn_defense_scaling {
//! Integration tests for Pirate FOBs, Pochven Observatory Flashpoint & World Ark,
//! Abyssal Deadspace Weather Dogma Modifiers, and Deathless Shipcaster to FOB:
//! - `feat:pve.pirate-fob-forward-operating-base`
//! - `feat:pve.triglavian-world-ark-observatory`
//! - `feat:navigation.deathless-shipcaster-fob`

use crate::common::*;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::pve_abyssal_hazards::{
    apply_abyssal_weather_dogma_modifiers_to_ship, restore_abyssal_weather_dogma_modifiers,
};
use eve_server::services::pve_incursion_sites::make_site_npc_ball;
use eve_server::services::pve_special_sites_abyssal::AbyssalWeatherKind;
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;
use std::collections::HashMap;

fn make_sess(char_id: i64, ship_id: i64, sys_id: u32, station_id: Option<i64>) -> SessionState {
    SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        ship_id: Some(ship_id),
        solar_system_id: Some(sys_id),
        station_id,
        ..Default::default()
    }
}

async fn call_svc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: Vec<PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, &args, &HashMap::new(), None)
        .await
}

async fn seed_ship_in_space(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) {
    ctx.db.with_mut_snapshot(|snap| {
        if !snap.characters.contains_key(&char_id) {
            let mut template = snap.characters.get(&90_001).or_else(|| snap.characters.values().next()).cloned().unwrap();
            template.character_id = char_id;
            template.name = format!("Pilot{char_id}");
            template.war_faction_id = None;
            template.loyalty_points.clear();
            snap.characters.insert(char_id, template);
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: ship_type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::new(),
                ..Default::default()
            },
        );
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            make_site_npc_ball(ship_id, ship_type_id, pos),
            ctx.build_type_combat_state(ship_type_id),
        )
        .await;
}

#[tokio::test]
async fn test_pirate_fob_spawn_defense_scaling_and_loot_bounty() {
    // parity: feat:pve.pirate-fob-forward-operating-base
    eve_testkit::attest(&["feat:pve.pirate-fob-forward-operating-base"]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let sys_id = 30_002_537; // Amamake (low-sec 0.44 -> Blood Raiders Stronghold 46364)
    let char_id = 90_001;
    let ship_id = 88_500_001;

    // Negative: invalid faction and null-sec system refused
    assert_eq!(
        eve_server::services::pve_fob_observatory::spawn_pirate_fob(&ctx, sys_id, 500_001).await.unwrap_err().code,
        "InvalidPirateFaction"
    );
    assert_eq!(
        eve_server::services::pve_fob_observatory::spawn_pirate_fob(&ctx, 30_000_157, 500_012).await.unwrap_err().code,
        "InvalidSystemSecurity"
    );

    // 1. Seed pilots in 5 low/mid-sec systems and verify cluster tick enforces <= 3 active FOBs at dynamic coordinates
    let mut extra_sys: Vec<u32> = ctx.sde.solar_systems.values()
        .filter(|s| s.solar_system_id > sys_id && s.security_status > 0.1 && s.security_status < 0.7)
        .map(|s| s.solar_system_id)
        .collect();
    extra_sys.sort_unstable();
    seed_ship_in_space(&ctx, char_id, ship_id, 11_567, sys_id, DVec3::ZERO).await;
    for (i, &es) in extra_sys.iter().take(4).enumerate() {
        seed_ship_in_space(&ctx, 90_050 + (i as i64), 88_550_000 + (i as i64), 587, es, DVec3::ZERO).await;
    }

    tick_background_cluster(&ctx).await;

    let (fob_id, fob_pos, active_fob_count) = ctx.db.with_snapshot(|snap| {
        let total = snap.structures.values().filter(|s| matches!(s.type_id, 46_363 | 46_364)).count();
        let st = snap.structures.values().find(|s| s.solar_system_id == sys_id && s.type_id == 46_364).unwrap();
        (st.structure_id, DVec3::from_array(st.position), total)
    });
    assert_eq!(active_fob_count, 3, "Living-world cluster tick must cap active Pirate FOBs at 3");
    assert_ne!(fob_pos, DVec3::new(200_000.0, 0.0, -200_000.0));

    // Place attacking fleet on FOB grid (Titan=4, BS=3, non-combat Cruiser bystander=2 -> weight 9) + 1 off-grid pilot
    let ship2_id = 88_500_002;
    seed_ship_in_space(&ctx, char_id, ship_id, 11_567, sys_id, fob_pos + DVec3::new(0.0, 0.0, 3_500.0)).await;
    seed_ship_in_space(&ctx, 90_002, ship2_id, 638, sys_id, fob_pos + DVec3::new(0.0, 0.0, -3_500.0)).await;
    seed_ship_in_space(&ctx, 90_003, 88_500_003, 621, sys_id, fob_pos + DVec3::new(15_000.0, 0.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_004, 88_500_004, 587, sys_id, fob_pos + DVec3::new(2_000_000.0, 0.0, 0.0)).await;

    let sys = ctx.systems.get_or_spawn(sys_id);
    let init_balls = sys.list_balls().await.unwrap_or_default();
    assert_eq!(init_balls.iter().filter(|b| b.type_id == 24_000).count(), 3, "FOB must launch 3 Corpatis Bishop raiding cruisers");

    // 2. Fit Smartbombs (3995) on the 2 combat attackers (no superweapons in low-sec!) and attack FOB via live RPCs
    let sb1_id = 88_500_010;
    let sb2_id = 88_500_011;
    ctx.db.with_mut_snapshot(|snap| {
        for (mid, owner, loc) in [(sb1_id, char_id, ship_id), (sb2_id, 90_002, ship2_id)] {
            snap.items.insert(mid, InventoryItemRecord {
                item_id: mid, type_id: 3_995, owner_id: owner, location_id: loc,
                flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
            });
            snap.module_online_states.insert(mid, true);
        }
        for cid in [char_id, 90_002, 90_003, 90_004] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.balance = IskCents::ZERO;
            }
        }
    });

    let mut sess1 = make_sess(char_id, ship_id, sys_id, None);
    call_svc(&ctx, &mut sess1, "dogmaIM", "Activate", vec![PyRep::Integer(sb1_id)])
        .await
        .expect("Smartbomb activation against FOB");

    let mid_balls = sys.list_balls().await.unwrap_or_default();
    assert_eq!(
        mid_balls.iter().filter(|b| b.type_id == 16_934).count(),
        9,
        "FOB defense response must scale to on-grid fleet composition (4+3+2=9 Corpus Prophets)"
    );

    // 3. Finish destroying FOB via second pilot's live Smartbomb -> drops Dark Blood loot (13791) & pays 30M ISK strictly to combat attackers
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&fob_id) {
            st.shield_hp = 0.0;
            st.armor_hp = 0.0;
            st.hull_hp = 50.0;
        }
    });
    let mut sess2 = make_sess(90_002, ship2_id, sys_id, None);
    call_svc(&ctx, &mut sess2, "dogmaIM", "Activate", vec![PyRep::Integer(sb2_id)])
        .await
        .expect("Killing Smartbomb activation against FOB");

    let loot_cont_id = fob_id + 90;
    let (st_state, loot_type, b1, b2, bystander_bal, offgrid_bal) = ctx.db.with_snapshot(|snap| {
        let state = snap.structures.get(&fob_id).map(|s| s.state.clone()).unwrap_or_default();
        let loot = snap.items.values().find(|i| i.location_id == loot_cont_id).map(|i| i.type_id);
        let bal = |cid: i64| snap.characters.get(&cid).map(|c| c.balance.to_wire_f64()).unwrap_or(-1.0);
        (state, loot, bal(char_id), bal(90_002), bal(90_003), bal(90_004))
    });
    assert_eq!(st_state, "Destroyed");
    assert_eq!(loot_type, Some(13_791), "Blood Raider FOB must drop SDE Dark Blood Dual Heavy Pulse Laser (13791)");
    assert!((b1 - 30_000_000.0).abs() < 1.0 && (b2 - 30_000_000.0).abs() < 1.0, "Each on-grid combat attacker must receive 30M ISK base FOB bounty");
    assert_eq!(bystander_bal, 0.0, "Non-combatant on-grid bystander must receive 0 ISK");
    assert_eq!(offgrid_bal, 0.0, "Off-grid pilot must receive 0 ISK");
}

#[tokio::test]
async fn test_pochven_observatory_flashpoint_and_world_ark() {
    // parity: feat:pve.triglavian-world-ark-observatory
    eve_testkit::attest(&["feat:pve.triglavian-world-ark-observatory"]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let pochven_sys_id = 30_000_157; // Otela in Pochven (region 10000070)
    let highsec_sys_id = 30_000_142;
    let char_id = 90_001;
    let ship_id = 88_600_001;

    // 1. Negative: non-Pochven system refused
    assert_eq!(
        eve_server::services::pve_fob_observatory::spawn_observatory_flashpoint(&ctx, highsec_sys_id).await.unwrap_err().code,
        "NotInPochven"
    );

    // 2. Seed pilots in 4 Pochven systems and verify cluster tick enforces <= 3 active Observatory Flashpoints
    let mut poch_systems: Vec<u32> = ctx.sde.solar_systems.values()
        .filter(|s| s.region_id == 10_000_070 && s.solar_system_id > pochven_sys_id)
        .map(|s| s.solar_system_id)
        .collect();
    poch_systems.sort_unstable();
    seed_ship_in_space(&ctx, char_id, ship_id, 587, pochven_sys_id, DVec3::ZERO).await;
    for (i, &ps) in poch_systems.iter().take(3).enumerate() {
        seed_ship_in_space(&ctx, 90_060 + (i as i64), 88_660_000 + (i as i64), 587, ps, DVec3::ZERO).await;
    }
    tick_background_cluster(&ctx).await;

    let active_obs = ctx.db.with_snapshot(|s| s.service_state_blobs.keys().filter(|k| k.starts_with("observatory_site:")).count());
    assert_eq!(active_obs, 3, "Pochven region must cap active Observatory Flashpoints at 3");

    let obs = eve_server::services::pve_fob_observatory::get_observatory_site_state(&ctx, pochven_sys_id)
        .expect("Observatory state must be persisted in Otela");
    let site_id = obs.site_id;
    let gate_id = obs.gate_id;
    let world_ark_id = obs.world_ark_id;
    let gate_pos = DVec3::from_array(obs.gate_pos);
    let site_pos = DVec3::from_array(obs.site_pos);
    let sys = ctx.systems.get_or_spawn(pochven_sys_id);
    let balls = sys.list_balls().await.unwrap_or_default();
    assert!(balls.iter().any(|b| b.id == world_ark_id && b.type_id == 52_361));
    assert_eq!(balls.iter().filter(|b| matches!(b.type_id, 52_338 | 52_184 | 52_187)).count(), 3);

    // 3. Negative gate/pocket checks: direct warp blocked, docked blocked, >2,500m range blocked, capital blocked, <3 fleet & <0 standing blocked
    let mut sess = make_sess(char_id, ship_id, pochven_sys_id, None);
    let err_warp = call_svc(&ctx, &mut sess, "beyonce", "CmdWarpToStuff", vec![PyRep::String("item".into()), PyRep::Integer(world_ark_id)]).await.unwrap_err();
    assert_eq!(err_warp.code, "WarpBlockedByDeadspacePocket");

    ctx.db.with_mut_snapshot(|s| { if let Some(c) = s.characters.get_mut(&char_id) { c.station_id = Some(60_003_760); } });
    let mut docked_sess = make_sess(char_id, ship_id, pochven_sys_id, Some(60_003_760));
    assert_eq!(call_svc(&ctx, &mut docked_sess, "keeper", "ActivateAccelerationGate", vec![PyRep::Integer(gate_id)]).await.unwrap_err().code, "CantDoThatWhileDocked");
    ctx.db.with_mut_snapshot(|s| { if let Some(c) = s.characters.get_mut(&char_id) { c.station_id = None; } });

    seed_ship_in_space(&ctx, char_id, ship_id, 587, pochven_sys_id, gate_pos + DVec3::new(10_000.0, 0.0, 0.0)).await;
    assert_eq!(call_svc(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", vec![PyRep::Integer(gate_id)]).await.unwrap_err().code, "OutsideOperationalRange");

    seed_ship_in_space(&ctx, char_id, ship_id, 11_567, pochven_sys_id, gate_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    assert_eq!(call_svc(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", vec![PyRep::Integer(gate_id)]).await.unwrap_err().code, "ShipClassRestricted");

    seed_ship_in_space(&ctx, char_id, ship_id, 587, pochven_sys_id, gate_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    assert_eq!(call_svc(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", vec![PyRep::Integer(gate_id)]).await.unwrap_err().code, "InsufficientFleetComposition");

    seed_ship_in_space(&ctx, 90_002, 88_600_002, 587, pochven_sys_id, gate_pos + DVec3::new(1_200.0, 0.0, 0.0)).await;
    seed_ship_in_space(&ctx, 90_003, 88_600_003, 587, pochven_sys_id, gate_pos + DVec3::new(1_400.0, 0.0, 0.0)).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(500_026, -2.5);
        }
        snap.fleets.insert(777_001, eve_db::FleetRecord {
            fleet_id: 777_001, boss_id: char_id, members: vec![char_id, 90_002, 90_003], ..Default::default()
        });
    });
    assert_eq!(call_svc(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", vec![PyRep::Integer(gate_id)]).await.unwrap_err().code, "StandingAccessDenied");

    // 4. Seed 16 subcapital fleet ships within 2,500m of gate + 1 off-grid fleet ship, enter pocket via gate, and kill World Ark via live subcap weapon
    let mut fleet_ids = Vec::new();
    for idx in 0..16_i64 {
        let cid = 95_000 + idx;
        let sid = 88_610_000 + idx;
        let hull = if idx == 0 { 638 } else { 587 };
        fleet_ids.push(cid);
        seed_ship_in_space(&ctx, cid, sid, hull, pochven_sys_id, gate_pos + DVec3::new((idx as f64) * 50.0, 0.0, 500.0)).await;
    }
    let offgrid_cid = 95_099_i64;
    seed_ship_in_space(&ctx, offgrid_cid, 88_610_099, 587, pochven_sys_id, DVec3::new(5_000_000.0, 0.0, 0.0)).await;
    let mut all_members = vec![offgrid_cid];
    all_members.extend_from_slice(&fleet_ids);

    let sb_id = 88_620_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        for &cid in &all_members {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.standings.insert(500_026, 1.0);
                ch.balance = IskCents::ZERO;
                ch.loyalty_points.clear();
            }
        }
        snap.fleets.insert(777_001, eve_db::FleetRecord {
            fleet_id: 777_001, boss_id: fleet_ids[0], members: all_members.clone(), ..Default::default()
        });
        snap.items.insert(sb_id, InventoryItemRecord {
            item_id: sb_id, type_id: 3_995, owner_id: fleet_ids[0], location_id: 88_610_000,
            flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
        });
        snap.module_online_states.insert(sb_id, true);
    });

    for (idx, &cid) in fleet_ids.iter().enumerate() {
        let mut fsess = make_sess(cid, 88_610_000 + (idx as i64), pochven_sys_id, None);
        call_svc(&ctx, &mut fsess, "keeper", "ActivateAccelerationGate", vec![PyRep::Integer(gate_id)])
            .await
            .expect("Observatory gate must admit subcapital fleet member within 2,500m");
    }

    let _ = sys.set_ball_position(88_610_000, site_pos + DVec3::new(0.0, 0.0, 2_000.0)).await;
    let mut ark_cs = sys.get_combat_state(world_ark_id).await.unwrap().unwrap();
    ark_cs.shield_hp = 0.0;
    ark_cs.armor_hp = 0.0;
    ark_cs.structure_hp = 50.0;
    let _ = sys.set_combat_state(world_ark_id, ark_cs).await;

    let mut leader_sess = make_sess(fleet_ids[0], 88_610_000, pochven_sys_id, None);
    call_svc(&ctx, &mut leader_sess, "dogmaIM", "Activate", vec![PyRep::Integer(sb_id)])
        .await
        .expect("Live subcapital weapon kills World Ark boss");

    let (off_isk, p15_isk, p15_lp, p16_isk, site_exists) = ctx.db.with_snapshot(|snap| {
        let coff = snap.characters.get(&offgrid_cid).unwrap();
        let c15 = snap.characters.get(&fleet_ids[14]).unwrap();
        let c16 = snap.characters.get(&fleet_ids[15]).unwrap();
        (
            coff.balance.to_wire_f64(),
            c15.balance.to_wire_f64(),
            c15.loyalty_points.get(&1_000_137).copied().unwrap_or(0),
            c16.balance.to_wire_f64(),
            snap.service_state_blobs.contains_key(&format!("observatory_site:{site_id}")),
        )
    });
    assert_eq!(off_isk, 0.0, "Off-grid fleet member must not consume an Observatory payout slot");
    assert!((p15_isk - 100_000_000.0).abs() < 1.0);
    assert_eq!(p15_lp, 10_000);
    assert_eq!(p16_isk, 0.0, "16th pocket fleet member must receive 0 payout");
    assert!(!site_exists, "observatory_site:<site_id> must be deleted on completion");
}

#[tokio::test]
async fn test_abyssal_weather_dogma_modifiers_all_five_storms() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let sys_id = 32_000_001;
    let char_id = 90_001;
    let ship_id = 88_700_001;
    seed_ship_in_space(&ctx, char_id, ship_id, 587, sys_id, DVec3::ZERO).await;

    for weather in [
        AbyssalWeatherKind::Electrical,
        AbyssalWeatherKind::Dark,
        AbyssalWeatherKind::Exotic,
        AbyssalWeatherKind::Firestorm,
        AbyssalWeatherKind::Gamma,
    ] {
        let (orig_267, orig_269) = ctx.sde.types.get(&587).map(|t| (
            t.default_attributes.get(&267).copied().unwrap_or(0.5),
            t.default_attributes.get(&269).copied().unwrap_or(0.65),
        )).unwrap_or((0.5, 0.65));
        let saved = apply_abyssal_weather_dogma_modifiers_to_ship(&ctx, ship_id, weather, 50.0, 50.0);
        assert!(!saved.is_empty());
        ctx.db.with_snapshot(|snap| {
            let ship = snap.items.get(&ship_id).unwrap();
            match weather {
                AbyssalWeatherKind::Electrical => {
                    assert!(ship.dynamic_attributes[&271] > 0.5);
                    assert!(ship.dynamic_attributes[&267] > orig_267);
                    assert!(ship.dynamic_attributes[&55] < 250_000.0);
                }
                AbyssalWeatherKind::Dark => {
                    assert!(ship.dynamic_attributes[&37] > 250.0);
                    assert!((ship.dynamic_attributes[&54] - 7_500.0).abs() < 1e-6);
                }
                AbyssalWeatherKind::Exotic => {
                    assert!(ship.dynamic_attributes[&273] > 0.5);
                    assert!(ship.dynamic_attributes[&269] > orig_269);
                    assert!(ship.dynamic_attributes[&564] > 250.0);
                }
                AbyssalWeatherKind::Firestorm => {
                    assert!(ship.dynamic_attributes[&265] > 500.0);
                    assert!(ship.dynamic_attributes[&274] > 0.5);
                }
                AbyssalWeatherKind::Gamma => {
                    assert!(ship.dynamic_attributes[&263] > 500.0);
                    assert!(ship.dynamic_attributes[&272] > 0.5);
                }
            }
        });
        restore_abyssal_weather_dogma_modifiers(&ctx, ship_id, &saved);
    }

    // Enter an Abyssal pocket (Electrical T1 filament 47765) and verify V24 SDE Electrical weather modifiers on NPC
    let mut sess = make_sess(char_id, ship_id, 30_000_142, None);
    let (_pid, r1_npcs) = eve_server::services::pve_special_sites_abyssal::enter_abyssal_pocket_with_trace(
        &ctx,
        &mut sess,
        char_id,
        47_765,
        1,
        30_000_142,
        None,
    )
    .await;
    assert!(!r1_npcs.is_empty());
    let abyss_sys = ctx.systems.get_or_spawn(32_000_001);
    let (_, npc_ball) = abyss_sys.get_ball_snapshot(r1_npcs[0]).await.unwrap().unwrap();
    let base_npc_cs = ctx.build_type_combat_state(npc_ball.type_id);
    let npc_cs = abyss_sys.get_combat_state(r1_npcs[0]).await.unwrap().unwrap();
    assert!(
        npc_cs.armor_resonance[0] >= base_npc_cs.armor_resonance[0]
            && npc_cs.capacitor_recharge_time_ms < base_npc_cs.capacitor_recharge_time_ms,
        "Abyssal NPCs must receive V24 SDE Electrical EM resonance penalty and faster capacitor recharge"
    );
}

#[tokio::test]
async fn test_deathless_shipcaster_zarzakh_to_fob() {
    // parity: feat:navigation.deathless-shipcaster-fob
    eve_testkit::attest(&["feat:navigation.deathless-shipcaster-fob"]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let zarzakh_id = 30_100_000;
    let campaign_fob_sys = 30_002_813; // Tama
    let connected_sys_id = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.to_solar_system_id == campaign_fob_sys)
        .map(|j| j.from_solar_system_id)
        .expect("Tama must have an SDE stargate neighbor");
    let unconnected_sys_id = 30_000_142; // Jita (not 1-hop connected to Tama)
    let char_id = 90_001;
    let ship_id = 88_400_001;
    let launcher_id = 88_400_010;
    let guristas_pad_id = 88_400_011;
    let unconnected_pad_id = 88_400_012;

    seed_ship_in_space(&ctx, char_id, ship_id, 587, zarzakh_id, DVec3::new(10_000.0, 0.0, 10_000.0)).await;
    let sys = ctx.systems.get_or_spawn(zarzakh_id);
    let mut l_ball = make_site_npc_ball(launcher_id, 77_262, DVec3::new(10_500.0, 0.0, 10_000.0));
    l_ball.mode = eve_destiny::MovementMode::Rigid;
    let _ = sys.spawn_ball(l_ball).await;

    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.insurgency_runtime.campaigns.insert(
            77,
            eve_db::PersistedInsurgencyCampaign {
                campaign_id: 77,
                pirate_faction_id: 500_010,
                target_faction_id: 500_001,
                fob_solar_system_id: campaign_fob_sys,
                systems: vec![campaign_fob_sys],
                created_at_unix: now_u,
                forecasting_end_unix: now_u,
                active_end_unix: now_u + 100_000,
                winddown_end_unix: now_u + 200_000,
                corruption_points: HashMap::new(),
                suppression_points: HashMap::new(),
            },
        );
    });

    eve_server::services::cluster_macho_misc_services::with_misc_state(&ctx, |st| {
        st.launchers.push(eve_server::services::cluster_macho_misc_services::LauncherRecord {
            item_id: launcher_id,
            type_id: 77_262,
            solar_system_id: zarzakh_id,
            faction_id: 0,
            position: DVec3::new(10_500.0, 0.0, 10_000.0),
            linked_pad_ids: vec![guristas_pad_id, unconnected_pad_id],
        });
        for (pid, psys) in [(guristas_pad_id, connected_sys_id), (unconnected_pad_id, unconnected_sys_id)] {
            st.landing_pads.push(eve_server::services::cluster_macho_misc_services::LandingPadRecord {
                item_id: pid,
                type_id: 77_263,
                built_type_id: 77_263,
                solar_system_id: psys,
                faction_id: 500_010, // Guristas
                position: DVec3::new(50_000.0, 0.0, 50_000.0),
                is_built: true,
                is_disrupted: false,
                link_timestamp: 1,
                required_materials: Default::default(),
                contributed_materials: Default::default(),
            });
        }
    });

    let mut sess = make_sess(char_id, ship_id, zarzakh_id, None);

    // 1. Negative: non-pirate character refused
    let err_non_pirate = call_svc(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        vec![PyRep::Integer(launcher_id), PyRep::Integer(i64::from(connected_sys_id)), PyRep::Integer(guristas_pad_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_non_pirate.code, "NonPirateRefused");

    // 2. Negative: Angel Cartel (500011) refused on Guristas (500010) FOB pad
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(500_011);
        }
    });
    let err_mismatch = call_svc(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        vec![PyRep::Integer(launcher_id), PyRep::Integer(i64::from(connected_sys_id)), PyRep::Integer(guristas_pad_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_mismatch.code, "FobFactionMismatch");

    // 3. Negative: unconnected destination system rejected via SDE stargate topology (`InsurgencyWarzoneNotConnected`)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = Some(500_010);
        }
    });
    let err_unconn = call_svc(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        vec![PyRep::Integer(launcher_id), PyRep::Integer(i64::from(unconnected_sys_id)), PyRep::Integer(unconnected_pad_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_unconn.code, "InsurgencyWarzoneNotConnected");

    // 4. Positive: Guristas (500010) jumps to SDE stargate-connected insurgency warzone system
    call_svc(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        vec![PyRep::Integer(launcher_id), PyRep::Integer(i64::from(connected_sys_id)), PyRep::Integer(guristas_pad_id)],
    )
    .await
    .unwrap();
    assert_eq!(sess.solar_system_id, Some(connected_sys_id));

    // 5. Negative: persisted per-target cooldown blocks immediate repeat jump
    seed_ship_in_space(&ctx, char_id, ship_id, 587, zarzakh_id, DVec3::new(10_000.0, 0.0, 10_000.0)).await;
    sess.solar_system_id = Some(zarzakh_id);
    let err_cd = call_svc(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        vec![PyRep::Integer(launcher_id), PyRep::Integer(i64::from(connected_sys_id)), PyRep::Integer(guristas_pad_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_cd.code, "ShipcasterTargetOnCooldown");
}

}

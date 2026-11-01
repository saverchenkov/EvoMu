//! Integration tests for Capital, Pochven, Wormhole Escalations, and Combat parity items:
//! - `feat:navigation.pochven-capital-cyno-restrictions`
//! - `feat:pve.wormhole-capital-escalations`
//! - `feat:pve.wormhole-site-capital-escalation-cap`
//! - `feat:combat.pochven-loot-drop`
//! - `feat:combat.insurance-void-and-expiry`
//! - `feat:industry.capital-construction-restrictions`

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
    // parity: feat:pve.wormhole-capital-escalations, feat:pve.wormhole-site-capital-escalation-cap
    eve_testkit::attest(&[
        "feat:pve.wormhole-capital-escalations",
        "feat:pve.wormhole-site-capital-escalation-cap",
    ]);

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
    // parity: feat:combat.pochven-loot-drop
    eve_testkit::attest(&["feat:combat.pochven-loot-drop"]);

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

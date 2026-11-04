//! Dedicated integration tests for remediated tactical modules parity rows (Part 1):
//! - `feat:modules.cyno-lighting-rules`
//! - `feat:modules.assault-damage-control`

#![forbid(unsafe_code)]

use crate::sovereignty_equinox_gameplay::{
    open_sqlite_ctx, place_ship_ball, register_online_pilot, rpc_call, set_pilot_location,
    temp_sqlite_path, ALLIANCE, CORP, PILOT, PILOT_SHIP,
};
use eve_db::{
    FleetRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_proto::PyRep;
use eve_server::services::structure_acl_defense_rules::TYPE_MOBILE_CYNO_INHIBITOR;
use glam::DVec3;
use std::collections::HashMap;

fn err_code<T: std::fmt::Debug>(res: Result<T, eve_proto::BridgeRpcError>) -> String {
    res.unwrap_err().code
}

#[tokio::test]
async fn test_cyno_lighting_rules_parity_remediation() {
    // parity: feat:modules.cyno-lighting-rules
    eve_testkit::attest(&["feat:modules.cyno-lighting-rules"]);

    let db_path = temp_sqlite_path("parity_cyno_lighting_rules");
    let ctx = open_sqlite_ctx(&db_path, true);
    let high_sys = 30_000_142u32; // Jita (0.95 highsec)
    let low_sys = 30_002_718u32;  // Rancer (0.23 lowsec)
    let wh_sys = 31_000_001u32;   // Wormhole space

    let mut s_recon = register_online_pilot(
        &ctx,
        88_101,
        "demo",
        "demo-secret",
        PILOT,
        CORP,
        Some(ALLIANCE),
        high_sys,
        PILOT_SHIP,
        11_957, // Falcon (Force Recon)
        DVec3::ZERO,
        0,
    )
    .await;

    let cyno_mod_id = 988_701i64;
    let ozone_id = 988_702i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cyno_mod_id,
            InventoryItemRecord {
                item_id: cyno_mod_id,
                type_id: 21_096, // Cynosural Field Generator I
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            ozone_id,
            InventoryItemRecord {
                item_id: ozone_id,
                type_id: 16_273, // Liquid Ozone
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::Cargo,
                quantity: 500,
                ..Default::default()
            },
        );
        snap.skills.entry(PILOT).or_default().insert(
            21_603, // Cynosural Field Theory V
            TrainedSkillRecord {
                type_id: 21_603,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });

    // 1. Hull restriction: normal cynos fit only on Force Recons and Black Ops.
    let mut cyno_skills = HashMap::new();
    cyno_skills.insert(21_603, 5); // Cynosural Field Theory V

    let mut fit_falcon = eve_dogma::ShipFittingEngine::from_sde(1, 11_957, &ctx.sde);
    fit_falcon.character_skills = cyno_skills.clone();
    assert!(
        fit_falcon.fit_module(2, 21_096, InventoryFlag::HiSlot0 as u16, &ctx.sde).is_ok(),
        "normal cyno must fit on Force Recon (Falcon 11957)"
    );

    let mut fit_redeemer = eve_dogma::ShipFittingEngine::from_sde(1, 22_428, &ctx.sde);
    fit_redeemer.character_skills = cyno_skills.clone();
    assert!(
        fit_redeemer.fit_module(2, 21_096, InventoryFlag::HiSlot0 as u16, &ctx.sde).is_ok(),
        "normal cyno must fit on Black Ops (Redeemer 22428)"
    );

    let mut fit_rifter = eve_dogma::ShipFittingEngine::from_sde(1, 587, &ctx.sde);
    fit_rifter.character_skills = cyno_skills.clone();
    assert!(
        fit_rifter.fit_module(2, 21_096, InventoryFlag::HiSlot0 as u16, &ctx.sde).is_err(),
        "normal cyno must not fit on Frigate (Rifter 587)"
    );

    let mut fit_megathron = eve_dogma::ShipFittingEngine::from_sde(1, 641, &ctx.sde);
    fit_megathron.character_skills = cyno_skills.clone();
    assert!(
        fit_megathron.fit_module(2, 21_096, InventoryFlag::HiSlot0 as u16, &ctx.sde).is_err(),
        "normal cyno must not fit on Battleship (Megathron 641)"
    );

    let mut fit_rook = eve_dogma::ShipFittingEngine::from_sde(1, 11_959, &ctx.sde);
    fit_rook.character_skills = cyno_skills.clone();
    assert!(
        fit_rook.fit_module(2, 21_096, InventoryFlag::HiSlot0 as u16, &ctx.sde).is_err(),
        "normal cyno must not fit on Combat Recon (Rook 11959)"
    );

    // 2. High-sec refusal: refused in high-sec.
    assert_eq!(
        err_code(
            rpc_call(
                &ctx,
                &mut s_recon,
                "dogmaIM",
                "Activate",
                &[PyRep::Integer(cyno_mod_id), PyRep::String("effects.CynosuralGeneration".into())],
            )
            .await
        ),
        "CannotLightCynoInSystem"
    );

    // Move to low-sec.
    let _ = ctx.systems.get_or_spawn(high_sys).remove_ball(PILOT_SHIP).await;
    set_pilot_location(&ctx, &mut s_recon, low_sys, None);
    place_ship_ball(&ctx, low_sys, PILOT, PILOT_SHIP, 11_957, DVec3::ZERO).await;

    // 3. Activation requires fleet membership.
    assert_eq!(
        err_code(
            rpc_call(
                &ctx,
                &mut s_recon,
                "dogmaIM",
                "Activate",
                &[PyRep::Integer(cyno_mod_id), PyRep::String("effects.CynosuralGeneration".into())],
            )
            .await
        ),
        "MustBeInFleet"
    );

    // Create fleet.
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            77_101,
            FleetRecord {
                fleet_id: 77_101,
                boss_id: PILOT,
                members: vec![PILOT],
                ..Default::default()
            },
        );
    });

    // 4. Activation requires speed below 500 m/s.
    let sys_low = ctx.systems.get_or_spawn(low_sys);
    let _ = sys_low.remove_ball(PILOT_SHIP).await;
    let cs_moving = ctx.build_ship_combat_state(PILOT, PILOT_SHIP, 11_957);
    let moving_ball = Ball {
        id: PILOT_SHIP,
        type_id: 11_957,
        position: DVec3::ZERO,
        velocity: DVec3::new(600.0, 0.0, 0.0), // 600 m/s >= 500 m/s
        max_velocity: 800.0,
        inertia: 1.0,
        mass: 10_000_000.0,
        radius: 100.0,
        speed_fraction: 0.75,
        mode: MovementMode::GotoDirection { direction: DVec3::X },
    };
    sys_low
        .spawn_ball_with_combat(moving_ball, cs_moving)
        .await
        .expect("spawn fast moving ball");

    assert_eq!(
        err_code(
            rpc_call(
                &ctx,
                &mut s_recon,
                "dogmaIM",
                "Activate",
                &[PyRep::Integer(cyno_mod_id), PyRep::String("effects.CynosuralGeneration".into())],
            )
            .await
        ),
        "ShipMovingTooFast"
    );

    // Restore ship ball to stationary (0 m/s < 500 m/s).
    place_ship_ball(&ctx, low_sys, PILOT, PILOT_SHIP, 11_957, DVec3::ZERO).await;

    // 5. Refused within range of Mobile Cynosural Inhibitor.
    let inhib_id = 988_705i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            inhib_id,
            InventoryItemRecord {
                item_id: inhib_id,
                type_id: TYPE_MOBILE_CYNO_INHIBITOR,
                owner_id: PILOT,
                location_id: low_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions.insert(inhib_id, [20_000.0, 0.0, 0.0]); // 20 km away
    });

    assert_eq!(
        err_code(
            rpc_call(
                &ctx,
                &mut s_recon,
                "dogmaIM",
                "Activate",
                &[PyRep::Integer(cyno_mod_id), PyRep::String("effects.CynosuralGeneration".into())],
            )
            .await
        ),
        "CynoInhibited"
    );

    // Remove inhibitor.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&inhib_id);
    });

    // 6. Refused in jammed systems.
    let jammer_id = 988_706i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            jammer_id,
            InventoryItemRecord {
                item_id: jammer_id,
                type_id: 33_700, // Cynosural Jammer item
                owner_id: PILOT,
                location_id: low_sys as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    assert_eq!(
        err_code(
            rpc_call(
                &ctx,
                &mut s_recon,
                "dogmaIM",
                "Activate",
                &[PyRep::Integer(cyno_mod_id), PyRep::String("effects.CynosuralGeneration".into())],
            )
            .await
        ),
        "CynoJammed"
    );

    // Clear jammer item.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&jammer_id);
    });

    // 7. Successful activation in low-sec: consumes Liquid Ozone, immobilizes ship.
    rpc_call(
        &ctx,
        &mut s_recon,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(cyno_mod_id), PyRep::String("effects.CynosuralGeneration".into())],
    )
    .await
    .expect("light normal cyno on Force Recon in low-sec");

    let ozone_left = ctx.db.with_snapshot(|s| s.items.get(&ozone_id).unwrap().quantity);
    assert_eq!(ozone_left, 450, "Force Recon consumes 50 units of Liquid Ozone");
    assert_eq!(
        err_code(rpc_call(&ctx, &mut s_recon, "ship", "Eject", &[]).await),
        "CynosuralFieldActive"
    );

    // 8. Wormhole space: cyno can be lit, but jump drive lock rejected.
    let _ = ctx.systems.get_or_spawn(low_sys).remove_ball(PILOT_SHIP).await;
    eve_server::services::space_destiny::space_warp_jump::deactivate_ship_modules_on_leave_space(
        &ctx, PILOT, PILOT_SHIP,
    );
    ctx.advance_secs(601);
    set_pilot_location(&ctx, &mut s_recon, wh_sys, None);
    place_ship_ball(&ctx, wh_sys, PILOT, PILOT_SHIP, 11_957, DVec3::ZERO).await;

    rpc_call(
        &ctx,
        &mut s_recon,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(cyno_mod_id), PyRep::String("effects.CynosuralGeneration".into())],
    )
    .await
    .expect("cyno can be lit in wormhole space");

    let jumper_cid = 90_000_102i64;
    let jumper_ship = 100_000_102i64;
    let mut s_jumper = register_online_pilot(
        &ctx,
        88_102,
        "jumper",
        "sec",
        jumper_cid,
        CORP,
        Some(ALLIANCE),
        low_sys,
        jumper_ship,
        22_428, // Redeemer (Black Ops)
        DVec3::ZERO,
        0,
    )
    .await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets
            .get_mut(&77_101)
            .unwrap()
            .members
            .push(jumper_cid);
    });

    assert_eq!(
        err_code(
            rpc_call(
                &ctx,
                &mut s_jumper,
                "beyonce",
                "CmdJumpToFleetModuleBeacon",
                &[PyRep::Integer(PILOT), PyRep::Integer(wh_sys as i64), PyRep::Integer(0)],
            )
            .await
        ),
        "JumpDriveForbiddenInWormholeSpace"
    );
}

#[tokio::test]
async fn test_assault_damage_control_parity_remediation() {
    // parity: feat:modules.assault-damage-control
    eve_testkit::attest(&["feat:modules.assault-damage-control"]);

    let db_path = temp_sqlite_path("parity_adc_remediation");
    let ctx = open_sqlite_ctx(&db_path, true);
    let low_sys = 30_002_718u32;
    let pilot_id = 90_000_201i64;
    let harpy_id = 100_000_201i64;
    let adc_id = 988_801i64;

    let mut sess = register_online_pilot(
        &ctx,
        88_201,
        "demo",
        "demo-secret",
        pilot_id,
        CORP,
        Some(ALLIANCE),
        low_sys,
        harpy_id,
        11_381, // Harpy (Assault Frigate, group 324)
        DVec3::ZERO,
        0,
    )
    .await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            adc_id,
            InventoryItemRecord {
                item_id: adc_id,
                type_id: 47_257, // Assault Damage Control II (group 60, effect 7012)
                owner_id: pilot_id,
                location_id: harpy_id,
                flag: InventoryFlag::LoSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(harpy_id);

    // 1. Activation on non-AF/non-HAC hull is rejected.
    let rifter_id = 100_000_202i64;
    let mut sess_rifter = register_online_pilot(
        &ctx,
        88_202,
        "demo2",
        "demo2-secret",
        90_000_202,
        CORP,
        Some(ALLIANCE),
        low_sys,
        rifter_id,
        587, // Rifter (Frigate, group 25)
        DVec3::new(1_000.0, 0.0, 0.0),
        0,
    )
    .await;
    let adc_rifter_id = 988_802i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            adc_rifter_id,
            InventoryItemRecord {
                item_id: adc_rifter_id,
                type_id: 47_257,
                owner_id: 90_000_202,
                location_id: rifter_id,
                flag: InventoryFlag::LoSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_ship_fitting(rifter_id);

    assert_eq!(
        err_code(
            ctx.dispatch_rpc(
                &mut sess_rifter,
                "dogmaIM",
                None,
                "Activate",
                &[
                    PyRep::Integer(adc_rifter_id),
                    PyRep::String("moduleBonusAssaultDamageControl".into()),
                    PyRep::None,
                    PyRep::Integer(0),
                ],
                &HashMap::new(),
                None,
            )
            .await
        ),
        "InvalidShipGroup"
    );

    // 2. Activation on Harpy triggers invulnerability burst (0.25 resonances = 75% resistance).
    let sys = ctx.systems.get_or_spawn(low_sys);
    let cs_before = sys.get_combat_state(harpy_id).await.unwrap().unwrap();

    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(adc_id),
            PyRep::String("moduleBonusAssaultDamageControl".into()),
            PyRep::None,
            PyRep::Integer(0),
        ],
        &HashMap::new(),
        None,
    )
    .await
    .expect("Activate Assault Damage Control II on Harpy");

    let cs_active = sys.get_combat_state(harpy_id).await.unwrap().unwrap();
    assert_eq!(cs_active.shield_resonance, [0.25; 4]);
    assert_eq!(cs_active.armor_resonance, [0.25; 4]);
    assert_eq!(cs_active.structure_resonance, [0.25; 4]);

    // 3. Short duration: step forward through cycle duration (15s for ADC II).
    for _ in 0..16 {
        sys.step_manual(1.0).await.unwrap();
        ctx.advance_secs(1);
    }

    let cs_after = sys.get_combat_state(harpy_id).await.unwrap().unwrap();
    assert_eq!(cs_after.shield_resonance, cs_before.shield_resonance);
    assert_eq!(cs_after.armor_resonance, cs_before.armor_resonance);
    assert_eq!(cs_after.structure_resonance, cs_before.structure_resonance);

    // 4. Long cooldown: reactivation during 150s delay must fail with ModuleReactivationDelay.
    let react_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(adc_id),
                PyRep::String("moduleBonusAssaultDamageControl".into()),
                PyRep::None,
                PyRep::Integer(0),
            ],
            &HashMap::new(),
            None,
        )
        .await;
    assert_eq!(
        react_err.expect_err("ADC II enforces 150s reactivation delay").code,
        "ModuleReactivationDelay"
    );

    // 5. Cooldown expiration allows reactivation.
    ctx.advance_secs(160);
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(adc_id),
            PyRep::String("moduleBonusAssaultDamageControl".into()),
            PyRep::None,
            PyRep::Integer(0),
        ],
        &HashMap::new(),
        None,
    )
    .await
    .expect("Reactivate ADC II after cooldown expires");
}

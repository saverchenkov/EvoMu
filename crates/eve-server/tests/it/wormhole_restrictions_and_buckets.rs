//! Parity test suite for wormhole cyno & supercapital restrictions, and show-info buckets.

use eve_db::{CosmicSignatureRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

// parity: feat:navigation.wormhole-cyno-supercap-restriction, feat:navigation.wormhole-info-buckets, plan:SO-6
#[tokio::test]
async fn test_wormhole_restrictions_and_buckets() {

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let char_id = 90_000_001;
    let mut sess = SessionState::new(901);
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login");
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // -------------------------------------------------------------------------
    // 1. Cynosural Field in Wormhole Space can be lit per feat:modules.cyno-lighting-rules
    // -------------------------------------------------------------------------
    // Put pilot in Thera (31000005) with Force Recon Arazu (type 11963), Cyno module (type 21096), and Liquid Ozone
    let cyno_mod_id = 600_000_888i64;
    let ozone_id = 600_000_889i64;
    let ship_id = 600_000_777i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = 31_000_005;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 11963, // Arazu (Recon Ship)
                owner_id: char_id,
                location_id: 31_000_005,
                flag: InventoryFlag::None,
                singleton: true,
                quantity: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            cyno_mod_id,
            InventoryItemRecord {
                item_id: cyno_mod_id,
                type_id: 21096, // Cynosural Field Generator I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                singleton: true,
                quantity: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            ozone_id,
            InventoryItemRecord {
                item_id: ozone_id,
                type_id: 16273, // Liquid Ozone
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                singleton: false,
                quantity: 1000,
                ..Default::default()
            },
        );
    });
    sess.solar_system_id = Some(31_000_005);
    sess.ship_id = Some(ship_id);
    sess.station_id = None;
    sess.structure_id = None;

    let thera_sys = ctx.systems.get_or_spawn(31_000_005);
    let _ = thera_sys
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 11963,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 12_000_000.0,
            radius: 120.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // Cynosural field can be lit in wormhole space per feat:modules.cyno-lighting-rules
    let cyno_res = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(cyno_mod_id),
                PyRep::String("cynosuralGeneration".into()),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await;
    assert!(cyno_res.is_ok(), "Cyno can be lit in wormhole space: {cyno_res:?}");

    // -------------------------------------------------------------------------
    // 2. Jump Drives from, to, or within Wormhole Space are rejected
    // -------------------------------------------------------------------------
    // Attempt CmdJumpToFleetModuleBeacon from wormhole space
    let jump_from_wh_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdJumpToFleetModuleBeacon",
            &[PyRep::Integer(char_id), PyRep::Integer(30_000_142), PyRep::Integer(999)],
            &kw,
            None,
        )
        .await
        .expect_err("Jump drive from wormhole space must be rejected");
    assert_eq!(jump_from_wh_err.code, "JumpDriveForbiddenInWormholeSpace");

    // Move session to K-Space (Jita 30000142) in a Dreadnought (Revelation 19720)
    let jita_sys_id = 30_000_142u32;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = jita_sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = 19720; // Revelation (Dreadnought)
            sh.location_id = jita_sys_id as i64;
        }
    });
    sess.solar_system_id = Some(jita_sys_id);
    let jita_sys = ctx.systems.get_or_spawn(jita_sys_id);
    let _ = jita_sys
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 19720,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 0.05,
            mass: 1_200_000_000.0,
            radius: 600.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // Attempt jump drive targeting wormhole space (31000005)
    let jump_to_wh_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdJumpToFleetModuleBeacon",
            &[PyRep::Integer(char_id), PyRep::Integer(31_000_005), PyRep::Integer(999)],
            &kw,
            None,
        )
        .await
        .expect_err("Jump drive to wormhole space must be rejected");
    assert_eq!(jump_to_wh_err.code, "JumpDriveForbiddenInWormholeSpace");

    // -------------------------------------------------------------------------
    // 3. Supercarrier and Titan absolute prohibition from entering wormholes
    // -------------------------------------------------------------------------
    // Set up a large wormhole in Jita (30000142)
    let wh_sig_id = "WH-CAP-TEST".to_string();
    let wh_item_id = 750_999_001i64;
    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let sigs = snap.cosmic_signatures.entry(jita_sys_id).or_default();
        sigs.retain(|s| s.signature_id != wh_sig_id);
        sigs.push(CosmicSignatureRecord {
            signature_id: wh_sig_id.clone(),
            solar_system_id: jita_sys_id,
            sig_group: "Wormhole".into(),
            type_name: "Wormhole Z988".into(),
            position: [500.0, 0.0, 0.0],
            signal_strength: 1.0,
            wormhole_type_id: Some(30645),
            destination_system_id: Some(31_000_011), // C6 w-space
            created_unix: now_unix,
            lifetime_seconds: 86_400,
            max_mass_kg: 50_000_000_000.0, // 50 billion kg (enormous)
            remaining_mass_kg: 50_000_000_000.0,
            max_jump_mass_kg: 2_000_000_000.0,
            collapsed: false,
        });
    });
    let _ = jita_sys
        .spawn_ball(Ball {
            id: wh_item_id,
            type_id: 26272,
            position: DVec3::new(500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 50_000_000_000.0,
            radius: 1500.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await;

    // A. Titan (Avatar 11567, group 30) jumps wormhole -> rejected!
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = 11567; // Avatar (Titan, group 30)
        }
    });
    let _ = jita_sys
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 11567,
            position: DVec3::new(600.0, 0.0, 0.0), // 100m from wormhole
            velocity: DVec3::ZERO,
            max_velocity: 70.0,
            inertia: 0.01,
            mass: 2_400_000_000.0,
            radius: 2000.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let titan_err = ctx
        .dispatch_rpc(
            &mut sess,
            "wormholeMgr",
            None,
            "WormholeJump",
            &[PyRep::String(wh_sig_id.clone())],
            &kw,
            None,
        )
        .await
        .expect_err("Titan cannot jump wormhole");
    assert_eq!(titan_err.code, "ShipRestrictedFromWormhole");

    // B. Supercarrier (Nyx 23919, group 659) jumps wormhole -> rejected!
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = 23919; // Nyx (Supercarrier, group 659)
        }
    });
    let _ = jita_sys
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 23919,
            position: DVec3::new(600.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 80.0,
            inertia: 0.02,
            mass: 1_700_000_000.0,
            radius: 1200.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let supercap_err = ctx
        .dispatch_rpc(
            &mut sess,
            "wormholeMgr",
            None,
            "WormholeJump",
            &[PyRep::String(wh_sig_id.clone())],
            &kw,
            None,
        )
        .await
        .expect_err("Supercarrier cannot jump wormhole");
    assert_eq!(supercap_err.code, "ShipRestrictedFromWormhole");

    // C. Regular Cruiser (Thorax 629, group 26) jumps wormhole -> allowed!
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = 629; // Thorax
        }
    });
    let _ = jita_sys
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 629,
            position: DVec3::new(600.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 240.0,
            inertia: 0.5,
            mass: 11_500_000.0,
            radius: 95.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let jump_ok = ctx
        .dispatch_rpc(
            &mut sess,
            "wormholeMgr",
            None,
            "WormholeJump",
            &[PyRep::String(wh_sig_id.clone())],
            &kw,
            None,
        )
        .await
        .expect("Thorax cruiser can jump wormhole");
    let PyRep::KeyVal(jkv) = jump_ok else { panic!("Expected KeyVal") };
    assert_eq!(jkv.get("destinationSystemID"), Some(&PyRep::Integer(31_000_011)));

    // -------------------------------------------------------------------------
    // 4. Live Wormhole Show-Info Text Buckets
    // -------------------------------------------------------------------------
    // Put pilot back in Jita and test all destination classes, ship sizes, lifetimes, and mass buckets
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = jita_sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    sess.solar_system_id = Some(jita_sys_id);

    // Verify destination classes via derive_wormhole_buckets:
    let now = ctx.now_unix();
    let cases = vec![
        (31_000_007, "C1-3"),       // C1 system (J105443)
        (31_000_355, "C1-3"),       // C2 system (J164417)
        (31_000_880, "C1-3"),       // C3 system (J160650)
        (31_001_375, "C4-5"),       // C4 system (J160225)
        (31_001_880, "C4-5"),       // C5 system (J152820)
        (31_002_367, "C6"),         // C6 system (J104859)
        (30_000_142, "High-Sec"),   // Jita (1.0)
        (30_002_813, "Low-Sec"),    // Tama (0.3)
        (30_002_904, "Null-Sec"),   // VFK-IV (-0.2)
        (30_000_157, "Triglavian"), // System in Pochven (region 10000070, Otela)
        (31_000_001, "Drifter"),    // Drifter system
        (31_000_005, "Thera"),      // Thera (class 12)
    ];

    for (target_sys, expected_class) in cases {
        let sig = CosmicSignatureRecord {
            signature_id: format!("SIG-{target_sys}"),
            solar_system_id: jita_sys_id,
            sig_group: "Wormhole".into(),
            type_name: "Test WH".into(),
            position: [0.0, 0.0, 0.0],
            signal_strength: 1.0,
            wormhole_type_id: Some(30579),
            destination_system_id: Some(target_sys),
            created_unix: now,
            lifetime_seconds: 86_400,
            max_mass_kg: 2_000_000_000.0,
            remaining_mass_kg: 2_000_000_000.0,
            max_jump_mass_kg: 375_000_000.0,
            collapsed: false,
        };
        let b = eve_server::services::space_destiny::space_cyno_filaments::derive_wormhole_buckets(&sig, now, &ctx.sde);
        assert_eq!(b.destination_class, expected_class, "target system {target_sys}");
    }

    // Verify maximum ship size categories:
    let size_cases = vec![
        (15_000_000.0, "Small"),
        (60_000_000.0, "Medium"),
        (350_000_000.0, "Large"),
        (1_500_000_000.0, "Extra Large"),
    ];
    for (max_jump_mass, expected_size) in size_cases {
        let sig = CosmicSignatureRecord {
            signature_id: "SIZE-TEST".into(),
            solar_system_id: jita_sys_id,
            sig_group: "Wormhole".into(),
            type_name: "Test WH".into(),
            position: [0.0, 0.0, 0.0],
            signal_strength: 1.0,
            wormhole_type_id: Some(30579),
            destination_system_id: Some(31_000_005),
            created_unix: now,
            lifetime_seconds: 86_400,
            max_mass_kg: 2_000_000_000.0,
            remaining_mass_kg: 2_000_000_000.0,
            max_jump_mass_kg: max_jump_mass,
            collapsed: false,
        };
        let b = eve_server::services::space_destiny::space_cyno_filaments::derive_wormhole_buckets(&sig, now, &ctx.sde);
        assert_eq!(b.max_ship_size, expected_size);
    }

    // Verify reliable lifetime buckets:
    // "<1 day" (>= 4h remaining)
    // "<4h" (1h..4h remaining)
    // "<1h" (<1h remaining)
    // "expired" (0s remaining or collapsed)
    let life_cases = vec![
        (now.saturating_sub(10 * 3600), 86_400, false, "<1 day"),
        (now.saturating_sub(22 * 3600), 86_400, false, "<4h"),
        (now.saturating_sub(86_400 - 1800), 86_400, false, "<1h"),
        (now.saturating_sub(90_000), 86_400, true, "expired"),
    ];
    for (created, lifetime, collapsed, expected_bucket) in life_cases {
        let sig = CosmicSignatureRecord {
            signature_id: "LIFE-TEST".into(),
            solar_system_id: jita_sys_id,
            sig_group: "Wormhole".into(),
            type_name: "Test WH".into(),
            position: [0.0, 0.0, 0.0],
            signal_strength: 1.0,
            wormhole_type_id: Some(30579),
            destination_system_id: Some(31_000_005),
            created_unix: created,
            lifetime_seconds: lifetime,
            max_mass_kg: 2_000_000_000.0,
            remaining_mass_kg: 2_000_000_000.0,
            max_jump_mass_kg: 375_000_000.0,
            collapsed,
        };
        let b = eve_server::services::space_destiny::space_cyno_filaments::derive_wormhole_buckets(&sig, now, &ctx.sde);
        assert_eq!(b.lifetime_bucket, expected_bucket);
    }

    // Verify mass stability buckets:
    // ">50%" (>50% remaining)
    // "<50%" (10%..50% remaining)
    // "<10%" (<10% remaining)
    let mass_cases = vec![
        (1_600_000_000.0, ">50%"),
        (800_000_000.0, "<50%"),
        (150_000_000.0, "<10%"),
    ];
    for (rem_mass, expected_mass_bucket) in mass_cases {
        let sig = CosmicSignatureRecord {
            signature_id: "MASS-TEST".into(),
            solar_system_id: jita_sys_id,
            sig_group: "Wormhole".into(),
            type_name: "Test WH".into(),
            position: [0.0, 0.0, 0.0],
            signal_strength: 1.0,
            wormhole_type_id: Some(30579),
            destination_system_id: Some(31_000_005),
            created_unix: now,
            lifetime_seconds: 86_400,
            max_mass_kg: 2_000_000_000.0,
            remaining_mass_kg: rem_mass,
            max_jump_mass_kg: 375_000_000.0,
            collapsed: false,
        };
        let b = eve_server::services::space_destiny::space_cyno_filaments::derive_wormhole_buckets(&sig, now, &ctx.sde);
        assert_eq!(b.mass_stability_bucket, expected_mass_bucket);
    }

    // Live query via wormholeMgr.GetWormholeInfo:
    let wh_info_sig = "WH-QUERY-1".to_string();
    ctx.db.with_mut_snapshot(|snap| {
        let sigs = snap.cosmic_signatures.entry(jita_sys_id).or_default();
        sigs.push(CosmicSignatureRecord {
            signature_id: wh_info_sig.clone(),
            solar_system_id: jita_sys_id,
            sig_group: "Wormhole".into(),
            type_name: "Wormhole B449".into(),
            position: [1000.0, 0.0, 0.0],
            signal_strength: 1.0,
            wormhole_type_id: Some(30647),
            destination_system_id: Some(31_000_005), // Thera
            created_unix: now - 3600 * 2, // 2h elapsed of 16h -> 14h remaining (<1 day)
            lifetime_seconds: 57_600,
            max_mass_kg: 2_000_000_000.0,
            remaining_mass_kg: 600_000_000.0, // 30% remaining (<50%)
            max_jump_mass_kg: 375_000_000.0,  // Large
            collapsed: false,
        });
    });

    // Live query via wormholeMgr.GetWormholePolarization:
    let pol_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "wormholeMgr",
            None,
            "GetWormholePolarization",
            &[PyRep::String(wh_info_sig)],
            &kw,
            None,
        )
        .await
        .expect("wormholeMgr.GetWormholePolarization");
    let PyRep::KeyVal(pkv) = pol_rep else { panic!("Expected KeyVal from GetWormholePolarization") };
    assert_eq!(pkv.get("signatureID"), Some(&PyRep::String("WH-QUERY-1".into())));
    assert!(pkv.contains_key("lifetimeStage"));
    assert!(pkv.contains_key("massStage"));
    assert_eq!(pkv.get("destinationClass"), Some(&PyRep::String("Thera".into())));
    assert_eq!(pkv.get("maxShipSize"), Some(&PyRep::String("Large".into())));
    assert_eq!(pkv.get("lifetimeBucket"), Some(&PyRep::String("<1 day".into())));
    assert_eq!(pkv.get("massStabilityBucket"), Some(&PyRep::String("<50%".into())));
}

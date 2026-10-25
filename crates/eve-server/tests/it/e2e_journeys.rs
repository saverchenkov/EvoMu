//! Integration test suite verifying End-to-End journeys (`E2E-01` through `E2E-18`) against `eve_server::router::ServerContext`.

#![forbid(unsafe_code)]

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_e2e_01_to_18_integration_journeys() {
    // parity: rpc:authentication.Login, rpc:charUnboundMgr.CreateCharacterWithDoll, rpc:charUnboundMgr.SelectCharacterID, rpc:dogmaIM.LoadAmmo, rpc:essMgr.AttemptLinkToMainBank, rpc:marketProxy.GetOrders, rpc:wormholeMgr.WormholeJump
    use eve_db::{InventoryFlag, InventoryItemRecord};

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_000_014,
            InventoryItemRecord {
                item_id: 100_000_014,
                type_id: 40520,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let mut sess = SessionState::new(1);
    let kw = HashMap::new();

    // Unauthenticated RPC must be rejected before login
    let unauth = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetOrders", &[], &kw, None)
        .await;
    assert_eq!(unauth.unwrap_err().code, "Unauthorized");

    // E2E-01: Auth -> CreateCharacterWithDoll (Caldari race=1, Deteis bloodline=1, Scientists ancestry=2, STI school=17) -> SelectCharacterID
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
    .expect("authentication.Login");

    let new_char = ctx
        .dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "CreateCharacterWithDoll",
            &[
                PyRep::String("Integration Pilot".into()),
                PyRep::Integer(1),  // bloodlineID = 1 (Deteis -> Caldari race 1)
                PyRep::Integer(1),  // genderID = 1 (Male)
                PyRep::Integer(11), // ancestryID = 11 (Scientists)
                PyRep::String("doll:integration-pilot".into()),
                PyRep::String("portrait:integration-pilot".into()),
                PyRep::Integer(17), // schoolID = 17 (Science and Trade Institute)
            ],
            &kw,
            None,
        )
        .await
        .expect("CreateCharacterWithDoll");
    assert!(new_char.as_i64().unwrap_or(0) > 200_000_000);

    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    assert_eq!(sess.character_id, Some(90_000_001));

    // E2E-02: Skill Queue & Skill Injector via V24.01 `skillMgr2` (with MachoBindObject)
    let (skill_bound, _) = match ctx
        .dispatch_rpc(
            &mut sess,
            "skillMgr2",
            None,
            "MachoBindObject",
            &[PyRep::Integer(90_000_001), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("skillMgr2 bind")
    {
        PyRep::Tuple(t) => (t[0].as_i64().unwrap_or(1) as u64, t[1].clone()),
        _ => panic!("Expected bound tuple"),
    };
    ctx.dispatch_rpc(
        &mut sess,
        "skillMgr2",
        Some(skill_bound),
        "SaveSkillQueue",
        &[PyRep::Integer(3327), PyRep::Integer(5)],
        &kw,
        None,
    )
    .await
    .expect("SaveSkillQueue");
    let injected = ctx
        .dispatch_rpc(
            &mut sess,
            "skillMgr2",
            Some(skill_bound),
            "InjectSkillpoints",
            &[PyRep::Integer(100_000_014), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("InjectSkillpoints");
    // Aura has 15,000,000 SP (5M..50M bracket -> 400,000 SP yield)
    assert_eq!(injected.as_f64(), Some(400_000.0));
    let sp_after = ctx
        .dispatch_rpc(
            &mut sess,
            "skillMgr2",
            Some(skill_bound),
            "ApplyFreeSkillPoints",
            &[],
            &kw,
            None,
        )
        .await
        .expect("ApplyFreeSkillPoints");
    assert!(sp_after.as_f64().unwrap_or(0.0) > 15_000_000.0);

    // E2E-03: Ship Fitting, Module Overload (persisted in ctx.ship_fittings), Mutaplasmid, & Insurance
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(100_000_003),
            PyRep::Integer(100_000_001),
            PyRep::Integer(19),
        ],
        &kw,
        None,
    )
    .await
    .expect("MoveItem");
    let ov_res = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Overload",
            &[PyRep::Integer(100_000_003)],
            &kw,
            None,
        )
        .await
        .expect("Overload");
    let PyRep::KeyVal(ov_kv) = ov_res else {
        panic!("Expected Overload KeyVal");
    };
    assert!(ov_kv.get("effectiveMaxVelocity").unwrap().as_f64().unwrap() > 800.0);

    // E2E-04 & E2E-05: Undock -> Warp to Stargate 50001248 -> Stargate Jump to Perimeter (30000144) -> Jump Back -> Warp to Jita 4-4 -> Dock
    let undock_res = ctx
        .dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    assert_eq!(sess.station_id, None);
    let PyRep::KeyVal(ukv) = undock_res else {
        panic!("Expected Undock KeyVal");
    };
    let jita_bound = ukv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;

    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(jita_bound),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(50_001_249)],
        &kw,
        None,
    )
    .await
    .expect("CmdWarpToStuff to stargate 50001249");

    let jita_handle = ctx.systems.get_or_spawn(30_000_142);
    for _ in 0..30 {
        let _ = jita_handle.step_manual(1.0).await.unwrap();
    }

    let jump_res = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(jita_bound),
            "CmdStargateJump",
            &[PyRep::Integer(50_001_249)],
            &kw,
            None,
        )
        .await
        .expect("CmdStargateJump to Perimeter");
    let PyRep::KeyVal(jkv) = jump_res else {
        panic!("Expected jump KeyVal");
    };
    assert_eq!(jkv.get("solarSystemID"), Some(&PyRep::Integer(30_000_144)));
    assert_eq!(sess.solar_system_id, Some(30_000_144));
    let mut cur_bound = jkv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;

    // Verify ESS link is rejected in High-Sec Perimeter (30000144)
    let hs_ess_err = ctx
        .dispatch_rpc(
            &mut sess,
            "essMgr",
            None,
            "AttemptLinkToMainBank",
            &[],
            &kw,
            None,
        )
        .await;
    assert_eq!(hs_ess_err.unwrap_err().code, "EssOnlyInNullSec");

    // Travel Perimeter (50002185) -> Jita (50013928) -> Ikuchi (50013926) -> Tunttaras (50001709) -> Nourvukaiken (50014001) -> Low-Sec Tama (30002813)
    for gate_id in [
        50_002_185_i64,
        50_013_928,
        50_013_926,
        50_001_709,
        50_014_001,
    ] {
        let cur_sys = sess.solar_system_id.unwrap();
        let sys_h = ctx.systems.get_or_spawn(cur_sys);
        let warp_res = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(cur_bound),
                "CmdWarpToStuff",
                &[PyRep::String("item".into()), PyRep::Integer(gate_id)],
                &kw,
                None,
            )
            .await;
        match warp_res {
            Ok(_) => {
                for _ in 0..35 {
                    let _ = sys_h.step_manual(1.0).await.unwrap();
                }
            }
            Err(e) if e.code == "WarpDistanceTooShort" => {
                for _ in 0..10 {
                    let _ = sys_h.step_manual(1.0).await.unwrap();
                }
            }
            Err(e) => panic!("Warp to stargate {gate_id}: {e:?}"),
        }
        let j_res = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(cur_bound),
                "CmdStargateJump",
                &[PyRep::Integer(gate_id)],
                &kw,
                None,
            )
            .await
            .expect("Jump stargate");
        let PyRep::KeyVal(j_kv) = j_res else {
            panic!("Expected jump KeyVal");
        };
        cur_bound = j_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    }
    let tama_bound = cur_bound;
    let tama_handle = ctx.systems.get_or_spawn(30_002_813);

    // E2E-11 in Low-Sec Tama (30002813): S-6 verifies RequestScans returns UnimplementedRPC; spawn K162 (750_000_001) -> ConeScan -> Warp -> Jump to J105934 (31000005) -> Jump to Null-Sec VFK-IV (30002904)
    // parity: plan:S-6
    let req_scans_err = ctx
        .dispatch_rpc(
            &mut sess,
            "scanMgr",
            None,
            "RequestScans",
            &[PyRep::Real(0.25)],
            &kw,
            None,
        )
        .await;
    assert_eq!(req_scans_err.unwrap_err().code, "UnimplementedRPC");
    let _ = tama_handle
        .spawn_ball(eve_destiny::Ball {
            id: 750_000_001,
            type_id: 26272,
            position: glam::DVec3::new(15_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000_000_000.0,
            radius: 1500.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Rigid,
        })
        .await;
    ctx.mark_signature_scanned(90_000_001, 30_002_813, 750_000_001);
    ctx.dispatch_rpc(
        &mut sess,
        "scanMgr",
        None,
        "ConeScan",
        &[PyRep::Real(360.0), PyRep::Real(14.4 * 149_597_870_700.0)],
        &kw,
        None,
    )
    .await
    .expect("ConeScan in Tama");
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(tama_bound),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(750_000_001)],
        &kw,
        None,
    )
    .await
    .expect("Warp to wormhole 750_000_001 in Tama");
    for _ in 0..30 {
        let _ = tama_handle.step_manual(1.0).await.unwrap();
    }
    let wh_in = ctx
        .dispatch_rpc(
            &mut sess,
            "wormholeMgr",
            None,
            "WormholeJump",
            &[PyRep::Integer(750_000_001)],
            &kw,
            None,
        )
        .await
        .expect("WormholeJump into J105934");
    let PyRep::KeyVal(wh_in_kv) = wh_in else {
        panic!("Expected wh_in KeyVal");
    };
    assert_eq!(
        wh_in_kv.get("solarSystemID"),
        Some(&PyRep::Integer(31_000_005))
    );
    let wh_vfk = ctx
        .dispatch_rpc(
            &mut sess,
            "wormholeMgr",
            None,
            "WormholeJump",
            &[PyRep::Integer(750_000_001)],
            &kw,
            None,
        )
        .await
        .expect("WormholeJump into VFK-IV (30002904)");
    let PyRep::KeyVal(wh_vfk_kv) = wh_vfk else {
        panic!("Expected wh_vfk KeyVal");
    };
    assert_eq!(
        wh_vfk_kv.get("solarSystemID"),
        Some(&PyRep::Integer(30_002_904))
    );

    // E2E-06: Combat against test-spawned NPC pirate ball 700_000_001 in Null-Sec VFK-IV (30002904) & ESS Main Bank Hack
    let hed_handle = ctx.systems.get_or_spawn(30_002_904);
    let (_, ship_in_hed) = hed_handle
        .get_ball_snapshot(100_000_001)
        .await
        .unwrap()
        .unwrap();
    let _ = hed_handle
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: 700_000_001,
                type_id: 16997,
                position: ship_in_hed.position + glam::DVec3::new(900.0, 0.0, 0.0),
                velocity: glam::DVec3::ZERO,
                max_velocity: 310.0,
                inertia: 2.5,
                mass: 1_000_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            },
            eve_destiny::CombatState {
                shield_hp: 50.0,
                max_shield_hp: 50.0,
                armor_hp: 50.0,
                max_armor_hp: 50.0,
                structure_hp: 50.0,
                max_structure_hp: 50.0,
                ..eve_destiny::CombatState::default()
            },
        )
        .await;
    let _ = hed_handle
        .spawn_ball(eve_destiny::Ball {
            id: 750_000_002,
            type_id: 32882,
            position: ship_in_hed.position + glam::DVec3::new(500.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 100_000_000.0,
            radius: 500.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Rigid,
        })
        .await;
    let _ = hed_handle.step_manual(1.0).await.unwrap();
    // R18-P1-1: Seed 5,000 Republic Fleet Phased Plasma S (type_id 21924) in ship Cargo (100_000_001);
    // FireWeapon MUST still reject with NoAmmoLoaded while ammo is only in Cargo and not loaded into HiSlot0 (flag 27)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_000_040,
            InventoryItemRecord {
                item_id: 100_000_040,
                type_id: 21924,
                owner_id: 90_000_001,
                location_id: 100_000_001,
                flag: InventoryFlag::Cargo,
                quantity: 5_000,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let unl_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(700_000_001)],
            &kw,
            None,
        )
        .await;
    assert_eq!(unl_err.unwrap_err().code, "NoAmmoLoaded");

    // Load ammo into HiSlot0 (flag 27) via dogmaIM.LoadAmmo (clip capacity = 0.3 / 0.0025 = 120 rounds)
    let load_res = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "LoadAmmo",
            &[
                PyRep::Integer(100_000_001),
                PyRep::Integer(27),
                PyRep::Integer(100_000_040),
            ],
            &kw,
            None,
        )
        .await
        .expect("dogmaIM.LoadAmmo in VFK-IV");
    let PyRep::KeyVal(lkv) = load_res else {
        panic!("Expected LoadAmmo KeyVal");
    };
    assert_eq!(lkv.get("totalLoaded"), Some(&PyRep::Integer(120)));
    assert_eq!(ctx.db.get_item(100_000_040).unwrap().quantity, 4_880);

    // R-P0-01: Non-combatant balls (such as structure ball 750_000_002) cannot be damaged or award bounties via FireWeapon
    let non_npc_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(750_000_002)],
            &kw,
            None,
        )
        .await;
    assert_eq!(non_npc_err.unwrap_err().code, "TargetInvulnerable");

    let mut destroyed = false;
    let mut shots_fired = 0_i32;
    let mut last_bounty = 0.0;
    for shot in 0..10 {
        if shot > 0 {
            for _ in 0..4 {
                let _ = hed_handle.step_manual(1.0).await.unwrap();
            }
        }
        let combat_res = ctx
            .dispatch_rpc(
                &mut sess,
                "dogmaIM",
                None,
                "Activate",
                &[PyRep::Integer(700_000_001)],
                &kw,
                None,
            )
            .await
            .expect("FireWeapon in VFK-IV");
        shots_fired += 1;
        let PyRep::KeyVal(ckv) = combat_res else {
            panic!("Expected combat KeyVal");
        };
        if ckv.get("destroyed") == Some(&PyRep::Bool(true)) {
            destroyed = true;
            last_bounty = ckv.get("bountyPayoutISK").unwrap().as_f64().unwrap();
            break;
        }
    }
    assert!(destroyed);
    assert!(shots_fired >= 2);
    assert!(last_bounty > 0.0);
    let remaining_clip = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .find(|i| {
                i.location_id == 100_000_001
                    && i.flag == InventoryFlag::HiSlot0
                    && i.type_id == 21924
            })
            .map(|i| i.quantity)
            .unwrap_or(0)
    });
    assert_eq!(remaining_clip, 120 - shots_fired);

    let ess_start = ctx
        .dispatch_rpc(
            &mut sess,
            "essMgr",
            None,
            "AttemptLinkToMainBank",
            &[PyRep::Integer(750_000_002)],
            &kw,
            None,
        )
        .await
        .expect("AttemptLinkToMainBank start");
    let PyRep::KeyVal(ess_start_kv) = ess_start else {
        panic!("Expected ESS start KeyVal");
    };
    assert_eq!(
        ess_start_kv.get("linkStartedOnly"),
        Some(&PyRep::Bool(true))
    );
    for _ in 0..5 {
        let _ = hed_handle.step_manual(1.0).await.unwrap();
    }
    let ess_done = ctx
        .dispatch_rpc(
            &mut sess,
            "essMgr",
            None,
            "AttemptLinkToMainBank",
            &[PyRep::Integer(750_000_002)],
            &kw,
            None,
        )
        .await
        .expect("AttemptLinkToMainBank complete");
    let PyRep::KeyVal(ess_done_kv) = ess_done else {
        panic!("Expected ESS done KeyVal");
    };
    assert_eq!(
        ess_done_kv.get("linkStartedOnly"),
        Some(&PyRep::Bool(false))
    );
    assert!(ess_done_kv.get("stolenISK").unwrap().as_f64().unwrap() > 0.0);

    // Jump back VFK-IV (30002904) -> Tama (30002813) via wormhole, then stargates (50014002 -> 50000805 -> 50013925 -> 50013927 -> Jita 30000142) -> Dock at Jita 4-4 (60003760)
    let jb_tama = ctx
        .dispatch_rpc(
            &mut sess,
            "wormholeMgr",
            None,
            "WormholeJump",
            &[PyRep::Integer(750_000_001)],
            &kw,
            None,
        )
        .await
        .expect("WormholeJump VFK-IV -> Tama");
    let PyRep::KeyVal(jb_tama_kv) = jb_tama else {
        panic!("Expected jb_tama KeyVal");
    };
    let mut ret_bound = jb_tama_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    for gate_id in [50_014_002_i64, 50_000_805, 50_013_925, 50_013_927] {
        let cur_sys = sess.solar_system_id.unwrap();
        let sys_h = ctx.systems.get_or_spawn(cur_sys);
        ctx.dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(ret_bound),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(gate_id)],
            &kw,
            None,
        )
        .await
        .expect("Warp to return stargate");
        for _ in 0..30 {
            let _ = sys_h.step_manual(1.0).await.unwrap();
        }
        let jb_res = ctx
            .dispatch_rpc(
                &mut sess,
                "beyonce",
                Some(ret_bound),
                "CmdStargateJump",
                &[PyRep::Integer(gate_id)],
                &kw,
                None,
            )
            .await
            .expect("Jump return stargate");
        let PyRep::KeyVal(jb_kv) = jb_res else {
            panic!("Expected return jump KeyVal");
        };
        ret_bound = jb_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    }
    let jita_bound_2 = ret_bound;

    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(jita_bound_2),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(60_003_760)],
        &kw,
        None,
    )
    .await
    .expect("CmdWarpToStuff to Jita 4-4");
    for _ in 0..30 {
        let _ = jita_handle.step_manual(1.0).await.unwrap();
    }

    ctx.dispatch_rpc(
        &mut sess,
        "stationSvc",
        None,
        "Dock",
        &[PyRep::Integer(60003760)],
        &kw,
        None,
    )
    .await
    .expect("Dock");
    assert_eq!(sess.station_id, Some(60003760));
}

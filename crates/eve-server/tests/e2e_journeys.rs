//! Integration test suite verifying End-to-End journeys (`E2E-01` through `E2E-18`) against `eve_server::router::ServerContext`.

#![forbid(unsafe_code)]

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_e2e_01_to_18_integration_journeys() {
    use eve_db::{InventoryFlag, InventoryItemRecord};

    let ctx = ServerContext::new();
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
                PyRep::Integer(1),  // Caldari
                PyRep::Integer(1),  // Deteis
                PyRep::Integer(11), // Scientists (bloodline 1)
                PyRep::Integer(17), // Science and Trade Institute (race 1)
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
            "InjectSkillPoints",
            &[PyRep::Integer(100_000_014), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("InjectSkillPoints");
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
        "MoveItem",
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
        "CmdWarpToPoint",
        &[
            PyRep::Real(-161_369_743_360.0),
            PyRep::Real(-30_690_549_760.0),
            PyRep::Real(-1_107_962_183_680.0),
        ],
        &kw,
        None,
    )
    .await
    .expect("CmdWarpToPoint to stargate 50001248");

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
            &[PyRep::Integer(50001248)],
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
    let perim_bound = jkv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;

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

    // Warp to Perimeter -> Tama stargate 50001250 and jump to Low-Sec Tama (30002813)
    let perim_handle = ctx.systems.get_or_spawn(30_000_144);
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(perim_bound),
        "CmdWarpToPoint",
        &[
            PyRep::Real(86_000_000_000.0),
            PyRep::Real(12_000_000_000.0),
            PyRep::Real(-141_000_000_000.0),
        ],
        &kw,
        None,
    )
    .await
    .expect("Warp to gate 50001250");
    for _ in 0..30 {
        let _ = perim_handle.step_manual(1.0).await.unwrap();
    }
    let jump_tama = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(perim_bound),
            "CmdStargateJump",
            &[PyRep::Integer(50001250)],
            &kw,
            None,
        )
        .await
        .expect("Jump to Tama (30002813)");
    let PyRep::KeyVal(tama_kv) = jump_tama else {
        panic!("Expected Tama jump KeyVal");
    };
    let tama_bound = tama_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    let tama_handle = ctx.systems.get_or_spawn(30_002_813);

    // E2E-11 in Low-Sec Tama (30002813): Scan exploration wormhole -> Warp to K162 (750_000_001) -> Jump to J105934 (31000005) -> Jump to Null-Sec VFK-IV (30002904)
    let scan_res = ctx
        .dispatch_rpc(
            &mut sess,
            "scanMgr",
            None,
            "RequestScans",
            &[PyRep::Real(0.25)],
            &kw,
            None,
        )
        .await
        .expect("RequestScans in Tama");
    let PyRep::KeyVal(skv) = scan_res else {
        panic!("Expected scan KeyVal");
    };
    assert_eq!(skv.get("warpable"), Some(&PyRep::Bool(true)));
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(tama_bound),
        "CmdWarpToPoint",
        &[
            PyRep::Real(15_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
        ],
        &kw,
        None,
    )
    .await
    .expect("Warp to wormhole 750_000_001 in Tama");
    for _ in 0..30 {
        let _ = tama_handle.step_manual(1.0).await.unwrap();
    }
    let wh_in = ctx
        .dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[], &kw, None)
        .await
        .expect("WormholeJump into J105934");
    let PyRep::KeyVal(wh_in_kv) = wh_in else {
        panic!("Expected wh_in KeyVal");
    };
    assert_eq!(wh_in_kv.get("solarSystemID"), Some(&PyRep::Integer(31_000_005)));
    let wh_vfk = ctx
        .dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[], &kw, None)
        .await
        .expect("WormholeJump into VFK-IV (30002904)");
    let PyRep::KeyVal(wh_vfk_kv) = wh_vfk else {
        panic!("Expected wh_vfk KeyVal");
    };
    assert_eq!(wh_vfk_kv.get("solarSystemID"), Some(&PyRep::Integer(30_002_904)));

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
                type_id: 11001,
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
    // R18-P1-1: Seed 5,000 Republic Fleet Phased Plasma S (type_id 22989) in ship Cargo (100_000_001);
    // FireWeapon MUST still reject with NoAmmoLoaded while ammo is only in Cargo and not loaded into HiSlot0 (flag 27)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_000_040,
            InventoryItemRecord {
                item_id: 100_000_040,
                type_id: 22989,
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
            "FireWeapon",
            &[PyRep::Integer(700_000_001)],
            &kw,
            None,
        )
        .await;
    assert_eq!(unl_err.unwrap_err().code, "NoAmmoLoaded");

    // Load ammo into HiSlot0 (flag 27) via dogmaIM.LoadAmmo (clip capacity = 5.0 / 0.0025 = 2,000 rounds)
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
    assert_eq!(lkv.get("totalLoaded"), Some(&PyRep::Integer(2_000)));
    assert_eq!(ctx.db.get_item(100_000_040).unwrap().quantity, 3_000);

    // R-P0-01: Non-combatant balls (such as structure ball 750_000_002) cannot be damaged or award bounties via FireWeapon
    let non_npc_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "FireWeapon",
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
                "FireWeapon",
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
            .find(|i| i.location_id == 100_000_001 && i.flag == InventoryFlag::HiSlot0 && i.type_id == 22989)
            .map(|i| i.quantity)
            .unwrap_or(0)
    });
    assert_eq!(remaining_clip, 2_000 - shots_fired);

    let ess_start = ctx
        .dispatch_rpc(
            &mut sess,
            "essMgr",
            None,
            "AttemptLinkToMainBank",
            &[],
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
            &[],
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

    // Jump back VFK-IV (30002904) -> Tama (30002813) via wormhole, then stargates (50001251 -> Perimeter 50001873 -> Jita 30000142) -> Dock at Jita 4-4 (60003760)
    let jb_tama = ctx
        .dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[], &kw, None)
        .await
        .expect("WormholeJump VFK-IV -> Tama");
    let PyRep::KeyVal(jb_tama_kv) = jb_tama else {
        panic!("Expected jb_tama KeyVal");
    };
    let tama_b2 = jb_tama_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(tama_b2),
        "CmdWarpToPoint",
        &[
            PyRep::Real(10_000_000_000.0),
            PyRep::Real(0.0),
            PyRep::Real(10_000_000_000.0),
        ],
        &kw,
        None,
    )
    .await
    .expect("Warp to gate 50001251 in Tama");
    for _ in 0..30 {
        let _ = tama_handle.step_manual(1.0).await.unwrap();
    }
    let jb_perim = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(tama_b2),
            "CmdStargateJump",
            &[PyRep::Integer(50001251)],
            &kw,
            None,
        )
        .await
        .expect("Jump Tama -> Perimeter");
    let PyRep::KeyVal(jb_perim_kv) = jb_perim else {
        panic!("Expected jb_perim KeyVal");
    };
    let perim_b2 = jb_perim_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(perim_b2),
        "CmdWarpToPoint",
        &[
            PyRep::Real(84_000_000_000.0),
            PyRep::Real(12_000_000_000.0),
            PyRep::Real(-139_000_000_000.0),
        ],
        &kw,
        None,
    )
    .await
    .expect("Warp to gate 50001873 in Perimeter");
    for _ in 0..30 {
        let _ = perim_handle.step_manual(1.0).await.unwrap();
    }
    let jump_back = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(perim_b2),
            "CmdStargateJump",
            &[PyRep::Integer(50001873)],
            &kw,
            None,
        )
        .await
        .expect("CmdStargateJump back to Jita");
    let PyRep::KeyVal(jb_kv) = jump_back else {
        panic!("Expected jump_back KeyVal");
    };
    let jita_bound_2 = jb_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;

    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(jita_bound_2),
        "CmdWarpToPoint",
        &[
            PyRep::Real(-107_303_362_560.0),
            PyRep::Real(-18_744_975_360.0),
            PyRep::Real(436_489_052_160.0),
        ],
        &kw,
        None,
    )
    .await
    .expect("CmdWarpToPoint to Jita 4-4");
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

#[tokio::test]
async fn test_blocking_findings_b01_to_b15_regressions() {
    use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};

    let ctx = ServerContext::new();
    let mut sess = SessionState::new(1);
    let kw = HashMap::new();

    // B-07 & AP-05: Production WorldRepository::with_demo_seed() has 0 GM accounts, only starter items 100_000_001..=100_000_012, and 0 items in 100_000_013..=100_000_028
    for starter_id in 100_000_001..=100_000_012 {
        assert!(
            ctx.db.get_item(starter_id).is_some(),
            "Expected starter item {starter_id} in with_demo_seed()"
        );
    }
    for test_only_item_id in 100_000_013..=100_000_028 {
        assert!(
            ctx.db.get_item(test_only_item_id).is_none(),
            "Production seed must not contain test-only fixture item {test_only_item_id}"
        );
    }

    // Seed expansion test fixtures explicitly inside the test harness
    ctx.db.with_mut_snapshot(|snap| {
        let extra_items = [
            (
                100_000_015,
                35832,
                100_000_001,
                InventoryFlag::Cargo,
                5,
                false,
            ),
            (
                100_000_016,
                56201,
                100_000_001,
                InventoryFlag::Cargo,
                5,
                false,
            ),
            (
                100_000_020,
                20424,
                60_003_760,
                InventoryFlag::Hangar,
                10,
                false,
            ),
            (
                100_000_028,
                20172,
                60_003_760,
                InventoryFlag::Hangar,
                10,
                false,
            ),
            (
                100_000_023,
                23055,
                100_000_024,
                InventoryFlag::FighterBay,
                9,
                false,
            ),
            (
                100_000_024,
                24483,
                60_003_760,
                InventoryFlag::Hangar,
                1,
                true,
            ),
            (
                100_000_025,
                4247,
                100_000_024,
                InventoryFlag::Cargo,
                500,
                false,
            ),
            (
                100_000_026,
                4247,
                100_000_001,
                InventoryFlag::Cargo,
                500,
                false,
            ),
            (
                100_000_027,
                55001,
                60_003_760,
                InventoryFlag::Hangar,
                1,
                false,
            ),
            (
                100_000_036,
                35840,
                100_000_001,
                InventoryFlag::Cargo,
                2,
                false,
            ),
        ];
        for (item_id, type_id, location_id, flag, quantity, singleton) in extra_items {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id,
                    owner_id: 90_000_001,
                    location_id,
                    flag,
                    quantity,
                    singleton,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        }
        let char_skills = snap.skills.entry(90_000_001).or_default();
        for (skill_id, level, sp) in [
            (3388, 4, 45_255.0),
            (11448, 4, 226_275.0),
            (11452, 4, 226_275.0),
            (21791, 4, 226_275.0),
            (24242, 4, 45_255.0),
            (33407, 4, 90_510.0),
            (21611, 2, 12_699.0),
        ] {
            char_skills.insert(
                skill_id,
                TrainedSkillRecord {
                    type_id: skill_id,
                    level,
                    skill_points: sp,
                },
            );
        }
    });

    let gm_fail = ctx
        .dispatch_rpc(
            &mut sess,
            "authentication",
            None,
            "Login",
            &[
                PyRep::String("admin".into()),
                PyRep::String("admin-secret".into()),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(gm_fail.unwrap_err().code, "AuthenticationFailed");

    // Login as demo capsuleer (90_000_001)
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
    .expect("Login demo");
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

    // R-P1-06: air_npe.AdvanceNode does not mutate career_goal_progress and gates node 3 (DestroySeekers) on actual NPC kill
    for _ in 0..2 {
        ctx.dispatch_rpc(&mut sess, "air_npe", None, "AdvanceNode", &[], &kw, None)
            .await
            .expect("AdvanceNode to node 3");
    }
    let npe_gate_err = ctx
        .dispatch_rpc(&mut sess, "air_npe", None, "AdvanceNode", &[], &kw, None)
        .await;
    assert_eq!(npe_gate_err.unwrap_err().code, "NodeObjectiveNotMet");

    // ApplySkinToShip rejects unlicensed SKIN before ActivateSkinLicense, and ActivateSkinLicense consumes item 100_000_027
    let unlicensed = ctx
        .dispatch_rpc(
            &mut sess,
            "shipCosmeticsMgr",
            None,
            "ApplySkinToShip",
            &[PyRep::Integer(587), PyRep::Integer(55001)],
            &kw,
            None,
        )
        .await;
    assert_eq!(unlicensed.unwrap_err().code, "SkinNotLicensed");

    ctx.dispatch_rpc(
        &mut sess,
        "shipCosmeticsMgr",
        None,
        "ActivateSkinLicense",
        &[PyRep::Integer(55001)],
        &kw,
        None,
    )
    .await
    .expect("ActivateSkinLicense");
    let no_second_license = ctx
        .dispatch_rpc(
            &mut sess,
            "shipCosmeticsMgr",
            None,
            "ActivateSkinLicense",
            &[PyRep::Integer(55001)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        no_second_license.unwrap_err().code,
        "SkinLicenseItemNotFound"
    );
    ctx.dispatch_rpc(
        &mut sess,
        "shipCosmeticsMgr",
        None,
        "ApplySkinToShip",
        &[PyRep::Integer(587), PyRep::Integer(55001)],
        &kw,
        None,
    )
    .await
    .expect("ApplySkinToShip after license activation");

    // B-04: Installing T2 Invention (activity 8) before copying any BPC from BPO 687 must fail with InventionRequiresBPC
    let bpo_inv_err = ctx
        .dispatch_rpc(
            &mut sess,
            "ramProxy",
            None,
            "InstallJob",
            &[
                PyRep::Integer(687),
                PyRep::Integer(8),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(bpo_inv_err.unwrap_err().code, "InventionRequiresBPC");

    // Copy Rifter BPO (687) twice (producing 2 BPCs with is_blueprint_copy=true), then test deterministic invention failure and success via server-side override
    let copy1 = ctx
        .dispatch_rpc(
            &mut sess,
            "ramProxy",
            None,
            "InstallJob",
            &[
                PyRep::Integer(687),
                PyRep::Integer(5),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect("Copy BPO 1");
    let PyRep::KeyVal(copy1_kv) = copy1 else {
        panic!("Expected copy1 KeyVal");
    };
    let copy1_id = copy1_kv.get("jobID").unwrap().as_i64().unwrap() as u64;
    ctx.advance_virtual_clock(10_000);
    ctx.dispatch_rpc(
        &mut sess,
        "industryManager",
        None,
        "CompleteJob",
        &[PyRep::Integer(copy1_id as i64)],
        &kw,
        None,
    )
    .await
    .expect("Complete Copy 1");

    let copy2 = ctx
        .dispatch_rpc(
            &mut sess,
            "ramProxy",
            None,
            "InstallJob",
            &[
                PyRep::Integer(687),
                PyRep::Integer(5),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect("Copy BPO 2");
    let PyRep::KeyVal(copy2_kv) = copy2 else {
        panic!("Expected copy2 KeyVal");
    };
    let copy2_id = copy2_kv.get("jobID").unwrap().as_i64().unwrap() as u64;
    ctx.advance_virtual_clock(10_000);
    ctx.dispatch_rpc(
        &mut sess,
        "industryManager",
        None,
        "CompleteJob",
        &[PyRep::Integer(copy2_id as i64)],
        &kw,
        None,
    )
    .await
    .expect("Complete Copy 2");

    // Install invention 1 and test failure roll (server-side override = 0.95 >= 0.4100)
    let inv1 = ctx
        .dispatch_rpc(
            &mut sess,
            "ramProxy",
            None,
            "InstallJob",
            &[
                PyRep::Integer(687),
                PyRep::Integer(8),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect("Install invention 1");
    let PyRep::KeyVal(inv1_kv) = inv1 else {
        panic!("Expected inv1 KeyVal");
    };
    let inv1_job_id = inv1_kv.get("jobID").unwrap().as_i64().unwrap() as u64;
    ctx.advance_virtual_clock(40_000);
    ctx.set_invention_roll_override(Some(0.95));
    let inv1_fail = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "CompleteJob",
            &[PyRep::Integer(inv1_job_id as i64)],
            &kw,
            None,
        )
        .await;
    assert_eq!(inv1_fail.unwrap_err().code, "InventionAttemptFailed");

    // Install invention 2 and test success roll (server-side override = 0.10 < 0.4100)
    let inv2 = ctx
        .dispatch_rpc(
            &mut sess,
            "ramProxy",
            None,
            "InstallJob",
            &[
                PyRep::Integer(687),
                PyRep::Integer(8),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect("Install invention 2");
    let PyRep::KeyVal(inv2_kv) = inv2 else {
        panic!("Expected inv2 KeyVal");
    };
    let inv2_job_id = inv2_kv.get("jobID").unwrap().as_i64().unwrap() as u64;
    ctx.advance_virtual_clock(40_000);
    ctx.set_invention_roll_override(Some(0.10));
    let inv2_ok = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "CompleteJob",
            &[PyRep::Integer(inv2_job_id as i64)],
            &kw,
            None,
        )
        .await
        .expect("Complete invention 2");
    ctx.set_invention_roll_override(None);
    let PyRep::KeyVal(inv2_ok_kv) = inv2_ok else {
        panic!("Expected inv2_ok KeyVal");
    };
    assert_eq!(
        inv2_ok_kv.get("productTypeID").and_then(|v| v.as_i64()),
        Some(11372)
    );

    // planetMgr.CreateColony + planetMgr.Flush materializes 200x Water (type_id 3645) into station hangar
    ctx.dispatch_rpc(
        &mut sess,
        "planetMgr",
        None,
        "CreateColony",
        &[PyRep::Integer(40009082)],
        &kw,
        None,
    )
    .await
    .expect("planetMgr.CreateColony");
    let flush = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "Flush",
            &[PyRep::Integer(40009082)],
            &kw,
            None,
        )
        .await
        .expect("planetMgr.Flush");
    let PyRep::KeyVal(flush_kv) = flush else {
        panic!("Expected flush KeyVal");
    };
    let delivered_water_id = flush_kv.get("deliveredItemID").unwrap().as_i64().unwrap() as u64;
    assert!(delivered_water_id > 0);

    async fn warp_and_jump_gate(
        ctx_ref: &ServerContext,
        sess_ref: &mut SessionState,
        kw_ref: &HashMap<String, PyRep>,
        gate_id: i64,
        pos: [f64; 3],
    ) {
        let sys_id = sess_ref.solar_system_id.unwrap();
        let bound = ctx_ref
            .bind_object(sess_ref, "beyonce", sys_id as i64)
            .expect("bind beyonce");
        let handle = ctx_ref.systems.get_or_spawn(sys_id);
        let warp_res = ctx_ref
            .dispatch_rpc(
                sess_ref,
                "beyonce",
                Some(bound),
                "CmdWarpToPoint",
                &[
                    PyRep::Real(pos[0]),
                    PyRep::Real(pos[1]),
                    PyRep::Real(pos[2]),
                ],
                kw_ref,
                None,
            )
            .await;
        match warp_res {
            Ok(_) => {
                for _ in 0..40 {
                    let _ = handle.step_manual(1.0).await.unwrap();
                }
            }
            Err(e) if e.code == "WarpDistanceTooShort" => {
                let ship_id = sess_ref.ship_id.unwrap();
                let (_, ball) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
                let dir = [
                    pos[0] - ball.position.x,
                    pos[1] - ball.position.y,
                    pos[2] - ball.position.z,
                ];
                ctx_ref
                    .dispatch_rpc(
                        sess_ref,
                        "beyonce",
                        Some(bound),
                        "CmdGotoDirection",
                        &[
                            PyRep::Real(dir[0]),
                            PyRep::Real(dir[1]),
                            PyRep::Real(dir[2]),
                        ],
                        kw_ref,
                        None,
                    )
                    .await
                    .expect("CmdGotoDirection to nearby stargate");
                for _ in 0..9 {
                    let _ = handle.step_manual(1.0).await.unwrap();
                }
                ctx_ref
                    .dispatch_rpc(sess_ref, "beyonce", Some(bound), "CmdStop", &[], kw_ref, None)
                    .await
                    .expect("CmdStop at stargate");
            }
            Err(e) => panic!("CmdWarpToPoint to stargate: {e:?}"),
        }
        ctx_ref
            .dispatch_rpc(
                sess_ref,
                "beyonce",
                Some(bound),
                "CmdStargateJump",
                &[PyRep::Integer(gate_id)],
                kw_ref,
                None,
            )
            .await
            .expect("CmdStargateJump");
    }

    // Undock in Rifter (100_000_001), travel Jita (50001248) -> Perimeter (50001250) -> Tama (30002813) to anchor & fuel Tama Cyno Citadel
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock Rifter");
    warp_and_jump_gate(
        &ctx,
        &mut sess,
        &kw,
        50001248,
        [-161_369_743_360.0, -30_690_549_760.0, -1_107_962_183_680.0],
    )
    .await;
    warp_and_jump_gate(
        &ctx,
        &mut sess,
        &kw,
        50001250,
        [86_000_000_000.0, 12_000_000_000.0, -141_000_000_000.0],
    )
    .await;
    assert_eq!(sess.solar_system_id, Some(30002813));

    // R2-P0-01: MaterializeSystemLOD2 rejects remote solar system arguments with CannotMaterializeRemoteSystem
    let remote_mat_err = ctx
        .dispatch_rpc(
            &mut sess,
            "taleMgr",
            None,
            "MaterializeSystemLOD2",
            &[PyRep::Integer(30_000_142)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        remote_mat_err.unwrap_err().code,
        "CannotMaterializeRemoteSystem"
    );

    // R2-P1-03: Anchoring an Upwell structure while in starter NPC corporation (1_000_172) is rejected with NpcCorpCannotAnchorStructure
    let npc_anchor_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structureDeployment",
            None,
            "Anchor",
            &[
                PyRep::Integer(35832),
                PyRep::String("Tama Cyno Citadel".into()),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        npc_anchor_err.unwrap_err().code,
        "NpcCorpCannotAnchorStructure"
    );

    // Create a player corporation on sess (90_000_001) before anchoring structures
    ctx.dispatch_rpc(
        &mut sess,
        "corpmgr",
        None,
        "CreateCorporation",
        &[
            PyRep::String("Tama Vanguard Corp".into()),
            PyRep::String("TVC".into()),
            PyRep::Real(0.05),
        ],
        &kw,
        None,
    )
    .await
    .expect("CreateCorporation TVC");

    // Anchor Upwell Astrahus in Tama (30002813), verify fuelBlocks == 0, and fuel it from Rifter cargo
    let anchor_res = ctx
        .dispatch_rpc(
            &mut sess,
            "structureDeployment",
            None,
            "Anchor",
            &[
                PyRep::Integer(35832),
                PyRep::String("Tama Cyno Citadel".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("Anchor Tama Cyno Citadel");
    let PyRep::KeyVal(anchor_kv) = anchor_res else {
        panic!("Expected anchor KeyVal");
    };
    assert_eq!(anchor_kv.get("fuelBlocks").and_then(|v| v.as_i64()), Some(0));
    let tama_struct_id = anchor_kv.get("structureID").unwrap().as_i64().unwrap() as u64;
    ctx.dispatch_rpc(
        &mut sess,
        "structureControl",
        None,
        "FuelStructure",
        &[PyRep::Integer(tama_struct_id as i64), PyRep::Integer(50)],
        &kw,
        None,
    )
    .await
    .expect("FuelStructure");

    // R-P1-07 & AP-06: Anchor and fuel a dedicated Pharolux Cyno Beacon (35840) in low-sec Tama (30002813)
    let anchor_beacon_res = ctx
        .dispatch_rpc(
            &mut sess,
            "structureDeployment",
            None,
            "Anchor",
            &[
                PyRep::Integer(35840),
                PyRep::String("Tama Cyno Beacon".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("Anchor Tama Cyno Beacon");
    let PyRep::KeyVal(beacon_kv) = anchor_beacon_res else {
        panic!("Expected beacon KeyVal");
    };
    let tama_beacon_id = beacon_kv.get("structureID").unwrap().as_i64().unwrap() as u64;
    ctx.dispatch_rpc(
        &mut sess,
        "structureControl",
        None,
        "FuelStructure",
        &[PyRep::Integer(tama_beacon_id as i64), PyRep::Integer(50)],
        &kw,
        None,
    )
    .await
    .expect("FuelStructure Tama Cyno Beacon");

    // Dock at Tama Cyno Citadel, install a jump clone there, and undock
    ctx.dispatch_rpc(
        &mut sess,
        "structureDocking",
        None,
        "Dock",
        &[PyRep::Integer(tama_struct_id as i64)],
        &kw,
        None,
    )
    .await
    .expect("Dock at Tama Cyno Citadel");
    let clone_tama = ctx
        .dispatch_rpc(
            &mut sess,
            "jumpCloneSvc",
            None,
            "InstallCloneInStation",
            &[],
            &kw,
            None,
        )
        .await
        .expect("InstallCloneInStation at Tama Cyno Citadel");
    let PyRep::KeyVal(ct_kv) = clone_tama else {
        panic!("Expected clone_tama KeyVal");
    };
    let tama_clone_id = ct_kv.get("jumpCloneID").unwrap().as_i64().unwrap();
    ctx.dispatch_rpc(
        &mut sess,
        "structureDocking",
        None,
        "Undock",
        &[],
        &kw,
        None,
    )
    .await
    .expect("Undock from Tama Cyno Citadel");

    // Return Rifter to Jita (60003760) via stargates 50001251 -> 50001873
    warp_and_jump_gate(
        &ctx,
        &mut sess,
        &kw,
        50001251,
        [10_000_000_000.0, 0.0, 10_000_000_000.0],
    )
    .await;
    warp_and_jump_gate(
        &ctx,
        &mut sess,
        &kw,
        50001873,
        [84_000_000_000.0, 12_000_000_000.0, -139_000_000_000.0],
    )
    .await;
    let jita_bound = ctx.bind_object(&mut sess, "beyonce", 30_000_142).unwrap();
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(jita_bound),
        "CmdWarpToPoint",
        &[
            PyRep::Real(-107_303_362_560.0),
            PyRep::Real(-18_744_975_360.0),
            PyRep::Real(436_489_052_160.0),
        ],
        &kw,
        None,
    )
    .await
    .expect("Warp to Jita 4-4");
    let jita_handle = ctx.systems.get_or_spawn(30_000_142);
    for _ in 0..40 {
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
    .expect("Dock Rifter at Jita");

    // N-P1-8 & R2-P0-01: Convoy 800_000_001 was automatically promoted to LOD 2 on system entry/undock; MaterializeSystemLOD2 requires GM role (0x02) and returns 0 when already at LOD 2
    assert_eq!(
        ctx.living_world
            .get_convoy(800_000_001)
            .map(|c| c.lod_level),
        Some(2)
    );
    let non_gm_mat = ctx
        .dispatch_rpc(
            &mut sess,
            "taleMgr",
            None,
            "MaterializeSystemLOD2",
            &[PyRep::Integer(30_000_142)],
            &kw,
            None,
        )
        .await;
    assert_eq!(non_gm_mat.unwrap_err().code, "InsufficientRole");
    sess.role |= 0x02;
    let mat_second = ctx
        .dispatch_rpc(
            &mut sess,
            "taleMgr",
            None,
            "MaterializeSystemLOD2",
            &[PyRep::Integer(30_000_142)],
            &kw,
            None,
        )
        .await
        .expect("GM MaterializeSystemLOD2");
    assert_eq!(mat_second.as_i64(), Some(0));
    sess.role = 0;

    // N-P1-5: home_station.set_home_station rejects remote station (60008494 while docked at 60003760) with MustBeDockedAtTargetStation
    let remote_home_err = ctx
        .dispatch_rpc(
            &mut sess,
            "home_station",
            None,
            "set_home_station",
            &[PyRep::Integer(60_008_494)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        remote_home_err.unwrap_err().code,
        "MustBeDockedAtTargetStation"
    );
    ctx.dispatch_rpc(
        &mut sess,
        "home_station",
        None,
        "set_home_station",
        &[PyRep::Integer(60_003_760)],
        &kw,
        None,
    )
    .await
    .expect("set_home_station while docked at 60003760");

    // Test cross-location CloneJump on sess (Jita 60003760 -> Tama Cyno Citadel -> back to Jita 60003760)
    ctx.dispatch_rpc(
        &mut sess,
        "jumpCloneSvc",
        None,
        "CloneJump",
        &[PyRep::Integer(tama_clone_id)],
        &kw,
        None,
    )
    .await
    .expect("CloneJump to Tama Cyno Citadel");
    assert_eq!(sess.structure_id, Some(tama_struct_id as i64));
    assert_eq!(sess.solar_system_id, Some(30002813));
    assert_ne!(sess.ship_id, Some(100_000_001));

    ctx.advance_virtual_clock(90_000);
    ctx.dispatch_rpc(
        &mut sess,
        "jumpCloneSvc",
        None,
        "CloneJump",
        &[PyRep::Integer(tama_clone_id)],
        &kw,
        None,
    )
    .await
    .expect("CloneJump back to Jita");
    assert_eq!(sess.station_id, Some(60003760));
    assert_eq!(sess.solar_system_id, Some(30000142));

    // Board Archon Carrier (100_000_024, type_id = 24483), undock, test fighterMgr NPC destruction & bounty payout, and verify LY jump range & security restrictions
    ctx.dispatch_rpc(
        &mut sess,
        "ship",
        None,
        "Board",
        &[PyRep::Integer(100_000_024)],
        &kw,
        None,
    )
    .await
    .expect("Board Archon Carrier");
    assert_eq!(sess.ship_id, Some(100_000_024));

    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock Archon");
    ctx.dispatch_rpc(
        &mut sess,
        "fighterMgr",
        None,
        "GetFightersForShip",
        &[],
        &kw,
        None,
    )
    .await
    .expect("GetFightersForShip from Archon");
    let (_, archon_ball) = jita_handle
        .get_ball_snapshot(100_000_024)
        .await
        .unwrap()
        .unwrap();
    let _ = jita_handle
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: 700_000_077,
                type_id: 11001,
                position: archon_ball.position + glam::DVec3::new(2_000.0, 0.0, 0.0),
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
    let ftr_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[PyRep::Integer(1), PyRep::Integer(700_000_077)],
            &kw,
            None,
        )
        .await
        .expect("CmdActivateAbilitySlots from Archon against NPC 700_000_077");
    let PyRep::KeyVal(ftr_kv) = ftr_res else {
        panic!("Expected fighter ability KeyVal");
    };
    assert_eq!(ftr_kv.get("destroyed"), Some(&PyRep::Bool(true)));
    assert_eq!(ftr_kv.get("bountyPayoutISK"), Some(&PyRep::Real(90_000.0)));

    // R-P1-07 & AP-06: Jumping out of high-sec Jita (30000142, sec 0.95) is rejected with CannotJumpFromHighSec
    let from_hs_jump_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCynoBeaconMgr",
            None,
            "CmdJumpToStructureBeacon",
            &[PyRep::Integer(30002813)],
            &kw,
            None,
        )
        .await;
    assert_eq!(from_hs_jump_err.unwrap_err().code, "CannotJumpFromHighSec");

    // Move Archon Carrier (100_000_024) into genuine low-sec Horkkisen (30002741, sec 0.4384, 8.668 LY from Tama 30002813)
    let _ = jita_handle.remove_ball(100_000_024).await;
    ctx.db
        .set_character_solar_system(90_000_001, 30_002_741, &ctx.sde)
        .expect("Move Archon to Horkkisen");
    sess.solar_system_id = Some(30_002_741);
    let hork_handle = ctx.systems.get_or_spawn(30_002_741);
    let _ = hork_handle
        .spawn_ball(eve_destiny::Ball {
            id: 100_000_024,
            type_id: 24483,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 85.0,
            inertia: 0.05,
            mass: 1_125_000_000.0,
            radius: 1_500.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;

    // B-08: Jumping to high-sec Perimeter (30000144, sec 0.95) is rejected with CannotJumpToHighSec
    let hs_jump_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCynoBeaconMgr",
            None,
            "CmdJumpToStructureBeacon",
            &[PyRep::Integer(30000144)],
            &kw,
            None,
        )
        .await;
    assert_eq!(hs_jump_err.unwrap_err().code, "CannotJumpToHighSec");

    // B-08: Jumping from Horkkisen (30002741) to VFK-IV (30002904, 15.86 LY away > 9.8 LY JDC II range) is rejected with JumpTargetOutOfRange
    let oor_jump_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCynoBeaconMgr",
            None,
            "CmdJumpToStructureBeacon",
            &[PyRep::Integer(30002904)],
            &kw,
            None,
        )
        .await;
    assert_eq!(oor_jump_err.unwrap_err().code, "JumpTargetOutOfRange");

    // B-08 & R-P1-07: Jumping from low-sec Horkkisen (30002741) to fueled Pharolux Cyno Beacon (35840) in low-sec Tama (30002813, 8.668 LY <= 9.8 LY JDC II range) succeeds and scales fuel (62 blocks) and jump fatigue
    let jump_cyno = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCynoBeaconMgr",
            None,
            "CmdJumpToStructureBeacon",
            &[PyRep::Integer(30002813)],
            &kw,
            None,
        )
        .await
        .expect("CmdJumpToStructureBeacon");
    let PyRep::KeyVal(jc_kv) = jump_cyno else {
        panic!("Expected jump_cyno KeyVal");
    };
    assert!(
        jc_kv
            .get("jumpCooldownMinutes")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
            > 6.5
    );
    assert_eq!(sess.solar_system_id, Some(30002813));
    assert_eq!(
        ctx.db.get_item(100_000_025).map(|i| i.quantity),
        Some(500 - 62)
    );

    // Immediate second jump fails due to jump fatigue cooldown
    let cooldown_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCynoBeaconMgr",
            None,
            "CmdJumpToStructureBeacon",
            &[PyRep::Integer(30002904)],
            &kw,
            None,
        )
        .await;
    assert_eq!(cooldown_err.unwrap_err().code, "JumpDriveOnCooldown");

    // Spawn second player (90_000_002 on account "other"), jump to Tama, create hostile corp, and siege structure across all 3 phases
    let mut sess2 = SessionState::new(2);
    ctx.dispatch_rpc(
        &mut sess2,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("other".into()),
            PyRep::String("other-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login sess2");
    ctx.dispatch_rpc(
        &mut sess2,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_002)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID 90_000_002");

    // Verify untrained character cannot install jump clone without Infomorph Psychology
    let no_infomorph = ctx
        .dispatch_rpc(
            &mut sess2,
            "jumpCloneSvc",
            None,
            "InstallCloneInStation",
            &[PyRep::Integer(60008494)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        no_infomorph.unwrap_err().code,
        "InfomorphPsychologyRequired"
    );

    ctx.dispatch_rpc(&mut sess2, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock 90_000_002");
    // Verify non-capital Rifter (100_000_002) is rejected by CmdJumpToStructureBeacon in high-sec and low-sec
    let non_cap_hs_jump = ctx
        .dispatch_rpc(
            &mut sess2,
            "structureCynoBeaconMgr",
            None,
            "CmdJumpToStructureBeacon",
            &[PyRep::Integer(30002813)],
            &kw,
            None,
        )
        .await;
    assert_eq!(non_cap_hs_jump.unwrap_err().code, "CannotJumpFromHighSec");
    // Travel via stargates Jita (50001248) -> Perimeter (50001250) -> Tama (30002813)
    warp_and_jump_gate(
        &ctx,
        &mut sess2,
        &kw,
        50001248,
        [-161_369_743_360.0, -30_690_549_760.0, -1_107_962_183_680.0],
    )
    .await;
    warp_and_jump_gate(
        &ctx,
        &mut sess2,
        &kw,
        50001250,
        [86_000_000_000.0, 12_000_000_000.0, -141_000_000_000.0],
    )
    .await;
    assert_eq!(sess2.solar_system_id, Some(30002813));
    let non_cap_jump = ctx
        .dispatch_rpc(
            &mut sess2,
            "structureCynoBeaconMgr",
            None,
            "CmdJumpToStructureBeacon",
            &[PyRep::Integer(30002741)],
            &kw,
            None,
        )
        .await;
    assert_eq!(non_cap_jump.unwrap_err().code, "ShipNotJumpCapable");
    let corp_hsc = ctx
        .dispatch_rpc(
            &mut sess2,
            "corpmgr",
            None,
            "CreateCorporation",
            &[
                PyRep::String("Hostile Siege Corp".into()),
                PyRep::String("HSC".into()),
                PyRep::Real(0.05),
            ],
            &kw,
            None,
        )
        .await
        .expect("CreateCorporation HSC");
    let PyRep::KeyVal(hsc_kv) = corp_hsc else {
        panic!("Expected hsc KeyVal");
    };
    // B-06: CreateCorporation does NOT auto-create an alliance; allianceRegistry.CreateAlliance creates it explicitly
    assert_eq!(hsc_kv.get("allianceID"), Some(&PyRep::None));
    let al_res = ctx
        .dispatch_rpc(
            &mut sess2,
            "allianceRegistry",
            None,
            "CreateAlliance",
            &[
                PyRep::String("Hostile Siege Alliance".into()),
                PyRep::String("HSA".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("CreateAlliance HSA");
    let PyRep::KeyVal(al_kv) = al_res else {
        panic!("Expected al_kv KeyVal");
    };
    assert!(al_kv.get("allianceID").and_then(|v| v.as_i64()).unwrap_or(0) > 99_000_000);

    let tama_handle = ctx.systems.get_or_spawn(30_002_813);
    for _ in 0..4 {
        let _ = tama_handle.step_manual(1.0).await.unwrap();
    }

    // R8-P1-3, R9-P1-2 & R18-P1-1: Seed Republic Fleet Phased Plasma S (22989: 2.3 kinetic + 11.5 thermal = 13.8 base damage) in Rifter 100_000_002 Cargo,
    // verify AttackStructure rejects with NoAmmoLoaded while ammo is only in Cargo, load into HiSlot0 (flag 27) via dogmaIM.LoadAmmo,
    // and verify a single frigate weapon cycle (200mm AutoCannon II 2889: 13.8 * 3.465 = 47.817 volley / 3.125s = 15.30144 DPS) deals strictly one volley and consumes 1 round
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_000_940,
            eve_db::InventoryItemRecord {
                item_id: 100_000_940,
                type_id: 22989,
                owner_id: 90_000_002,
                location_id: 100_000_002,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 2_000,
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
    let unl_frigate_poke = ctx
        .dispatch_rpc(
            &mut sess2,
            "structureControl",
            None,
            "AttackStructure",
            &[PyRep::Integer(tama_struct_id as i64)],
            &kw,
            None,
        )
        .await;
    assert_eq!(unl_frigate_poke.unwrap_err().code, "NoAmmoLoaded");
    ctx.dispatch_rpc(
        &mut sess2,
        "dogmaIM",
        None,
        "LoadAmmo",
        &[
            PyRep::Integer(100_000_002),
            PyRep::Integer(27),
            PyRep::Integer(100_000_940),
        ],
        &kw,
        None,
    )
    .await
    .expect("LoadAmmo 22989 into Rifter 100_000_002 HiSlot0");
    let frigate_poke = ctx
        .dispatch_rpc(
            &mut sess2,
            "structureControl",
            None,
            "AttackStructure",
            &[PyRep::Integer(tama_struct_id as i64)],
            &kw,
            None,
        )
        .await
        .expect("AttackStructure single frigate cycle");
    let PyRep::KeyVal(poke_kv) = frigate_poke else {
        panic!("Expected poke_kv KeyVal");
    };
    assert_eq!(
        poke_kv.get("state").and_then(|v| v.as_str()),
        Some("ShieldVulnerable")
    );
    let poke_shield_hp = poke_kv.get("shieldHP").and_then(|v| v.as_f64()).unwrap();
    assert!((poke_shield_hp - 1_799_952.183).abs() < 1e-4);
    assert_eq!(ctx.db.get_item(100_000_940).unwrap().quantity, 1_999);

    // R12-P1-1 / R13-P0-1 / R16-P1-1 / R17-P1-1 / R18-P1-1: Dock at tama_struct_id, board a Raven battleship (type_id 638: 11,000 MW PG, 750 tf CPU, 7 high slots, 6 launcher hardpoints),
    // fit 6x Torpedo Launcher II (type_id 2420: 1,375 MW PG, 66 tf CPU, 10.5s cycle, 15.0 m3 capacity) loaded with Inferno Torpedo (type_id 2510: 0.05 m3 -> 300 torpedoes per launcher, 450.0 thermal damage each -> 2,700.0 volley / 257.14 DPS total)
    // via invbroker.MoveItem & dogmaIM.LoadAmmo into HiSlot0..=HiSlot5 (flags 27..=32), undock, and cycle 667 volleys (667 * 2,700 = 1,800,900 >= 1,799,952.183) to strip the remaining shield HP
    ctx.dispatch_rpc(
        &mut sess2,
        "structureDocking",
        None,
        "Dock",
        &[PyRep::Integer(tama_struct_id as i64)],
        &kw,
        None,
    )
    .await
    .expect("Dock sess2 at tama_struct_id to refit");
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_000_950,
            eve_db::InventoryItemRecord {
                item_id: 100_000_950,
                type_id: 638, // Raven battleship
                owner_id: 90_000_002,
                location_id: tama_struct_id as i64,
                flag: eve_db::InventoryFlag::Hangar,
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
        // Seed 1 Heavy Entropic Disintegrator II (47936) to verify Raven (638) rejects Triglavian cruiser weapons,
        // plus 6 canonical Torpedo Launcher II (2420) for 90_000_002
        snap.items.insert(
            100_000_959,
            eve_db::InventoryItemRecord {
                item_id: 100_000_959,
                type_id: 47936, // Heavy Entropic Disintegrator II (restricted to Cruiser/HAC)
                owner_id: 90_000_002,
                location_id: tama_struct_id as i64,
                flag: eve_db::InventoryFlag::Hangar,
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
        if let Some(ch2) = snap.characters.get_mut(&90_000_002) {
            ch2.clone_grade = 2; // Omega clone so Torpedo Specialization IV can be queued and trained
            ch2.free_skill_points = 1_642_039.0; // 256,000 (3319 V) + 1,024,000 (3325 V) + 362,039 (20212 IV)
        }
        for idx in 0..6_i64 {
            let mod_id = 100_000_951 + idx;
            snap.items.insert(
                mod_id,
                eve_db::InventoryItemRecord {
                    item_id: mod_id,
                    type_id: 2420, // Torpedo Launcher II
                    owner_id: 90_000_002,
                    location_id: tama_struct_id as i64,
                    flag: eve_db::InventoryFlag::Hangar,
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
        }
    });
    for (sk_id, target_lvl) in [(3319_i64, 5_i64), (3325_i64, 5_i64), (20212_i64, 4_i64)] {
        ctx.dispatch_rpc(
            &mut sess2,
            "skillMgr2",
            None,
            "SaveSkillQueue",
            &[PyRep::Integer(sk_id), PyRep::Integer(target_lvl)],
            &kw,
            None,
        )
        .await
        .expect("Queue missile skill for Raven");
    }
    ctx.dispatch_rpc(
        &mut sess2,
        "skillMgr2",
        None,
        "ApplyFreeSkillPoints",
        &[],
        &kw,
        None,
    )
    .await
    .expect("ApplyFreeSkillPoints to complete missile skill queue for Raven");
    ctx.db.with_snapshot(|snap| {
        let skills = snap.skills.get(&90_000_002).expect("90_000_002 skills");
        assert_eq!(skills.get(&3319).map(|s| s.level), Some(5));
        assert_eq!(skills.get(&3325).map(|s| s.level), Some(5));
        assert_eq!(skills.get(&20212).map(|s| s.level), Some(4));
        assert_eq!(
            snap.skill_queues.get(&90_000_002).map(|q| q.len()),
            Some(0)
        );
    });
    ctx.dispatch_rpc(
        &mut sess2,
        "ship",
        None,
        "Board",
        &[PyRep::Integer(100_000_950)],
        &kw,
        None,
    )
    .await
    .expect("Board Raven 100_000_950 at tama_struct_id");
    // Verify Heavy Entropic Disintegrator II (47936) is rejected on Raven (638)
    let bad_trig_fit = ctx
        .dispatch_rpc(
            &mut sess2,
            "invbroker",
            None,
            "MoveItem",
            &[
                PyRep::Integer(100_000_959),
                PyRep::Integer(100_000_950),
                PyRep::Integer(27),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(bad_trig_fit.unwrap_err().code, "FittingValidationFailed");
    for idx in 0..6_i64 {
        let mod_id = 100_000_951 + idx;
        let flag_id = 27 + idx;
        ctx.dispatch_rpc(
            &mut sess2,
            "invbroker",
            None,
            "MoveItem",
            &[
                PyRep::Integer(mod_id),
                PyRep::Integer(100_000_950),
                PyRep::Integer(flag_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("Fit Torpedo Launcher II via MoveItem");
    }
    let undock_res = ctx
        .dispatch_rpc(
            &mut sess2,
            "structureDocking",
            None,
            "Undock",
            &[],
            &kw,
            None,
        )
        .await
        .expect("Undock Raven from tama_struct_id");
    let PyRep::KeyVal(u_kv) = undock_res else {
        panic!("Expected undock KeyVal");
    };
    let raven_b_id = u_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    ctx.dispatch_rpc(
        &mut sess2,
        "beyonce",
        Some(raven_b_id),
        "CmdStop",
        &[],
        &kw,
        None,
    )
    .await
    .expect("Stop Raven on grid at tama_struct_id");

    // R17-P1-1: Verify unloaded Torpedo Launcher II (2420) cannot fire without torpedo charges
    for _ in 0..11 {
        let _ = tama_handle.step_manual(1.0).await.unwrap();
    }
    let unloaded_atk = ctx
        .dispatch_rpc(
            &mut sess2,
            "structureControl",
            None,
            "AttackStructure",
            &[PyRep::Integer(tama_struct_id as i64)],
            &kw,
            None,
        )
        .await;
    assert_eq!(unloaded_atk.unwrap_err().code, "NoAmmoLoaded");

    // Seed canonical Inferno Torpedo charges (type_id 2510: group 89, 0.05 m3, 450.0 thermal damage) in Raven 100_000_950 Cargo
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_000_960,
            eve_db::InventoryItemRecord {
                item_id: 100_000_960,
                type_id: 2510, // Inferno Torpedo
                owner_id: 90_000_002,
                location_id: 100_000_950,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 20_000,
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

    // R18-P1-1: Having Inferno Torpedoes in Cargo alone is NOT sufficient; AttackStructure must reject with NoAmmoLoaded until charges are loaded into weapon slots
    let cargo_only_atk = ctx
        .dispatch_rpc(
            &mut sess2,
            "structureControl",
            None,
            "AttackStructure",
            &[PyRep::Integer(tama_struct_id as i64)],
            &kw,
            None,
        )
        .await;
    assert_eq!(cargo_only_atk.unwrap_err().code, "NoAmmoLoaded");

    let raven_slots = PyRep::List(
        (27_i64..=32)
            .map(PyRep::Integer)
            .collect(),
    );
    let load_torps = ctx
        .dispatch_rpc(
            &mut sess2,
            "dogmaIM",
            None,
            "LoadAmmo",
            &[
                PyRep::Integer(100_000_950),
                raven_slots.clone(),
                PyRep::Integer(100_000_960),
            ],
            &kw,
            None,
        )
        .await
        .expect("LoadAmmo 6x Torpedo Launcher II");
    let PyRep::KeyVal(lt_kv) = load_torps else {
        panic!("Expected LoadAmmo KeyVal");
    };
    assert_eq!(lt_kv.get("totalLoaded"), Some(&PyRep::Integer(1_800)));
    assert_eq!(ctx.db.get_item(100_000_960).unwrap().quantity, 18_200);

    let mut atk1_state = String::new();
    for volley_idx in 0..670 {
        for _ in 0..11 {
            let _ = tama_handle.step_manual(1.0).await.unwrap();
        }
        let atk1 = match ctx
            .dispatch_rpc(
                &mut sess2,
                "structureControl",
                None,
                "AttackStructure",
                &[PyRep::Integer(tama_struct_id as i64)],
                &kw,
                None,
            )
            .await
        {
            Ok(res) => res,
            Err(e) if e.code == "NoAmmoLoaded" => {
                ctx.dispatch_rpc(
                    &mut sess2,
                    "dogmaIM",
                    None,
                    "LoadAmmo",
                    &[
                        PyRep::Integer(100_000_950),
                        raven_slots.clone(),
                        PyRep::Integer(100_000_960),
                    ],
                    &kw,
                    None,
                )
                .await
                .expect("Reload Torpedo Launcher II in Shield siege");
                ctx.dispatch_rpc(
                    &mut sess2,
                    "structureControl",
                    None,
                    "AttackStructure",
                    &[PyRep::Integer(tama_struct_id as i64)],
                    &kw,
                    None,
                )
                .await
                .expect("AttackStructure Shield volley after reload")
            }
            Err(e) => panic!("Unexpected AttackStructure Shield error: {e:?}"),
        };
        if volley_idx == 0 {
            for flag_u16 in 27_u16..=32 {
                let slot_qty = ctx.db.with_snapshot(|s| {
                    s.items
                        .values()
                        .find(|i| {
                            i.location_id == 100_000_950
                                && (i.flag as u16) == flag_u16
                                && i.type_id == 2510
                        })
                        .map(|i| i.quantity)
                        .unwrap_or(0)
                });
                assert_eq!(slot_qty, 299);
            }
        }
        let PyRep::KeyVal(atk1_kv) = atk1 else {
            panic!("Expected atk1 KeyVal");
        };
        atk1_state = atk1_kv
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if atk1_state == "ArmorReinforced" {
            break;
        }
    }
    assert_eq!(atk1_state, "ArmorReinforced");

    // Structure remains invulnerable even after 901s because ArmorReinforced timer is 24h (86,400s)
    ctx.advance_virtual_clock(901);
    for _ in 0..11 {
        let _ = tama_handle.step_manual(1.0).await.unwrap();
    }
    let invuln_err = ctx
        .dispatch_rpc(
            &mut sess2,
            "structureControl",
            None,
            "AttackStructure",
            &[PyRep::Integer(tama_struct_id as i64)],
            &kw,
            None,
        )
        .await;
    assert_eq!(invuln_err.unwrap_err().code, "StructureReinforcedInvulnerable");

    // Advance another 86,400s past the 24h ArmorReinforced timer and cycle 667 volleys (667 * 2,700 = 1,800,900 >= 1,800,000) to strip Armor
    ctx.advance_virtual_clock(86_400);
    let mut atk2_state = String::new();
    for _ in 0..670 {
        for _ in 0..11 {
            let _ = tama_handle.step_manual(1.0).await.unwrap();
        }
        let atk2 = match ctx
            .dispatch_rpc(
                &mut sess2,
                "structureControl",
                None,
                "AttackStructure",
                &[PyRep::Integer(tama_struct_id as i64)],
                &kw,
                None,
            )
            .await
        {
            Ok(res) => res,
            Err(e) if e.code == "NoAmmoLoaded" => {
                ctx.dispatch_rpc(
                    &mut sess2,
                    "dogmaIM",
                    None,
                    "LoadAmmo",
                    &[
                        PyRep::Integer(100_000_950),
                        raven_slots.clone(),
                        PyRep::Integer(100_000_960),
                    ],
                    &kw,
                    None,
                )
                .await
                .expect("Reload Torpedo Launcher II in Armor siege");
                ctx.dispatch_rpc(
                    &mut sess2,
                    "structureControl",
                    None,
                    "AttackStructure",
                    &[PyRep::Integer(tama_struct_id as i64)],
                    &kw,
                    None,
                )
                .await
                .expect("AttackStructure Armor volley after reload")
            }
            Err(e) => panic!("Unexpected AttackStructure Armor error: {e:?}"),
        };
        let PyRep::KeyVal(atk2_kv) = atk2 else {
            panic!("Expected atk2 KeyVal");
        };
        atk2_state = atk2_kv
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if atk2_state == "HullReinforced" {
            break;
        }
    }
    assert_eq!(atk2_state, "HullReinforced");

    // Advance past the 48h (172,800s) HullReinforced timer and cycle 1,334 volleys (1,334 * 2,700 = 3,601,800 >= 3,600,000) to destroy Hull
    ctx.advance_virtual_clock(172_800 + 1);
    let mut atk3_state = String::new();
    let mut dropped_core = 0_i64;
    for _ in 0..1340 {
        for _ in 0..11 {
            let _ = tama_handle.step_manual(1.0).await.unwrap();
        }
        let atk3 = match ctx
            .dispatch_rpc(
                &mut sess2,
                "structureControl",
                None,
                "AttackStructure",
                &[PyRep::Integer(tama_struct_id as i64)],
                &kw,
                None,
            )
            .await
        {
            Ok(res) => res,
            Err(e) if e.code == "NoAmmoLoaded" => {
                ctx.dispatch_rpc(
                    &mut sess2,
                    "dogmaIM",
                    None,
                    "LoadAmmo",
                    &[
                        PyRep::Integer(100_000_950),
                        raven_slots.clone(),
                        PyRep::Integer(100_000_960),
                    ],
                    &kw,
                    None,
                )
                .await
                .expect("Reload Torpedo Launcher II in Hull siege");
                ctx.dispatch_rpc(
                    &mut sess2,
                    "structureControl",
                    None,
                    "AttackStructure",
                    &[PyRep::Integer(tama_struct_id as i64)],
                    &kw,
                    None,
                )
                .await
                .expect("AttackStructure Hull volley after reload")
            }
            Err(e) => panic!("Unexpected AttackStructure Hull error: {e:?}"),
        };
        let PyRep::KeyVal(atk3_kv) = atk3 else {
            panic!("Expected atk3 KeyVal");
        };
        atk3_state = atk3_kv
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        dropped_core = atk3_kv
            .get("droppedQuantumCoreItemID")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        if atk3_state == "Destroyed" {
            break;
        }
    }
    assert_eq!(atk3_state, "Destroyed");
    assert!(dropped_core > 0);

    // B-09 & R-P1-03: Faction Warfare CaptureComplex in low-sec Tama (30002813, complex beacon at [250_000.0, 0.0, -150_000.0], 600s duration, starts at 270s / 45.0%)
    ctx.dispatch_rpc(
        &mut sess2,
        "fwCharacterEnlistmentMgr",
        None,
        "CreateMyDirectEnlistment",
        &[PyRep::Integer(500001)],
        &kw,
        None,
    )
    .await
    .expect("Enlist FW");

    // Attempting to capture at stargate/structure position (outside 30km of [250_000.0, 0.0, -150_000.0]) is rejected
    let out_of_radius = ctx
        .dispatch_rpc(&mut sess2, "facWarMgr", None, "CaptureComplex", &[], &kw, None)
        .await;
    assert_eq!(
        out_of_radius.unwrap_err().code,
        "OutsideComplexCaptureRadius"
    );

    // Warp to complex beacon [250_000.0, 0.0, -150_000.0]
    let tama_b2 = ctx.bind_object(&mut sess2, "beyonce", 30_002_813).unwrap();
    ctx.dispatch_rpc(
        &mut sess2,
        "beyonce",
        Some(tama_b2),
        "CmdWarpToPoint",
        &[
            PyRep::Real(250_000.0),
            PyRep::Real(0.0),
            PyRep::Real(-150_000.0),
        ],
        &kw,
        None,
    )
    .await
    .expect("Warp sess2 to Tama FW complex beacon");
    for _ in 0..40 {
        let _ = tama_handle.step_manual(1.0).await.unwrap();
    }

    // First 2 capture ticks only advance elapsed timer from 270s -> 271s -> 272s out of 600s (0 LP awarded)
    for expected_elapsed in [271_i64, 272_i64] {
        let _ = tama_handle.step_manual(1.0).await.unwrap();
        let fw_tick = ctx
            .dispatch_rpc(&mut sess2, "facWarMgr", None, "CaptureComplex", &[], &kw, None)
            .await
            .expect("CaptureComplex tick");
        let PyRep::KeyVal(fw_kv) = fw_tick else {
            panic!("Expected fw_tick KeyVal");
        };
        assert_eq!(
            fw_kv.get("captureSecondsElapsed").and_then(|v| v.as_i64()),
            Some(expected_elapsed)
        );
        assert_eq!(fw_kv.get("lpAwarded").and_then(|v| v.as_i64()), Some(0));
    }

    // R-P1-03 & AP-07: CaptureComplex clamps each pulse to <= 30 ticks while within 30km; step 11 bounded 30-tick intervals (11 * 30 >= 328s) to reach 600s / 100.0% and award 15,000 LP
    let mut fw_final_kv = None;
    for _ in 0..11 {
        for _ in 0..30 {
            let _ = tama_handle.step_manual(1.0).await.unwrap();
        }
        let fw_step = ctx
            .dispatch_rpc(&mut sess2, "facWarMgr", None, "CaptureComplex", &[], &kw, None)
            .await
            .expect("CaptureComplex bounded 30-tick step");
        let PyRep::KeyVal(step_kv) = fw_step else {
            panic!("Expected fw_step KeyVal");
        };
        fw_final_kv = Some(step_kv);
    }
    let fw_final_kv = fw_final_kv.expect("Expected 11th CaptureComplex result");
    assert_eq!(
        fw_final_kv.get("contestedPct").and_then(|v| v.as_f64()),
        Some(100.0)
    );
    assert_eq!(
        fw_final_kv.get("lpAwarded").and_then(|v| v.as_i64()),
        Some(15_000)
    );
}

#[tokio::test]
async fn test_review_r6_np0_np1_regressions() {
    use eve_db::{IskCents, MarketOrderRecord};

    let ctx = ServerContext::new();
    let kw = HashMap::new();

    // N-P1-6: Starter character 90_000_001 belongs to Republic Military School (1_000_170, school_id 14)
    let ch1 = ctx.db.get_character(90_000_001).expect("90_000_001 exists");
    assert_eq!(ch1.corporation_id, 1_000_170);

    let mut s1 = SessionState::new(10);
    ctx.dispatch_rpc(
        &mut s1,
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
    .unwrap();
    ctx.dispatch_rpc(
        &mut s1,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .unwrap();

    // N-P1-3: planetMgr.CreateColony rejects forged planet_id where (planet_id / 1000) == sys_id
    let bad_planet = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "CreateColony",
            &[PyRep::Integer(30_000_142_000)],
            &kw,
            None,
        )
        .await;
    assert_eq!(bad_planet.unwrap_err().code, "InvalidPlanetID");
    let wrong_sys_planet = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "CreateColony",
            &[PyRep::Integer(40_139_295)], // Amarr VIII is in 30002187, not Jita 30000142
            &kw,
            None,
        )
        .await;
    assert_eq!(
        wrong_sys_planet.unwrap_err().code,
        "PlanetNotInCurrentSystem"
    );

    // CreateColony on Jita planet 40009082 and verify Flush deducts POCO export tax from wallet
    ctx.dispatch_rpc(
        &mut s1,
        "planetMgr",
        None,
        "CreateColony",
        &[PyRep::Integer(40_009_082)],
        &kw,
        None,
    )
    .await
    .unwrap();
    let bal_before_flush = ctx.db.get_character(90_000_001).unwrap().balance.0;
    ctx.dispatch_rpc(
        &mut s1,
        "planetMgr",
        None,
        "Flush",
        &[PyRep::Integer(40_009_082)],
        &kw,
        None,
    )
    .await
    .unwrap();
    let bal_after_flush = ctx.db.get_character(90_000_001).unwrap().balance.0;
    assert!(bal_after_flush < bal_before_flush);

    // N-P0-2: Two-party Courier contract and self-accept rejection
    let mut s2 = SessionState::new(11);
    ctx.dispatch_rpc(
        &mut s2,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("other".into()),
            PyRep::String("other-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut s2,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_002)],
        &kw,
        None,
    )
    .await
    .unwrap();

    // s2 (starts docked at 60003760) creates Courier contract 60003760 -> 60008494
    let c_res = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::String("Courier".into()),
                PyRep::Integer(60_003_760),
                PyRep::Integer(60_008_494),
                PyRep::Real(10_000.0),
                PyRep::Real(25_000.0),
            ],
            &kw,
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(c_kv) = c_res else {
        panic!("Expected contract KeyVal");
    };
    let cid = c_kv.get("contractID").unwrap().as_i64().unwrap();

    // Self-accept by issuer (s2) is rejected
    let self_acc = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "AcceptContract",
            &[PyRep::Integer(cid)],
            &kw,
            None,
        )
        .await;
    assert_eq!(self_acc.unwrap_err().code, "CannotAcceptOwnContract");

    // Move s2 to Amarr (60008494) and create a contract there so s1 (at 60003760) is rejected with MustBeDockedAtStartStation
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch2) = snap.characters.get_mut(&90_000_002) {
            ch2.station_id = Some(60_008_494);
            ch2.solar_system_id = 30_002_187;
        }
        if let Some(sh2) = snap.items.get_mut(&100_000_002) {
            sh2.location_id = 60_008_494;
        }
    });
    s2.station_id = Some(60_008_494);
    s2.solar_system_id = Some(30_002_187);

    let c_amarr = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::String("Courier".into()),
                PyRep::Integer(60_008_494),
                PyRep::Integer(60_003_760),
                PyRep::Real(10_000.0),
                PyRep::Real(25_000.0),
            ],
            &kw,
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(ca_kv) = c_amarr else {
        panic!("Expected ca KeyVal");
    };
    let cid_amarr = ca_kv.get("contractID").unwrap().as_i64().unwrap();

    let wrong_start = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "AcceptContract",
            &[PyRep::Integer(cid_amarr)],
            &kw,
            None,
        )
        .await;
    assert_eq!(wrong_start.unwrap_err().code, "MustBeDockedAtStartStation");

    // Move s2 and ship 100_000_002 back to Jita 60003760; s1 creates cid2 (60003760 -> 60008494) and s2 accepts it
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch2) = snap.characters.get_mut(&90_000_002) {
            ch2.station_id = Some(60_003_760);
            ch2.solar_system_id = 30_000_142;
        }
        if let Some(sh2) = snap.items.get_mut(&100_000_002) {
            sh2.location_id = 60_003_760;
        }
    });
    s2.station_id = Some(60_003_760);
    s2.solar_system_id = Some(30_000_142);

    let c2_res = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::String("Courier".into()),
                PyRep::Integer(60_003_760),
                PyRep::Integer(60_008_494),
                PyRep::Real(10_000.0),
                PyRep::Real(25_000.0),
            ],
            &kw,
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(c2_kv) = c2_res else {
        panic!("Expected c2 KeyVal");
    };
    let cid2 = c2_kv.get("contractID").unwrap().as_i64().unwrap();

    ctx.dispatch_rpc(
        &mut s2,
        "contractProxy",
        None,
        "AcceptContract",
        &[PyRep::Integer(cid2)],
        &kw,
        None,
    )
    .await
    .unwrap();

    // Completing while still at 60003760 (not 60008494) is rejected with CourierNotAtDestinationStation
    let early_comp = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "CompleteContract",
            &[PyRep::Integer(cid2)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        early_comp.unwrap_err().code,
        "CourierNotAtDestinationStation"
    );

    // Deliver s2's ship 100_000_002 (containing Courier Plastic Wrap 24445) to 60008494 and complete
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch2) = snap.characters.get_mut(&90_000_002) {
            ch2.station_id = Some(60_008_494);
            ch2.solar_system_id = 30_002_187;
        }
        if let Some(sh2) = snap.items.get_mut(&100_000_002) {
            sh2.location_id = 60_008_494;
        }
    });
    s2.station_id = Some(60_008_494);
    s2.solar_system_id = Some(30_002_187);

    let comp_ok = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "CompleteContract",
            &[PyRep::Integer(cid2)],
            &kw,
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(comp_kv) = comp_ok else {
        panic!("Expected comp KeyVal");
    };
    assert_eq!(
        comp_kv.get("status").and_then(|v| v.as_str()),
        Some("Completed")
    );

    // N-P1-7 & R7-P1-3: Seed a player ask order in Jita for Tritanium (34) at 4.00 ISK, verify Auction contract rejection (R7-P1-1), undock s1 in Jita (auto-promoting convoy 800_000_001 to LOD 2), destroy convoy via dogmaIM.FireWeapon, and verify space wreck + +15% NPC ask spike + player order untouched
    let auction_err = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::String("Auction".into()),
                PyRep::Real(1_000.0),
                PyRep::Real(0.0),
                PyRep::Integer(100_000_008),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(auction_err.unwrap_err().code, "InvalidContractType");

    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.insert(
            999_999,
            MarketOrderRecord {
                order_id: 999_999,
                owner_id: 90_000_002,
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 34,
                price: IskCents::from_f64_isk(4.0),
                vol_entered: 100,
                vol_remaining: 100,
                min_volume: 1,
                is_bid: false,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
    });
    ctx.dispatch_rpc(&mut s1, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .unwrap();
    let non_gm_interdict = ctx
        .dispatch_rpc(
            &mut s1,
            "taleMgr",
            None,
            "InterdictConvoy",
            &[PyRep::Integer(800_000_001)],
            &kw,
            None,
        )
        .await;
    assert_eq!(non_gm_interdict.unwrap_err().code, "InsufficientRole");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_000_941,
            eve_db::InventoryItemRecord {
                item_id: 100_000_941,
                type_id: 22989,
                owner_id: 90_000_001,
                location_id: 100_000_001,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 2_000,
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
    ctx.dispatch_rpc(
        &mut s1,
        "dogmaIM",
        None,
        "LoadAmmo",
        &[
            PyRep::Integer(100_000_001),
            PyRep::Integer(27),
            PyRep::Integer(100_000_941),
        ],
        &kw,
        None,
    )
    .await
    .unwrap();
    let jita_h = ctx.systems.get_or_spawn(30_000_142);
    let mut ikv = HashMap::new();
    for shot in 0..10 {
        if shot > 0 {
            for _ in 0..4 {
                let _ = jita_h.step_manual(1.0).await.unwrap();
            }
        }
        let interdict = ctx
            .dispatch_rpc(
                &mut s1,
                "dogmaIM",
                None,
                "FireWeapon",
                &[PyRep::Integer(800_000_001)],
                &kw,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(kv) = interdict else {
            panic!("Expected convoy combat destruction KeyVal");
        };
        if kv.get("destroyed") == Some(&PyRep::Bool(true)) {
            ikv = kv;
            break;
        }
    }
    assert_eq!(ikv.get("destroyed"), Some(&PyRep::Bool(true)));
    let wreck_id = ikv.get("wreckItemID").unwrap().as_i64().unwrap();
    let loot_id = ikv.get("lootItemID").unwrap().as_i64().unwrap();
    let wreck_item = ctx.db.get_item(wreck_id).expect("wreck item in db");
    let loot_item = ctx.db.get_item(loot_id).expect("loot item in db");
    assert_eq!(wreck_item.type_id, 26557);
    assert_eq!(wreck_item.location_id, 30_000_142);
    assert_eq!(loot_item.location_id, wreck_id);
    // Player ask order 999_999 remains at 4.00 ISK (400 cents), while NPC ask order 700_000_001 spiked by +15% from 5.00 ISK (500 cents) to 5.75 ISK (575 cents)
    let player_price = ctx.db.with_snapshot(|s| s.market_orders[&999_999].price.0);
    let npc_price = ctx.db.with_snapshot(|s| s.market_orders[&700_000_001].price.0);
    assert_eq!(player_price, 400);
    assert_eq!(npc_price, 575);
}

#[tokio::test]
async fn test_review_r8_regressions() {
    use eve_db::InventoryFlag;

    let ctx = ServerContext::new();
    let kw = HashMap::new();

    let mut s1 = SessionState::new(30);
    ctx.dispatch_rpc(
        &mut s1,
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
    .unwrap();
    ctx.dispatch_rpc(
        &mut s1,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .unwrap();

    let mut s2 = SessionState::new(31);
    ctx.dispatch_rpc(
        &mut s2,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("other".into()),
            PyRep::String("other-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut s2,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_002)],
        &kw,
        None,
    )
    .await
    .unwrap();

    // 1. R8-P0-1: Escrow 1MN Afterburner II (100_000_003, type 438) into an ItemExchange contract;
    // verify CreateDynamicItem cannot mutate a contract-locked item, AcceptContract fails with ContractEscrowItemMissing and zero ISK transfer if the escrowed item is missing, and DeleteContract restores it.
    let c_ab = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::String("ItemExchange".into()),
                PyRep::Integer(60_003_760),
                PyRep::Integer(60_003_760),
                PyRep::Real(50_000.0),
                PyRep::Real(0.0),
                PyRep::Integer(100_000_003),
            ],
            &kw,
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(cab_kv) = c_ab else {
        panic!("Expected cab_kv");
    };
    let cid_ab = cab_kv.get("contractID").unwrap().as_i64().unwrap();

    let muta_locked = ctx
        .dispatch_rpc(
            &mut s1,
            "dynamicItemService",
            None,
            "CreateDynamicItem",
            &[PyRep::Integer(47702), PyRep::Integer(100_000_003)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        muta_locked.unwrap_err().code,
        "TargetItemLockedOrUnreachable"
    );

    // Verify AcceptContract fails with ContractEscrowItemMissing and zero ISK transfer if the escrowed item is missing
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&100_000_003) {
            it.flag = InventoryFlag::Hangar;
        }
    });
    let s2_bal_before = ctx.db.get_character(90_000_002).unwrap().balance.0;
    let missing_acc = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "AcceptContract",
            &[PyRep::Integer(cid_ab)],
            &kw,
            None,
        )
        .await;
    assert_eq!(missing_acc.unwrap_err().code, "ContractEscrowItemMissing");
    assert_eq!(ctx.db.get_character(90_000_002).unwrap().balance.0, s2_bal_before);

    // Restore lock and delete contract via DeleteContract; verify item 100_000_003 is back in Hangar at 60_003_760
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&100_000_003) {
            it.flag = InventoryFlag::Locked;
        }
    });
    ctx.dispatch_rpc(
        &mut s1,
        "contractProxy",
        None,
        "DeleteContract",
        &[PyRep::Integer(cid_ab)],
        &kw,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        ctx.db.get_item(100_000_003).unwrap().flag,
        InventoryFlag::Hangar
    );

    // 2. R8-P1-1: Install industry job (locks Rifter BPO 100_000_007), verify CreateContract rejects escrowing 100_000_007 with ContractItemLocked,
    // and verify CompleteJob rejects completion with BlueprintMissingOrUnlocked if blueprint was unlocked/moved.
    let job_res = ctx
        .dispatch_rpc(
            &mut s1,
            "industryManager",
            None,
            "InstallJob",
            &[
                PyRep::Integer(687),
                PyRep::Integer(4), // ME research on BPO 100_000_007
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(jkv) = job_res else {
        panic!("Expected job KeyVal");
    };
    let jid = jkv.get("jobID").unwrap().as_i64().unwrap();

    let contract_bp_err = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::String("ItemExchange".into()),
                PyRep::Integer(60_003_760),
                PyRep::Integer(60_003_760),
                PyRep::Real(10_000.0),
                PyRep::Real(0.0),
                PyRep::Integer(100_000_007),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(contract_bp_err.unwrap_err().code, "ContractItemLocked");

    ctx.advance_virtual_clock(3_600);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(bp) = snap.items.get_mut(&100_000_007) {
            bp.flag = InventoryFlag::Hangar;
        }
    });
    let comp_unlocked_err = ctx
        .dispatch_rpc(
            &mut s1,
            "industryManager",
            None,
            "CompleteJob",
            &[PyRep::Integer(jid)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        comp_unlocked_err.unwrap_err().code,
        "BlueprintMissingOrUnlocked"
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(bp) = snap.items.get_mut(&100_000_007) {
            bp.flag = InventoryFlag::Locked;
        }
    });
    ctx.dispatch_rpc(
        &mut s1,
        "industryManager",
        None,
        "CompleteJob",
        &[PyRep::Integer(jid)],
        &kw,
        None,
    )
    .await
    .unwrap();

    // 3. R8-P1-2 & R9-P1-1: Expired contract settlement for both Outstanding and InProgress Courier contracts
    let s1_bal_pre = ctx.db.get_character(90_000_001).unwrap().balance.0;
    let c_exp = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::String("Courier".into()),
                PyRep::Integer(60_003_760),
                PyRep::Integer(60_008_494),
                PyRep::Real(5_000.0),
                PyRep::Real(20_000.0),
            ],
            &kw,
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(cexp_kv) = c_exp else {
        panic!("Expected cexp_kv");
    };
    let cid_exp = cexp_kv.get("contractID").unwrap().as_i64().unwrap();
    assert_eq!(
        ctx.db.get_character(90_000_001).unwrap().balance.0,
        s1_bal_pre - 500_000
    );
    ctx.advance_virtual_clock(86_405);
    let acc_exp = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "AcceptContract",
            &[PyRep::Integer(cid_exp)],
            &kw,
            None,
        )
        .await;
    assert_eq!(acc_exp.unwrap_err().code, "ContractExpired");
    // Escrowed 5,000 ISK reward was durably refunded to issuer s1
    assert_eq!(
        ctx.db.get_character(90_000_001).unwrap().balance.0,
        s1_bal_pre
    );

    // R9-P1-1: InProgress Courier contract resets delivery deadline on acceptance; on expiry, issuer gets collateral + reward and contracted item transfers to acceptor at its current location (not teleported back to issuer)
    let c_inprog = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::String("Courier".into()),
                PyRep::Integer(60_003_760),
                PyRep::Integer(60_003_760),
                PyRep::Real(5_000.0),
                PyRep::Real(20_000.0),
                PyRep::Integer(100_000_003),
            ],
            &kw,
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(cin_kv) = c_inprog else {
        panic!("Expected cin_kv");
    };
    let cid_in = cin_kv.get("contractID").unwrap().as_i64().unwrap();
    let create_exp = cin_kv.get("expirationUnix").unwrap().as_i64().unwrap();
    // Advance 80,000s (late in 24h offer window) and accept with s2
    ctx.advance_virtual_clock(80_000);
    let acc_in = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "AcceptContract",
            &[PyRep::Integer(cid_in)],
            &kw,
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(accin_kv) = acc_in else {
        panic!("Expected accin_kv");
    };
    let delivery_exp = accin_kv.get("expirationUnix").unwrap().as_i64().unwrap();
    assert_eq!(delivery_exp, create_exp + 80_000);
    // Advance past the new 24h delivery deadline (86,405s) and call CompleteContract
    ctx.advance_virtual_clock(86_405);
    let comp_exp = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "CompleteContract",
            &[PyRep::Integer(cid_in)],
            &kw,
            None,
        )
        .await;
    assert_eq!(comp_exp.unwrap_err().code, "ContractExpired");
    // Issuer s1 received back the 5,000 ISK reward + 20,000 ISK forfeited collateral (+2,000,000 cents net over s1_bal_pre)
    assert_eq!(
        ctx.db.get_character(90_000_001).unwrap().balance.0,
        s1_bal_pre + 2_000_000
    );
    // Contracted item 100_000_003 stayed in s2's ship (100_000_002) and transferred ownership to s2 (90_000_002)
    let forfeited_item = ctx.db.get_item(100_000_003).unwrap();
    assert_eq!(forfeited_item.owner_id, 90_000_002);
    assert_eq!(forfeited_item.location_id, 100_000_002);
    assert_eq!(forfeited_item.flag, InventoryFlag::Cargo);

    // 4. R8-P1-4 & R9-P1-3: Accepting missions allocates unique NPC ball IDs per character and mission instance;
    // s1 accepting 1001 & 1002 and s2 accepting 1001 produce 3 distinct NPC balls that never collide.
    let PyRep::KeyVal(u1_kv) = ctx
        .dispatch_rpc(&mut s1, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .unwrap()
    else {
        panic!("Expected u1_kv");
    };
    let s1_b_id = u1_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    ctx.dispatch_rpc(&mut s1, "beyonce", Some(s1_b_id), "CmdStop", &[], &kw, None)
        .await
        .unwrap();
    let PyRep::KeyVal(u2_kv) = ctx
        .dispatch_rpc(&mut s2, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .unwrap()
    else {
        panic!("Expected u2_kv");
    };
    let s2_b_id = u2_kv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    ctx.dispatch_rpc(&mut s2, "beyonce", Some(s2_b_id), "CmdStop", &[], &kw, None)
        .await
        .unwrap();
    let mut s1_npcs = Vec::new();
    for mid in [1001_i64, 1002_i64] {
        ctx.dispatch_rpc(
            &mut s1,
            "agentMgr",
            None,
            "RequestMission",
            &[PyRep::Integer(mid)],
            &kw,
            None,
        )
        .await
        .unwrap();
        let am = ctx
            .dispatch_rpc(
                &mut s1,
                "agentMgr",
                None,
                "AcceptMission",
                &[PyRep::Integer(mid)],
                &kw,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(am_kv) = am else {
            panic!("Expected am_kv");
        };
        s1_npcs.push(am_kv.get("spawnedNpcBallID").unwrap().as_i64().unwrap());
    }
    ctx.dispatch_rpc(
        &mut s2,
        "agentMgr",
        None,
        "RequestMission",
        &[PyRep::Integer(1001)],
        &kw,
        None,
    )
    .await
    .unwrap();
    let am_s2 = ctx
        .dispatch_rpc(
            &mut s2,
            "agentMgr",
            None,
            "AcceptMission",
            &[PyRep::Integer(1001)],
            &kw,
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(ams2_kv) = am_s2 else {
        panic!("Expected ams2_kv");
    };
    let s2_npc_1001 = ams2_kv.get("spawnedNpcBallID").unwrap().as_i64().unwrap();
    assert_ne!(s1_npcs[0], s1_npcs[1]);
    assert_ne!(s1_npcs[0], s2_npc_1001);
    assert_ne!(s1_npcs[1], s2_npc_1001);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_000_942,
            eve_db::InventoryItemRecord {
                item_id: 100_000_942,
                type_id: 22989,
                owner_id: 90_000_001,
                location_id: 100_000_001,
                flag: InventoryFlag::Cargo,
                quantity: 2_000,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            100_000_943,
            eve_db::InventoryItemRecord {
                item_id: 100_000_943,
                type_id: 22989,
                owner_id: 90_000_002,
                location_id: 100_000_002,
                flag: InventoryFlag::Cargo,
                quantity: 2_000,
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
    ctx.dispatch_rpc(
        &mut s1,
        "dogmaIM",
        None,
        "LoadAmmo",
        &[
            PyRep::Integer(100_000_001),
            PyRep::Integer(27),
            PyRep::Integer(100_000_942),
        ],
        &kw,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut s2,
        "dogmaIM",
        None,
        "LoadAmmo",
        &[
            PyRep::Integer(100_000_002),
            PyRep::Integer(27),
            PyRep::Integer(100_000_943),
        ],
        &kw,
        None,
    )
    .await
    .unwrap();

    let jita_handle = ctx.systems.get_or_spawn(30_000_142);
    for _ in 0..4 {
        let _ = jita_handle.step_manual(1.0).await.unwrap();
    }
    // Third-party pilot (`s2`) cannot attack `s1`'s instanced mission pocket NPC
    let cross_fire = ctx
        .dispatch_rpc(
            &mut s2,
            "dogmaIM",
            None,
            "FireWeapon",
            &[PyRep::Integer(s1_npcs[0])],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        cross_fire.unwrap_err().code,
        "MissionPocketInstanceRestricted"
    );

    let mut fw1_destroyed = false;
    for shot in 0..10 {
        if shot > 0 {
            for _ in 0..4 {
                let _ = jita_handle.step_manual(1.0).await.unwrap();
            }
        }
        let fw1 = ctx
            .dispatch_rpc(
                &mut s1,
                "dogmaIM",
                None,
                "FireWeapon",
                &[PyRep::Integer(s1_npcs[0])],
                &kw,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(fw1_kv) = fw1 else {
            panic!("Expected fw1_kv");
        };
        if fw1_kv.get("destroyed") == Some(&PyRep::Bool(true)) {
            fw1_destroyed = true;
            break;
        }
    }
    assert!(fw1_destroyed);

    let gate_m2_early = ctx
        .dispatch_rpc(
            &mut s1,
            "keeper",
            None,
            "ActivateAccelerationGate",
            &[PyRep::Integer(1002)],
            &kw,
            None,
        )
        .await;
    assert_eq!(gate_m2_early.unwrap_err().code, "RoomNpcsRemaining");
    let m2_early = ctx
        .dispatch_rpc(
            &mut s1,
            "agentMgr",
            None,
            "CompleteMission",
            &[PyRep::Integer(1002)],
            &kw,
            None,
        )
        .await;
    assert_eq!(m2_early.unwrap_err().code, "MissionIncomplete");

    let mut fw2_destroyed = false;
    for _ in 0..10 {
        for _ in 0..4 {
            let _ = jita_handle.step_manual(1.0).await.unwrap();
        }
        let fw2 = ctx
            .dispatch_rpc(
                &mut s1,
                "dogmaIM",
                None,
                "FireWeapon",
                &[PyRep::Integer(s1_npcs[1])],
                &kw,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(fw2_kv) = fw2 else {
            panic!("Expected fw2_kv");
        };
        if fw2_kv.get("destroyed") == Some(&PyRep::Bool(true)) {
            fw2_destroyed = true;
            break;
        }
    }
    assert!(fw2_destroyed);
    ctx.dispatch_rpc(
        &mut s1,
        "keeper",
        None,
        "ActivateAccelerationGate",
        &[PyRep::Integer(1002)],
        &kw,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut s1,
        "agentMgr",
        None,
        "CompleteMission",
        &[PyRep::Integer(1002)],
        &kw,
        None,
    )
    .await
    .unwrap();

    // s2's mission 1001 NPC is still alive and independent; s2 destroys it and completes mission 1001
    let mut fws2_destroyed = false;
    for shot in 0..10 {
        if shot > 0 {
            for _ in 0..4 {
                let _ = jita_handle.step_manual(1.0).await.unwrap();
            }
        }
        let fw_s2 = ctx
            .dispatch_rpc(
                &mut s2,
                "dogmaIM",
                None,
                "FireWeapon",
                &[PyRep::Integer(s2_npc_1001)],
                &kw,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(fws2_kv) = fw_s2 else {
            panic!("Expected fws2_kv");
        };
        if fws2_kv.get("destroyed") == Some(&PyRep::Bool(true)) {
            fws2_destroyed = true;
            break;
        }
    }
    assert!(fws2_destroyed);
    ctx.dispatch_rpc(
        &mut s2,
        "keeper",
        None,
        "ActivateAccelerationGate",
        &[PyRep::Integer(1001)],
        &kw,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut s2,
        "agentMgr",
        None,
        "CompleteMission",
        &[PyRep::Integer(1001)],
        &kw,
        None,
    )
    .await
    .unwrap();

    // Mission abandon/decline via RemoveOfferFromJournal despawns the pocket NPC and clears the journal entry
    ctx.dispatch_rpc(
        &mut s1,
        "agentMgr",
        None,
        "RequestMission",
        &[PyRep::Integer(1003)],
        &kw,
        None,
    )
    .await
    .unwrap();
    let am_1003 = ctx
        .dispatch_rpc(
            &mut s1,
            "agentMgr",
            None,
            "AcceptMission",
            &[PyRep::Integer(1003)],
            &kw,
            None,
        )
        .await
        .unwrap();
    let PyRep::KeyVal(am1003_kv) = am_1003 else {
        panic!("Expected am1003_kv");
    };
    let npc_1003 = am1003_kv.get("spawnedNpcBallID").unwrap().as_i64().unwrap();
    assert!(jita_handle.get_ball_snapshot(npc_1003).await.unwrap().is_some());
    ctx.dispatch_rpc(
        &mut s1,
        "agentMgr",
        None,
        "RemoveOfferFromJournal",
        &[PyRep::Integer(1003)],
        &kw,
        None,
    )
    .await
    .unwrap();
    assert!(jita_handle.get_ball_snapshot(npc_1003).await.unwrap().is_none());

    // N1 & N2: Cannot abandon an already completed (1002) or already quit (1003) mission, and cannot re-request either to farm rewards/bounties
    let quit_completed = ctx
        .dispatch_rpc(
            &mut s1,
            "agentMgr",
            None,
            "RemoveOfferFromJournal",
            &[PyRep::Integer(1002)],
            &kw,
            None,
        )
        .await;
    assert_eq!(quit_completed.unwrap_err().code, "MissionNotActive");
    let quit_already_quit = ctx
        .dispatch_rpc(
            &mut s1,
            "agentMgr",
            None,
            "RemoveOfferFromJournal",
            &[PyRep::Integer(1003)],
            &kw,
            None,
        )
        .await;
    assert_eq!(quit_already_quit.unwrap_err().code, "MissionNotActive");
    let rereq_completed = ctx
        .dispatch_rpc(
            &mut s1,
            "agentMgr",
            None,
            "RequestMission",
            &[PyRep::Integer(1002)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        rereq_completed.unwrap_err().code,
        "MissionAlreadyActiveOrCompleted"
    );
    let rereq_quit = ctx
        .dispatch_rpc(
            &mut s1,
            "agentMgr",
            None,
            "RequestMission",
            &[PyRep::Integer(1003)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        rereq_quit.unwrap_err().code,
        "MissionAlreadyActiveOrCompleted"
    );

    // N4: Cannot fit a module into a ship slot while in space (`CannotFitInSpace`)
    let fit_in_space = ctx
        .dispatch_rpc(
            &mut s1,
            "invbroker",
            None,
            "MoveItem",
            &[
                PyRep::Integer(100_000_003),
                PyRep::Integer(100_000_001),
                PyRep::Integer(12),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(fit_in_space.unwrap_err().code, "CannotFitInSpace");

    // N7: Cannot warp to arbitrary unscanned/unbookmarked coordinates (`InvalidWarpDestination`)
    let s1_bound = ctx.bind_object(&mut s1, "beyonce", 30_000_142).unwrap();
    let invalid_warp = ctx
        .dispatch_rpc(
            &mut s1,
            "beyonce",
            Some(s1_bound),
            "CmdWarpToPoint",
            &[
                PyRep::Real(900_000_000_000.0),
                PyRep::Real(900_000_000_000.0),
                PyRep::Real(900_000_000_000.0),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(invalid_warp.unwrap_err().code, "InvalidWarpDestination");

    // R12-P1-3: Cannot warp to another player's unprobed ship in space even when its Ball exists in the Ballpark
    let jita_handle = ctx.systems.get_or_spawn(30_000_142);
    let _ = jita_handle
        .spawn_ball(eve_destiny::Ball {
            id: 100_000_002,
            type_id: 587,
            position: glam::DVec3::new(500_000_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 3.45,
            mass: 1_067_000.0,
            radius: 37.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    let warp_to_other_ship = ctx
        .dispatch_rpc(
            &mut s1,
            "beyonce",
            Some(s1_bound),
            "CmdWarpToPoint",
            &[
                PyRep::Real(500_000_000.0),
                PyRep::Real(0.0),
                PyRep::Real(0.0),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        warp_to_other_ship.unwrap_err().code,
        "InvalidWarpDestination"
    );

    // R12-P1-3: CmdWarpToStuff resolves ("item", stargate_id) and rejects invalid targets
    let warp_stuff_ok = ctx
        .dispatch_rpc(
            &mut s1,
            "beyonce",
            Some(s1_bound),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(50_001_248)],
            &kw,
            None,
        )
        .await;
    assert!(warp_stuff_ok.is_ok(), "CmdWarpToStuff to stargate 50001248");
    let warp_stuff_bad = ctx
        .dispatch_rpc(
            &mut s1,
            "beyonce",
            Some(s1_bound),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(999_999_999)],
            &kw,
            None,
        )
        .await;
    assert_eq!(warp_stuff_bad.unwrap_err().code, "InvalidWarpDestination");

    // R12-P1-1: Fabricated type 3520 is absent from SDE, and T2 BPO (11372) / rolled Abyssal (47740) are not sold on NPC market
    assert!(!ctx.sde.types.contains_key(&3520));
    let (orders_11372, orders_47740) = ctx.db.with_snapshot(|snap| {
        (
            snap.market_orders.values().filter(|o| o.type_id == 11372).count(),
            snap.market_orders.values().filter(|o| o.type_id == 47740).count(),
        )
    });
    assert_eq!(orders_11372, 0);
    assert_eq!(orders_47740, 0);

    // R12-P1-2: Dock s2 at Jita 4-4 and verify slot-type mismatch and turret hardpoint overflow are rejected
    ctx.db
        .dock_character(90_000_002, 60_003_760, false)
        .expect("Dock 90_000_002 at Jita 4-4");
    let mut s2 = SessionState::new(820);
    ctx.dispatch_rpc(
        &mut s2,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("other".into()),
            PyRep::String("other-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut s2,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_002)],
        &kw,
        None,
    )
    .await
    .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        for (iid, tid) in [
            (100_009_901_i64, 438_u32),  // 1MN Afterburner II (medPower 13)
            (100_009_902_i64, 2889_u32), // 200mm AutoCannon II #2 (turretFitted 42)
            (100_009_903_i64, 2889_u32), // 200mm AutoCannon II #3 (turretFitted 42)
            (100_009_904_i64, 2889_u32), // 200mm AutoCannon II #4 (exceeds Rifter's 3 turret hardpoints)
        ] {
            snap.items.insert(
                iid,
                eve_db::InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: 90_000_002,
                    location_id: 60_003_760,
                    flag: eve_db::InventoryFlag::Hangar,
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
        }
    });
    // Fitting med-slot 1MN Afterburner II (438) into HiSlot1 (flag 28) fails with FittingValidationFailed
    let bad_slot = ctx
        .dispatch_rpc(
            &mut s2,
            "invbroker",
            None,
            "MoveItem",
            &[
                PyRep::Integer(100_009_901),
                PyRep::Integer(100_000_002),
                PyRep::Integer(28),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(bad_slot.unwrap_err().code, "FittingValidationFailed");

    // Fitting turrets #2 and #3 into HiSlot1 (28) and HiSlot2 (29) succeeds on Rifter (3 turret hardpoints),
    // but fitting turret #4 into HiSlot3 (30) fails (exceeds Rifter's 3 turret hardpoints / 4 high slots)
    ctx.dispatch_rpc(
        &mut s2,
        "invbroker",
        None,
        "MoveItem",
        &[
            PyRep::Integer(100_009_902),
            PyRep::Integer(100_000_002),
            PyRep::Integer(28),
        ],
        &kw,
        None,
    )
    .await
    .expect("Fit 2nd turret on Rifter");
    ctx.dispatch_rpc(
        &mut s2,
        "invbroker",
        None,
        "MoveItem",
        &[
            PyRep::Integer(100_009_903),
            PyRep::Integer(100_000_002),
            PyRep::Integer(29),
        ],
        &kw,
        None,
    )
    .await
    .expect("Fit 3rd turret on Rifter");
    let fourth_turret = ctx
        .dispatch_rpc(
            &mut s2,
            "invbroker",
            None,
            "MoveItem",
            &[
                PyRep::Integer(100_009_904),
                PyRep::Integer(100_000_002),
                PyRep::Integer(30),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(fourth_turret.unwrap_err().code, "FittingValidationFailed");

    // R13-P0-1: RepackageItems rejects ships/containers with fitted modules or cargo (`CannotRepackageNonEmptyItem`)
    // and PlaceCharOrder / CreateContract reject selling/contracting non-empty ships or items in ship cargo
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_009_910,
            eve_db::InventoryItemRecord {
                item_id: 100_009_910,
                type_id: 587, // Spare Rifter in Jita 4-4 Hangar
                owner_id: 90_000_002,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
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
        snap.items.insert(
            100_009_911,
            eve_db::InventoryItemRecord {
                item_id: 100_009_911,
                type_id: 1248, // Overdrive Injector System II fitted on spare Rifter 100_009_910
                owner_id: 90_000_002,
                location_id: 100_009_910,
                flag: eve_db::InventoryFlag::LoSlot0,
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
    });
    let repack_non_empty = ctx
        .dispatch_rpc(
            &mut s2,
            "repackagingSvc",
            None,
            "RepackageItems",
            &[PyRep::Integer(100_009_910)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        repack_non_empty.unwrap_err().code,
        "CannotRepackageNonEmptyItem"
    );
    let contract_non_empty = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::String("ItemExchange".into()),
                PyRep::Integer(60_003_760),
                PyRep::Integer(60_003_760),
                PyRep::Real(100_000.0),
                PyRep::Real(0.0),
                PyRep::Integer(100_009_910),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        contract_non_empty.unwrap_err().code,
        "CannotContractNonEmptyContainer"
    );

    // Unfitting 100_009_911 back to Hangar allows 100_009_910 to be repackaged
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.items.get_mut(&100_009_911) {
            m.location_id = 60_003_760;
            m.flag = eve_db::InventoryFlag::Hangar;
        }
    });
    ctx.dispatch_rpc(
        &mut s2,
        "repackagingSvc",
        None,
        "RepackageItems",
        &[PyRep::Integer(100_009_910)],
        &kw,
        None,
    )
    .await
    .expect("Repackage empty spare Rifter 100_009_910");

    // R13-P1-1: Missing skill prerequisites reject module fitting (`FittingValidationFailed`)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sk) = snap.skills.get_mut(&90_000_002) {
            sk.remove(&3452); // Remove Acceleration Control IV (required for 1MN Afterburner II 438)
        }
    });
    ctx.invalidate_ship_fitting(100_000_002);
    let missing_skill_fit = ctx
        .dispatch_rpc(
            &mut s2,
            "invbroker",
            None,
            "MoveItem",
            &[
                PyRep::Integer(100_009_901),
                PyRep::Integer(100_000_002),
                PyRep::Integer(19),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(missing_skill_fit.unwrap_err().code, "FittingValidationFailed");

    // Restore Acceleration Control IV (3452: 4) and verify 1MN Afterburner II (438) fits into MedSlot0 (19)
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_002).or_default().insert(
            3452,
            eve_db::TrainedSkillRecord {
                type_id: 3452,
                level: 4,
                skill_points: 181_020.0,
            },
        );
    });
    ctx.invalidate_ship_fitting(100_000_002);
    ctx.dispatch_rpc(
        &mut s2,
        "invbroker",
        None,
        "MoveItem",
        &[
            PyRep::Integer(100_009_901),
            PyRep::Integer(100_000_002),
            PyRep::Integer(19),
        ],
        &kw,
        None,
    )
    .await
    .expect("Fit 1MN Afterburner II after training Acceleration Control IV");

    // R13-P1-2: Applying a mutaplasmid to a multi-unit stack (quantity 5) decrements the stack to 4 and creates a new singleton Abyssal item
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_009_920,
            eve_db::InventoryItemRecord {
                item_id: 100_009_920,
                type_id: 438, // 5x repackaged 1MN Afterburner II stack
                owner_id: 90_000_002,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 5,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            100_009_921,
            eve_db::InventoryItemRecord {
                item_id: 100_009_921,
                type_id: 47702, // Decayed 1MN Afterburner Mutaplasmid
                owner_id: 90_000_002,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
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
    let muta_stack_res = ctx
        .dispatch_rpc(
            &mut s2,
            "dynamicItemService",
            None,
            "CreateDynamicItem",
            &[PyRep::Integer(47702), PyRep::Integer(100_009_920)],
            &kw,
            None,
        )
        .await
        .expect("CreateDynamicItem on 5x stack");
    let PyRep::KeyVal(ms_kv) = muta_stack_res else {
        panic!("Expected CreateDynamicItem KeyVal");
    };
    let split_abyssal_id = ms_kv.get("itemID").unwrap().as_i64().unwrap();
    assert_ne!(split_abyssal_id, 100_009_920);
    let orig_stack = ctx.db.get_item(100_009_920).unwrap();
    assert_eq!(orig_stack.type_id, 438);
    assert_eq!(orig_stack.quantity, 4);
    assert!(!orig_stack.singleton);
    let split_abyssal = ctx.db.get_item(split_abyssal_id).unwrap();
    assert_eq!(split_abyssal.type_id, 47740);
    assert_eq!(split_abyssal.quantity, 1);
    assert!(split_abyssal.singleton);
    // R22-P1-4: Verify rolled Abyssal attributes lie within [min_mult * base, max_mult * base] for mutaplasmid 47702 on type 438
    let base_438 = &ctx.sde.types.get(&438).unwrap().default_attributes;
    for (attr_id, min_m, max_m) in [
        (6_u16, 0.85, 1.20),
        (20_u16, 0.90, 1.15),
        (30_u16, 0.85, 1.25),
        (50_u16, 0.80, 1.30),
    ] {
        let base_val = base_438[&attr_id];
        let rolled_val = split_abyssal.dynamic_attributes[&attr_id];
        assert!(
            rolled_val >= base_val * min_m - 1e-6 && rolled_val <= base_val * max_m + 1e-6,
            "Abyssal attr {attr_id} rolled {rolled_val} outside [{}, {}]",
            base_val * min_m,
            base_val * max_m
        );
    }

    // R13-P1-3: SaveFitting validates and persists caller-supplied description and module list, and rejects invalid module/hull combinations
    let bad_save_fit = ctx
        .dispatch_rpc(
            &mut s2,
            "charFittingMgr",
            None,
            "SaveFitting",
            &[
                PyRep::String("Invalid Trig Rifter".into()),
                PyRep::Integer(587),
                PyRep::String("Should fail".into()),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(47936),
                    PyRep::Integer(27),
                    PyRep::Integer(1),
                ])]),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(bad_save_fit.unwrap_err().code, "InvalidFittingModule");

    let saved_fid = ctx
        .dispatch_rpc(
            &mut s2,
            "charFittingMgr",
            None,
            "SaveFitting",
            &[
                PyRep::String("Custom Rifter Tackler".into()),
                PyRep::Integer(587),
                PyRep::String("Fast tackle with autocannons and overdrive".into()),
                PyRep::List(vec![
                    PyRep::Tuple(vec![
                        PyRep::Integer(2889),
                        PyRep::Integer(27),
                        PyRep::Integer(1),
                    ]),
                    PyRep::Tuple(vec![
                        PyRep::Integer(438),
                        PyRep::Integer(19),
                        PyRep::Integer(1),
                    ]),
                    PyRep::Tuple(vec![
                        PyRep::Integer(1248),
                        PyRep::Integer(11),
                        PyRep::Integer(1),
                    ]),
                ]),
            ],
            &kw,
            None,
        )
        .await
        .expect("SaveFitting with custom modules and description")
        .as_i64()
        .unwrap();
    let saved_rec = ctx
        .db
        .with_snapshot(|snap| snap.saved_fittings.get(&saved_fid).cloned())
        .unwrap();
    assert_eq!(
        saved_rec.description,
        "Fast tackle with autocannons and overdrive"
    );
    assert_eq!(
        saved_rec.items,
        vec![
            (2889, InventoryFlag::HiSlot0, 1),
            (438, InventoryFlag::MedSlot0, 1),
            (1248, InventoryFlag::LoSlot0, 1),
        ]
    );

    // R13-P1-4: While docked at Jita 4-4 (60_003_760), ship.Board rejects boarding an owned ship floating in space in Jita (30_000_142)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_009_930,
            eve_db::InventoryItemRecord {
                item_id: 100_009_930,
                type_id: 587, // Assembled Rifter floating in space in Jita (30_000_142)
                owner_id: 90_000_002,
                location_id: 30_000_142,
                flag: eve_db::InventoryFlag::None,
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
    });
    let board_space_while_docked = ctx
        .dispatch_rpc(
            &mut s2,
            "ship",
            None,
            "Board",
            &[PyRep::Integer(100_009_930)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        board_space_while_docked.unwrap_err().code,
        "ShipNotBoardable"
    );

    // R14-P1-2 / R17-P1-1 / R18-P1-2: Weapon modules (2889, 2410, 2420, 54800, 47936, 84010) have no fabricated damage (114, 116, 117, 118) attributes,
    // and missile launchers/charges (2410, 2420, 211, 2510) have no fabricated chargeSize (128) attribute
    let torp_attrs = &ctx.sde.types.get(&2420).unwrap().default_attributes;
    let lml_attrs = &ctx.sde.types.get(&2410).unwrap().default_attributes;
    for attr_id in 1298..=1301 {
        assert!(
            !torp_attrs.contains_key(&attr_id),
            "Torpedo Launcher II (2420) must not have fabricated canFitShipGroup attr {attr_id}"
        );
    }
    for weapon_type_id in [2889_u32, 2410, 2420, 54800, 47936, 84010] {
        let w_attrs = &ctx.sde.types.get(&weapon_type_id).unwrap().default_attributes;
        for attr_id in [114_u16, 116, 117, 118] {
            assert!(
                !w_attrs.contains_key(&attr_id),
                "Weapon module {weapon_type_id} must not have damage attr {attr_id}"
            );
        }
    }
    for missile_type_id in [2410_u32, 2420, 211, 2510] {
        let m_attrs = &ctx.sde.types.get(&missile_type_id).unwrap().default_attributes;
        assert!(
            !m_attrs.contains_key(&128),
            "Missile launcher/charge {missile_type_id} must not have chargeSize (128)"
        );
    }
    assert_eq!(torp_attrs.get(&51).copied(), Some(10500.0));
    assert_eq!(torp_attrs.get(&604).copied(), Some(89.0));
    assert_eq!(lml_attrs.get(&51).copied(), Some(12000.0));
    assert_eq!(lml_attrs.get(&604).copied(), Some(384.0));
    let inferno_torp_attrs = &ctx.sde.types.get(&2510).unwrap().default_attributes;
    assert_eq!(inferno_torp_attrs.get(&118).copied(), Some(450.0));
    assert_eq!(inferno_torp_attrs.get(&653).copied(), Some(78.0));
    assert_eq!(inferno_torp_attrs.get(&654).copied(), Some(405.0));
    assert_eq!(inferno_torp_attrs.get(&1353).copied(), Some(0.944));
    let inferno_lml_attrs = &ctx.sde.types.get(&211).unwrap().default_attributes;
    assert_eq!(inferno_lml_attrs.get(&118).copied(), Some(83.0));
    assert_eq!(inferno_lml_attrs.get(&653).copied(), Some(170.0));
    assert_eq!(inferno_lml_attrs.get(&654).copied(), Some(40.0));
    assert_eq!(inferno_lml_attrs.get(&1353).copied(), Some(0.604));
    let rf_pp_attrs = &ctx.sde.types.get(&22989).unwrap().default_attributes;
    assert_eq!(rf_pp_attrs.get(&117).copied(), Some(2.3));
    assert_eq!(rf_pp_attrs.get(&118).copied(), Some(11.5));
    assert_eq!(rf_pp_attrs.get(&128).copied(), Some(1.0));
    assert_eq!(rf_pp_attrs.get(&137).copied(), Some(55.0));
    let vorton_attrs = &ctx.sde.types.get(&54800).unwrap().default_attributes;
    let breacher_attrs = &ctx.sde.types.get(&84010).unwrap().default_attributes;
    assert!(!vorton_attrs.contains_key(&1298) && !vorton_attrs.contains_key(&1302));
    assert!(!breacher_attrs.contains_key(&1298) && !breacher_attrs.contains_key(&1302));
    assert_eq!(vorton_attrs.get(&51).copied(), Some(6000.0));
    assert_eq!(vorton_attrs.get(&54).copied(), Some(22460.0));
    assert_eq!(vorton_attrs.get(&64).copied(), Some(1.0));
    assert_eq!(vorton_attrs.get(&604).copied(), Some(4061.0));
    assert_eq!(vorton_attrs.get(&653).copied(), Some(190.0));
    assert_eq!(vorton_attrs.get(&654).copied(), Some(43.0));
    assert_eq!(vorton_attrs.get(&1353).copied(), Some(0.40));

    // R18-P1-1: Attempting to load an incompatible charge (Inferno Light Missile 211) into 200mm AutoCannon II (2889) is rejected with IncompatibleCharge
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_009_940,
            eve_db::InventoryItemRecord {
                item_id: 100_009_940,
                type_id: 211,
                owner_id: 90_000_002,
                location_id: 100_000_002,
                flag: InventoryFlag::Cargo,
                quantity: 100,
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
    let incomp_load = ctx
        .dispatch_rpc(
            &mut s2,
            "dogmaIM",
            None,
            "LoadAmmo",
            &[
                PyRep::Integer(100_000_002),
                PyRep::Integer(27),
                PyRep::Integer(100_009_940),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(incomp_load.unwrap_err().code, "IncompatibleAmmo");

    // R14-P1-1: SaveSkillQueue rejects skills with unmet prerequisites (Torpedo Specialization 20212 requires Torpedoes 3325 V)
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_002).or_default().remove(&3325);
    });
    let unmet_prereq_queue = ctx
        .dispatch_rpc(
            &mut s2,
            "skillMgr2",
            None,
            "SaveSkillQueue",
            &[PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(20212),
                PyRep::Integer(1),
            ])])],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        unmet_prereq_queue.unwrap_err().code,
        "SkillPrerequisiteNotMet"
    );

    // R14-P1-1: ApplyFreeSkillPoints partial application does not forge trained skill level before threshold SP is reached
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.free_skill_points = 500.0;
        }
        snap.skills.entry(90_000_002).or_default().remove(&3449);
        snap.skill_queues.entry(90_000_002).or_default().clear();
    });
    // Queue Navigation (3449, rank 1, requires Spaceship Command 3327 I which 90_000_002 has at level 3) to level 1 (needs 250 SP)
    ctx.dispatch_rpc(
        &mut s2,
        "skillMgr2",
        None,
        "SaveSkillQueue",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(3449),
            PyRep::Integer(1),
        ])])],
        &kw,
        None,
    )
    .await
    .expect("Queue Navigation I");
    let partial_sp_res = ctx
        .dispatch_rpc(
            &mut s2,
            "skillMgr2",
            None,
            "ApplyFreeSkillPoints",
            &[PyRep::Real(10.0)],
            &kw,
            None,
        )
        .await
        .expect("Apply 10 partial free SP");
    assert!(partial_sp_res.as_f64().is_some());
    let nav_lvl_after_partial = ctx
        .db
        .with_snapshot(|snap| {
            snap.skills
                .get(&90_000_002)
                .and_then(|m| m.get(&3449))
                .map(|s| (s.level, s.skill_points))
        })
        .unwrap();
    assert_eq!(nav_lvl_after_partial.0, 0);
    assert!((nav_lvl_after_partial.1 - 10.0).abs() < 1e-6);

    // Applying the remaining 240 SP reaches 250 SP and advances Navigation (3449) to level 1
    let full_sp_res = ctx
        .dispatch_rpc(
            &mut s2,
            "skillMgr2",
            None,
            "ApplyFreeSkillPoints",
            &[PyRep::Real(240.0)],
            &kw,
            None,
        )
        .await
        .expect("Apply remaining 240 free SP");
    assert!(full_sp_res.as_f64().is_some());
    let nav_lvl_after_full = ctx
        .db
        .with_snapshot(|snap| {
            snap.skills
                .get(&90_000_002)
                .and_then(|m| m.get(&3449))
                .map(|s| s.level)
        })
        .unwrap();
    assert_eq!(nav_lvl_after_full, 1);

    // R14-P1-4: marketProxy.GetCharOrders returns only the caller's own orders (not NPC orders),
    // and marketProxy.BuyMultipleItems requires a non-empty item list and executes multi-item purchases atomically
    let char_orders_before = ctx
        .dispatch_rpc(&mut s2, "marketProxy", None, "GetCharOrders", &[], &kw, None)
        .await
        .expect("GetCharOrders");
    let PyRep::Rowset { rows: co_lines, .. } = char_orders_before else {
        panic!("Expected Rowset from GetCharOrders");
    };
    assert!(
        co_lines.is_empty(),
        "Character 90_000_002 has no personal market orders initially (NPC orders must be excluded)"
    );

    let empty_multi_buy = ctx
        .dispatch_rpc(&mut s2, "marketProxy", None, "BuyMultipleItems", &[], &kw, None)
        .await;
    assert_eq!(empty_multi_buy.unwrap_err().code, "InvalidBuyMultipleItems");

    let multi_buy_res = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "BuyMultipleItems",
            &[PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(34),
                PyRep::Integer(10),
                PyRep::Real(10.0),
            ])])],
            &kw,
            None,
        )
        .await
        .expect("BuyMultipleItems 10x Tritanium");
    let PyRep::KeyVal(mb_kv) = multi_buy_res else {
        panic!("Expected KeyVal from BuyMultipleItems");
    };
    assert_eq!(mb_kv.get("purchasedCount"), Some(&PyRep::Integer(1)));
    assert_eq!(mb_kv.get("totalQuantity"), Some(&PyRep::Integer(10)));
    assert_eq!(mb_kv.get("totalCostISK"), Some(&PyRep::Real(50.0)));

    // R15-P1-1: marketProxy.PlaceCharOrder rejects placing an order at a remote station in the same region (`StationMismatch`)
    // and rejects placing an order while undocked in space (`NotDockedAtStation`)
    let remote_station_order = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "PlaceCharOrder",
            &[
                PyRep::Integer(60_003_766), // Perimeter station in same region 10000002 while s2 is docked at Jita 60003760
                PyRep::Integer(34),
                PyRep::Real(5.0),
                PyRep::Integer(10),
                PyRep::Bool(true),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(remote_station_order.unwrap_err().code, "StationMismatch");

    // s1 is currently in space in Jita (30_000_142) after undocking earlier in this test
    let in_space_order = ctx
        .dispatch_rpc(
            &mut s1,
            "marketProxy",
            None,
            "PlaceCharOrder",
            &[
                PyRep::Integer(60_003_760),
                PyRep::Integer(34),
                PyRep::Real(5.0),
                PyRep::Integer(10),
                PyRep::Bool(true),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(in_space_order.unwrap_err().code, "NotDockedAtStation");

    // R20-P1-3: Verify PlaceCharOrder resolves station-specific owner_corporation_id (1000035) and owner_faction_id (500001)
    // standings separately for Jita 4-4 (60003760) instead of passing corp standing as both faction and corp standing
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.balance = eve_db::IskCents::from_isk(100_000);
            ch.standings.clear();
            // Caldari State (500001) = 5.0, Caldari Navy (1000035) = 2.0
            ch.standings.insert(500_001, 5.0);
            ch.standings.insert(1_000_035, 2.0);
        }
    });
    // Place a Resting Bid at 1.0 ISK x 10_000 = 10,000.00 ISK
    // With Broker Relations IV (level 4), faction standing 5.0, corp standing 2.0:
    // rate = 0.03 - 0.003*4 - 0.0003*5.0 - 0.0002*2.0 = 0.018 - 0.0015 - 0.0004 = 0.0161 (1.61%)
    // Broker fee on 10,000 ISK = 161.00 ISK (whereas using corp standing 2.0 for both would yield 0.0170 -> 170.00 ISK)
    let _ = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "PlaceCharOrder",
            &[
                PyRep::Integer(60_003_760),
                PyRep::Integer(34),
                PyRep::Real(1.0),
                PyRep::Integer(10_000),
                PyRep::Bool(true),
            ],
            &kw,
            None,
        )
        .await
        .expect("PlaceCharOrder resting bid with separate faction and corp standings");
    let bal_after_bid = ctx
        .db
        .with_snapshot(|snap| snap.characters[&90_000_002].balance.0);
    // 100,000.00 ISK - 10,000.00 ISK escrow - 161.00 ISK broker fee = 89,839.00 ISK (8_983_900 cents)
    assert_eq!(bal_after_bid, 8_983_900);

    // R19-P1-1 & R22-P1-1: Vorton Projector (Small Vorton Projector I 54800 loaded with GalvaSurge Condenser Pack S 54807)
    // uses missile_damage_factor (explosion radius 654 = 43.0m, explosion velocity 653 = 190.0 m/s, DRF 1353 = 0.40),
    // never rolls turret wrecking hits, and arcs up to 9 additional targets (2104 = 9.0) strictly within 10,000m of the primary target with no secondary hopping
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_009_950,
            eve_db::InventoryItemRecord {
                item_id: 100_009_950,
                type_id: 587, // Spare Rifter frigate hull
                owner_id: 90_000_002,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
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
        snap.items.insert(
            100_009_951,
            eve_db::InventoryItemRecord {
                item_id: 100_009_951,
                type_id: 54800, // Small Vorton Projector I
                owner_id: 90_000_002,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
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
        snap.items.insert(
            100_009_952,
            eve_db::InventoryItemRecord {
                item_id: 100_009_952,
                type_id: 54807, // GalvaSurge Condenser Pack S (116 EM + 17 Kin = 133 base damage)
                owner_id: 90_000_002,
                location_id: 100_009_950,
                flag: InventoryFlag::Cargo,
                quantity: 200,
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
    ctx.dispatch_rpc(
        &mut s2,
        "ship",
        None,
        "Board",
        &[PyRep::Integer(100_009_950)],
        &kw,
        None,
    )
    .await
    .expect("Board Skybreaker 100_009_950");
    ctx.dispatch_rpc(
        &mut s2,
        "invbroker",
        None,
        "MoveItem",
        &[
            PyRep::Integer(100_009_951),
            PyRep::Integer(100_009_950),
            PyRep::Integer(27),
        ],
        &kw,
        None,
    )
    .await
    .expect("Fit Small Vorton Projector I onto Skybreaker");
    ctx.dispatch_rpc(
        &mut s2,
        "dogmaIM",
        None,
        "LoadAmmo",
        &[
            PyRep::Integer(100_009_950),
            PyRep::Integer(27),
            PyRep::Integer(100_009_952),
        ],
        &kw,
        None,
    )
    .await
    .expect("Load GalvaSurge Condenser Pack S into Small Vorton Projector I");
    ctx.dispatch_rpc(&mut s2, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock Skybreaker");
    for _ in 0..7 {
        let _ = jita_handle.step_manual(1.0).await.unwrap();
    }
    let (_, sky_ball) = jita_handle
        .get_ball_snapshot(100_009_950)
        .await
        .unwrap()
        .unwrap();
    for (bid, offset_x) in [
        (700_009_950_i64, 5_000.0),  // Primary target at 5 km (<= 22.46 km optimal)
        (700_009_951_i64, 13_000.0), // Secondary target 8 km from primary (<= 10 km arc radius)
        (700_009_952_i64, 17_000.0), // Out-of-arc target 12 km from primary (> 10 km arc radius, even though 4 km <= 10 km from secondary 700_009_951)
    ] {
        let _ = jita_handle
            .spawn_ball_with_combat(
                eve_destiny::Ball {
                    id: bid,
                    type_id: 11001,
                    position: sky_ball.position + glam::DVec3::new(offset_x, 0.0, 0.0),
                    velocity: glam::DVec3::new(380.0, 0.0, 0.0),
                    max_velocity: 400.0,
                    inertia: 2.0,
                    mass: 1_000_000.0,
                    radius: 35.0,
                    speed_fraction: 0.95,
                    mode: eve_destiny::MovementMode::Stop,
                },
                eve_destiny::CombatState {
                    shield_hp: 35.0,
                    max_shield_hp: 35.0,
                    armor_hp: 35.0,
                    max_armor_hp: 35.0,
                    structure_hp: 35.0,
                    max_structure_hp: 35.0,
                    signature_radius: 35.0,
                    ..eve_destiny::CombatState::default()
                },
            )
            .await;
    }
    let vorton_res = ctx
        .dispatch_rpc(
            &mut s2,
            "dogmaIM",
            None,
            "FireWeapon",
            &[PyRep::Integer(700_009_950)],
            &kw,
            None,
        )
        .await
        .expect("FireWeapon Small Vorton Projector I shot 1");
    let PyRep::KeyVal(v_kv) = vorton_res else {
        panic!("Expected KeyVal from Vorton FireWeapon");
    };
    assert_eq!(
        v_kv.get("weaponKind").and_then(|v| v.as_str()),
        Some("VortonProjector")
    );
    assert_eq!(v_kv.get("isWreckingShot"), Some(&PyRep::Bool(false)));
    assert_eq!(v_kv.get("secondaryTargetsHit"), Some(&PyRep::Integer(1)));
    assert_eq!(v_kv.get("destroyed"), Some(&PyRep::Bool(false)));
    let expected_vorton_dmg = 133.0 * ((35.0_f64 * 190.0) / (43.0 * 380.0)).powf(0.40);
    let actual_vorton_dmg = v_kv.get("appliedDamage").and_then(|v| v.as_f64()).unwrap();
    assert!(
        (actual_vorton_dmg - expected_vorton_dmg).abs() < 1e-4 && actual_vorton_dmg < 133.0,
        "Vorton appliedDamage {actual_vorton_dmg} must match missile_damage_factor {expected_vorton_dmg}"
    );
    // Secondary target (105 total HP) survived shot 1 because missile_damage_factor reduced 133.0 -> 92.83 (< 105.0)
    assert!(
        jita_handle
            .get_ball_snapshot(700_009_951)
            .await
            .unwrap()
            .is_some()
    );
    // Step 7s for 6.0s Vorton cycle and fire shot 2: both primary (700_009_950) and secondary (700_009_951) are destroyed, while 700_009_952 (12 km > 10 km from primary, 4 km from secondary) is untouched
    for _ in 0..7 {
        let _ = jita_handle.step_manual(1.0).await.unwrap();
    }
    let vorton_res2 = ctx
        .dispatch_rpc(
            &mut s2,
            "dogmaIM",
            None,
            "FireWeapon",
            &[PyRep::Integer(700_009_950)],
            &kw,
            None,
        )
        .await
        .expect("FireWeapon Small Vorton Projector I shot 2");
    let PyRep::KeyVal(v_kv2) = vorton_res2 else {
        panic!("Expected KeyVal from Vorton FireWeapon shot 2");
    };
    assert_eq!(v_kv2.get("destroyed"), Some(&PyRep::Bool(true)));
    assert!(
        jita_handle
            .get_ball_snapshot(700_009_951)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        jita_handle
            .get_ball_snapshot(700_009_952)
            .await
            .unwrap()
            .is_some()
    );

    // R21-P1-1: planetMgr.CreateColony gates max colonies per character on 1 + Interplanetary Consolidation (2495) trained level
    // Character 90_000_002 has Interplanetary Consolidation untrained (0 -> max 1 colony); establishing 1st colony on Jita 40009082 succeeds,
    // and attempting a 2nd colony on Jita 40009077 fails with MaxColoniesReached
    ctx.dispatch_rpc(
        &mut s2,
        "planetMgr",
        None,
        "CreateColony",
        &[PyRep::Integer(40_009_082)],
        &kw,
        None,
    )
    .await
    .expect("1st colony on Jita 40009082 with Interplanetary Consolidation 0");
    let second_colony_untrained = ctx
        .dispatch_rpc(
            &mut s2,
            "planetMgr",
            None,
            "CreateColony",
            &[PyRep::Integer(40_009_077)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        second_colony_untrained.unwrap_err().code,
        "MaxColoniesReached"
    );

    // R21-P1-4 & R22-P1-2: Dock s2 back at Jita 4-4 (60003760) and verify PlaceCharOrder returns CannotSellAssembledItem, CannotSellItemWithContents,
    // and charges exact integer ppm sales tax from the seller's own Accounting (16622) skill level (levels 0 and 4)
    let (_, sky_ball_now) = jita_handle
        .get_ball_snapshot(100_009_950)
        .await
        .unwrap()
        .unwrap();
    let jita_station_pos = glam::DVec3::new(-107_303_362_560.0, -18_744_975_360.0, 436_489_052_160.0);
    if sky_ball_now.position.distance(jita_station_pos) > 2_000.0 {
        let _ = jita_handle
            .spawn_ball(eve_destiny::Ball {
                id: 100_009_950,
                position: jita_station_pos + glam::DVec3::new(500.0, 0.0, 0.0),
                ..sky_ball_now
            })
            .await;
    }
    ctx.dispatch_rpc(
        &mut s2,
        "stationSvc",
        None,
        "Dock",
        &[PyRep::Integer(60_003_760)],
        &kw,
        None,
    )
    .await
    .expect("Dock s2 back at Jita 4-4");

    ctx.db.with_mut_snapshot(|snap| {
        // Ensure 100_009_911 (type 1248 Overdrive Injector System II) in Jita 4-4 hangar is assembled (singleton = true) and empty
        if let Some(m) = snap.items.get_mut(&100_009_911) {
            m.location_id = 60_003_760;
            m.flag = InventoryFlag::Hangar;
            m.singleton = true;
        }
    });
    let sell_assembled = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "PlaceCharOrder",
            &[
                PyRep::Integer(60_003_760),
                PyRep::Integer(1248),
                PyRep::Real(250_000.0),
                PyRep::Integer(1),
                PyRep::Bool(false),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(sell_assembled.unwrap_err().code, "CannotSellAssembledItem");

    // 100_000_002 (type 587 Rifter) is in Jita 4-4 hangar and contains fitted modules (100_009_901..903)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&100_009_910);
    });
    let sell_with_contents = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "PlaceCharOrder",
            &[
                PyRep::Integer(60_003_760),
                PyRep::Integer(587),
                PyRep::Real(250_000.0),
                PyRep::Integer(1),
                PyRep::Bool(false),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        sell_with_contents.unwrap_err().code,
        "CannotSellItemWithContents"
    );

    // R22-P1-2: Place a resting player bid order for 2x Overdrive Injector System II (1248) at 500,000.00 ISK (50_000_000 cents, above the 329,000.00 ISK NPC bid),
    // repackage 100_009_911 into a 2-unit stack, and verify PlaceCharOrder sell fills charge exact integer ppm salesTaxISK for Accounting level 0 (75,000 ppm = 37,500.00 ISK) and level 4 (42,000 ppm = 21,000.00 ISK)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.items.get_mut(&100_009_911) {
            m.singleton = false;
            m.quantity = 2;
        }
        if let Some(sk) = snap.skills.get_mut(&90_000_002) {
            sk.remove(&16622); // Accounting 0
        }
        if let Some(c) = snap.characters.get_mut(&90_000_002) {
            c.balance = eve_db::IskCents(10_000_000); // 100,000.00 ISK
        }
        snap.market_orders.insert(
            777_000_124,
            eve_db::MarketOrderRecord {
                order_id: 777_000_124,
                owner_id: 90_000_001,
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 1248,
                price: eve_db::IskCents(50_000_000), // 500,000.00 ISK per unit
                vol_entered: 2,
                vol_remaining: 2,
                min_volume: 1,
                is_bid: true,
                is_npc: false,
                escrow: eve_db::IskCents(100_000_000),
            },
        );
    });
    let sell_acct_0 = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "PlaceCharOrder",
            &[
                PyRep::Integer(60_003_760),
                PyRep::Integer(1248),
                PyRep::Real(500_000.0),
                PyRep::Integer(1),
                PyRep::Bool(false),
            ],
            &kw,
            None,
        )
        .await
        .expect("Sell 1x 1248 at Accounting level 0");
    let PyRep::KeyVal(sa0_kv) = sell_acct_0 else {
        panic!("Expected KeyVal from PlaceCharOrder sell at Accounting 0");
    };
    assert_eq!(sa0_kv.get("filledImmediately"), Some(&PyRep::Bool(true)));
    assert_eq!(sa0_kv.get("salesTaxISK"), Some(&PyRep::Real(37_500.0)));
    let bal_after_acct_0 = ctx
        .db
        .with_snapshot(|snap| snap.characters[&90_000_002].balance.0);
    // 100,000.00 ISK - 8,050.00 ISK broker fee (16,100 ppm) + (500,000.00 ISK - 37,500.00 ISK sales tax) = 554,450.00 ISK (55_445_000 cents)
    assert_eq!(bal_after_acct_0, 55_445_000);

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_002).or_default().insert(
            16622,
            eve_db::TrainedSkillRecord {
                type_id: 16622,
                level: 4,
                skill_points: 135_765.0,
            },
        );
    });
    let sell_acct_4 = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "PlaceCharOrder",
            &[
                PyRep::Integer(60_003_760),
                PyRep::Integer(1248),
                PyRep::Real(500_000.0),
                PyRep::Integer(1),
                PyRep::Bool(false),
            ],
            &kw,
            None,
        )
        .await
        .expect("Sell 1x 1248 at Accounting level 4");
    let PyRep::KeyVal(sa4_kv) = sell_acct_4 else {
        panic!("Expected KeyVal from PlaceCharOrder sell at Accounting 4");
    };
    assert_eq!(sa4_kv.get("filledImmediately"), Some(&PyRep::Bool(true)));
    assert_eq!(sa4_kv.get("salesTaxISK"), Some(&PyRep::Real(21_000.0)));
    let bal_after_acct_4 = ctx
        .db
        .with_snapshot(|snap| snap.characters[&90_000_002].balance.0);
    // 554,450.00 ISK - 8,050.00 ISK broker fee (16,100 ppm) + (500,000.00 ISK - 21,000.00 ISK sales tax) = 1,025,400.00 ISK (102_540_000 cents)
    assert_eq!(bal_after_acct_4, 102_540_000);

    // R23-P1-1: standingMgr.GetStandingTransactions records only actual standing deltas (no Decline penalty, exact derived Complete delta)
    fn extract_rowset_rows(rep: &PyRep) -> &Vec<Vec<PyRep>> {
        let PyRep::Rowset { rows, .. } = rep else {
            panic!("Expected PyRep::Rowset, got {:?}", rep);
        };
        rows
    }
    let st_tx_initial = ctx
        .dispatch_rpc(
            &mut s2,
            "standingMgr",
            None,
            "GetStandingTransactions",
            &[PyRep::Integer(1_000_035)],
            &kw,
            None,
        )
        .await
        .expect("GetStandingTransactions initial for s2");
    let init_rows = extract_rowset_rows(&st_tx_initial);
    assert_eq!(init_rows.len(), 1);
    assert_eq!(init_rows[0][3], PyRep::Real(0.125));
    assert_eq!(init_rows[0][4], PyRep::Integer(85));

    ctx.dispatch_rpc(
        &mut s2,
        "agentMgr",
        None,
        "RequestMission",
        &[PyRep::Integer(1002)],
        &kw,
        None,
    )
    .await
    .expect("RequestMission 1002 for s2 decline");
    ctx.dispatch_rpc(
        &mut s2,
        "agentMgr",
        None,
        "RemoveOfferFromJournal",
        &[PyRep::Integer(1002)],
        &kw,
        None,
    )
    .await
    .expect("RemoveOfferFromJournal 1002 for s2 decline");
    let st_tx_after_decline = ctx
        .dispatch_rpc(
            &mut s2,
            "standingMgr",
            None,
            "GetStandingTransactions",
            &[PyRep::Integer(1_000_035)],
            &kw,
            None,
        )
        .await
        .expect("GetStandingTransactions after decline");
    assert_eq!(extract_rowset_rows(&st_tx_after_decline).len(), 1);

    ctx.dispatch_rpc(
        &mut s2,
        "agentMgr",
        None,
        "RequestMission",
        &[PyRep::Integer(1003)],
        &kw,
        None,
    )
    .await
    .expect("RequestMission 1003 for s2 quit");
    ctx.dispatch_rpc(
        &mut s2,
        "agentMgr",
        None,
        "AcceptMission",
        &[PyRep::Integer(1003)],
        &kw,
        None,
    )
    .await
    .expect("AcceptMission 1003 for s2 quit");
    ctx.dispatch_rpc(
        &mut s2,
        "agentMgr",
        None,
        "QuitMission",
        &[PyRep::Integer(1003)],
        &kw,
        None,
    )
    .await
    .expect("QuitMission 1003 for s2");
    let st_tx_after_quit = ctx
        .dispatch_rpc(
            &mut s2,
            "standingMgr",
            None,
            "GetStandingTransactions",
            &[PyRep::Integer(1_000_035)],
            &kw,
            None,
        )
        .await
        .expect("GetStandingTransactions after quit");
    let quit_rows = extract_rowset_rows(&st_tx_after_quit);
    assert_eq!(quit_rows.len(), 2);
    assert_eq!(quit_rows[1][3], PyRep::Real(-0.1));
    assert_eq!(quit_rows[1][4], PyRep::Integer(83));

    // R23-P1-2: jumpCloneSvc.GetPriceForClone matches flat 900,000 ISK charged by InstallCloneInStation
    let jc_quote = ctx
        .dispatch_rpc(&mut s2, "jumpCloneSvc", None, "GetPriceForClone", &[], &kw, None)
        .await
        .expect("GetPriceForClone");
    assert_eq!(jc_quote, PyRep::Real(900_000.0));

    // R23-P1-3: taleMgr.GetGlobalWorldEventTales and get_active_tales_by_template return UnimplementedRPC
    let tale_err_1 = ctx
        .dispatch_rpc(
            &mut s2,
            "taleMgr",
            None,
            "GetGlobalWorldEventTales",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("GetGlobalWorldEventTales must return UnimplementedRPC");
    assert_eq!(tale_err_1.code, "UnimplementedRPC");
    let tale_err_2 = ctx
        .dispatch_rpc(
            &mut s2,
            "taleMgr",
            None,
            "get_active_tales_by_template",
            &[PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect_err("get_active_tales_by_template must return UnimplementedRPC");
    assert_eq!(tale_err_2.code, "UnimplementedRPC");
}


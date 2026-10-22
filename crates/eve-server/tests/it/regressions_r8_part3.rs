//! Integration regression tests covering Round 8 findings (Part 3).

#![forbid(unsafe_code)]

use eve_db::InventoryFlag;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

pub(crate) async fn run_r8_part3(
    ctx: &ServerContext,
    mut s1: SessionState,
    mut s2: SessionState,
) {
    let kw = HashMap::new();
    let jita_handle = ctx.systems.get_or_spawn(30_000_142);
    // R21-P1-1: planetMgr.UserUpdateNetwork gates max colonies per character on 1 + Interplanetary Consolidation (2495) trained level
    // Character 90_000_002 has Interplanetary Consolidation untrained (0 -> max 1 colony); establishing 1st colony on Jita 40009082 succeeds,
    // and attempting a 2nd colony on Jita 40009077 fails with MaxColoniesReached
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id1 = snap.next_entity_id;
        snap.items.insert(
            id1,
            eve_db::InventoryItemRecord {
                item_id: id1,
                type_id: 2534,
                owner_id: 90_000_002,
                location_id: 100_009_950,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let id2 = snap.next_entity_id;
        snap.items.insert(
            id2,
            eve_db::InventoryItemRecord {
                item_id: id2,
                type_id: 2524,
                owner_id: 90_000_002,
                location_id: 100_009_950,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.dispatch_rpc(
        &mut s2,
        "planetMgr",
        None,
        "UserUpdateNetwork",
        &[
            PyRep::Integer(40_009_082),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::Integer(2534),
                PyRep::Real(0.1),
                PyRep::Real(0.2),
            ])]),
        ],
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
            "UserUpdateNetwork",
            &[
                PyRep::Integer(40_009_077),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(1),
                    PyRep::Integer(1),
                    PyRep::Integer(2524),
                    PyRep::Real(0.1),
                    PyRep::Real(0.2),
                ])]),
            ],
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
    let jita_station_pos =
        glam::DVec3::new(-107_303_362_560.0, -18_744_975_360.0, 436_489_052_160.0);
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
        // Ensure 100_009_911 (type 1236 Overdrive Injector System II) in Jita 4-4 hangar is assembled (singleton = true) and empty
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
            "PlaceMultiSellOrder",
            &[
                PyRep::List(vec![PyRep::keyval([
                    ("stationID", PyRep::Integer(60_003_760)),
                    ("typeID", PyRep::Integer(1236)),
                    ("price", PyRep::Real(250_000.0)),
                    ("quantity", PyRep::Integer(1)),
                ])]),
                PyRep::Bool(false),
                PyRep::Integer(90),
                PyRep::Real(0.0),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(sell_assembled.unwrap_err().code, "CannotSellAssembledItem");

    // 100_000_002 (type 587 Rifter) is in Jita 4-4 hangar and contains fitted modules (100_009_901..903)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&100_009_910);
        snap.items.remove(&100_009_950);
    });
    let sell_with_contents = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "PlaceMultiSellOrder",
            &[
                PyRep::List(vec![PyRep::keyval([
                    ("stationID", PyRep::Integer(60_003_760)),
                    ("typeID", PyRep::Integer(587)),
                    ("price", PyRep::Real(250_000.0)),
                    ("quantity", PyRep::Integer(1)),
                ])]),
                PyRep::Bool(false),
                PyRep::Integer(90),
                PyRep::Real(0.0),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        sell_with_contents.unwrap_err().code,
        "CannotSellItemWithContents"
    );

    // R22-P1-2: Place a resting player bid order for 2x Overdrive Injector System II (1236) at 500,000.00 ISK (50_000_000 cents, above the 329,000.00 ISK NPC bid),
    // repackage 100_009_911 into a 2-unit stack, and verify PlaceMultiSellOrder sell fills charge exact integer ppm salesTaxISK for Accounting level 0 (75,000 ppm = 37,500.00 ISK) and level 4 (42,000 ppm = 21,000.00 ISK)
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
                type_id: 1236,
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
            "PlaceMultiSellOrder",
            &[
                PyRep::List(vec![PyRep::keyval([
                    ("stationID", PyRep::Integer(60_003_760)),
                    ("typeID", PyRep::Integer(1236)),
                    ("price", PyRep::Real(500_000.0)),
                    ("quantity", PyRep::Integer(1)),
                ])]),
                PyRep::Bool(false),
                PyRep::Integer(90),
                PyRep::Real(0.0),
            ],
            &kw,
            None,
        )
        .await
        .expect("Sell 1x 1236 at Accounting level 0");
    let PyRep::KeyVal(sa0_kv) = sell_acct_0 else {
        panic!("Expected KeyVal from PlaceMultiSellOrder sell at Accounting 0");
    };
    assert_eq!(sa0_kv.get("filledImmediately"), Some(&PyRep::Bool(true)));
    assert_eq!(sa0_kv.get("salesTaxISK"), Some(&PyRep::Real(37_500.0)));
    let bal_after_acct_0 = ctx
        .db
        .with_snapshot(|snap| snap.characters[&90_000_002].balance.0);
    // 100,000.00 ISK + (500,000.00 ISK - 37,500.00 ISK sales tax) = 562,500.00 ISK (56_250_000 cents; no broker fee on immediate fill)
    assert_eq!(bal_after_acct_0, 56_250_000);

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
            "PlaceMultiSellOrder",
            &[
                PyRep::List(vec![PyRep::keyval([
                    ("stationID", PyRep::Integer(60_003_760)),
                    ("typeID", PyRep::Integer(1236)),
                    ("price", PyRep::Real(500_000.0)),
                    ("quantity", PyRep::Integer(1)),
                ])]),
                PyRep::Bool(false),
                PyRep::Integer(90),
                PyRep::Real(0.0),
            ],
            &kw,
            None,
        )
        .await
        .expect("Sell 1x 1236 at Accounting level 4");
    let PyRep::KeyVal(sa4_kv) = sell_acct_4 else {
        panic!("Expected KeyVal from PlaceMultiSellOrder sell at Accounting 4");
    };
    assert_eq!(sa4_kv.get("filledImmediately"), Some(&PyRep::Bool(true)));
    assert_eq!(sa4_kv.get("salesTaxISK"), Some(&PyRep::Real(21_000.0)));
    let bal_after_acct_4 = ctx
        .db
        .with_snapshot(|snap| snap.characters[&90_000_002].balance.0);
    // 562,500.00 ISK + (500,000.00 ISK - 21,000.00 ISK sales tax) = 1,041,500.00 ISK (104_150_000 cents; no broker fee on immediate fill)
    assert_eq!(bal_after_acct_4, 104_150_000);

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
            &[PyRep::Integer(1_000_419)],
            &kw,
            None,
        )
        .await
        .expect("GetStandingTransactions initial for s2");
    let init_rows = extract_rowset_rows(&st_tx_initial);
    assert_eq!(init_rows.len(), 1);
    assert_eq!(init_rows[0][3], PyRep::Real(0.15));
    assert_eq!(init_rows[0][4], PyRep::Integer(85));

    // Co-locate s2 (90_000_002) with Caldari Navy (1_000_035) L1 agent 3_012_018 at Maurasi IV - Moon 4 (30_000_144 / 60_003_754)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.solar_system_id = 30_000_144;
            ch.station_id = Some(60_003_754);
        }
    });
    s2.solar_system_id = Some(30_000_144);
    s2.station_id = Some(60_003_754);
    let rm_decline = ctx
        .dispatch_rpc(
            &mut s2,
            "agentMgr",
            None,
            "DoAction",
            &[PyRep::Integer(2), PyRep::Integer(3_012_018)],
            &kw,
            None,
        )
        .await
        .expect("DoAction(RequestMission) for s2 decline");
    let PyRep::KeyVal(rm_dec_kv) = rm_decline else {
        panic!("Expected rm_dec_kv");
    };
    let s2_dec_mid = rm_dec_kv.get("missionID").unwrap().as_i64().unwrap();
    ctx.dispatch_rpc(
        &mut s2,
        "agentMgr",
        None,
        "RemoveOfferFromJournal",
        &[PyRep::Integer(s2_dec_mid)],
        &kw,
        None,
    )
    .await
    .expect("RemoveOfferFromJournal for s2 decline");
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
    assert_eq!(extract_rowset_rows(&st_tx_after_decline).len(), 0);

    let rm_quit = ctx
        .dispatch_rpc(&mut s2, "agentMgr", None, "DoAction", &[PyRep::Integer(2)], &kw, None)
        .await
        .expect("DoAction(RequestMission) for s2 quit");
    let PyRep::KeyVal(rm_quit_kv) = rm_quit else {
        panic!("Expected rm_quit_kv");
    };
    let s2_quit_mid = rm_quit_kv.get("missionID").unwrap().as_i64().unwrap();
    ctx.dispatch_rpc(
        &mut s2,
        "agentMgr",
        None,
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(s2_quit_mid)],
        &kw,
        None,
    )
    .await
    .expect("DoAction(AcceptMission) for s2 quit");
    ctx.dispatch_rpc(
        &mut s2,
        "agentMgr",
        None,
        "DoAction",
        &[PyRep::Integer(11), PyRep::Integer(s2_quit_mid)],
        &kw,
        None,
    )
    .await
    .expect("DoAction(QuitMission) for s2");
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
    assert_eq!(quit_rows.len(), 1);
    assert_eq!(quit_rows[0][3], PyRep::Real(-0.1));
    assert_eq!(quit_rows[0][4], PyRep::Integer(83));

    // R23-P1-2: jumpCloneSvc.GetPriceForClone matches flat 900,000 ISK charged by InstallCloneInStation
    let jc_quote = ctx
        .dispatch_rpc(
            &mut s2,
            "jumpCloneSvc",
            None,
            "GetPriceForClone",
            &[],
            &kw,
            None,
        )
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

    // -------------------------------------------------------------------------
    // Track 2: SDE Loaders & Auditor Fixes
    // -------------------------------------------------------------------------

    // parity: plan:D-2
    // Verify O(1) indexed stations map in SdeRegistry and preserved columns
    assert_eq!(ctx.sde.stations.len(), 5_210);
    let jita_sta = ctx.sde.stations.get(&60_003_760).expect("Jita 4-4 station");
    assert_eq!(jita_sta.station_id, 60_003_760);
    assert_eq!(jita_sta.solar_system_id, 30_000_142);
    assert_eq!(jita_sta.constellation_id, 20_000_020);
    assert_eq!(jita_sta.region_id, 10_000_002);
    assert_eq!(jita_sta.operation_id, 14);
    assert_eq!(jita_sta.reprocessing_efficiency, 0.5);
    assert_eq!(jita_sta.reprocessing_stations_take, 0.05);
    assert!(!jita_sta.is_conquerable);
    assert_eq!(jita_sta.orbit_id, Some(40_009_087));
    assert_eq!(jita_sta.owner_faction_id, Some(500_001));

    // Station without NPC corp faction_id has None owner_faction_id (no hardcoded Caldari 500001 fallback)
    let non_emp_sta = ctx.sde.stations.get(&60_014_839).expect("station 60014839");
    assert_eq!(non_emp_sta.owner_faction_id, None);

    // Non-empire race has None corvette_type_id (no hardcoded Ibis 601 fallback)
    assert_eq!(ctx.sde.races.get(&16).unwrap().corvette_type_id, None);
    assert_eq!(ctx.sde.races.get(&1).unwrap().corvette_type_id, Some(601));

    // RPC GetStation uses O(1) indexed station map
    let gsta = ctx
        .dispatch_rpc(
            &mut s1,
            "station",
            None,
            "GetStation",
            &[PyRep::Integer(60_003_760)],
            &kw,
            None,
        )
        .await
        .expect("station.GetStation via indexed map");
    let PyRep::KeyVal(gsta_kv) = gsta else {
        panic!("Expected gsta_kv");
    };
    assert_eq!(
        gsta_kv.get("stationID").unwrap(),
        &PyRep::Integer(60_003_760)
    );
    assert_eq!(
        gsta_kv.get("solarSystemID").unwrap(),
        &PyRep::Integer(30_000_142)
    );
    assert_eq!(
        gsta_kv.get("stationName").unwrap(),
        &PyRep::String("Jita IV - Moon 4 - Caldari Navy Assembly Plant".into())
    );

    // parity: sde:res:/staticdata/types.fsdbinary
    // Verify InvType full columns preserved in SDE registry
    let rifter_t = ctx.sde.types.get(&587).expect("Rifter type");
    assert_eq!(rifter_t.description_id, Some(93841));
    assert_eq!(rifter_t.icon_id, None);
    assert_eq!(rifter_t.graphic_id, Some(46));
    assert_eq!(rifter_t.sound_id, Some(20078));
    assert_eq!(rifter_t.meta_group_id, Some(1));
    assert_eq!(rifter_t.meta_level, Some(0));
    assert_eq!(rifter_t.tech_level, Some(1));

    let trit_t = ctx.sde.types.get(&34).expect("Tritanium type");
    assert_eq!(trit_t.description_id, Some(87134));
    assert_eq!(trit_t.icon_id, Some(22));
    assert_eq!(trit_t.graphic_id, None);
    assert_eq!(trit_t.sound_id, None);
    assert_eq!(trit_t.meta_group_id, None);
    assert_eq!(trit_t.meta_level, None);
    assert_eq!(trit_t.tech_level, None);

    let nemesis_t = ctx.sde.types.get(&11377).expect("Nemesis type");
    assert_eq!(nemesis_t.description_id, Some(93939));
    assert_eq!(nemesis_t.icon_id, None);
    assert_eq!(nemesis_t.graphic_id, Some(1909));
    assert_eq!(nemesis_t.sound_id, Some(20072));
    assert_eq!(nemesis_t.meta_group_id, Some(2));
    assert_eq!(nemesis_t.meta_level, Some(5));
    assert_eq!(nemesis_t.tech_level, Some(2));

    // parity: sde:res:/staticdata/skins.static
    // Verify skin materials and expanded skin fields
    assert_eq!(ctx.sde.skin_materials.len(), 875);
    let smat1 = ctx.sde.skin_materials.get(&1).expect("skin material 1");
    assert_eq!(smat1.skin_material_id, 1);
    assert_eq!(smat1.display_name_id, Some(505346));
    assert_eq!(smat1.material_set_id, Some(1));

    let skin5 = ctx.sde.skins.get(&5).expect("skin 5");
    assert_eq!(skin5.internal_name, "Megathron Quafe");
    assert_eq!(skin5.types, vec![641]);
    assert_eq!(skin5.skin_material_id, 3);
    assert!(skin5.allow_ccp_devs);
    assert!(!skin5.visible_serenity);
    assert!(skin5.visible_tranquility);
    assert!(!skin5.is_structure_skin);

    // Cosmetics RPC validation against ctx.sde.skins and ctx.sde.skin_licenses
    s1.role |= eve_machonet::service_roles::ROLE_GML;
    // GiveSkin rejects invalid skinID
    let bad_giveskin = ctx
        .dispatch_rpc(
            &mut s1,
            "shipCosmeticsMgr",
            None,
            "GiveSkin",
            &[PyRep::Integer(99_999_999)],
            &kw,
            None,
        )
        .await;
    assert_eq!(bad_giveskin.unwrap_err().code, "InvalidSkinLicenseType");

    // GiveSkin succeeds with valid skinID (170 for Rifter Krusual)
    let good_giveskin = ctx
        .dispatch_rpc(
            &mut s1,
            "shipCosmeticsMgr",
            None,
            "GiveSkin",
            &[PyRep::Integer(170)],
            &kw,
            None,
        )
        .await
        .expect("GiveSkin with valid skinID 170");
    let PyRep::KeyVal(gsk_kv) = good_giveskin else {
        panic!("Expected gsk_kv");
    };
    assert_eq!(gsk_kv.get("skinID").unwrap(), &PyRep::Integer(170));
    assert_eq!(gsk_kv.get("licensed").unwrap(), &PyRep::Bool(true));

    // ActivateSkinLicense rejects invalid skin license typeID
    let bad_act = ctx
        .dispatch_rpc(
            &mut s1,
            "shipCosmeticsMgr",
            None,
            "ActivateSkinLicense",
            &[PyRep::Integer(99_999_999)],
            &kw,
            None,
        )
        .await;
    assert_eq!(bad_act.unwrap_err().code, "InvalidSkinLicenseType");

    // ActivateSkinLicense rejects item that is not category 91 (e.g. Tritanium 34)
    let non_cat91_act = ctx
        .dispatch_rpc(
            &mut s1,
            "shipCosmeticsMgr",
            None,
            "ActivateSkinLicense",
            &[PyRep::Integer(34)],
            &kw,
            None,
        )
        .await;
    assert_eq!(non_cat91_act.unwrap_err().code, "InvalidSkinLicenseType");

    // ApplySkinToShip rejects invalid skinID
    let bad_apply = ctx
        .dispatch_rpc(
            &mut s1,
            "shipCosmeticsMgr",
            None,
            "ApplySkinToShip",
            &[PyRep::Integer(587), PyRep::Integer(99_999_999)],
            &kw,
            None,
        )
        .await;
    assert_eq!(bad_apply.unwrap_err().code, "SkinNotLicensed");

    // ApplySkinToShip rejects incompatible ship type (skin 5 is for Megathron 641, not Rifter 587)
    let incomp_apply = ctx
        .dispatch_rpc(
            &mut s1,
            "shipCosmeticsMgr",
            None,
            "ApplySkinToShip",
            &[PyRep::Integer(587), PyRep::Integer(5)],
            &kw,
            None,
        )
        .await;
    assert_eq!(incomp_apply.unwrap_err().code, "InvalidShipTypeForSkin");

    // parity: sde:res:/staticdata/fighterabilities.static
    // Verify fighter abilities full columns, mappings, and fighterMgr.CmdActivateAbilitySlots SDE wiring
    let fa2 = ctx.sde.fighter_abilities.get(&2).expect("fighter ability 2");
    assert_eq!(fa2.tooltip_text_id, Some(516650));
    let fa18 = ctx.sde.fighter_abilities.get(&18).expect("fighter ability 18");
    assert_eq!(fa18.turret_graphic_id, Some(11515));
    assert_eq!(fa18.tooltip_text_id, Some(516660));

    s1.solar_system_id = Some(30_000_142); // Jita (high-sec)
    let jita_sys = ctx.systems.get_or_spawn(30_000_142);
    for (bid, tid, x) in [(880_001_i64, 32340_u32, 0.0), (880_002_i64, 40362_u32, 100.0), (100_000_002_i64, 282_u32, 2_000.0)] {
        jita_sys
            .spawn_ball_with_combat(
                eve_destiny::Ball {
                    id: bid,
                    type_id: tid,
                    position: glam::DVec3::new(x, 0.0, 0.0),
                    velocity: glam::DVec3::ZERO,
                    max_velocity: 1_000.0,
                    inertia: 1.0,
                    mass: 10_000.0,
                    radius: 20.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                ctx.build_type_combat_state(tid),
            )
            .await
            .expect("spawn ball in Jita");
    }
    ctx.active_fighters.write().unwrap().insert(
        880_001,
        eve_server::router::context::ActiveFighterSquadron {
            ball_id: 880_001,
            item_id: 770_001,
            type_id: 32340, // Malleus I (slot 2 -> ability 18: 9 charges, 6s rearm, turretGraphicID 11515, tooltipTextID 516660)
            owner_char_id: 90_000_001,
            parent_ship_id: 100_000_001,
            tube_index: 0,
            squadron_size: 6,
            target_id: None,
            is_abandoned: false,
            mwd_active: false,
        },
    );
    ctx.active_fighters.write().unwrap().insert(
        880_002,
        eve_server::router::context::ActiveFighterSquadron {
            ball_id: 880_002,
            item_id: 770_002,
            type_id: 40362, // Ametat I (slot 2 -> ability 7: disallow_in_high_sec = true)
            owner_char_id: 90_000_001,
            parent_ship_id: 100_000_001,
            tube_index: 1,
            squadron_size: 6,
            target_id: None,
            is_abandoned: false,
            mwd_active: false,
        },
    );

    // Unlocked target must be rejected with TargetNotLocked
    let unlocked_err = ctx
        .dispatch_rpc(
            &mut s1,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[
                PyRep::Integer(880_001),
                PyRep::Integer(2),
                PyRep::Integer(100_000_002),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Unlocked target must be rejected");
    assert_eq!(unlocked_err.code, "TargetNotLocked");

    // Lock target 100_000_002 from controlling ship 100_000_001
    jita_sys
        .add_target_lock(
            100_000_001,
            eve_destiny::TargetLock {
                target_id: 100_000_002,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(100_000.0),
            },
        )
        .await
        .expect("lock target 100_000_002");

    let act_fa18 = ctx
        .dispatch_rpc(
            &mut s1,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[
                PyRep::Integer(880_001),
                PyRep::Integer(2),
                PyRep::Integer(100_000_002),
            ],
            &kw,
            None,
        )
        .await
        .expect("CmdActivateAbilitySlots slot 2 on Malleus I 32340");
    let PyRep::KeyVal(fa18_kv) = act_fa18 else {
        panic!("Expected fa18_kv");
    };
    assert_eq!(fa18_kv.get("abilityID").unwrap(), &PyRep::Integer(18));
    assert_eq!(
        fa18_kv.get("targetMode").unwrap(),
        &PyRep::String("itemTargeted".into())
    );
    assert_eq!(fa18_kv.get("chargeCount").unwrap(), &PyRep::Integer(9));
    assert_eq!(fa18_kv.get("remainingCharges").unwrap(), &PyRep::Integer(8));
    assert_eq!(fa18_kv.get("rearmTimeSeconds").unwrap(), &PyRep::Real(6.0));
    assert_eq!(
        fa18_kv.get("tooltipTextID").unwrap(),
        &PyRep::Integer(516660)
    );
    assert_eq!(
        fa18_kv.get("turretGraphicID").unwrap(),
        &PyRep::Integer(11515)
    );

    let act_hisec_err = ctx
        .dispatch_rpc(
            &mut s1,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[PyRep::Integer(880_002), PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .expect_err("Ability 7 must be disallowed in high-sec Jita");
    assert_eq!(act_hisec_err.code, "AbilityDisallowedInHighSec");
}

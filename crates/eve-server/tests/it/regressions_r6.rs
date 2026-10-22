//! Integration regression tests covering Round 6 NP-0 and NP-1 findings.

#![forbid(unsafe_code)]

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_review_r6_np0_np1_regressions() {
    // parity: rpc:authentication.Login, rpc:charUnboundMgr.SelectCharacterID, rpc:dogmaIM.LoadAmmo, rpc:planetMgr.Flush, rpc:planetMgr.UserUpdateNetwork
    use eve_db::{IskCents, MarketOrderRecord};

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
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

    // N-P1-3: planetMgr.UserUpdateNetwork rejects forged planet_id where (planet_id / 1000) == sys_id
    let bad_planet = ctx
        .dispatch_rpc(
            &mut s1,
            "planetMgr",
            None,
            "UserUpdateNetwork",
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
            "UserUpdateNetwork",
            &[PyRep::Integer(40_139_295)], // Amarr VIII is in 30002187, not Jita 30000142
            &kw,
            None,
        )
        .await;
    assert_eq!(
        wrong_sys_planet.unwrap_err().code,
        "PlanetNotInCurrentSystem"
    );

    // UserUpdateNetwork on Jita planet 40009082 and verify Flush deducts POCO export tax from wallet
    let s1_ship_id = s1.ship_id.unwrap_or(100_000_001);
    let prev_st = s1.station_id;
    s1.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = None;
        }
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            eve_db::InventoryItemRecord {
                item_id: id,
                type_id: 2534,
                owner_id: 90_000_001,
                location_id: s1_ship_id,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.dispatch_rpc(
        &mut s1,
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
    .unwrap();
    s1.station_id = prev_st;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = prev_st;
        }
    });
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

    let (cour_item_1, cour_item_amarr, cour_item_2) = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let i1 = snap.next_entity_id;
        snap.items.insert(
            i1,
            eve_db::InventoryItemRecord {
                item_id: i1,
                type_id: 34,
                owner_id: 90_000_002,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let ia = snap.next_entity_id;
        snap.items.insert(
            ia,
            eve_db::InventoryItemRecord {
                item_id: ia,
                type_id: 34,
                owner_id: 90_000_002,
                location_id: 60_008_494,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let i2 = snap.next_entity_id;
        snap.items.insert(
            i2,
            eve_db::InventoryItemRecord {
                item_id: i2,
                type_id: 34,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        (i1, ia, i2)
    });

    // s2 (starts docked at 60003760) creates Courier contract 60003760 -> 60008494
    let c_res = ctx
        .dispatch_rpc(
            &mut s2,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(3), // Courier
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(1440),
                PyRep::Integer(1),
                PyRep::Integer(60_003_760),
                PyRep::Integer(60_008_494),
                PyRep::Real(0.0),
                PyRep::Real(10_000.0),
                PyRep::Real(25_000.0),
                PyRep::String("".into()),
                PyRep::String("".into()),
                PyRep::List(vec![PyRep::Integer(cour_item_1)]),
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
                PyRep::Integer(3), // Courier
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(1440),
                PyRep::Integer(1),
                PyRep::Integer(60_008_494),
                PyRep::Integer(60_003_760),
                PyRep::Real(0.0),
                PyRep::Real(10_000.0),
                PyRep::Real(25_000.0),
                PyRep::String("".into()),
                PyRep::String("".into()),
                PyRep::List(vec![PyRep::Integer(cour_item_amarr)]),
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
                PyRep::Integer(3), // Courier
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(1440),
                PyRep::Integer(1),
                PyRep::Integer(60_003_760),
                PyRep::Integer(60_008_494),
                PyRep::Real(0.0),
                PyRep::Real(10_000.0),
                PyRep::Real(25_000.0),
                PyRep::String("".into()),
                PyRep::String("".into()),
                PyRep::List(vec![PyRep::Integer(cour_item_2)]),
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

    let acc_c2 = ctx
        .dispatch_rpc(
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
    let PyRep::KeyVal(acc_c2_kv) = acc_c2 else {
        panic!("Expected acc_c2 KeyVal");
    };
    let wrap_id_c2 = acc_c2_kv.get("courierPackageID").unwrap().as_i64().unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(w) = snap.items.get_mut(&wrap_id_c2) {
            w.location_id = 100_000_002;
            w.flag = eve_db::InventoryFlag::Cargo;
        }
    });

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

    // N-P1-7 & R7-P1-3: Seed a player ask order in Jita for Tritanium (34) at 4.00 ISK, verify invalid contract type rejection, undock s1 in Jita (auto-promoting convoy 800_000_001 to LOD 2), destroy convoy via dogmaIM.FireWeapon, and verify space wreck + +15% NPC ask spike + player order untouched
    let auction_err = ctx
        .dispatch_rpc(
            &mut s1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::String("UnsupportedType".into()),
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
    ctx.seed_demo_world_fixtures();
    ctx.dispatch_rpc(&mut s1, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .unwrap();
    let non_gm_interdict = eve_server::services::cosmetics_events::interdict_convoy(
        &ctx,
        &s1,
        &[PyRep::Integer(800_000_001)],
    )
    .await;
    assert_eq!(non_gm_interdict.unwrap_err().code, "InsufficientRole");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_000_941,
            eve_db::InventoryItemRecord {
                item_id: 100_000_941,
                type_id: 21924,
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
    let PyRep::Tuple(beyonce_bind) = ctx
        .dispatch_rpc(
            &mut s1,
            "beyonce",
            None,
            "MachoBindObject",
            &[PyRep::Integer(30_000_142), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("bind beyonce in Jita")
    else {
        panic!("Expected bind tuple");
    };
    let beyonce_oid = beyonce_bind[0].as_i64().unwrap() as u64;
    let mut ikv = HashMap::new();
    for shot in 0..25 {
        if shot > 0 {
            for _ in 0..4 {
                let _ = jita_h.step_manual(1.0).await.unwrap();
            }
        }
        let interdict = match ctx
            .dispatch_rpc(
                &mut s1,
                "dogmaIM",
                None,
                "Activate",
                &[PyRep::Integer(800_000_001)],
                &kw,
                None,
            )
            .await
        {
            Ok(v) => v,
            // The convoy keeps travelling; close back in to weapon range and keep shooting
            Err(e) if e.code == "TargetOutOfRange" => {
                ctx.dispatch_rpc(
                    &mut s1,
                    "beyonce",
                    Some(beyonce_oid),
                    "CmdFollowBall",
                    &[PyRep::Integer(800_000_001), PyRep::Real(5_000.0)],
                    &kw,
                    None,
                )
                .await
                .expect("CmdFollowBall toward convoy");
                for _ in 0..20 {
                    let _ = jita_h.step_manual(1.0).await.unwrap();
                }
                continue;
            }
            Err(e) => panic!("FireWeapon failed: {e:?}"),
        };
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
    let npc_price = ctx
        .db
        .with_snapshot(|s| s.market_orders[&700_000_001].price.0);
    assert_eq!(player_price, 400);
    assert_eq!(npc_price, 575);
}

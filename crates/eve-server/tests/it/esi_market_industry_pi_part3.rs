//! `ESI-6` integration tests for Contracts and Planetary Interaction routes against live
//! `WorldRepository` and `SdeRegistry` state populated via `contractProxy`, `planetMgr`, `posMgr`,
//! and `planetOrbitalRegistryBroker` RPCs, validated against `OpenApiSpec::pinned()`.

#![forbid(unsafe_code)]

use crate::esi_market_industry_pi::{
    assert_status, get_json, issue_token_with_scopes, make_corp, make_item, rpc_call,
};
use eve_db::{
    corp_roles::{ROLE_CONTRACT_MANAGER, ROLE_DIRECTOR, ROLE_STATION_MANAGER},
    InventoryFlag, IskCents, TrainedSkillRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn test_esi6_contracts_character_corporation_and_public_routes() {
    // parity: esi:GET /characters/{character_id}/contracts, esi:GET /characters/{character_id}/contracts/{contract_id}/items
    // parity: esi:GET /characters/{character_id}/contracts/{contract_id}/bids, esi:GET /contracts/public/{region_id}
    // parity: esi:GET /contracts/public/items/{contract_id}, esi:GET /contracts/public/bids/{contract_id}
    // parity: esi:GET /corporations/{corporation_id}/contracts, esi:GET /corporations/{corporation_id}/contracts/{contract_id}/items
    // parity: esi:GET /corporations/{corporation_id}/contracts/{contract_id}/bids
    // parity: feat:esi.contracts.char-list, feat:esi.contracts.char-items-bids, feat:esi.contracts.corp, feat:esi.contracts.public
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/contracts",
        "esi:GET /characters/{character_id}/contracts/{contract_id}/items",
        "esi:GET /characters/{character_id}/contracts/{contract_id}/bids",
        "esi:GET /contracts/public/{region_id}",
        "esi:GET /contracts/public/items/{contract_id}",
        "esi:GET /contracts/public/bids/{contract_id}",
        "esi:GET /corporations/{corporation_id}/contracts",
        "esi:GET /corporations/{corporation_id}/contracts/{contract_id}/items",
        "esi:GET /corporations/{corporation_id}/contracts/{contract_id}/bids",
        "feat:esi.contracts.char-list",
        "feat:esi.contracts.char-items-bids",
        "feat:esi.contracts.corp",
        "feat:esi.contracts.public",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();
    let char_id = 90_000_001_i64;
    let rival_char_id = 90_000_002_i64;
    let corp_id = 98_000_621_i64;
    let rival_corp_id = 98_000_699_i64;
    let jita_station = 60_003_760_i64;
    let escrow_item_id = 880_621_i64;
    let private_item_id = 880_622_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(jita_station);
            ch.solar_system_id = 30_000_142;
            ch.balance = IskCents::from_isk(250_000_000);
        }
        if let Some(ch2) = snap.characters.get_mut(&rival_char_id) {
            ch2.corporation_id = rival_corp_id;
            ch2.station_id = Some(jita_station);
            ch2.solar_system_id = 30_000_142;
            ch2.balance = IskCents::from_isk(250_000_000);
        }
        snap.corporations.insert(
            corp_id,
            make_corp(corp_id, "ESI6 Contracts Corp", "E6CT", char_id, None, 0.0),
        );
        snap.corporations.insert(
            rival_corp_id,
            make_corp(rival_corp_id, "Rival Contracts Corp", "RVCT", rival_char_id, None, 0.0),
        );
        snap.items.insert(
            escrow_item_id,
            make_item(escrow_item_id, 587, char_id, jita_station, InventoryFlag::Hangar, 1, true),
        );
        snap.items.insert(
            private_item_id,
            make_item(private_item_id, 587, rival_char_id, jita_station, InventoryFlag::Hangar, 1, true),
        );
    });

    // 1. Create public corporation Auction contract via contractProxy.CreateContract
    let mut issuer_sess = SessionState::new(9821);
    issuer_sess.user_id = Some(1);
    issuer_sess.character_id = Some(char_id);
    issuer_sess.corporation_id = Some(corp_id);
    issuer_sess.corp_role = ROLE_DIRECTOR | ROLE_CONTRACT_MANAGER;
    issuer_sess.station_id = Some(jita_station);
    issuer_sess.solar_system_id = Some(30_000_142);

    let pub_args = [
        PyRep::Integer(2), // Auction
        PyRep::Integer(0), // Public
        PyRep::Integer(0), // assigneeID
        PyRep::Integer(1440),
        PyRep::Integer(7),
        PyRep::Integer(jita_station),
        PyRep::Integer(jita_station),
        PyRep::Real(1_500_000.0),
        PyRep::Real(0.0),
        PyRep::Real(5_000_000.0), // buyout
        PyRep::String("Jita Rifter Auction".into()),
        PyRep::String(String::new()),
        PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(escrow_item_id), PyRep::Integer(1)])]),
        PyRep::Integer(4),
        PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(34), PyRep::Integer(1_000)])]),
        PyRep::Bool(true), // forCorp
    ];
    let pub_rep = rpc_call(&ctx, &mut issuer_sess, "contractProxy", "CreateContract", &pub_args).await;
    let public_contract_id = pub_rep.dict_get("contractID").and_then(|v| v.as_i64()).unwrap();

    // 2. Place two bids from rival_char_id via contractProxy.PlaceBid and create a private personal contract
    let mut rival_sess = SessionState::new(9822);
    rival_sess.user_id = Some(1);
    rival_sess.character_id = Some(rival_char_id);
    rival_sess.corporation_id = Some(rival_corp_id);
    rival_sess.station_id = Some(jita_station);
    rival_sess.solar_system_id = Some(30_000_142);

    let bid1 = [PyRep::Integer(public_contract_id), PyRep::Real(2_000_000.0)];
    rpc_call(&ctx, &mut rival_sess, "contractProxy", "PlaceBid", &bid1).await;
    ctx.db.clock().advance_secs(600);
    let bid2 = [PyRep::Integer(public_contract_id), PyRep::Real(2_500_000.0)];
    rpc_call(&ctx, &mut rival_sess, "contractProxy", "PlaceBid", &bid2).await;

    let priv_args = [
        PyRep::Integer(1), // ItemExchange
        PyRep::Integer(1), // Personal
        PyRep::Integer(rival_char_id),
        PyRep::Integer(1440),
        PyRep::Integer(1),
        PyRep::Integer(jita_station),
        PyRep::Integer(jita_station),
        PyRep::Real(100_000.0),
        PyRep::Real(0.0),
        PyRep::Real(0.0),
        PyRep::String("Private Rival Contract".into()),
        PyRep::String(String::new()),
        PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(private_item_id), PyRep::Integer(1)])]),
        PyRep::Integer(4),
        PyRep::List(vec![]),
        PyRep::Bool(false),
    ];
    let priv_rep = rpc_call(&ctx, &mut rival_sess, "contractProxy", "CreateContract", &priv_args).await;
    let private_contract_id = priv_rep.dict_get("contractID").and_then(|v| v.as_i64()).unwrap();

    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-contracts.read_character_contracts.v1",
            "esi-contracts.read_corporation_contracts.v1",
        ],
    );

    let char_contracts =
        get_json(&router, &spec, format!("/v1/characters/{char_id}/contracts/"), Some(&token));
    let char_contracts_arr = char_contracts.as_array().unwrap();
    assert_eq!(char_contracts_arr.len(), 1);
    assert_eq!(char_contracts_arr[0]["contract_id"], json!(public_contract_id));
    assert_eq!(char_contracts_arr[0]["type"], json!("auction"));
    assert_eq!(char_contracts_arr[0]["buyout"], json!(5_000_000.0));

    let char_items = get_json(
        &router,
        &spec,
        format!("/v1/characters/{char_id}/contracts/{public_contract_id}/items/"),
        Some(&token),
    );
    let char_items_arr = char_items.as_array().unwrap();
    assert_eq!(char_items_arr.len(), 2);
    assert!(char_items_arr.iter().any(|i| i["type_id"] == json!(587) && i["is_included"] == json!(true)));
    assert!(char_items_arr.iter().any(|i| i["type_id"] == json!(34) && i["is_included"] == json!(false)));

    assert_status(
        &router,
        format!("/v1/characters/{char_id}/contracts/{private_contract_id}/items/"),
        Some(&token),
        403,
    );

    let char_bids = get_json(
        &router,
        &spec,
        format!("/v1/characters/{char_id}/contracts/{public_contract_id}/bids/"),
        Some(&token),
    );
    let char_bids_arr = char_bids.as_array().unwrap();
    assert_eq!(char_bids_arr.len(), 2);
    assert_eq!(char_bids_arr[0]["amount"], json!(2_500_000.0));
    assert_eq!(char_bids_arr[0]["bidder_id"], json!(rival_char_id));

    let pub_contracts = get_json(&router, &spec, "/v1/contracts/public/10000002/", None);
    assert!(pub_contracts
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["contract_id"] == json!(public_contract_id)));

    let pub_items =
        get_json(&router, &spec, format!("/v1/contracts/public/items/{public_contract_id}/"), None);
    assert_eq!(pub_items.as_array().unwrap().len(), 2);
    assert_status(&router, format!("/v1/contracts/public/items/{private_contract_id}/"), None, 404);

    let pub_bids =
        get_json(&router, &spec, format!("/v1/contracts/public/bids/{public_contract_id}/"), None);
    assert_eq!(pub_bids.as_array().unwrap().len(), 2);
    assert_eq!(pub_bids[0]["amount"], json!(2_500_000.0));

    let corp_contracts =
        get_json(&router, &spec, format!("/v1/corporations/{corp_id}/contracts/"), Some(&token));
    assert!(corp_contracts
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["contract_id"] == json!(public_contract_id)));

    let corp_items = get_json(
        &router,
        &spec,
        format!("/v1/corporations/{corp_id}/contracts/{public_contract_id}/items/"),
        Some(&token),
    );
    assert_eq!(corp_items.as_array().unwrap().len(), 2);

    let corp_bids = get_json(
        &router,
        &spec,
        format!("/v1/corporations/{corp_id}/contracts/{public_contract_id}/bids/"),
        Some(&token),
    );
    assert_eq!(corp_bids.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn test_esi6_planetary_interaction_and_plan_closure() {
    // parity: esi:GET /characters/{character_id}/planets, esi:GET /characters/{character_id}/planets/{planet_id}
    // parity: esi:GET /corporations/{corporation_id}/customs_offices, esi:GET /universe/schematics/{schematic_id}
    // parity: feat:esi.planetary-interaction.colonies, feat:esi.planetary-interaction.layout
    // parity: feat:esi.planetary-interaction.customs, feat:esi.planetary-interaction.schematics
    // parity: plan:ESI-6
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/planets",
        "esi:GET /characters/{character_id}/planets/{planet_id}",
        "esi:GET /corporations/{corporation_id}/customs_offices",
        "esi:GET /universe/schematics/{schematic_id}",
        "feat:esi.planetary-interaction.colonies",
        "feat:esi.planetary-interaction.layout",
        "feat:esi.planetary-interaction.customs",
        "feat:esi.planetary-interaction.schematics",
        "plan:ESI-6",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();
    let char_id = 90_000_001_i64;
    let corp_id = 98_000_641_i64;
    let ship_id = 100_000_001_i64;
    let planet_id = 40_009_080_i64; // Jita III (Planet (Temperate))
    let lowsec_sys = 30_000_192_u32;
    let lowsec_planet_id = ctx.sde.solar_systems[&lowsec_sys].planet_ids[0];
    let cc_item_id = 890_001_i64;
    let gantry_item_id = 890_010_i64;
    let poco_upgrade_item_id = 890_011_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = None;
            ch.solar_system_id = 30_000_142;
            ch.active_ship_id = ship_id;
            ch.balance = IskCents::from_isk(250_000_000);
        }
        snap.corporations.insert(
            corp_id,
            make_corp(corp_id, "ESI6 Planetary Corp", "E6PI", char_id, None, 0.0),
        );
        let sk = snap.skills.entry(char_id).or_default();
        for tid in [2495u32, 2505u32] {
            sk.insert(
                tid,
                TrainedSkillRecord {
                    type_id: tid,
                    level: 4,
                    skill_points: 256_000.0,
                },
            );
        }
        snap.items.insert(
            cc_item_id,
            make_item(cc_item_id, 2254, char_id, ship_id, InventoryFlag::Cargo, 1, false),
        );
        snap.items.insert(
            gantry_item_id,
            make_item(gantry_item_id, 3962, corp_id, ship_id, InventoryFlag::Cargo, 1, false),
        );
        snap.items.insert(
            poco_upgrade_item_id,
            make_item(poco_upgrade_item_id, 2233, corp_id, ship_id, InventoryFlag::Cargo, 1, false),
        );
    });

    // 1. Deploy Temperate Command Center, upgrade to level 4, place ECU + Factory, install schematic & extractor program,
    //    link pins, and route extracted Aqueous Liquids (2268) into the Command Center via planetMgr.UserUpdateNetwork.
    let mut pi_sess = SessionState::new(9830);
    pi_sess.user_id = Some(1);
    pi_sess.character_id = Some(char_id);
    pi_sess.corporation_id = Some(corp_id);
    pi_sess.corp_role = ROLE_DIRECTOR | ROLE_STATION_MANAGER;
    pi_sess.solar_system_id = Some(30_000_142);
    pi_sess.ship_id = Some(ship_id);

    let net_cmds = PyRep::List(vec![
        PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::Integer(101),
            PyRep::Integer(2254),
            PyRep::Real(0.45),
            PyRep::Real(1.15),
        ]),
        PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(101), PyRep::Integer(4)]),
        PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::Integer(102),
            PyRep::Integer(3068),
            PyRep::Real(0.50),
            PyRep::Real(1.20),
        ]),
        PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::Integer(103),
            PyRep::Integer(2469),
            PyRep::Real(0.52),
            PyRep::Real(1.24),
        ]),
        PyRep::Tuple(vec![PyRep::Integer(8), PyRep::Integer(103), PyRep::Integer(121)]),
        PyRep::Tuple(vec![
            PyRep::Integer(10),
            PyRep::Integer(102),
            PyRep::Integer(2268),
            PyRep::Integer(300),
            PyRep::Integer(1),
        ]),
        PyRep::Tuple(vec![
            PyRep::Integer(3),
            PyRep::Integer(102),
            PyRep::Integer(101),
            PyRep::Integer(2),
        ]),
        PyRep::Tuple(vec![
            PyRep::Integer(6),
            PyRep::Integer(7),
            PyRep::List(vec![PyRep::Integer(102), PyRep::Integer(101)]),
            PyRep::Integer(2268),
            PyRep::Integer(20),
        ]),
    ]);
    rpc_call(
        &ctx,
        &mut pi_sess,
        "planetMgr",
        "UserUpdateNetwork",
        &[PyRep::Integer(planet_id), net_cmds],
    )
    .await;

    // Advance simulated clock by 600s and call planetMgr.GetColony so extraction cycles populate pin contents
    ctx.db.clock().advance_secs(600);
    rpc_call(&ctx, &mut pi_sess, "planetMgr", "GetColony", &[PyRep::Integer(planet_id)]).await;

    // 2. Anchor Customs Office Gantry in lowsec 30_000_192, upgrade to POCO, and configure tax rates via RPCs
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = lowsec_sys;
            ch.standings.insert(500_026, 2.0);
        }
    });
    pi_sess.solar_system_id = Some(lowsec_sys);
    rpc_call(
        &ctx,
        &mut pi_sess,
        "posMgr",
        "AnchorOrbital",
        &[PyRep::Integer(gantry_item_id), PyRep::Integer(lowsec_planet_id)],
    )
    .await;
    rpc_call(
        &ctx,
        &mut pi_sess,
        "posMgr",
        "OnlineOrbital",
        &[PyRep::Integer(gantry_item_id), PyRep::Integer(poco_upgrade_item_id)],
    )
    .await;
    let poco_args = [
        PyRep::Integer(lowsec_planet_id),
        PyRep::Integer(18),
        PyRep::keyval([
            ("corporation", PyRep::Real(0.04)),
            ("alliance", PyRep::Real(0.06)),
            ("standingHigh", PyRep::Real(0.05)),
            ("standingGood", PyRep::Real(0.08)),
            ("standingNeutral", PyRep::Real(0.10)),
            ("standingBad", PyRep::Real(0.15)),
            ("standingHorrible", PyRep::Real(0.20)),
        ]),
        PyRep::Real(0.0),
        PyRep::Bool(true),
        PyRep::Bool(true),
    ];
    rpc_call(&ctx, &mut pi_sess, "planetOrbitalRegistryBroker", "UpdateSettings", &poco_args)
        .await;

    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-planets.manage_planets.v1",
            "esi-planets.read_customs_offices.v1",
        ],
    );

    let planets =
        get_json(&router, &spec, format!("/v1/characters/{char_id}/planets/"), Some(&token));
    let planets_arr = planets.as_array().unwrap();
    assert_eq!(planets_arr.len(), 1);
    assert_eq!(planets_arr[0]["planet_id"], json!(planet_id));
    assert_eq!(planets_arr[0]["upgrade_level"], json!(4));
    assert_eq!(planets_arr[0]["num_pins"], json!(3));
    assert_eq!(planets_arr[0]["planet_type"], json!("temperate"));

    let layout = get_json(
        &router,
        &spec,
        format!("/v3/characters/{char_id}/planets/{planet_id}/"),
        Some(&token),
    );
    let pins_arr = layout["pins"].as_array().unwrap();
    assert_eq!(pins_arr.len(), 3);
    let cc_pin = pins_arr.iter().find(|p| p["type_id"] == json!(2254)).expect("CC pin");
    assert_eq!(cc_pin["contents"][0]["type_id"], json!(2268));
    assert!(cc_pin["contents"][0]["amount"].as_i64().unwrap() > 0);
    let fac_pin = pins_arr.iter().find(|p| p["type_id"] == json!(2469)).expect("Factory pin");
    assert_eq!(fac_pin["schematic_id"], json!(121));
    assert_eq!(layout["links"][0]["link_level"], json!(2));
    assert_eq!(layout["routes"][0]["route_id"], json!(7));
    assert_eq!(layout["routes"][0]["quantity"], json!(20.0));
    assert_status(&router, format!("/v3/characters/{char_id}/planets/99999999/"), Some(&token), 404);

    let pocos = get_json(
        &router,
        &spec,
        format!("/v1/corporations/{corp_id}/customs_offices/"),
        Some(&token),
    );
    let pocos_arr = pocos.as_array().unwrap();
    assert_eq!(pocos_arr.len(), 1);
    assert_eq!(pocos_arr[0]["office_id"], json!(lowsec_planet_id));
    assert_eq!(pocos_arr[0]["system_id"], json!(lowsec_sys));
    assert_eq!(pocos_arr[0]["corporation_tax_rate"], json!(0.04));
    assert_eq!(pocos_arr[0]["alliance_tax_rate"], json!(0.06));
    assert_status(
        &router,
        format!("/v1/corporations/{corp_id}/customs_offices/?page=2"),
        Some(&token),
        404,
    );

    // Schematic lookup from real SDE planet_schematics
    let (&schem_id, schem_rec) = ctx.sde.planet_schematics.iter().next().expect("SDE schematics");
    let schem_json = get_json(&router, &spec, format!("/v1/universe/schematics/{schem_id}/"), None);
    assert_eq!(schem_json["schematic_name"], json!(schem_rec.name));
    assert_eq!(schem_json["cycle_time"], json!(schem_rec.cycle_time as i64));
    assert_status(&router, "/v1/universe/schematics/999999999/", None, 404);
}

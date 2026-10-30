//! Remediation integration tests for PI SDE schematic recipes & facility tier validation
//! (`feat:pi.factory-cycles`, `feat:pi.schematic-input-output-routing`) and Paragon V24 SDE
//! EverMarks catalog & SKINR sequencing accrual (`feat:wallet.evermarks`).

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, TypeId};
use eve_machonet::SessionState;
use eve_proto::quasar::{encode_bytes_field, encode_string_field, encode_varint_field};
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::Arc;

const JITA_SOLAR_SYSTEM_ID: u32 = 30_000_142;
const JITA_PLANET_ID: i64 = 40_009_082;
const PARAGON_CORP_ID: i64 = 1_000_419;

async fn setup_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
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
    sess
}

fn seed_item(
    snap: &mut eve_db::WorldStateSnapshot,
    type_id: TypeId,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) -> i64 {
    snap.next_entity_id += 1;
    let item_id = snap.next_entity_id;
    snap.items.insert(
        item_id,
        InventoryItemRecord {
            item_id,
            type_id,
            owner_id,
            location_id,
            flag,
            quantity,
            singleton: false,
            version: 1,
            dynamic_attributes: HashMap::new(),
            source_mutaplasmid_id: None,
            insured_payout_isk: 0.0,
            is_blueprint_copy: false,
            blueprint_runs_remaining: None,
        },
    );
    item_id
}

#[tokio::test]
async fn test_pi_sde_schematics_facility_tier_validation_and_routing() {
    // parity: feat:pi.factory-cycles, feat:pi.schematic-input-output-routing
    eve_testkit::attest(&[
        "feat:pi.factory-cycles",
        "feat:pi.schematic-input-output-routing",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    assert!(
        ctx.sde.planet_schematics.len() > 60,
        "SDE must load full planet_schematics table"
    );

    let kw = HashMap::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 731, "demo", "demo-secret", char_id).await;
    sess.solar_system_id = Some(JITA_SOLAR_SYSTEM_ID);
    sess.station_id = None;
    sess.structure_id = None;
    let ship_id = sess.ship_id.expect("Active ship");

    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = JITA_SOLAR_SYSTEM_ID;
        ch.balance = IskCents::from_isk(100_000_000);
        seed_item(snap, 2534, char_id, ship_id, InventoryFlag::Cargo, 1);
        snap.skills.entry(char_id).or_default().insert(
            2505,
            TrainedSkillRecord {
                type_id: 2505,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });

    let find_schem = |out_tid: TypeId| -> i64 {
        ctx.sde
            .planet_schematics
            .iter()
            .find(|(_, s)| s.types.iter().any(|t| !t.is_input && t.type_id == out_tid))
            .map(|(&id, _)| i64::from(id))
            .unwrap_or_else(|| panic!("missing SDE schematic for output {out_tid}"))
    };
    let water_schem = find_schem(3645);
    let biocells_schem = find_schem(9832);
    let nanofactory_schem = find_schem(2875);

    // Deploy Command Center + Basic (2492) + Advanced (2494) + High-Tech (2482) + Storage (2541) + Launchpad (2543)
    let setup = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(101), PyRep::Integer(2534), PyRep::Real(0.10), PyRep::Real(0.10)]),
                    PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(101), PyRep::Integer(5)]),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(102), PyRep::Integer(2492), PyRep::Real(0.11), PyRep::Real(0.11)]),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(103), PyRep::Integer(2494), PyRep::Real(0.12), PyRep::Real(0.12)]),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(104), PyRep::Integer(2482), PyRep::Real(0.13), PyRep::Real(0.13)]),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(105), PyRep::Integer(2541), PyRep::Real(0.14), PyRep::Real(0.14)]),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(106), PyRep::Integer(2543), PyRep::Real(0.15), PyRep::Real(0.15)]),
                ]),
            ],
            &kw,
            None,
        )
        .await
        .expect("Deploy PI facilities");
    let PyRep::List(pins) = setup.dict_get("pins").unwrap() else {
        panic!("expected pins list");
    };
    let basic_id = pins[1].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();
    let adv_id = pins[2].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();
    let htech_id = pins[3].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();
    let store_id = pins[4].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();
    let pad_id = pins[5].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();

    // Negative facility-tier & unknown schematic checks against SDE planetSchematicsPinMap
    for (pin_id, bad_schem, label) in [
        (basic_id, 999_999, "unknown schematic ID"),
        (basic_id, biocells_schem, "P2 schematic on Basic facility"),
        (adv_id, water_schem, "P1 schematic on Advanced facility"),
        (adv_id, nanofactory_schem, "P4 schematic on Advanced facility"),
        (htech_id, water_schem, "P1 schematic on High-Tech facility"),
    ] {
        let err = ctx
            .dispatch_rpc(
                &mut sess,
                "planetMgr",
                None,
                "UserUpdateNetwork",
                &[
                    PyRep::Integer(JITA_PLANET_ID),
                    PyRep::List(vec![PyRep::Tuple(vec![
                        PyRep::Integer(7),
                        PyRep::Integer(pin_id),
                        PyRep::Integer(bad_schem),
                    ])]),
                ],
                &kw,
                None,
            )
            .await
            .expect_err(label);
        assert_eq!(err.code, "InvalidSchematic", "{label}");
    }

    // Assign valid SDE schematics for each facility tier:
    // - Water (3000 Aqueous Liquids -> 20 Water, 1800s) on Basic (2492)
    // - Biocells (40 Biofuels + 40 Precious Metals -> 5 Biocells, 3600s) on Advanced (2494)
    // - Nano-Factory (6 Industrial Fibers + 6 Reactive Metals + 6 Ukomi Super Conductors -> 1 Nano-Factory, 3600s) on High-Tech (2482)
    ctx.dispatch_rpc(
        &mut sess,
        "planetMgr",
        None,
        "UserUpdateNetwork",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::List(vec![
                PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(basic_id), PyRep::Integer(water_schem)]),
                PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(adv_id), PyRep::Integer(biocells_schem)]),
                PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(htech_id), PyRep::Integer(nanofactory_schem)]),
            ]),
        ],
        &kw,
        None,
    )
    .await
    .expect("Install matching SDE schematics");

    // Verify GetPin reports exact SDE cycleTimeSeconds for each facility
    for (pin_id, expected_cycle) in [(basic_id, 1800), (adv_id, 3600), (htech_id, 3600)] {
        let pin_info = ctx
            .dispatch_rpc(
                &mut sess,
                "planetMgr",
                None,
                "GetPin",
                &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(pin_id)],
                &kw,
                None,
            )
            .await
            .expect("GetPin");
        assert_eq!(
            pin_info.dict_get("cycleTimeSeconds"),
            Some(&PyRep::Integer(expected_cycle))
        );
    }

    let nano_inputs: Vec<(TypeId, u64)> = ctx
        .sde
        .planet_schematics
        .get(&(nanofactory_schem as u32))
        .expect("Nano-Factory schematic in SDE")
        .types
        .iter()
        .filter(|t| t.is_input)
        .map(|t| (t.type_id, t.quantity))
        .collect();
    assert!(!nano_inputs.is_empty());

    // Seed inputs into Storage without routes -> 0 factory cycles must execute
    ctx.db.with_mut_snapshot(|snap| {
        let mut net =
            eve_server::services::industry_pi::load_pi_network(snap, char_id, JITA_PLANET_ID, 5);
        if let Some(pin) = net.pins.iter_mut().find(|p| p.pin_id == store_id) {
            pin.contents.insert(2268, 6_000);
            pin.contents.insert(3645, 80);
            pin.contents.insert(2390, 80);
            for &(in_tid, in_qty) in &nano_inputs {
                pin.contents.insert(in_tid, in_qty * 2);
            }
        }
        eve_server::services::industry_pi::save_pi_network(snap, char_id, JITA_PLANET_ID, &net);
        if let Some(col) = snap.planetary_colonies.get_mut(&(char_id, JITA_PLANET_ID)) {
            col.last_extracted_unix = ctx.now_unix();
        }
    });
    ctx.advance_virtual_clock(3_600);
    let col_unrouted = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "GetColony",
            &[PyRep::Integer(JITA_PLANET_ID)],
            &kw,
            None,
        )
        .await
        .expect("GetColony unrouted");
    assert_eq!(
        col_unrouted.dict_get("simulatedProcessedQuantity"),
        Some(&PyRep::Integer(0))
    );

    // Wire links & input/output routes -> 2 Basic cycles (40 Water) + 1 Adv cycle (5 Biocells) + 1 High-Tech cycle (1 Nano-Factory) = 46 units
    let mut route_cmds = vec![
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(store_id), PyRep::Integer(basic_id), PyRep::Integer(2)]),
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(basic_id), PyRep::Integer(pad_id), PyRep::Integer(2)]),
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(store_id), PyRep::Integer(adv_id), PyRep::Integer(2)]),
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(adv_id), PyRep::Integer(pad_id), PyRep::Integer(2)]),
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(store_id), PyRep::Integer(htech_id), PyRep::Integer(2)]),
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(htech_id), PyRep::Integer(pad_id), PyRep::Integer(2)]),
        PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(store_id), PyRep::Integer(basic_id)]), PyRep::Integer(2268), PyRep::Integer(3_000)]),
        PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(2), PyRep::List(vec![PyRep::Integer(basic_id), PyRep::Integer(pad_id)]), PyRep::Integer(3645), PyRep::Integer(20)]),
        PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(3), PyRep::List(vec![PyRep::Integer(store_id), PyRep::Integer(adv_id)]), PyRep::Integer(3645), PyRep::Integer(40)]),
        PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(4), PyRep::List(vec![PyRep::Integer(store_id), PyRep::Integer(adv_id)]), PyRep::Integer(2390), PyRep::Integer(40)]),
        PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(5), PyRep::List(vec![PyRep::Integer(adv_id), PyRep::Integer(pad_id)]), PyRep::Integer(9832), PyRep::Integer(5)]),
        PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(9), PyRep::List(vec![PyRep::Integer(htech_id), PyRep::Integer(pad_id)]), PyRep::Integer(2875), PyRep::Integer(1)]),
    ];
    for (idx, &(in_tid, in_qty)) in nano_inputs.iter().enumerate() {
        route_cmds.push(PyRep::Tuple(vec![
            PyRep::Integer(6),
            PyRep::Integer(10 + idx as i64),
            PyRep::List(vec![PyRep::Integer(store_id), PyRep::Integer(htech_id)]),
            PyRep::Integer(i64::from(in_tid)),
            PyRep::Integer(in_qty as i64),
        ]));
    }
    ctx.dispatch_rpc(
        &mut sess,
        "planetMgr",
        None,
        "UserUpdateNetwork",
        &[
            PyRep::Integer(JITA_PLANET_ID),
            PyRep::List(route_cmds),
        ],
        &kw,
        None,
    )
    .await
    .expect("Create PI factory routes");

    ctx.advance_virtual_clock(3_600);
    let col_routed = ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "GetColony",
            &[PyRep::Integer(JITA_PLANET_ID)],
            &kw,
            None,
        )
        .await
        .expect("GetColony routed");
    assert_eq!(
        col_routed.dict_get("simulatedProcessedQuantity"),
        Some(&PyRep::Integer(46))
    );
}

#[tokio::test]
async fn test_wallet_evermarks_sde_paragon_catalog_and_redemption() {
    // parity: feat:wallet.evermarks
    eve_testkit::attest(&["feat:wallet.evermarks"]);

    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 732, "demo", "demo-secret", char_id).await;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(60_015_169);
            ch.balance = IskCents::from_isk(1_000_000);
            let sid = ch.active_ship_id;
            if let Some(ship) = snap.items.get_mut(&sid) {
                ship.location_id = 60_015_169;
            }
        }
    });
    sess.station_id = Some(60_015_169);

    // 1. Out-of-the-box Paragon EverMarks catalog loaded from V24 shipcosmeticlicenses.fsdbinary
    let offers_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "LPStoreMgr",
            None,
            "GetAvailableOffersFromCorp",
            &[PyRep::Integer(PARAGON_CORP_ID)],
            &kw,
            None,
        )
        .await
        .expect("GetAvailableOffersFromCorp Paragon");
    let PyRep::List(offers) = offers_rep else {
        panic!("expected List of Paragon offers");
    };
    assert!(
        offers.len() > 800,
        "Expected >800 V24 SDE Paragon ship emblem offers, got {}",
        offers.len()
    );
    let rifter_offer_id = 600_000 + 72_964;
    let slasher_alliance_offer_id = 600_000 + 73_394;
    let rifter_offer = offers
        .iter()
        .find(|o| {
            o.dict_get("offerID") == Some(&PyRep::Integer(rifter_offer_id))
                && o.dict_get("typeID") == Some(&PyRep::Integer(72964))
        })
        .expect("Rifter emblem offer present from shipcosmeticlicenses.fsdbinary");
    let Some(PyRep::Integer(rifter_lp_cost)) = rifter_offer.dict_get("lpCost") else {
        panic!("expected integer lpCost on Rifter emblem offer");
    };
    assert!(*rifter_lp_cost > 0);
    assert_eq!(rifter_offer.dict_get("iskCost"), Some(&PyRep::Real(0.0)));

    let slasher_offer = offers
        .iter()
        .find(|o| {
            o.dict_get("offerID") == Some(&PyRep::Integer(slasher_alliance_offer_id))
                && o.dict_get("typeID") == Some(&PyRep::Integer(73394))
        })
        .expect("Slasher alliance emblem offer present from shipcosmeticlicenses.fsdbinary");
    let Some(PyRep::Integer(slasher_lp_cost)) = slasher_offer.dict_get("lpCost") else {
        panic!("expected integer lpCost on Slasher alliance emblem offer");
    };
    assert!(*slasher_lp_cost > 0);
    assert_eq!(slasher_offer.dict_get("iskCost"), Some(&PyRep::Real(0.0)));

    // Verify ESI GET /v1/loyalty/stores/1000419/offers exposes the same V24 SDE catalog
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();
    let esi_req = EsiHttpRequest::new("GET", "/v1/loyalty/stores/1000419/offers/");
    let esi_resp = router.handle_request(&esi_req);
    assert_eq!(esi_resp.status_code, 200);
    spec.validate_response(
        "GET",
        "/v1/loyalty/stores/1000419/offers/",
        200,
        &esi_resp.headers,
        &esi_resp.body,
    )
    .expect("OpenAPI validation for Paragon LP store offers");
    let esi_json = esi_resp.json().expect("JSON");
    assert_eq!(esi_json.as_array().unwrap().len(), offers.len());

    // 2. Negative redemption check when character has 0 EverMarks
    let insuf_err = ctx
        .dispatch_rpc(
            &mut sess,
            "LPStoreMgr",
            None,
            "TakeOfferForCharacter",
            &[
                PyRep::Integer(PARAGON_CORP_ID),
                PyRep::Integer(rifter_offer_id),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Redeeming without EverMarks must fail");
    assert_eq!(insuf_err.code, "InsufficientLoyaltyPoints");

    // 3. Accrue EverMarks via validated SKINR sequencing jobs (empty payload awards 0 EM)
    let mk_req = |corr: &[u8], type_name: &str, payload: Vec<u8>| {
        eve_proto::quasar::envelopes::EvePublicRequest {
            issued: Some(eve_proto::quasar::types::GoogleTimestamp {
                seconds: ctx.now_unix() as i64,
                nanos: 0,
            }),
            correlation_uuid: corr.to_vec(),
            payload: Some(eve_proto::quasar::types::GoogleAny {
                type_url: format!("{}{type_name}", eve_proto::quasar::TYPE_GOOGLEAPIS_PREFIX),
                value: payload,
            }),
            external_origin: "eve-client".into(),
            application_instance_uuid: b"em-remediation-01".to_vec(),
            authoritative_context: None,
            trace_context: None,
        }
    };
    let empty_rsp = ctx.quasar_gateway.handle_public_request(
        &sess,
        &mk_req(
            b"corr-em-empty",
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
            Vec::new(),
        ),
        None,
        false,
        ctx.now_unix(),
    );
    assert_eq!(empty_rsp.status_code, 200);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&char_id].loyalty_points.get(&PARAGON_CORP_ID).copied().unwrap_or(0)),
        0
    );

    let skinr_cat = eve_server::services::quasar_cosmetics_skinr::SkinrStaticCatalog::load(&ctx.sde);
    let coating_comp = skinr_cat
        .components
        .values()
        .find(|c| !c.is_pattern() && !c.associated_types.is_empty() && !c.disallowed_slots.contains(&1))
        .expect("SDE SKINR coating");
    let (coating_item_type_id, _) = coating_comp.associated_types[0];
    let coating_item_id = ctx.db.with_mut_snapshot(|snap| {
        seed_item(snap, coating_item_type_id, char_id, 60_015_169, InventoryFlag::Hangar, 2)
    });

    let mut seq_item = Vec::new();
    encode_varint_field(1, coating_item_id as u64, &mut seq_item);
    let mut item_outer = Vec::new();
    encode_bytes_field(1, &seq_item, &mut item_outer);
    let mut consume_req = Vec::new();
    encode_bytes_field(1, &item_outer, &mut consume_req);
    encode_varint_field(2, 2, &mut consume_req);
    assert_eq!(
        ctx.quasar_gateway
            .handle_public_request(
                &sess,
                &mk_req(
                    b"corr-em-consume",
                    "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeRequest",
                    consume_req,
                ),
                None,
                false,
                ctx.now_unix(),
            )
            .status_code,
        200
    );

    for (idx, name) in [(1, "Rifter Gold"), (2, "Rifter Crimson")] {
        let mut seq_comp = Vec::new();
        encode_varint_field(1, coating_comp.component_id as u64, &mut seq_comp);
        let mut coat_cfg = Vec::new();
        encode_bytes_field(1, &seq_comp, &mut coat_cfg);
        let mut cfg_outer = Vec::new();
        encode_bytes_field(1, &coat_cfg, &mut cfg_outer);
        let mut seq_slot = Vec::new();
        encode_varint_field(1, 1, &mut seq_slot);
        let mut slot_msg = Vec::new();
        encode_bytes_field(1, &seq_slot, &mut slot_msg);
        encode_bytes_field(2, &cfg_outer, &mut slot_msg);
        let mut seq_hull = Vec::new();
        encode_varint_field(1, 587, &mut seq_hull);
        let mut layout = Vec::new();
        encode_bytes_field(1, &slot_msg, &mut layout);
        encode_bytes_field(2, &seq_hull, &mut layout);
        encode_varint_field(3, 1, &mut layout);
        let mut init_req = Vec::new();
        encode_string_field(1, name, &mut init_req);
        encode_string_field(2, "Paragon Custom", &mut init_req);
        encode_bytes_field(3, &layout, &mut init_req);
        encode_varint_field(5, 1, &mut init_req);

        let rsp = ctx.quasar_gateway.handle_public_request(
            &sess,
            &mk_req(
                format!("corr-em-seq-{idx}").as_bytes(),
                "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
                init_req,
            ),
            None,
            false,
            ctx.now_unix(),
        );
        assert_eq!(rsp.status_code, 200);
    }

    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&char_id].loyalty_points[&PARAGON_CORP_ID]),
        1_000
    );

    // 4. Redeem Rifter Corporation Emblem (72964) from V24 SDE Paragon catalog
    let take_res = ctx
        .dispatch_rpc(
            &mut sess,
            "LPStoreMgr",
            None,
            "TakeOfferForCharacter",
            &[
                PyRep::Integer(PARAGON_CORP_ID),
                PyRep::Integer(rifter_offer_id),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect("Redeem Rifter Corporation Emblem");
    assert_eq!(take_res.dict_get("deliveredTypeID"), Some(&PyRep::Integer(72964)));
    assert_eq!(
        take_res.dict_get("remainingLP"),
        Some(&PyRep::Integer(1_000 - *rifter_lp_cost))
    );
}

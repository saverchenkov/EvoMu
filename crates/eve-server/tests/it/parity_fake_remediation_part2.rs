//! End-to-end remediation integration tests for ESI corp blueprints/structures
//! and Quasar title definitions, space CDN, freelance projects/item delivery,
//! and payment schedules/tokens/store (`esi:*` & `quasar:*` fake_done closure).

#![forbid(unsafe_code)]

use crate::esi_market_industry_pi::{issue_token_with_scopes, make_item, rpc_call};
use crate::quasar_sov_equinox_hub::{drain, field, fields, id_req, idm, rpc, sess, subscribe};
use eve_db::corp_roles::{ROLE_DIRECTOR, ROLE_FACTORY_MANAGER, ROLE_STATION_MANAGER};
use eve_db::{CorporationRecord, InventoryFlag, IskCents};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::quasar::{encode_bytes_field, encode_string_field, encode_varint_field};
use eve_proto::PyRep;
use eve_server::esi::router::EsiRouter;
use eve_server::esi::{EsiHttpRequest, OpenApiSpec};
use eve_server::quasar_gateway::{STATUS_OK, STATUS_UNAUTHORIZED};
use eve_server::router::ServerContext;
use eve_server::services::quasar_payment_store_types::{
    save_store_catalog, PaymentScheduleRecord, PaymentTokenRecord, StoreCatalog, StoreCategory,
    StoreOffer, StoreProduct,
};
use glam::DVec3;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

fn esi_get_validated(router: &EsiRouter, spec: &OpenApiSpec, path: &str, token: Option<&str>) -> Value {
    let mut req = EsiHttpRequest::new("GET", path);
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
    let resp = router.handle_request(&req);
    assert_eq!(resp.status_code, 200, "GET {path} -> {}: {}", resp.status_code, String::from_utf8_lossy(&resp.body));
    let raw_path = path.split('?').next().unwrap_or(path);
    spec.validate_response("GET", raw_path, resp.status_code, &resp.headers, &resp.body)
        .unwrap_or_else(|e| panic!("OpenAPI validation failed for GET {path}: {e}"));
    serde_json::from_slice(&resp.body).unwrap_or(Value::Null)
}

#[tokio::test]
async fn test_parity_remediation_esi_corp_blueprints_and_structures_live_rpcs() {
    // parity: esi:GET /corporations/{corporation_id}/blueprints, esi:GET /corporations/{corporation_id}/structures
    eve_testkit::attest(&[
        "esi:GET /corporations/{corporation_id}/blueprints",
        "esi:GET /corporations/{corporation_id}/structures",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let ceo_id = 90_000_001_i64;
    let corp_id = 98_600_010_i64;
    let jita_station = 60_003_760_i64;
    let perim_sys = 30_000_144_u32;
    let ship_id = 80_600_001_i64;
    let bpo_id = 99_600_101_i64;
    let astrahus_item_id = 99_600_102_i64;
    let market_svc_id = 99_600_103_i64;

    let anchor_pos = ctx
        .sde
        .celestials_in_system(perim_sys)
        .into_iter()
        .find(|c| c.group_id == 7)
        .map(|p| [p.position.x + 25_000_000.0, p.position.y, p.position.z])
        .unwrap_or([100_000_000.0, 0.0, 0.0]);

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&ceo_id].clone();
        ch.corporation_id = corp_id;
        ch.station_id = Some(jita_station);
        ch.solar_system_id = 30_000_142;
        ch.active_ship_id = ship_id;
        ch.balance = IskCents::from_isk(500_000_000);
        snap.characters.insert(ceo_id, ch);
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Industrial Structure Corp".into(),
                ticker: "INDS".into(),
                ceo_id,
                master_wallet: IskCents::from_isk(100_000_000),
                tax_rate: 0.05,
                members: HashSet::from([ceo_id]),
                member_roles: HashMap::from([(
                    ceo_id,
                    ROLE_DIRECTOR | ROLE_FACTORY_MANAGER | ROLE_STATION_MANAGER,
                )]),
                ..CorporationRecord::default()
            },
        );
        // Seed corp BPO (Rifter Blueprint = 684) and Astrahus item (35832) + Standup Market Hub I (35892)
        snap.items.insert(
            bpo_id,
            make_item(bpo_id, 684, corp_id, jita_station, InventoryFlag::CorpSAG1, 1, true),
        );
        snap.items.insert(
            astrahus_item_id,
            make_item(astrahus_item_id, 35832, ceo_id, ship_id, InventoryFlag::Cargo, 1, false),
        );
        snap.items.insert(
            market_svc_id,
            make_item(market_svc_id, 35892, corp_id, jita_station, InventoryFlag::CorpSAG1, 1, false),
        );
    });

    let mut sess = SessionState::new(70_201);
    sess.user_id = Some(1);
    sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
    sess.character_id = Some(ceo_id);
    sess.corporation_id = Some(corp_id);
    sess.station_id = Some(jita_station);
    sess.solar_system_id = Some(30_000_142);
    sess.corp_role = ROLE_DIRECTOR | ROLE_FACTORY_MANAGER | ROLE_STATION_MANAGER;

    // 1. Live industryManager.InstallJob (ME Research = activityID 4) + CompleteJob on corp BPO
    let me_dict = PyRep::Dict(vec![
        (PyRep::String("blueprintID".into()), PyRep::Integer(bpo_id)),
        (PyRep::String("activityID".into()), PyRep::Integer(4)),
        (PyRep::String("runs".into()), PyRep::Integer(2)),
        (PyRep::String("facilityID".into()), PyRep::Integer(jita_station)),
        (PyRep::String("useCorp".into()), PyRep::Bool(true)),
    ]);
    let job_rep = rpc_call(&ctx, &mut sess, "industryManager", "InstallJob", &[me_dict]).await;
    let job_id = job_rep.dict_get("jobID").and_then(|v| v.as_i64()).expect("ME jobID");
    ctx.db.clock().advance_secs(50_000);
    rpc_call(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(job_id)]).await;

    let tok = issue_token_with_scopes(
        &ctx,
        ceo_id,
        vec![
            "esi-corporations.read_blueprints.v1",
            "esi-corporations.read_structures.v1",
        ],
    );
    let bps = esi_get_validated(
        &router,
        &spec,
        &format!("/v1/corporations/{corp_id}/blueprints/"),
        Some(&tok),
    );
    let bp_entry = bps
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["item_id"] == json!(bpo_id))
        .expect("researched corp BPO in /corporations/{id}/blueprints");
    assert_eq!(bp_entry["material_efficiency"], json!(2));
    assert_eq!(bp_entry["runs"], json!(-1));

    // 2. Live structureDeployment.AnchorStructure + structureServices.SetServiceOnlineState
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.station_id = None;
            ch.solar_system_id = perim_sys;
        }
    });
    sess.station_id = None;
    sess.solar_system_id = Some(perim_sys);
    sess.ship_id = Some(ship_id);
    ctx.systems
        .get_or_spawn(perim_sys)
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::from_array(anchor_pos),
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 3.0,
            mass: 1_000_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();

    let anchor_rep = rpc_call(
        &ctx,
        &mut sess,
        "structureDeployment",
        "Anchor",
        &[
            PyRep::Integer(astrahus_item_id),
            PyRep::Tuple(vec![
                PyRep::Real(anchor_pos[0] + 60_000.0),
                PyRep::Real(anchor_pos[1]),
                PyRep::Real(anchor_pos[2]),
            ]),
            PyRep::String("Perimeter Trade Citadel".into()),
            PyRep::Integer(1),
            PyRep::Integer(18),
        ],
    )
    .await;
    let structure_id = anchor_rep
        .as_i64()
        .or_else(|| anchor_rep.dict_get("structureID").and_then(|v| v.as_i64()))
        .expect("anchored structure_id");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&structure_id) {
            st.state = "ShieldVulnerable".into();
        }
        if let Some(svc) = snap.items.get_mut(&market_svc_id) {
            svc.location_id = structure_id;
            svc.flag = InventoryFlag::ServiceSlot0;
        }
    });
    rpc_call(
        &ctx,
        &mut sess,
        "structureDeployment",
        "RenameStructure",
        &[PyRep::Integer(structure_id), PyRep::String("Perimeter Trade Citadel".into())],
    )
    .await;

    let structs = esi_get_validated(
        &router,
        &spec,
        &format!("/v1/corporations/{corp_id}/structures/"),
        Some(&tok),
    );
    let st_entry = structs
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["structure_id"] == json!(structure_id))
        .expect("anchored structure in /corporations/{id}/structures");
    assert_eq!(st_entry["name"], json!("Perimeter Trade Citadel"));
    assert_eq!(st_entry["reinforce_hour"], json!(21));
    assert_eq!(
        st_entry["services"],
        json!([{ "name": "Standup Market Hub I", "state": "offline" }])
    );
}

#[tokio::test]
async fn test_parity_remediation_quasar_title_definitions_and_space_cdn() {
    // parity: quasar:eveProto/generated/eve_public/cosmetic/character/title/definition/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/space/api/cdn/requests_pb2.pyj, quasar:eveProto/generated/eve_public/achievement/definition/api/cdn/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/cosmetic/character/title/definition/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/space/api/cdn/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/achievement/definition/api/cdn/requests_pb2.pyj",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let mut s = sess(70_202, 90_000_001, 98_000_001, 30_000_142, 0);

    // 1. Character title definitions GetLatestRequest returns CDN URL; no school-synthesized titles
    let latest_rsp = rpc(
        &ctx,
        &mut s,
        "eve_public.cosmetic.character.title.definition.api.GetLatestRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(latest_rsp.status_code, STATUS_OK);
    let latest_val = latest_rsp.payload.expect("GetLatestResponse").value;
    let cp = field(&latest_val, 1).expect("checkpoint");
    let url_bytes = field(&cp, 1).expect("url");
    let url = String::from_utf8(url_bytes).expect("utf8 url");
    let path = url.strip_prefix("http://127.0.0.1:26004").expect("local ESI HTTP CDN URL");
    assert_eq!(path, "/cdn/cosmetics/character/title/definitions/v24.1.0/en-us.json");
    let cdn_http = router.handle_request(&EsiHttpRequest::new("GET", path));
    assert_eq!(cdn_http.status_code, 200);
    let parsed_defs: Value = serde_json::from_slice(&cdn_http.body).expect("title defs JSON");
    assert_eq!(parsed_defs, json!([]));

    // 3. Space CDN GetUpdateRequest returns configurable CDN URL; EsiRouter does not serve invented MAPT binary
    let mut no_local = Vec::new();
    encode_varint_field(2, 1, &mut no_local);
    let space_rsp = rpc(&ctx, &mut s, "eve_public.space.api.cdn.GetUpdateRequest", no_local).await;
    assert_eq!(space_rsp.status_code, STATUS_OK);
    let space_cp = field(&space_rsp.payload.unwrap().value, 1).expect("space checkpoint");
    let space_url = String::from_utf8(field(&space_cp, 1).unwrap()).unwrap();
    let space_path = space_url.strip_prefix("http://127.0.0.1:26004").expect("local ESI HTTP URL");
    assert_eq!(space_path, format!("/cdn/maptags/v24.1.{}/universe.fb", ctx.sde.solar_systems.len()));
    let map_http = router.handle_request(&EsiHttpRequest::new("GET", space_path));
    assert_eq!(map_http.status_code, 404);

    // 4. Achievement definition CDN GetLatestRequest returns local EsiRouter JSON bundle
    let ach_rsp = rpc(
        &ctx,
        &mut s,
        "eve_public.achievement.definition.api.cdn.GetLatestRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(ach_rsp.status_code, STATUS_OK);
    let ach_cp = field(&ach_rsp.payload.unwrap().value, 1).expect("ach checkpoint");
    let ach_url = String::from_utf8(field(&ach_cp, 1).unwrap()).unwrap();
    let ach_path = ach_url.strip_prefix("http://127.0.0.1:26004").expect("local ESI HTTP URL");
    let ach_mgr = &ctx.quasar_gateway.progression.achievements;
    assert_eq!(ach_path, format!("/cdn/achievements/v24.1.{}/{}.json", ach_mgr.sde_achievement_count, ach_mgr.sde_achievement_hash));
    let ach_http = router.handle_request(&EsiHttpRequest::new("GET", ach_path));
    assert_eq!(ach_http.status_code, 200);
    let ach_defs: Value = serde_json::from_slice(&ach_http.body).expect("ach defs JSON");
    assert_eq!(ach_defs.as_array().unwrap().len(), ach_mgr.sde_achievement_count);
}

#[tokio::test]
async fn test_parity_remediation_quasar_freelance_projects_and_item_delivery() {
    // parity: quasar:eveProto/generated/eve_public/freelance/project/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/freelance/project/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/freelance/contributionmethod/itemdelivery/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/freelance/contributionmethod/itemdelivery/api/notices_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/freelance/project/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/freelance/project/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/freelance/contributionmethod/itemdelivery/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/freelance/contributionmethod/itemdelivery/api/notices_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let manager_id = 90_000_001_i64;
    let pilot_id = 90_000_002_i64;
    let corp_id = 98_600_020_i64;
    let jita_station = 60_003_760_i64;
    let ore_item_id = 99_600_201_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let tpl = snap.characters[&manager_id].clone();
        for (cid, name, c_id) in [
            (manager_id, "Project Manager", corp_id),
            (pilot_id, "Contractor Pilot", 1_000_009),
        ] {
            let mut ch = snap.characters.get(&cid).cloned().unwrap_or_else(|| tpl.clone());
            ch.character_id = cid;
            ch.name = name.into();
            ch.corporation_id = c_id;
            ch.station_id = Some(jita_station);
            ch.solar_system_id = 30_000_142;
            ch.balance = IskCents::from_isk(10_000_000);
            snap.characters.insert(cid, ch);
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Freelance Sponsor Corp".into(),
                ticker: "FRSC".into(),
                ceo_id: manager_id,
                master_wallet: IskCents::from_isk(100_000_000),
                tax_rate: 0.05,
                members: HashSet::from([manager_id]),
                member_roles: HashMap::from([(manager_id, ROLE_DIRECTOR)]),
                ..CorporationRecord::default()
            },
        );
        // Seed 500 Tritanium (type_id 34) in contractor's Jita hangar
        snap.items.insert(
            ore_item_id,
            make_item(ore_item_id, 34, pilot_id, jita_station, InventoryFlag::Hangar, 500, false),
        );
    });

    let mut mgr_sess = sess(70_203, manager_id, corp_id, 30_000_142, ROLE_DIRECTOR);
    let mut pilot_sess = sess(70_204, pilot_id, 1_000_009, 30_000_142, 0);
    let mut pilot_rx = subscribe(&ctx, &mut pilot_sess).await;

    // 1. CreateRequest with explicit 2,000,000 ISK budget (10,000 ISK/unit * 200), batch=100, type_id=34
    let (mut cfg_b, mut param_b, mut opts_b, mut mod_b, mut rew_b, mut isk_b) = (
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    encode_string_field(1, "34", &mut opts_b);
    encode_bytes_field(3, &opts_b, &mut param_b);
    encode_bytes_field(3, &param_b, &mut cfg_b);
    encode_varint_field(4, 100, &mut mod_b);
    encode_varint_field(1, 10_000, &mut isk_b);
    encode_bytes_field(1, &isk_b, &mut rew_b);

    let mut create_req = Vec::new();
    encode_string_field(1, "Supply Jita Tritanium", &mut create_req);
    encode_string_field(2, "Deliver 200 Tritanium to Jita 4-4", &mut create_req);
    encode_varint_field(3, 200, &mut create_req);
    encode_bytes_field(6, &cfg_b, &mut create_req);
    encode_bytes_field(7, &mod_b, &mut create_req);
    encode_bytes_field(10, &rew_b, &mut create_req);
    let create_rsp = rpc(
        &ctx,
        &mut mgr_sess,
        "eve_public.freelance.project.api.CreateRequest",
        create_req.clone(),
    )
    .await;
    assert_eq!(create_rsp.status_code, STATUS_OK);
    let id_msg = field(&create_rsp.payload.unwrap().value, 1).expect("project Identifier");
    assert!(id_msg.len() >= 16);

    // 2. CommitRequest as contractor
    assert_eq!(
        rpc(&ctx, &mut pilot_sess, "eve_public.freelance.project.api.CommitRequest", id_msg.clone())
            .await
            .status_code,
        STATUS_OK
    );

    // 3. DeliverRequest for 200 units -> moves items to CorpDeliveries & emits ContributionSucceededNotice
    let mut deliver_req = id_msg.clone();
    encode_varint_field(4, 200, &mut deliver_req);
    let deliv_rsp = rpc(
        &ctx,
        &mut pilot_sess,
        "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest",
        deliver_req,
    )
    .await;
    assert_eq!(deliv_rsp.status_code, STATUS_OK);

    let notices = drain(&mut pilot_rx);
    assert!(
        notices.iter().any(|(t, _)| t.ends_with("ContributionSucceededNotice")),
        "DeliverRequest must emit ContributionSucceededNotice"
    );

    // Verify items moved from pilot Hangar (500 -> 300) into CorpDeliveries (200)
    ctx.db.with_snapshot(|snap| {
        let pilot_qty: i32 = snap
            .items
            .values()
            .filter(|i| i.owner_id == pilot_id && i.type_id == 34 && i.flag == InventoryFlag::Hangar)
            .map(|i| i.quantity)
            .sum();
        let corp_deliv_qty: i32 = snap
            .items
            .values()
            .filter(|i| i.owner_id == corp_id && i.type_id == 34 && i.flag == InventoryFlag::CorpDeliveries)
            .map(|i| i.quantity)
            .sum();
        assert_eq!(pilot_qty, 300);
        assert_eq!(corp_deliv_qty, 200);
    });

    // 4. RedeemRewardRequest credits contractor & emits ParticipantRewardRedeemedNotice
    let redeem_rsp = rpc(
        &ctx,
        &mut pilot_sess,
        "eve_public.freelance.project.api.RedeemRewardRequest",
        id_msg.clone(),
    )
    .await;
    assert_eq!(redeem_rsp.status_code, STATUS_OK);
    let redeem_notices = drain(&mut pilot_rx);
    assert!(
        redeem_notices.iter().any(|(t, _)| t.ends_with("ParticipantRewardRedeemedNotice")),
        "RedeemRewardRequest must emit ParticipantRewardRedeemedNotice"
    );

    // 5. Cancelled project DeliverRequest must fail without duplicating items into CorpDeliveries
    let create_rsp2 = rpc(&ctx, &mut mgr_sess, "eve_public.freelance.project.api.CreateRequest", create_req).await;
    assert_eq!(create_rsp2.status_code, STATUS_OK);
    let id_msg2 = field(&create_rsp2.payload.unwrap().value, 1).expect("project2 Identifier");
    assert_eq!(rpc(&ctx, &mut pilot_sess, "eve_public.freelance.project.api.CommitRequest", id_msg2.clone()).await.status_code, STATUS_OK);
    assert_eq!(rpc(&ctx, &mut mgr_sess, "eve_public.freelance.project.api.CloseRequest", id_msg2.clone()).await.status_code, STATUS_OK);
    let mut deliver_req2 = id_msg2;
    encode_varint_field(4, 100, &mut deliver_req2);
    assert_ne!(
        rpc(&ctx, &mut pilot_sess, "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest", deliver_req2)
            .await
            .status_code,
        STATUS_OK
    );
    ctx.db.with_snapshot(|snap| {
        let pilot_qty: i32 = snap.items.values().filter(|i| i.owner_id == pilot_id && i.type_id == 34 && i.flag == InventoryFlag::Hangar).map(|i| i.quantity).sum();
        let corp_deliv_qty: i32 = snap.items.values().filter(|i| i.owner_id == corp_id && i.type_id == 34 && i.flag == InventoryFlag::CorpDeliveries).map(|i| i.quantity).sum();
        assert_eq!(pilot_qty, 300);
        assert_eq!(corp_deliv_qty, 200);
    });
}

#[tokio::test]
async fn test_parity_remediation_quasar_payment_schedules_tokens_and_store() {
    // parity: quasar:eveProto/generated/eve_public/payment/schedule/notices_pb2.pyj, quasar:eveProto/generated/eve_public/payment/token/api_pb2.pyj, quasar:eveProto/generated/eve_public/payment/token/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/payment/purchase/api_pb2.pyj, quasar:eveProto/generated/eve_public/store/category/api_pb2.pyj, quasar:eveProto/generated/eve_public/store/offer/api_pb2.pyj, quasar:eveProto/generated/eve_public/store/product/api_pb2.pyj, quasar:eveProto/generated/eve_public/store/targetaudience/offer/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/payment/schedule/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/payment/token/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/payment/token/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/payment/purchase/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/store/category/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/store/offer/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/store/product/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/store/targetaudience/offer/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    ctx.db.with_mut_snapshot(|snap| {
        save_store_catalog(
            snap,
            &StoreCatalog {
                categories: vec![StoreCategory {
                    id: 1,
                    name: "Omega".into(),
                    description: "Omega Clone State".into(),
                }],
                products: vec![StoreProduct {
                    id: 101,
                    name: "30 Days Omega".into(),
                    description: "Omega time".into(),
                    type_id: 3966,
                    quantity: 1,
                }],
                offers: vec![StoreOffer {
                    id: 1001,
                    name: "30 Days Omega".into(),
                    description: "Monthly Omega".into(),
                    category_id: 1,
                    price_plex: 500,
                    product_ids: vec![101],
                    targeted: true,
                    price_cents: Some(1_999),
                    currency: Some("USD".into()),
                }],
            },
        );
        let tokens = vec![
            PaymentTokenRecord {
                id: 88,
                user_id: 1,
                token_str: "tok_live_88".into(),
                is_quick_pay: true,
                consumed: false,
            },
            PaymentTokenRecord {
                id: 89,
                user_id: 1,
                token_str: "tok_live_89".into(),
                is_quick_pay: true,
                consumed: false,
            },
        ];
        snap.service_state_blobs
            .insert("payment_tokens".into(), serde_json::to_string(&tokens).unwrap());
    });

    let mut s = sess(70_205, 90_000_001, 98_000_001, 30_000_142, 0);
    let mut rx = subscribe(&ctx, &mut s).await;

    // 1. Store category, product, offer, and targetaudience endpoints
    assert_eq!(rpc(&ctx, &mut s, "eve_public.store.category.api.GetAllRequest", Vec::new()).await.status_code, STATUS_OK);
    assert_eq!(rpc(&ctx, &mut s, "eve_public.store.product.api.GetRequest", id_req(1, 101)).await.status_code, STATUS_OK);
    assert_eq!(rpc(&ctx, &mut s, "eve_public.store.offer.api.GetAllRequest", Vec::new()).await.status_code, STATUS_OK);
    let ta_rsp = rpc(
        &ctx,
        &mut s,
        "eve_public.store.targetaudience.offer.api.ListRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(ta_rsp.status_code, STATUS_OK);
    assert!(!fields(&ta_rsp.payload.unwrap().value, 1).is_empty());

    // 2. Payment token GetQuickPayRequest + DisableAllRequest emits QuickPayRemovedNotice (and unauth -> 401)
    let mut unauth = SessionState::new(70_206);
    assert_eq!(
        rpc(&ctx, &mut unauth, "eve_public.payment.token.api.GetQuickPayRequest", Vec::new())
            .await
            .status_code,
        STATUS_UNAUTHORIZED
    );
    let qp_rsp = rpc(&ctx, &mut s, "eve_public.payment.token.api.GetQuickPayRequest", Vec::new()).await;
    assert_eq!(qp_rsp.status_code, STATUS_OK);
    assert!(!fields(&qp_rsp.payload.unwrap().value, 1).is_empty());

    // 3. Purchase Omega via TokenRequest -> creates PaymentScheduleRecord
    let mut purchase_req = Vec::new();
    let mut order = Vec::new();
    let mut off_ident = Vec::new();
    encode_string_field(1, "VEVC-1001", &mut off_ident);
    encode_bytes_field(1, &off_ident, &mut order);
    encode_varint_field(2, 1, &mut order);
    encode_bytes_field(1, &order, &mut purchase_req);
    encode_bytes_field(2, &idm(88), &mut purchase_req);
    assert_eq!(
        rpc(&ctx, &mut s, "eve_public.payment.purchase.api.TokenRequest", purchase_req)
            .await
            .status_code,
        STATUS_OK
    );

    // 4. Advance clock past 30-day billing cycle with quick-pay token -> GetActiveByType is pure read, tick renews & records receipt
    let sched_due: u64 = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs
            .get("payment_schedules")
            .and_then(|j| serde_json::from_str::<Vec<PaymentScheduleRecord>>(j).ok())
            .and_then(|v| v.first().map(|s| s.next_payment_unix))
            .unwrap()
    });
    let advance = sched_due.saturating_add(60).saturating_sub(ctx.now_unix() as u64);
    ctx.advance_secs(advance);
    let mut by_type = Vec::new();
    encode_varint_field(1, 1, &mut by_type);
    assert_eq!(
        rpc(&ctx, &mut s, "eve_public.payment.schedule.api.GetActiveByTypeRequest", by_type.clone())
            .await
            .status_code,
        STATUS_OK
    );
    let receipts_before_tick: usize = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs
            .get("payment_receipts")
            .and_then(|j| serde_json::from_str::<Vec<eve_server::services::quasar_payment_store_types::PaymentReceiptRecord>>(j).ok())
            .map(|v| v.len())
            .unwrap_or(0)
    });
    assert_eq!(receipts_before_tick, 1);

    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let (renewed_due, receipts_after_tick) = ctx.db.with_snapshot(|snap| {
        let due = snap
            .service_state_blobs
            .get("payment_schedules")
            .and_then(|j| serde_json::from_str::<Vec<PaymentScheduleRecord>>(j).ok())
            .and_then(|v| v.first().map(|s| s.next_payment_unix))
            .unwrap();
        let rcpts = snap
            .service_state_blobs
            .get("payment_receipts")
            .and_then(|j| serde_json::from_str::<Vec<eve_server::services::quasar_payment_store_types::PaymentReceiptRecord>>(j).ok())
            .unwrap_or_default();
        (due, rcpts)
    });
    assert!(renewed_due > sched_due);
    assert_eq!(receipts_after_tick.len(), 2);
    assert_eq!(receipts_after_tick[1].amount_in_cents, 1_999);

    // 5. Disable quick-pay tokens, advance past renewed due date, and tick -> PaymentFailedNotice & CancelledNotice
    assert_eq!(
        rpc(&ctx, &mut s, "eve_public.payment.token.api.DisableAllRequest", Vec::new())
            .await
            .status_code,
        STATUS_OK
    );
    let tok_notices = drain(&mut rx);
    assert!(tok_notices.iter().any(|(t, _)| t.ends_with("QuickPayRemovedNotice")));

    let advance2 = renewed_due.saturating_add(60).saturating_sub(ctx.now_unix() as u64);
    ctx.advance_secs(advance2);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let sched_notices = drain(&mut rx);
    assert!(sched_notices.iter().any(|(t, _)| t.ends_with("PaymentFailedNotice")));
    assert!(sched_notices.iter().any(|(t, _)| t.ends_with("CancelledNotice")));
    assert_ne!(
        rpc(&ctx, &mut s, "eve_public.payment.schedule.api.GetActiveByTypeRequest", by_type)
            .await
            .status_code,
        STATUS_OK
    );
}

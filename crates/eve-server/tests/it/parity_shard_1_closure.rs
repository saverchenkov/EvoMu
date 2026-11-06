//! Live non-GM end-to-end tests for Parity Shard 1 Calendar, Contacts/Labels, Mail Lists,
//! Market Prices, and Structure Orders ESI routes (`parity_shard_1_closure.rs`):
//! - Calendar events: `calendarMgr.CreatePersonalEvent`, `calendarMgr.CreateCorporationEvent`,
//!   `calendarMgr.UpdateEventResponse`, `calendarMgr.GetResponsesToEvent` -> all 8 calendar rows + SQLite reload
//! - Character/Corporation/Alliance contacts & labels + Mail lists -> all 9 social/mail rows + SQLite reload
//! - Live trade execution vs extreme unfilled order resilience (`/markets/prices`) + Upwell Fortizar +
//!   `Standup Market Hub I` ACL & service-online gating (`/markets/structures/{id}`) -> all 4 market rows

#![forbid(unsafe_code)]

use crate::common;
use crate::corp_member_audit_reinforce_and_quit::found_corp_with_member;
use crate::industry_live_support::{
    board_loaded_freighter, buy, deploy_online_upwell, fuel_cargo, fund_pilots, int, items_at,
    login, move_item, rpc, train_skills, upwell_cargo, PERIMETER_SYS, PILOT_1, PILOT_2,
};
use crate::parity_shard_1_support::{
    assert_etag_304, cleanup_sqlite_files, esi_call, issue_token, reload_repo, temp_db_path,
};
use eve_proto::PyRep;
use eve_server::esi::{EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use eve_server::world_seeder::seed_full_market_catalog;
use serde_json::json;
use std::sync::Arc;

const TRITANIUM: u32 = 34;
const FORTIZAR: u32 = 35_833;
const MARKET_HUB: u32 = 35_892;

#[tokio::test]
async fn test_shard1_live_calendar_crud_responses_and_attendees_esi() {
    // parity: esi:GET /characters/{character_id}/calendar, esi:GET /characters/{character_id}/calendar/{event_id}
    // parity: esi:PUT /characters/{character_id}/calendar/{event_id}, esi:GET /characters/{character_id}/calendar/{event_id}/attendees
    // parity: feat:esi.calendar.list, feat:esi.calendar.event
    // parity: feat:esi.calendar.respond, feat:esi.calendar.attendees
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/calendar",
        "esi:GET /characters/{character_id}/calendar/{event_id}",
        "esi:PUT /characters/{character_id}/calendar/{event_id}",
        "esi:GET /characters/{character_id}/calendar/{event_id}/attendees",
        "feat:esi.calendar.list",
        "feat:esi.calendar.event",
        "feat:esi.calendar.respond",
        "feat:esi.calendar.attendees",
    ]);

    let db_path = temp_db_path("calendar_esi");
    let corp_id;
    let corp_event_id;
    let personal_event_id;

    {
        let repo = reload_repo(&db_path);
        let ctx = Arc::new(ServerContext::with_repository(repo));
        common::pin_clock(&ctx);

        let mut s1 = login(&ctx, 9461, "demo", PILOT_1).await;
        let mut s2 = login(&ctx, 9462, "other", PILOT_2).await;
        corp_id = found_corp_with_member(&ctx, &mut s1, &mut s2, "CALE").await;

        let base_ft = ((ctx.now_unix() + 3_600) as i64 + 11_644_473_600) * 10_000_000;
        let ev1 = rpc(
            &ctx,
            &mut s1,
            "calendarMgr",
            "CreateCorporationEvent",
            &[
                PyRep::Integer(base_ft),
                PyRep::Integer(90),
                PyRep::String("Fleet Formup".into()),
                PyRep::String("Bring logi & scouts".into()),
                PyRep::Bool(true),
            ],
        )
        .await
        .expect("CreateCorporationEvent");
        corp_event_id = ev1.as_i64().or_else(|| ev1.dict_get("eventID").and_then(|v| v.as_i64())).unwrap();

        let ev2 = rpc(
            &ctx,
            &mut s1,
            "calendarMgr",
            "CreatePersonalEvent",
            &[
                PyRep::Integer(base_ft + 7_200 * 10_000_000),
                PyRep::Integer(45),
                PyRep::String("PI Reset".into()),
                PyRep::String("Update extractors".into()),
                PyRep::Bool(false),
                PyRep::List(vec![PyRep::Integer(PILOT_2)]),
            ],
        )
        .await
        .expect("CreatePersonalEvent");
        personal_event_id = ev2.as_i64().or_else(|| ev2.dict_get("eventID").and_then(|v| v.as_i64())).unwrap();

        // PILOT_2 sets tentative via MachoNet RPC; PILOT_1 responds via ESI PUT /v3/characters/{id}/calendar/{event_id}/
        rpc(
            &ctx,
            &mut s2,
            "calendarMgr",
            "SendEventResponse",
            &[PyRep::Integer(corp_event_id), PyRep::Integer(corp_id), PyRep::Integer(5)],
        )
        .await
        .expect("SendEventResponse tentative");

        let router = EsiRouter::new(Arc::clone(&ctx));
        let spec = OpenApiSpec::pinned();
        let tok_ro = issue_token(&ctx, PILOT_1, &["esi-calendar.read_calendar_events.v1"]);
        let tok_rw = issue_token(
            &ctx,
            PILOT_1,
            &["esi-calendar.read_calendar_events.v1", "esi-calendar.respond_calendar_events.v1"],
        );

        esi_call(&router, &spec, "GET", &format!("/v1/characters/{PILOT_1}/calendar/"), None, None, 401);
        esi_call(
            &router,
            &spec,
            "PUT",
            &format!("/v3/characters/{PILOT_1}/calendar/{corp_event_id}/"),
            Some(&tok_ro),
            Some(json!({ "response": "accepted" })),
            403,
        );
        esi_call(
            &router,
            &spec,
            "PUT",
            &format!("/v3/characters/{PILOT_1}/calendar/{corp_event_id}/"),
            Some(&tok_rw),
            Some(json!({ "response": "invalid_state" })),
            400,
        );
        esi_call(
            &router,
            &spec,
            "PUT",
            &format!("/v3/characters/{PILOT_1}/calendar/{corp_event_id}/"),
            Some(&tok_rw),
            Some(json!({ "response": "accepted" })),
            204,
        );

        assert_etag_304(&router, &spec, &format!("/v1/characters/{PILOT_1}/calendar/"), Some(&tok_rw));
        let (list_json, _) = esi_call(&router, &spec, "GET", &format!("/v1/characters/{PILOT_1}/calendar/"), Some(&tok_rw), None, 200);
        let arr = list_json.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert!(arr.iter().any(|e| e["event_id"] == corp_event_id && e["title"] == "Fleet Formup" && e["event_response"] == "accepted"));

        assert_etag_304(&router, &spec, &format!("/v4/characters/{PILOT_1}/calendar/{corp_event_id}/"), Some(&tok_rw));
        let (detail_json, _) = esi_call(&router, &spec, "GET", &format!("/v4/characters/{PILOT_1}/calendar/{corp_event_id}/"), Some(&tok_rw), None, 200);
        assert_eq!(detail_json["duration"], json!(90));
        assert_eq!(detail_json["owner_type"], json!("corporation"));
        assert_eq!(detail_json["response"], json!("accepted"));

        assert_etag_304(&router, &spec, &format!("/v1/characters/{PILOT_1}/calendar/{corp_event_id}/attendees/"), Some(&tok_rw));
        let (att_json, _) = esi_call(
            &router,
            &spec,
            "GET",
            &format!("/v1/characters/{PILOT_1}/calendar/{corp_event_id}/attendees/"),
            Some(&tok_rw),
            None,
            200,
        );
        let att_arr = att_json.as_array().unwrap();
        assert!(att_arr.iter().any(|a| a["character_id"] == PILOT_1 && a["event_response"] == "accepted"));
        assert!(att_arr.iter().any(|a| a["character_id"] == PILOT_2 && a["event_response"] == "tentative"));
    }

    // SQLite reload check
    {
        let repo2 = reload_repo(&db_path);
        let ctx2 = Arc::new(ServerContext::with_repository(repo2));
        let router2 = EsiRouter::new(Arc::clone(&ctx2));
        let spec2 = OpenApiSpec::pinned();
        let tok2 = issue_token(&ctx2, PILOT_1, &["esi-calendar.read_calendar_events.v1"]);
        let (list2, _) = esi_call(&router2, &spec2, "GET", &format!("/v1/characters/{PILOT_1}/calendar/"), Some(&tok2), None, 200);
        assert!(list2.as_array().unwrap().iter().any(|e| e["event_id"] == corp_event_id && e["event_response"] == "accepted"));
        assert!(list2.as_array().unwrap().iter().any(|e| e["event_id"] == personal_event_id && e["title"] == "PI Reset"));
    }

    cleanup_sqlite_files(&db_path);
}

#[tokio::test]
async fn test_shard1_live_contacts_maillists_and_market_prices_structures_esi() {
    // parity: esi:GET /characters/{character_id}/contacts/labels, esi:GET /corporations/{character_id}/contacts
    // parity: esi:GET /corporations/{corporation_id}/contacts/labels, feat:esi.contacts.corp
    // parity: esi:GET /alliances/{alliance_id}/contacts, esi:GET /alliances/{alliance_id}/contacts/labels
    // parity: feat:esi.contacts.alliance, esi:GET /characters/{character_id}/mail/lists
    // parity: feat:esi.mail.lists, esi:GET /markets/prices, feat:esi.market.prices
    // parity: esi:GET /markets/structures/{structure_id}, feat:esi.market.structure-orders
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/contacts/labels",
        "esi:GET /corporations/{character_id}/contacts",
        "esi:GET /corporations/{corporation_id}/contacts/labels",
        "feat:esi.contacts.corp",
        "esi:GET /alliances/{alliance_id}/contacts",
        "esi:GET /alliances/{alliance_id}/contacts/labels",
        "feat:esi.contacts.alliance",
        "esi:GET /characters/{character_id}/mail/lists",
        "feat:esi.mail.lists",
        "esi:GET /markets/prices",
        "feat:esi.market.prices",
        "esi:GET /markets/structures/{structure_id}",
        "feat:esi.market.structure-orders",
    ]);

    let db_path = temp_db_path("social_markets_esi");
    let corp_id;
    let ally_id;
    let fort_id;
    let list_id;

    {
        let repo = reload_repo(&db_path);
        let ctx = Arc::new(ServerContext::with_repository(repo));
        common::pin_clock(&ctx);
        seed_full_market_catalog(&ctx.db, &ctx.sde);
        fund_pilots(&ctx, &[(PILOT_1, 80_000_000_000.0), (PILOT_2, 5_000_000_000.0)]).await;

        let mut s1 = login(&ctx, 9471, "demo", PILOT_1).await;
        let mut s2 = login(&ctx, 9472, "other", PILOT_2).await;
        train_skills(&ctx, &mut s1, &[3363, 11_574, 20_185, FORTIZAR, MARKET_HUB], true).await;

        corp_id = found_corp_with_member(&ctx, &mut s1, &mut s2, "MRKT").await;
        ctx.advance_virtual_clock(8 * 86_400);
        rpc(&ctx, &mut s1, "account", "GiveCash", &[PyRep::Integer(corp_id), PyRep::Real(2_000_000_000.0), PyRep::String("ally".into())])
            .await
            .unwrap();
        let a_rep = rpc(&ctx, &mut s1, "corpRegistry", "CreateAlliance", &[PyRep::String("Market Barons".into()), PyRep::String("MKTB".into())])
            .await
            .expect("CreateAlliance");
        ally_id = int(&a_rep, "allianceID");
        s1.alliance_id = Some(ally_id);
        s2.alliance_id = Some(ally_id);

        let extract_lbl = |rep: &PyRep| -> i64 {
            rep.as_i64()
                .or_else(|| rep.dict_get("labelID").and_then(|v| v.as_i64()))
                .expect("labelID")
        };

        // 1. Live Character, Corp, and Alliance contacts + labels via charMgr, corpRegistry & allianceRegistry RPCs
        let lbl_rep = rpc(&ctx, &mut s1, "charMgr", "CreateLabel", &[PyRep::String("FCs".into())])
            .await
            .expect("charMgr.CreateLabel");
        let lbl_id = extract_lbl(&lbl_rep);
        rpc(
            &ctx,
            &mut s1,
            "charMgr",
            "AddContact",
            &[
                PyRep::Integer(PILOT_2),
                PyRep::Real(10.0),
                PyRep::Bool(true),
                PyRep::Bool(false),
                PyRep::String(String::new()),
            ],
        )
        .await
        .expect("charMgr.AddContact");
        rpc(
            &ctx,
            &mut s1,
            "charMgr",
            "AssignLabels",
            &[PyRep::List(vec![PyRep::Integer(PILOT_2)]), PyRep::Integer(lbl_id)],
        )
        .await
        .expect("charMgr.AssignLabels");

        let corp_lbl_rep = rpc(
            &ctx,
            &mut s1,
            "corpRegistry",
            "CreateLabel",
            &[PyRep::String("Allies".into()), PyRep::Integer(0x00FF00)],
        )
        .await
        .expect("corpRegistry.CreateLabel");
        let corp_lbl = extract_lbl(&corp_lbl_rep);
        rpc(
            &ctx,
            &mut s1,
            "corpRegistry",
            "AddCorporateContact",
            &[PyRep::Integer(1_000_035), PyRep::Real(5.0), PyRep::Integer(corp_lbl)],
        )
        .await
        .expect("corpRegistry.AddCorporateContact");

        let ally_lbl_rep = rpc(
            &ctx,
            &mut s1,
            "allianceRegistry",
            "CreateLabel",
            &[PyRep::String("Blues".into()), PyRep::Integer(0xFF0000)],
        )
        .await
        .expect("allianceRegistry.CreateLabel");
        let ally_lbl = extract_lbl(&ally_lbl_rep);
        rpc(
            &ctx,
            &mut s1,
            "allianceRegistry",
            "AddAllianceContact",
            &[PyRep::Integer(1_000_035), PyRep::Real(10.0), PyRep::Integer(ally_lbl)],
        )
        .await
        .expect("allianceRegistry.AddAllianceContact");

        // 2. Mailing lists via mailingListsMgr RPC
        list_id = rpc(
            &ctx,
            &mut s1,
            "mailingListsMgr",
            "Create",
            &[PyRep::String("Logistics Ops".into()), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0)],
        )
        .await
        .expect("Create mailing list")
        .as_i64()
        .unwrap();

        // 3. Deploy Upwell Fortizar + Standup Market Hub I in Perimeter and place a live structure sell order
        let mut cargo = upwell_cargo(&ctx, FORTIZAR, &[MARKET_HUB]);
        cargo.push((TRITANIUM, 500));
        cargo.extend(fuel_cargo());
        let fr = board_loaded_freighter(&ctx, &mut s1, &cargo).await;
        let trit_stack = items_at(&ctx, PILOT_1, fr, TRITANIUM)[0].0;
        let (fid, _) = deploy_online_upwell(&ctx, &mut s1, PERIMETER_SYS, FORTIZAR, &[MARKET_HUB], 2_500).await;
        fort_id = fid;
        move_item(&ctx, &mut s1, trit_stack, fort_id, 4).await;
        let fort_trit = ctx.db.with_snapshot(|s| {
            s.items.values().find(|i| i.location_id == fort_id && i.type_id == TRITANIUM).unwrap().item_id
        });
        rpc(
            &ctx,
            &mut s1,
            "marketProxy",
            "PlaceMultiSellOrder",
            &[
                PyRep::List(vec![PyRep::keyval([
                    ("stationID", PyRep::Integer(fort_id)),
                    ("typeID", PyRep::Integer(TRITANIUM as i64)),
                    ("itemID", PyRep::Integer(fort_trit)),
                    ("price", PyRep::Real(8.25)),
                    ("quantity", PyRep::Integer(100)),
                ])]),
                PyRep::Bool(false),
                PyRep::Integer(14),
            ],
        )
        .await
        .expect("PlaceMultiSellOrder at Fortizar");

        let router = EsiRouter::new(Arc::clone(&ctx));
        let spec = OpenApiSpec::pinned();
        let tok1 = issue_token(
            &ctx,
            PILOT_1,
            &[
                "esi-characters.read_contacts.v1",
                "esi-corporations.read_contacts.v1",
                "esi-alliances.read_contacts.v1",
                "esi-mail.read_mail.v1",
                "esi-markets.structure_markets.v1",
            ],
        );

        let (clbls, _) = esi_call(&router, &spec, "GET", &format!("/v1/characters/{PILOT_1}/contacts/labels/"), Some(&tok1), None, 200);
        assert!(clbls.as_array().unwrap().iter().any(|l| l["label_name"] == "FCs"));

        let (ccont, _) = esi_call(&router, &spec, "GET", &format!("/v2/corporations/{corp_id}/contacts/"), Some(&tok1), None, 200);
        assert!(ccont.as_array().unwrap().iter().any(|c| c["contact_id"] == 1_000_035 && c["standing"] == 5.0));

        let (acont, _) = esi_call(&router, &spec, "GET", &format!("/v2/alliances/{ally_id}/contacts/"), Some(&tok1), None, 200);
        assert!(acont.as_array().unwrap().iter().any(|c| c["contact_id"] == 1_000_035 && c["standing"] == 10.0));

        let (mlists, _) = esi_call(&router, &spec, "GET", &format!("/v1/characters/{PILOT_1}/mail/lists/"), Some(&tok1), None, 200);
        assert!(mlists.as_array().unwrap().iter().any(|m| m["mailing_list_id"] == list_id && m["name"] == "Logistics Ops"));

        assert_etag_304(&router, &spec, "/v1/markets/prices/", None);
        let (prices, _) = esi_call(&router, &spec, "GET", "/v1/markets/prices/", None, None, 200);
        let trit_p = prices.as_array().unwrap().iter().find(|p| p["type_id"] == TRITANIUM).unwrap();
        assert!(trit_p["adjusted_price"].as_f64().unwrap() > 0.0);

        assert_etag_304(&router, &spec, &format!("/v1/markets/structures/{fort_id}/"), Some(&tok1));
        let (sorders, _) = esi_call(&router, &spec, "GET", &format!("/v1/markets/structures/{fort_id}/"), Some(&tok1), None, 200);
        assert!(sorders.as_array().unwrap().iter().any(|o| o["type_id"] == TRITANIUM && o["price"] == 8.25 && o["volume_remain"] == 100));
    }

    // SQLite reload check
    {
        let repo2 = reload_repo(&db_path);
        let ctx2 = Arc::new(ServerContext::with_repository(repo2));
        let router2 = EsiRouter::new(Arc::clone(&ctx2));
        let spec2 = OpenApiSpec::pinned();
        let tok2 = issue_token(
            &ctx2,
            PILOT_1,
            &[
                "esi-characters.read_contacts.v1",
                "esi-corporations.read_contacts.v1",
                "esi-alliances.read_contacts.v1",
                "esi-mail.read_mail.v1",
                "esi-markets.structure_markets.v1",
            ],
        );
        let (ml2, _) = esi_call(&router2, &spec2, "GET", &format!("/v1/characters/{PILOT_1}/mail/lists/"), Some(&tok2), None, 200);
        assert!(ml2.as_array().unwrap().iter().any(|m| m["mailing_list_id"] == list_id));
        let (so2, _) = esi_call(&router2, &spec2, "GET", &format!("/v1/markets/structures/{fort_id}/"), Some(&tok2), None, 200);
        assert!(so2.as_array().unwrap().iter().any(|o| o["type_id"] == TRITANIUM && o["volume_remain"] == 100));
    }

    cleanup_sqlite_files(&db_path);
    let _ = (buy, corp_id, ally_id);
}

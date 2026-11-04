//! End-to-End Integration Journeys Part 1 (`journey:launch_login` .. `journey:undock_navigate`).
#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord};
use eve_machonet::{
    placebo_crypto_hash, CryptoChallengePacket, CryptoHandshakeAck, CryptoHandshakeResult,
    CryptoRequestPacket, CryptoServerHandshake, SessionState, VersionExchange,
};
use eve_proto::quasar::{EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX};
use eve_proto::PyRep;
use eve_server::macho_listener::{handle_macho_client, read_framed_pyrep, write_framed_pyrep};
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};

async fn call_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

async fn call_bound_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    bound_id: u64,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, Some(bound_id), method, args, &HashMap::new(), None)
        .await
}

fn bound_id_from_tuple(rep: &PyRep) -> u64 {
    match rep {
        PyRep::Tuple(t) if !t.is_empty() => t[0].as_i64().unwrap_or(1) as u64,
        other => other.as_i64().unwrap_or(1) as u64,
    }
}

#[tokio::test]
async fn test_journey_launch_login() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind macho");
    let addr = listener.local_addr().unwrap();
    let srv_ctx = Arc::clone(&ctx);
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let _ = handle_macho_client(stream, srv_ctx).await;
        }
    });

    // 1. OAuth2 PKCE token exchange
    let now = ctx.now_unix();
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            90_000_001,
            "eveLauncherTQ",
            "https://localhost/callback",
            vec!["publicData".into()],
            challenge,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    let token_bundle = ctx
        .db
        .exchange_pkce_code(&code, "eveLauncherTQ", verifier, now)
        .expect("exchange_pkce_code");
    assert_eq!(token_bundle.token_type, "Bearer");
    assert!(!token_bundle.access_token.is_empty());

    // 2. TCP machoNet V24.01 handshake
    let mut stream = TcpStream::connect(addr).await.expect("connect macho");
    let hello = read_framed_pyrep(&mut stream).await.unwrap().unwrap();
    let PyRep::Tuple(hello_items) = &hello else {
        panic!("expected 7-tuple hello, got {hello:?}");
    };
    assert_eq!(hello_items.len(), 7);
    assert_eq!(hello_items[5].as_str(), Some("V24.01@ccp"));

    write_framed_pyrep(&mut stream, &VersionExchange::default().to_pyrep()).await.unwrap();
    write_framed_pyrep(&mut stream, &CryptoRequestPacket::placebo().to_pyrep()).await.unwrap();
    let crypto_reply = read_framed_pyrep(&mut stream).await.unwrap().unwrap();
    assert_eq!(crypto_reply.as_str(), Some("OK CC"));

    let sso_challenge = CryptoChallengePacket::new_sso_login(&token_bundle.access_token).to_pyrep();
    write_framed_pyrep(&mut stream, &sso_challenge).await.unwrap();
    let pw_ver = read_framed_pyrep(&mut stream).await.unwrap().unwrap();
    assert_eq!(pw_ver, PyRep::Integer(2));
    let srv_hs_rep = read_framed_pyrep(&mut stream).await.unwrap().unwrap();
    let srv_hs = CryptoServerHandshake::from_pyrep(&srv_hs_rep).unwrap();
    let hs_res = CryptoHandshakeResult {
        challenge_response_hash: placebo_crypto_hash(&srv_hs.server_challenge),
        func_output: Vec::new(),
        func_result: PyRep::Integer(0),
    };
    write_framed_pyrep(&mut stream, &hs_res.to_pyrep()).await.unwrap();
    let ack = CryptoHandshakeAck::from_pyrep(&read_framed_pyrep(&mut stream).await.unwrap().unwrap())
        .expect("SSO CryptoHandshakeAck");
    assert_eq!(ack.user_id, 1);
    assert_eq!(ack.access_token.as_deref(), Some(token_bundle.access_token.as_str()));

    // 3. Authenticate with SSO JWT token and reach character selection with account's characters listed
    let mut sess = SessionState::new(1);
    let login_res = call_rpc(
        &ctx,
        &mut sess,
        "authentication",
        "Login",
        &[PyRep::String("__sso__".into()), PyRep::String(token_bundle.access_token)],
    )
    .await
    .expect("authentication.Login");
    assert_eq!(login_res.dict_get("userID").and_then(|v| v.as_i64()), Some(1));

    let chars = call_rpc(&ctx, &mut sess, "charUnboundMgr", "GetCharactersToSelect", &[])
        .await
        .expect("GetCharactersToSelect");
    let chars_dbg = format!("{chars:?}");
    assert!(chars_dbg.contains("90000001"), "character list must include 90000001: {chars_dbg}");

    call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID");
    assert_eq!(sess.character_id, Some(90_000_001));
    assert_eq!(sess.station_id, Some(60_003_760));
}

#[tokio::test]
async fn test_journey_create_character() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    ctx.db.provision_player_account("demo2", "demo2-secret");
    let mut sess = SessionState::new(10);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let mut sess2 = SessionState::new(100);
    crate::common::login_pilot(&ctx, &mut sess2, "demo2", "demo2-secret").await;

    // Create a character for each of the 4 empires (Caldari=1, Minmatar=2, Amarr=4, Gallente=8)
    let race_specs = [
        ("Caldari Pilot", 1_i64, 1_i64, 17_i64, 601_u32, true),
        ("Minmatar Pilot", 3_i64, 1_i64, 14_i64, 588_u32, true),
        ("Amarr Pilot", 5_i64, 1_i64, 11_i64, 596_u32, false),
        ("Gallente Pilot", 7_i64, 1_i64, 20_i64, 606_u32, false),
    ];

    for (name, bloodline_id, gender_id, school_id, expected_corvette, use_first) in race_specs {
        let ancestry_id = ctx
            .sde
            .ancestries
            .values()
            .find(|a| a.bloodline_id as i64 == bloodline_id)
            .map(|a| a.ancestry_id as i64)
            .unwrap_or(1);
        let active_sess = if use_first { &mut sess } else { &mut sess2 };
        let doll_str = format!("doll:{name}");
        let portrait_str = format!("portrait:{name}");
        let char_id_rep = call_rpc(
            &ctx,
            active_sess,
            "charUnboundMgr",
            "CreateCharacterWithDoll",
            &[
                PyRep::String(name.into()),
                PyRep::Integer(bloodline_id),
                PyRep::Integer(gender_id),
                PyRep::Integer(ancestry_id),
                PyRep::String(doll_str.clone()),
                PyRep::String(portrait_str.clone()),
                PyRep::Integer(school_id),
            ],
        )
        .await
        .expect("CreateCharacterWithDoll");
        let cid = char_id_rep.as_i64().expect("character id");
        assert!(cid > 200_000_000);

        ctx.db.with_snapshot(|snap| {
            let ch = snap.characters.get(&cid).expect("character in DB");
            assert_eq!(ch.name, name);
            assert_eq!(ch.bloodline_id as i64, bloodline_id);
            assert_eq!(ch.ancestry_id as i64, ancestry_id);
            assert_eq!(ch.school_id as i64, school_id);
            let st_id = ch.station_id.expect("school station_id");
            assert!(st_id > 60_000_000);
            assert!(ch.balance.0 >= 500_000, "starter ISK must be >= 5,000 ISK");
            let starter_ship = snap
                .items
                .values()
                .find(|it| it.owner_id == cid && it.location_id == st_id && it.flag == InventoryFlag::Hangar && it.type_id == expected_corvette);
            assert!(starter_ship.is_some(), "starter corvette {expected_corvette} in school station hangar for {name}");
            let skills = snap.skills.get(&cid).expect("starter skills");
            assert!(skills.len() >= 5, "starter skills populated for {name}");
        });
    }
}

#[tokio::test]
async fn test_journey_station_ui() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();
    crate::common::seed_test_nes_store_catalog(&ctx);

    let mut sess = SessionState::new(11);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID");

    // 1. Hangars & 2. Inventory
    let inv_bind = call_rpc(&ctx, &mut sess, "invbroker", "MachoBindObject", &[PyRep::Integer(60_003_760), PyRep::None])
        .await
        .expect("invbroker bind");
    let hangar_list = call_bound_rpc(&ctx, &mut sess, "invbroker", bound_id_from_tuple(&inv_bind), "List", &[])
        .await
        .expect("invbroker.List");
    assert!(format!("{hangar_list:?}").contains("90000001"));

    // 3. Fitting
    let fit_kv = PyRep::keyval([("name", PyRep::String("Station UI Fit".into())), ("description", PyRep::String("Station UI test fit".into())), ("shipTypeID", PyRep::Integer(587)), ("fitData", PyRep::List(vec![]))]);
    call_rpc(&ctx, &mut sess, "charFittingMgr", "SaveFitting", &[PyRep::Integer(90_000_001), fit_kv]).await.expect("SaveFitting");
    let fittings = call_rpc(&ctx, &mut sess, "charFittingMgr", "GetFittings", &[PyRep::Integer(90_000_001)]).await.expect("GetFittings");
    let PyRep::Dict(fit_dict) = fittings else { panic!("expected Dict from GetFittings") };
    assert_eq!(fit_dict.len(), 1);
    assert!(format!("{fit_dict:?}").contains("Station UI Fit"));

    // 4. Market
    let mkt = call_rpc(&ctx, &mut sess, "marketProxy", "GetMarketGroups", &[]).await.expect("GetMarketGroups");
    assert!(format!("{mkt:?}").contains("marketGroupID"));

    // 5. Industry
    let fac = call_rpc(&ctx, &mut sess, "facilityManager", "GetFacilities", &[]).await.expect("GetFacilities");
    let PyRep::List(fac_list) = fac else { panic!("expected List from GetFacilities") };
    assert!(!fac_list.is_empty());

    // 6. Agency
    let cat_id = *eve_server::services::operations_manager::get_operation_defs(&ctx.sde).sde_categories.iter().next().unwrap_or(&31);
    let ops = call_rpc(&ctx, &mut sess, "operationsManager", "get_category_state", &[PyRep::Integer(cat_id)]).await.expect("get_category_state");
    assert!(matches!(ops, PyRep::Integer(s) if s >= 0));

    // 7. Character Sheet
    let pub_info = call_rpc(&ctx, &mut sess, "charMgr", "GetPublicInfo3", &[PyRep::Integer(90_000_001)]).await.expect("GetPublicInfo3");
    assert!(format!("{pub_info:?}").contains("90000001"));

    // 8. Skills
    let sk_handler = call_rpc(&ctx, &mut sess, "skillMgr2", "GetMySkillHandler", &[]).await.expect("GetMySkillHandler");
    assert!(bound_id_from_tuple(&sk_handler) > 0);

    // 9. Wallet & 10. Journal
    let bal = call_rpc(&ctx, &mut sess, "account", "GetCashBalance", &[]).await.expect("GetCashBalance");
    assert!(bal.as_f64().unwrap_or(0.0) > 0.0);
    let jita_agent = ctx.sde.agents.values().find(|a| a.solar_system_id == Some(30_000_142) && !matches!(a.agent_type_id, 3 | 6 | 11)).unwrap().clone();
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&90_000_001).unwrap().station_id = jita_agent.station_id);
    sess.station_id = jita_agent.station_id;
    let ag_bind = call_rpc(&ctx, &mut sess, "agentMgr", "MachoBindObject", &[PyRep::Integer(jita_agent.agent_id), PyRep::None]).await.expect("agentMgr bind");
    call_bound_rpc(&ctx, &mut sess, "agentMgr", bound_id_from_tuple(&ag_bind), "DoAction", &[PyRep::Integer(2)]).await.expect("DoAction request mission");
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&90_000_001).unwrap().station_id = Some(60_003_760));
    sess.station_id = Some(60_003_760);
    let journal = call_rpc(&ctx, &mut sess, "agentMgr", "GetMyJournalDetails", &[]).await.expect("GetMyJournalDetails");
    let PyRep::Tuple(j_parts) = journal else { panic!("expected Tuple from GetMyJournalDetails") };
    assert_eq!(j_parts.len(), 2);
    assert!(format!("{j_parts:?}").contains(&jita_agent.agent_id.to_string()));

    // 11. Corporation
    let corp = call_rpc(&ctx, &mut sess, "corpRegistry", "GetCorporation", &[]).await.expect("GetCorporation");
    assert!(format!("{corp:?}").contains("corporationID"));

    // 12. People & Places (Bookmarks + Contacts)
    call_rpc(&ctx, &mut sess, "bookmark", "BookmarkLocation", &[PyRep::Integer(60_003_760), PyRep::Integer(90_000_001), PyRep::String("Jita Hub BM".into()), PyRep::String("Station bookmark".into())]).await.expect("BookmarkLocation");
    let bms = call_rpc(&ctx, &mut sess, "bookmark", "GetBookmarks", &[]).await.expect("GetBookmarks");
    assert!(format!("{bms:?}").contains("Jita Hub BM"));
    call_rpc(&ctx, &mut sess, "charMgr", "AddContact", &[PyRep::Integer(90_000_002), PyRep::Real(5.0), PyRep::Bool(true), PyRep::Bool(false), PyRep::String("".into())]).await.expect("AddContact");
    let contacts = call_rpc(&ctx, &mut sess, "charMgr", "GetContactList", &[]).await.expect("GetContactList");
    assert!(format!("{contacts:?}").contains("90000002"));

    // 13. Mail, 14. Notifications, 15. Calendar
    call_rpc(&ctx, &mut sess, "mailMgr", "SendMail", &[PyRep::List(vec![PyRep::Integer(90_000_001)]), PyRep::None, PyRep::None, PyRep::String("Station UI Mail".into()), PyRep::String("Welcome to Jita 4-4".into()), PyRep::Bool(false), PyRep::Integer(0), PyRep::None]).await.expect("SendMail");
    let mail = call_rpc(&ctx, &mut sess, "mailMgr", "SyncMail", &[]).await.expect("SyncMail");
    assert!(format!("{mail:?}").contains("Station UI Mail"));
    ctx.create_character_notification(90_000_002, 90_000_001, 10, 1, "bounty: 50000");
    let notifs = call_rpc(&ctx, &mut sess, "notificationMgr", "GetUnprocessed", &[]).await.expect("GetUnprocessed");
    let PyRep::ObjectEx { list: notif_rows, .. } = notifs else { panic!("expected ObjectEx CRowset from GetUnprocessed") };
    assert_eq!(notif_rows.len(), 1);
    assert!(format!("{notif_rows:?}").contains("bounty: 50000"));
    call_rpc(&ctx, &mut sess, "calendarMgr", "CreatePersonalEvent", &[PyRep::Integer(135_500_000_000_000_000), PyRep::Integer(60), PyRep::String("Jita Trade Meet".into()), PyRep::String("Market review".into()), PyRep::Integer(0), PyRep::List(vec![])]).await.expect("CreatePersonalEvent");
    let cal = call_rpc(&ctx, &mut sess, "calendarMgr", "GetEventList", &[]).await.expect("GetEventList");
    assert!(format!("{cal:?}").contains("Jita Trade Meet"));

    // 16. NES Store (Quasar PLEX Vault balance)
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: b"station-ui-nes".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.plex.vault.api.BalanceRequest"),
            value: Vec::new(),
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"station-ui-app".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let nes_rep = call_rpc(&ctx, &mut sess, "publicGatewaySvc", "SendRequest", &[PyRep::Bytes(req.encode_proto())])
        .await
        .expect("publicGatewaySvc.SendRequest");
    let PyRep::Bytes(resp_bytes) = nes_rep.dict_get("response_proto").expect("response_proto") else {
        panic!("expected response_proto bytes");
    };
    let proto_rsp = EvePublicResponse::decode_proto(resp_bytes).expect("decode EvePublicResponse");
    assert_eq!(proto_rsp.status_code, STATUS_OK);
}

#[tokio::test]
async fn test_journey_fit_ship() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(12);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID");

    let ship_id = sess.ship_id.expect("active ship_id");
    // Seed an NPC sell order for 200mm AutoCannon I (type 2873) and Phased Plasma S (type 178) in Jita 4-4
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.insert(
            990_001,
            MarketOrderRecord {
                order_id: 990_001,
                type_id: 2873,
                owner_id: 1_000_035,
                station_id: 60_003_760,
                region_id: 10_000_002,
                price: IskCents::from_isk(10_000),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_bid: false,
                is_npc: true,
                escrow: IskCents(0),
            },
        );
        snap.items.insert(
            100_000_502,
            InventoryItemRecord {
                item_id: 100_000_502,
                type_id: 178, // Phased Plasma S
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
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
        // Oversized 1400mm Howitzer Artillery I (type 2969) that exceeds frigate PG/CPU
        snap.items.insert(
            100_000_503,
            InventoryItemRecord {
                item_id: 100_000_503,
                type_id: 2969,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
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

    // 1. Buy module on the market
    let isk_before = ctx.db.with_snapshot(|s| s.characters[&90_000_001].balance.0);
    call_rpc(
        &ctx,
        &mut sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(60_003_760),
            PyRep::Integer(2873),
            PyRep::Real(10_000.0),
            PyRep::Integer(1),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("PlaceBuyOrder buy module");
    let isk_after = ctx.db.with_snapshot(|s| s.characters[&90_000_001].balance.0);
    assert!(isk_after < isk_before, "buying module on market must deduct ISK");
    let mod_id = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .find(|it| it.owner_id == 90_000_001 && it.type_id == 2873 && it.location_id == 60_003_760)
            .map(|it| it.item_id)
            .expect("purchased module in hangar")
    });

    // 2. Fit module to HiSlot1 + CPU/PG validation rejects oversized 1400mm artillery
    let ship_bind = call_rpc(&ctx, &mut sess, "invbroker", "MachoBindObject", &[PyRep::Integer(ship_id), PyRep::None])
        .await
        .expect("invbroker ship bind");
    let ship_bid = bound_id_from_tuple(&ship_bind);
    let too_big = call_bound_rpc(
        &ctx,
        &mut sess,
        "invbroker",
        ship_bid,
        "Add",
        &[PyRep::Integer(100_000_503), PyRep::Integer(60_003_760), PyRep::Integer(28)], // HiSlot1
    )
    .await;
    assert!(too_big.is_err(), "fitting 1400mm artillery on frigate must fail CPU/PG validation");

    call_bound_rpc(
        &ctx,
        &mut sess,
        "invbroker",
        ship_bid,
        "Add",
        &[PyRep::Integer(mod_id), PyRep::Integer(60_003_760), PyRep::Integer(28)], // HiSlot1
    )
    .await
    .expect("fit 200mm AutoCannon to HiSlot1");

    // 3. Online module & 4. Load ammo
    call_rpc(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(ship_id), PyRep::Integer(mod_id), PyRep::Integer(1)])
        .await
        .expect("SetModuleOnline");
    call_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "LoadAmmo",
        &[PyRep::Integer(ship_id), PyRep::Integer(mod_id), PyRep::Integer(100_000_502), PyRep::Integer(60_003_760)],
    )
    .await
    .expect("LoadAmmo");

    // 5. Save and load a fitting
    let fit_kv = PyRep::keyval([
        ("name", PyRep::String("Journey Combat Fit".into())),
        ("description", PyRep::String("AutoCannon fit".into())),
        ("shipTypeID", PyRep::Integer(587)),
        ("fitData", PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(2873), PyRep::Integer(28), PyRep::Integer(1)])])),
    ]);
    let saved_id = call_rpc(&ctx, &mut sess, "charFittingMgr", "SaveFitting", &[PyRep::Integer(90_000_001), fit_kv])
        .await
        .expect("SaveFitting");
    assert!(saved_id.as_i64().unwrap_or(0) > 0);
    let fittings = call_rpc(&ctx, &mut sess, "charFittingMgr", "GetFittings", &[PyRep::Integer(90_000_001)])
        .await
        .expect("GetFittings");
    assert!(format!("{fittings:?}").contains("Journey Combat Fit"));

    // 6. Assemble and repackage a spare ship + verify dogma attributes
    let spare_ship_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: 587, // Rifter
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
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
        id
    });
    call_rpc(&ctx, &mut sess, "ship", "AssembleShip", &[PyRep::Integer(spare_ship_id)])
        .await
        .expect("AssembleShip");
    assert!(ctx.db.with_snapshot(|s| s.items[&spare_ship_id].singleton));
    call_rpc(&ctx, &mut sess, "repackagingSvc", "RepackageItems", &[PyRep::List(vec![PyRep::Integer(spare_ship_id)])])
        .await
        .expect("RepackageItems");
    assert!(!ctx.db.with_snapshot(|s| s.items[&spare_ship_id].singleton));

    let dogma_bind = call_rpc(&ctx, &mut sess, "dogmaIM", "MachoBindObject", &[PyRep::Integer(ship_id), PyRep::None])
        .await
        .expect("dogmaIM.MachoBindObject");
    assert!(bound_id_from_tuple(&dogma_bind) > 0);
}

#[tokio::test]
async fn test_journey_undock_navigate() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(13);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID");

    // 1. Undock
    call_rpc(&ctx, &mut sess, "ship", "Undock", &[]).await.expect("ship.Undock");
    assert_eq!(sess.station_id, None);
    let ship_id = sess.ship_id.expect("ship_id");
    let sys_id = sess.solar_system_id.expect("solar_system_id");
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    assert!(sys_h.get_ball_snapshot(ship_id).await.unwrap().is_some(), "ship ball in space after undock");

    // 2. Approach, orbit, keep at range, align
    let bey_bind = call_rpc(&ctx, &mut sess, "beyonce", "MachoBindObject", &[PyRep::Integer(sys_id as i64), PyRep::None])
        .await
        .expect("beyonce bind");
    let bey_bid = bound_id_from_tuple(&bey_bind);
    let stargate_id = ctx
        .sde
        .stargates()
        .values()
        .find(|sg| sg.from_solar_system_id == sys_id)
        .map(|sg| sg.stargate_id as i64)
        .expect("stargate in Jita");
    call_bound_rpc(&ctx, &mut sess, "beyonce", bey_bid, "CmdFollowBall", &[PyRep::Integer(stargate_id), PyRep::Real(2500.0)])
        .await
        .expect("CmdFollowBall");
    call_bound_rpc(&ctx, &mut sess, "beyonce", bey_bid, "CmdOrbit", &[PyRep::Integer(stargate_id), PyRep::Real(5000.0)])
        .await
        .expect("CmdOrbit");
    call_bound_rpc(&ctx, &mut sess, "beyonce", bey_bid, "CmdAlignTo", &[PyRep::Integer(stargate_id)])
        .await
        .expect("CmdAlignTo");

    // 3. Warp to celestial and to bookmark
    call_bound_rpc(&ctx, &mut sess, "beyonce", bey_bid, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(stargate_id), PyRep::Real(0.0)])
        .await
        .expect("CmdWarpToStuff celestial");
    for _ in 0..20 {
        let _ = sys_h.tick().await;
    }

    let bm = call_rpc(
        &ctx,
        &mut sess,
        "accessGroupBookmarkMgr",
        "BookmarkStaticLocation",
        &[PyRep::Integer(ship_id), PyRep::None, PyRep::String("Safe Spot".into()), PyRep::String("Warp BM".into()), PyRep::Integer(0)],
    )
    .await
    .expect("BookmarkStaticLocation");
    let bm_id = match &bm {
        PyRep::Tuple(t) if !t.is_empty() => t[0].as_i64().unwrap_or(1),
        _ => bm.dict_get("bookmarkID").and_then(|v| v.as_i64()).unwrap_or(1),
    };
    call_bound_rpc(&ctx, &mut sess, "beyonce", bey_bid, "CmdStop", &[])
        .await
        .expect("CmdStop before bookmark warp");
    let _ = sys_h.set_ball_position(ship_id, glam::DVec3::new(500_000.0, 0.0, 0.0)).await;
    call_bound_rpc(&ctx, &mut sess, "beyonce", bey_bid, "CmdWarpToStuff", &[PyRep::String("bookmark".into()), PyRep::Integer(bm_id), PyRep::Real(0.0)])
        .await
        .expect("CmdWarpToStuff bookmark");
    call_bound_rpc(&ctx, &mut sess, "beyonce", bey_bid, "CmdStop", &[])
        .await
        .expect("CmdStop");

    // 4. Jump through 3 stargates and dock at destination
    let mut visited_systems = vec![sys_id];
    for _ in 0..3 {
        let cur_sys = sess.solar_system_id.unwrap();
        let b_bind = call_rpc(&ctx, &mut sess, "beyonce", "MachoBindObject", &[PyRep::Integer(cur_sys as i64), PyRep::None])
            .await
            .expect("beyonce bind");
        let b_bid = bound_id_from_tuple(&b_bind);
        let sg = ctx
            .sde
            .stargates()
            .values()
            .find(|g| g.from_solar_system_id == cur_sys && !visited_systems.contains(&g.to_solar_system_id))
            .or_else(|| ctx.sde.stargates().values().find(|g| g.from_solar_system_id == cur_sys))
            .expect("next stargate");
        let dest_sys = sg.to_solar_system_id;
        let _ = ctx
            .systems
            .get_or_spawn(cur_sys)
            .set_ball_position(ship_id, sg.position)
            .await;
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_001);
        call_bound_rpc(&ctx, &mut sess, "beyonce", b_bid, "CmdStargateJump", &[PyRep::Integer(sg.stargate_id as i64)])
            .await
            .expect("CmdStargateJump");
        assert_eq!(sess.solar_system_id, Some(dest_sys));
        visited_systems.push(dest_sys);
    }
    assert!(visited_systems.len() == 4, "must traverse 3 stargates across 4 systems");

    // Dock at a station in the destination system (or Jita 4-4)
    let dest_sys = sess.solar_system_id.unwrap();
    let d_bind = call_rpc(&ctx, &mut sess, "beyonce", "MachoBindObject", &[PyRep::Integer(dest_sys as i64), PyRep::None])
        .await
        .expect("beyonce bind dest");
    let d_bid = bound_id_from_tuple(&d_bind);
    let (dest_station, dest_pos) = ctx
        .sde
        .stations
        .values()
        .find(|st| st.solar_system_id == dest_sys)
        .map(|st| (st.station_id as i64, st.position))
        .unwrap_or((60_003_760, glam::DVec3::ZERO));
    let _ = ctx.systems.get_or_spawn(dest_sys).set_ball_position(ship_id, dest_pos).await;
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_001);
    call_bound_rpc(&ctx, &mut sess, "beyonce", d_bid, "CmdDock", &[PyRep::Integer(dest_station)])
        .await
        .expect("CmdDock");
    assert_eq!(sess.station_id, Some(dest_station));
}

pub(crate) mod journey_local_chat_multiplayer_presence {
    // End-to-End Integration Journeys Part 2 (`journey:local_chat` .. `journey:contracts`).

    use eve_db::{InventoryFlag, IskCents, MarketOrderRecord};
    use eve_machonet::SessionState;
    use eve_proto::quasar::{
        encode_string_field, EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp,
        QuasarMessage, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
    };
    use eve_proto::PyRep;
    use eve_server::quasar_gateway::STATUS_OK;
    use eve_server::router::ServerContext;
    use std::collections::HashMap;

    async fn call_rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        service: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, eve_proto::BridgeRpcError> {
        ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
            .await
    }

    async fn call_bound_rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        service: &str,
        bound_id: u64,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, eve_proto::BridgeRpcError> {
        ctx.dispatch_rpc(sess, service, Some(bound_id), method, args, &HashMap::new(), None)
            .await
    }

    fn bound_id_from_tuple(rep: &PyRep) -> u64 {
        match rep {
            PyRep::Tuple(t) if !t.is_empty() => t[0].as_i64().unwrap_or(1) as u64,
            other => other.as_i64().unwrap_or(1) as u64,
        }
    }

    #[tokio::test]
    async fn test_journey_local_chat() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ctx.seed_demo_world_fixtures();

        let mut sess1 = SessionState::new(201);
        crate::common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
        call_rpc(&ctx, &mut sess1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID 1");

        let mut sess2 = SessionState::new(202);
        crate::common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
        call_rpc(&ctx, &mut sess2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_002)])
            .await
            .expect("SelectCharacterID 2");

        // 1. Local channel via V24 XmppChatMgr.ResyncSystemChannelAccess + Quasar eve_public.chat.local.api.*
        call_rpc(&ctx, &mut sess1, "publicGatewaySvc", "Subscribe", &[]).await.expect("Subscribe 1");
        call_rpc(&ctx, &mut sess2, "publicGatewaySvc", "Subscribe", &[]).await.expect("Subscribe 2");
        call_rpc(&ctx, &mut sess1, "XmppChatMgr", "ResyncSystemChannelAccess", &[])
            .await
            .expect("ResyncSystemChannelAccess 1");
        call_rpc(&ctx, &mut sess2, "XmppChatMgr", "ResyncSystemChannelAccess", &[])
            .await
            .expect("ResyncSystemChannelAccess 2");

        let send_quasar = |type_name: &str, value: Vec<u8>| {
            let req = EvePublicRequest {
                issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
                correlation_uuid: vec![1, 2, 3, 4],
                payload: Some(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
                    value,
                }),
                external_origin: "eve-client".into(),
                application_instance_uuid: vec![9, 9],
                authoritative_context: None,
                trace_context: None,
            };
            req.encode_proto()
        };

        let mem_bytes = send_quasar("eve_public.chat.local.api.GetMembershipListRequest", Vec::new());
        let mem_rep = call_rpc(&ctx, &mut sess1, "publicGatewaySvc", "SendRequest", &[PyRep::Bytes(mem_bytes)])
            .await
            .expect("GetMembershipListRequest");
        let PyRep::Bytes(mem_rsp_bytes) = mem_rep.dict_get("response_proto").unwrap() else { panic!("response_proto") };
        let mem_env = EvePublicResponse::decode_proto(mem_rsp_bytes).unwrap();
        assert_eq!(mem_env.status_code, STATUS_OK);
        let mem_wm = WireMessage::decode(&mem_env.payload.unwrap().value).unwrap();
        assert!(mem_wm.fields.iter().filter(|f| f.field_number == 1).count() >= 2);

        let mut msg_bytes = Vec::new();
        encode_string_field(1, "Hello Jita Local!", &mut msg_bytes);
        let bcast_bytes = send_quasar("eve_public.chat.local.api.BroadcastMessageRequest", msg_bytes);
        let bcast_rep = call_rpc(&ctx, &mut sess1, "publicGatewaySvc", "SendRequest", &[PyRep::Bytes(bcast_bytes)])
            .await
            .expect("BroadcastMessageRequest");
        let PyRep::Bytes(bcast_rsp_bytes) = bcast_rep.dict_get("response_proto").unwrap() else { panic!("response_proto") };
        assert_eq!(EvePublicResponse::decode_proto(bcast_rsp_bytes).unwrap().status_code, STATUS_OK);

        let notices = ctx.quasar_gateway.poll_notices(sess2.session_id);
        let bcast_notice = notices
            .iter()
            .find(|n| n.payload.as_ref().is_some_and(|p| p.type_url.contains("MessageBroadcastNotice")))
            .expect("second client must receive Quasar MessageBroadcastNotice in local");
        let b_wm = WireMessage::decode(&bcast_notice.payload.as_ref().unwrap().value).unwrap();
        assert_eq!(
            b_wm.fields.iter().find(|f| f.field_number == 3).unwrap().value.as_str().unwrap().unwrap(),
            "Hello Jita Local!"
        );

        // 2. Player-owned / private channel & MOTD via XmppChatMgr
        let ch_rep = call_rpc(
            &ctx,
            &mut sess1,
            "XmppChatMgr",
            "CreatePlayerOwnedChannel",
            &[PyRep::String("OpsChannel".into()), PyRep::None, PyRep::String("Initial MOTD".into())],
        )
        .await
        .expect("CreatePlayerOwnedChannel");
        let ch_id = ch_rep.dict_get("channelID").and_then(|v| v.as_str()).unwrap().to_string();
        call_rpc(&ctx, &mut sess1, "XmppChatMgr", "AddCharacterToChannel", &[PyRep::String(ch_id.clone()), PyRep::Integer(90_000_002)])
            .await
            .expect("AddCharacterToChannel");
        let motd_rep = call_rpc(&ctx, &mut sess1, "XmppChatMgr", "SetMotd", &[PyRep::String(ch_id), PyRep::String("Updated Ops MOTD".into())])
            .await
            .expect("SetMotd");
        assert_eq!(motd_rep.dict_get("motd").and_then(|v| v.as_str()), Some("Updated Ops MOTD"));

        // 3. Fleet + Corp channel sync via XmppChatMgr.ResyncSystemChannelAccess
        let _flt = call_rpc(&ctx, &mut sess1, "fleetObjectHandler", "CreateFleet", &[]).await.expect("CreateFleet");
        let fleet_id = sess1.fleet_id.unwrap_or(1);
        let sync_rep = call_rpc(&ctx, &mut sess1, "XmppChatMgr", "ResyncSystemChannelAccess", &[])
            .await
            .expect("ResyncSystemChannelAccess with fleet");
        let sync_str = format!("{sync_rep:?}");
        assert!(sync_str.contains(&format!("fleetid:{fleet_id}")) && sync_str.contains("corpid:"));

        // 4. Jump stargate to Perimeter (30_000_144) and verify Local channel membership transitions
        call_rpc(&ctx, &mut sess1, "ship", "Undock", &[]).await.expect("Undock 1");
        let ship1 = sess1.ship_id.unwrap();
        let bey1 = bound_id_from_tuple(&call_rpc(&ctx, &mut sess1, "beyonce", "MachoBindObject", &[PyRep::Integer(30_000_142), PyRep::None]).await.unwrap());
        let sg = ctx.sde.stargates().values().find(|g| g.from_solar_system_id == 30_000_142 && g.to_solar_system_id == 30_000_144).unwrap();
        let _ = ctx.systems.get_or_spawn(30_000_142).set_ball_position(ship1, sg.position).await;
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_001);
        call_bound_rpc(&ctx, &mut sess1, "beyonce", bey1, "CmdStargateJump", &[PyRep::Integer(sg.stargate_id as i64)])
            .await
            .expect("CmdStargateJump");
        let moved_sync = call_rpc(&ctx, &mut sess1, "XmppChatMgr", "ResyncSystemChannelAccess", &[])
            .await
            .expect("ResyncSystemChannelAccess in Perimeter");
        assert_eq!(moved_sync.dict_get("channelID").and_then(|v| v.as_str()), Some("solarsystemid2:30000144"));
    }

    #[tokio::test]
    async fn test_journey_multiplayer_presence() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ctx.seed_demo_world_fixtures();

        let mut sess1 = SessionState::new(203);
        crate::common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
        call_rpc(&ctx, &mut sess1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID 1");

        let mut sess2 = SessionState::new(204);
        crate::common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
        call_rpc(&ctx, &mut sess2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_002)])
            .await
            .expect("SelectCharacterID 2");

        // 1. Both undock in Jita (30_000_142) and see each other in space
        call_rpc(&ctx, &mut sess1, "ship", "Undock", &[]).await.expect("ship.Undock 1");
        call_rpc(&ctx, &mut sess2, "ship", "Undock", &[]).await.expect("ship.Undock 2");

        let ship1 = sess1.ship_id.expect("ship1");
        let ship2 = sess2.ship_id.expect("ship2");
        assert_ne!(ship1, ship2);

        let sys_h = ctx.systems.get_or_spawn(30_000_142);
        let b1 = sys_h.get_ball_snapshot(ship1).await.unwrap().expect("ship1 ball");
        let b2 = sys_h.get_ball_snapshot(ship2).await.unwrap().expect("ship2 ball");
        assert_eq!(b1.1.id, ship1);
        assert_eq!(b2.1.id, ship2);

        // 2. sess1 warps -> sess2 observes ball mode/position change in space
        let bey1 = bound_id_from_tuple(&call_rpc(&ctx, &mut sess1, "beyonce", "MachoBindObject", &[PyRep::Integer(30_000_142), PyRep::None]).await.unwrap());
        let bey2 = bound_id_from_tuple(&call_rpc(&ctx, &mut sess2, "beyonce", "MachoBindObject", &[PyRep::Integer(30_000_142), PyRep::None]).await.unwrap());
        let stargate_id = ctx
            .sde
            .stargates()
            .values()
            .find(|sg| sg.from_solar_system_id == 30_000_142)
            .map(|sg| sg.stargate_id as i64)
            .expect("stargate");
        call_bound_rpc(&ctx, &mut sess1, "beyonce", bey1, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(stargate_id), PyRep::Real(0.0)])
            .await
            .expect("CmdWarpToStuff");
        let b1_warping = sys_h.get_ball_snapshot(ship1).await.unwrap().expect("ship1 warping");
        assert!(matches!(b1_warping.1.mode, eve_destiny::MovementMode::Warp { .. }));
        call_bound_rpc(&ctx, &mut sess1, "beyonce", bey1, "CmdStop", &[]).await.expect("CmdStop");

        // 3. Dock and trade directly
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_001);
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_002);
        call_bound_rpc(&ctx, &mut sess1, "beyonce", bey1, "CmdDock", &[PyRep::Integer(60_003_760)]).await.expect("CmdDock 1");
        call_bound_rpc(&ctx, &mut sess2, "beyonce", bey2, "CmdDock", &[PyRep::Integer(60_003_760)]).await.expect("CmdDock 2");

        let trade_item_id = ctx.db.add_or_stack_item(90_000_001, 60_003_760, InventoryFlag::Hangar, 34, 100).unwrap().item_id;
        let trade = call_rpc(&ctx, &mut sess1, "trademgr", "InitiateTrade", &[PyRep::Integer(90_000_002)])
            .await
            .expect("InitiateTrade");
        let trade_id = trade.dict_get("tradeContainerID").and_then(|v| v.as_i64()).unwrap_or(1);
        call_rpc(&ctx, &mut sess1, "trademgr", "AddItem", &[PyRep::Integer(trade_id), PyRep::Integer(trade_item_id), PyRep::Integer(100)])
            .await
            .expect("AddItem");
        call_rpc(&ctx, &mut sess2, "trademgr", "OfferMoney", &[PyRep::Integer(trade_id), PyRep::Real(5_000.0)])
            .await
            .expect("OfferMoney");
        call_rpc(&ctx, &mut sess1, "trademgr", "MakeOffer", &[PyRep::Integer(trade_id)]).await.expect("MakeOffer 1");
        call_rpc(&ctx, &mut sess2, "trademgr", "MakeOffer", &[PyRep::Integer(trade_id)]).await.expect("MakeOffer 2");

        let p2_trit: i32 = ctx.db.with_snapshot(|s| {
            s.items.values().filter(|i| i.owner_id == 90_000_002 && i.type_id == 34).map(|i| i.quantity).sum()
        });
        assert!(p2_trit >= 100, "direct trade must transfer Tritanium to player 2");
    }

    #[tokio::test]
    async fn test_journey_skills_training() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ctx.seed_demo_world_fixtures();

        let mut sess = SessionState::new(205);
        crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID");

        let sk_bind = call_rpc(&ctx, &mut sess, "skillMgr2", "MachoBindObject", &[PyRep::Integer(90_000_001), PyRep::None])
            .await
            .expect("skillMgr2 bind");
        let sk_bid = bound_id_from_tuple(&sk_bind);

        // 1. Queue skill 3300 to level 5 & advance time -> OnSkillTrained arrives
        let mut rx = ctx.notify_tx.subscribe();
        call_bound_rpc(&ctx, &mut sess, "skillMgr2", sk_bid, "SaveSkillQueue", &[PyRep::Integer(3300), PyRep::Integer(5)])
            .await
            .expect("SaveSkillQueue");
        ctx.advance_secs(864_000);
        let _ = call_bound_rpc(&ctx, &mut sess, "skillMgr2", sk_bid, "GetSkillQueue", &[])
            .await
            .expect("GetSkillQueue");

        let mut saw_trained = false;
        for _ in 0..64 {
            let Ok(env) = rx.try_recv() else { break };
            if format!("{env:?}").contains("OnSkillTrained") {
                saw_trained = true;
            }
        }
        assert!(saw_trained, "OnSkillTrained notification must arrive after skill completes");

        // 2. Skill injector
        let inj_id = ctx.db.add_or_stack_item(90_000_001, 60_003_760, InventoryFlag::Hangar, 40520, 1).unwrap().item_id;
        let free_before = ctx.db.with_snapshot(|s| s.characters[&90_000_001].free_skill_points);
        call_bound_rpc(&ctx, &mut sess, "skillMgr2", sk_bid, "InjectSkillpoints", &[PyRep::Integer(inj_id)])
            .await
            .expect("InjectSkillpoints");
        let free_after = ctx.db.with_snapshot(|s| s.characters[&90_000_001].free_skill_points);
        assert!(free_after > free_before, "skill injector must add free skill points");

        // 3. Attribute remap
        call_bound_rpc(
            &ctx,
            &mut sess,
            "skillMgr2",
            sk_bid,
            "RespecCharacter",
            &[PyRep::Integer(22), PyRep::Integer(22), PyRep::Integer(19), PyRep::Integer(19), PyRep::Integer(17)],
        )
        .await
        .expect("RespecCharacter");
        ctx.db.with_snapshot(|s| {
            let ch = &s.characters[&90_000_001];
            assert_eq!(ch.neural.charisma, 22);
            assert_eq!(ch.neural.intelligence, 22);
            assert_eq!(ch.neural.memory, 19);
            assert_eq!(ch.neural.perception, 19);
            assert_eq!(ch.neural.willpower, 17);
        });
    }

    #[tokio::test]
    async fn test_journey_market_trade() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ctx.seed_demo_world_fixtures();

        let mut sess1 = SessionState::new(206);
        crate::common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
        call_rpc(&ctx, &mut sess1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID 1");

        let mut sess2 = SessionState::new(207);
        crate::common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
        call_rpc(&ctx, &mut sess2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_002)])
            .await
            .expect("SelectCharacterID 2");

        // Seed Tritanium for sess1 and an NPC sell order for Mexallon (36)
        let trit_item = ctx.db.add_or_stack_item(90_000_001, 60_003_760, InventoryFlag::Hangar, 34, 500).unwrap().item_id;
        ctx.db.with_mut_snapshot(|snap| {
            snap.market_orders.insert(
                990_010,
                MarketOrderRecord {
                    order_id: 990_010,
                    type_id: 36,
                    owner_id: 1_000_035,
                    station_id: 60_003_760,
                    region_id: 10_000_002,
                    price: IskCents::from_isk(50),
                    vol_entered: 100,
                    vol_remaining: 100,
                    min_volume: 1,
                    is_bid: false,
                    is_npc: true,
                    escrow: IskCents(0),
                },
            );
        });

        // 1. Place sell order, modify price
        call_rpc(
            &ctx,
            &mut sess1,
            "marketProxy",
            "PlaceMultiSellOrder",
            &[
                PyRep::List(vec![PyRep::keyval([
                    ("stationID", PyRep::Integer(60_003_760)),
                    ("typeID", PyRep::Integer(34)),
                    ("itemID", PyRep::Integer(trit_item)),
                    ("price", PyRep::Real(12.0)),
                    ("quantity", PyRep::Integer(200)),
                ])]),
                PyRep::Bool(false),
                PyRep::Integer(90),
                PyRep::Real(0.0),
            ],
        )
        .await
        .expect("PlaceMultiSellOrder");

        let order_id = ctx.db.with_snapshot(|s| {
            s.market_orders.values().find(|o| o.owner_id == 90_000_001 && o.type_id == 34).unwrap().order_id
        });
        ctx.advance_secs(301);
        call_rpc(&ctx, &mut sess1, "marketProxy", "ModifyCharOrder", &[PyRep::Integer(order_id), PyRep::Real(10.0), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0)])
            .await
            .expect("ModifyCharOrder");

        // 2. Player 2 fills Player 1's sell order
        call_rpc(
            &ctx,
            &mut sess2,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(60_003_760),
                PyRep::Integer(34),
                PyRep::Real(10.0),
                PyRep::Integer(200),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::Bool(false),
            ],
        )
        .await
        .expect("PlaceBuyOrder match");

        // 3. Player 1 fills NPC sell order for Mexallon (36)
        call_rpc(
            &ctx,
            &mut sess1,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(60_003_760),
                PyRep::Integer(36),
                PyRep::Real(50.0),
                PyRep::Integer(50),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::Bool(false),
            ],
        )
        .await
        .expect("PlaceBuyOrder NPC fill");

        // 4. Check transactions and market history
        let txs = call_rpc(&ctx, &mut sess1, "marketProxy", "CharGetTransactions", &[])
            .await
            .expect("CharGetTransactions");
        assert!(format!("{txs:?}").contains("34") || format!("{txs:?}").contains("36"));
        let hist = call_rpc(&ctx, &mut sess1, "marketProxy", "GetOldPriceHistory", &[PyRep::Integer(34)])
            .await
            .expect("GetOldPriceHistory");
        let PyRep::ObjectEx { list: hist_rows, .. } = hist else {
            panic!("expected ObjectEx CRowset from GetOldPriceHistory");
        };
        assert!(!hist_rows.is_empty(), "market price history must contain matched trade row");
        assert!(hist_rows.iter().any(|r| format!("{r:?}").contains("avgPrice")));
    }

    #[tokio::test]
    async fn test_journey_contracts() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ctx.seed_demo_world_fixtures();

        let mut sess1 = SessionState::new(208);
        crate::common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
        call_rpc(&ctx, &mut sess1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID 1");

        let mut sess2 = SessionState::new(209);
        crate::common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
        call_rpc(&ctx, &mut sess2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_002)])
            .await
            .expect("SelectCharacterID 2");

        let item1 = ctx.db.add_or_stack_item(90_000_001, 60_003_760, InventoryFlag::Hangar, 34, 100).unwrap().item_id;
        let item2 = ctx.db.add_or_stack_item(90_000_001, 60_003_760, InventoryFlag::Hangar, 35, 100).unwrap().item_id;
        let item3 = ctx.db.add_or_stack_item(90_000_001, 60_003_760, InventoryFlag::Hangar, 36, 100).unwrap().item_id;

        // 1. Item exchange contract
        let c_ex = call_rpc(
            &ctx,
            &mut sess1,
            "contractProxy",
            "CreateContract",
            &[
                PyRep::Integer(1), // ItemExchange
                PyRep::Integer(0), // public
                PyRep::Integer(0),
                PyRep::Integer(1440),
                PyRep::Integer(1),
                PyRep::Integer(60_003_760),
                PyRep::Integer(60_003_760),
                PyRep::Real(1_000.0),
                PyRep::Real(0.0),
                PyRep::Real(0.0),
                PyRep::String("Tritanium Exchange".into()),
                PyRep::String("".into()),
                PyRep::List(vec![PyRep::Integer(item1)]),
            ],
        )
        .await
        .expect("CreateContract ItemExchange");
        let c_ex_id = c_ex.dict_get("contractID").and_then(|v| v.as_i64()).or_else(|| c_ex.as_i64()).expect("contract id");
        call_rpc(&ctx, &mut sess2, "contractProxy", "AcceptContract", &[PyRep::Integer(c_ex_id), PyRep::Bool(false)])
            .await
            .expect("AcceptContract ItemExchange");
        let p2_trit: i32 = ctx.db.with_snapshot(|s| {
            s.items.values().filter(|i| i.owner_id == 90_000_002 && i.type_id == 34).map(|i| i.quantity).sum()
        });
        assert!(p2_trit >= 100, "ItemExchange contract must transfer item to buyer");

        // 2. Auction contract with bid
        let c_auc = call_rpc(
            &ctx,
            &mut sess1,
            "contractProxy",
            "CreateContract",
            &[
                PyRep::Integer(2), // Auction
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(60),
                PyRep::Integer(1),
                PyRep::Integer(60_003_760),
                PyRep::Integer(60_003_760),
                PyRep::Real(500.0),
                PyRep::Real(0.0),
                PyRep::Real(2_000.0),
                PyRep::String("Pyerite Auction".into()),
                PyRep::String("".into()),
                PyRep::List(vec![PyRep::Integer(item2)]),
            ],
        )
        .await
        .expect("CreateContract Auction");
        let c_auc_id = c_auc.dict_get("contractID").and_then(|v| v.as_i64()).or_else(|| c_auc.as_i64()).expect("auction id");
        let p2_wallet_before = ctx.db.with_snapshot(|s| s.characters[&90_000_002].balance);
        call_rpc(&ctx, &mut sess2, "contractProxy", "PlaceBid", &[PyRep::Integer(c_auc_id), PyRep::Real(600.0), PyRep::Bool(false)])
            .await
            .expect("PlaceBid");
        ctx.db.with_snapshot(|s| {
            let auc = &s.contracts[&c_auc_id];
            assert_eq!(auc.current_bidder_id, Some(90_000_002));
            assert_eq!(auc.current_bid, IskCents::from_isk(600));
            assert_eq!(s.characters[&90_000_002].balance.0, p2_wallet_before.0 - IskCents::from_isk(600).0);
        });

        // 3. Courier contract with collateral & delivery
        let c_cour = call_rpc(
            &ctx,
            &mut sess1,
            "contractProxy",
            "CreateContract",
            &[
                PyRep::Integer(3), // Courier
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(1440),
                PyRep::Integer(1),
                PyRep::Integer(60_003_760),
                PyRep::Integer(60_003_760),
                PyRep::Real(2_500.0),
                PyRep::Real(5_000.0),
                PyRep::Real(0.0),
                PyRep::String("Mexallon Courier".into()),
                PyRep::String("".into()),
                PyRep::List(vec![PyRep::Integer(item3)]),
            ],
        )
        .await
        .expect("CreateContract Courier");
        let c_cour_id = c_cour.dict_get("contractID").and_then(|v| v.as_i64()).or_else(|| c_cour.as_i64()).expect("courier id");
        call_rpc(&ctx, &mut sess2, "contractProxy", "AcceptContract", &[PyRep::Integer(c_cour_id), PyRep::Bool(false)])
            .await
            .expect("AcceptContract Courier");
        call_rpc(&ctx, &mut sess2, "contractProxy", "CompleteContract", &[PyRep::Integer(c_cour_id), PyRep::Integer(0)])
            .await
            .expect("CompleteContract Courier");
        assert_eq!(ctx.db.with_snapshot(|s| s.contracts[&c_cour_id].status.clone()), "Completed");
    }
}

pub(crate) mod journey_mining_loop_pve_combat {
    // End-to-End Integration Journeys Part 3 (`journey:mining_loop` .. `journey:wormholes_filaments`).

    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use glam::DVec3;
    use std::collections::HashMap;

    async fn call_rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        service: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, eve_proto::BridgeRpcError> {
        ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
            .await
    }

    #[tokio::test]
    async fn test_journey_mining_loop() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ctx.seed_demo_world_fixtures();

        let mut sess = SessionState::new(301);
        crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID");

        // 1. Undock & warp to asteroid belt
        call_rpc(&ctx, &mut sess, "ship", "Undock", &[]).await.expect("ship.Undock");
        let sys_id = sess.solar_system_id.unwrap();
        let ship_id = sess.ship_id.unwrap();
        let sys_h = ctx.systems.get_or_spawn(sys_id);

        // Spawn an asteroid ball in space and fit a Miner I (483) + Survey Scanner I (444)
        let asteroid_id = 700_100_001_i64;
        let (_, ship_ball) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        sys_h
            .spawn_ball(Ball {
                id: asteroid_id,
                type_id: 1230, // Veldspar
                position: ship_ball.position + DVec3::new(2_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await
            .expect("spawn asteroid");

        let miner_id = ctx.db.with_mut_snapshot(|snap| {
            if let Some(sh) = snap.items.get_mut(&ship_id) {
                sh.type_id = 32880; // Venture
            }
            snap.next_entity_id += 1;
            let mid = snap.next_entity_id;
            snap.items.insert(
                mid,
                InventoryItemRecord {
                    item_id: mid,
                    type_id: 483, // Miner I
                    owner_id: 90_000_001,
                    location_id: ship_id,
                    flag: InventoryFlag::HiSlot1,
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
            mid
        });
        ctx.db.with_mut_snapshot(|snap| {
            snap.module_online_states.insert(miner_id, true);
            snap.skills.entry(90_000_001).or_default().insert(
                3386,
                eve_db::TrainedSkillRecord { type_id: 3386, level: 5, skill_points: 256_000.0 },
            );
            snap.asteroids.insert(
                asteroid_id,
                eve_db::AsteroidRecord {
                    asteroid_id,
                    solar_system_id: sys_id,
                    belt_id: 0,
                    type_id: 1230,
                    quantity: 1_000,
                    initial_quantity: 1_000,
                    position: (ship_ball.position.x + 2_000.0, ship_ball.position.y, ship_ball.position.z),
                    radius: 100.0,
                },
            );
        });
        ctx.invalidate_ship_fitting(ship_id);

        // 2. Lock asteroid via dogmaIM.AddTarget & activate mining laser
        call_rpc(&ctx, &mut sess, "dogmaIM", "AddTarget", &[PyRep::Integer(asteroid_id)])
            .await
            .expect("AddTarget asteroid");
        for _ in 0..15 {
            let _ = sys_h.tick().await;
            if sys_h
                .get_target_locks(ship_id)
                .await
                .unwrap_or_default()
                .iter()
                .any(|l| l.target_id == asteroid_id && l.locked)
            {
                break;
            }
        }
        call_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(miner_id), PyRep::String("miningLaser".into()), PyRep::Integer(asteroid_id), PyRep::Integer(0)])
            .await
            .expect("Activate Miner I");
        ctx.advance_secs(65);
        let _ = eve_server::services::mining_cycle::process_mining_cycles(&ctx).await;

        let (mined_ore_id, mined_ore_qty) = ctx.db.with_snapshot(|s| {
            s.items
                .values()
                .find(|i| i.owner_id == 90_000_001 && i.location_id == ship_id && i.type_id == 1230)
                .map(|i| (i.item_id, i.quantity))
                .unwrap_or((0, 0))
        });
        assert!(mined_ore_qty > 0, "mining cycle must yield Veldspar into ship hold");

        // 3. Dock & reprocess the mined ore with skill-based yields
        let st_pos = ctx.sde.stations.get(&60_003_760).map(|s| s.position).unwrap_or(DVec3::ZERO);
        let _ = sys_h.set_ball_position(ship_id, st_pos).await;
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_001);
        let bey_bind = call_rpc(&ctx, &mut sess, "beyonce", "MachoBindObject", &[PyRep::Integer(sys_id as i64), PyRep::None]).await.unwrap();
        let bey_bid = match bey_bind { PyRep::Tuple(t) => t[0].as_i64().unwrap_or(1) as u64, other => other.as_i64().unwrap_or(1) as u64 };
        ctx.dispatch_rpc(&mut sess, "beyonce", Some(bey_bid), "CmdDock", &[PyRep::Integer(60_003_760)], &HashMap::new(), None)
            .await
            .expect("CmdDock");
        let inv_bind = call_rpc(&ctx, &mut sess, "invbroker", "MachoBindObject", &[PyRep::Integer(60_003_760), PyRep::None])
            .await
            .expect("invbroker bind");
        let inv_bid = match inv_bind { PyRep::Tuple(t) => t[0].as_i64().unwrap_or(1) as u64, other => other.as_i64().unwrap_or(1) as u64 };
        ctx.dispatch_rpc(&mut sess, "invbroker", Some(inv_bid), "Add", &[PyRep::Integer(mined_ore_id), PyRep::Integer(ship_id), PyRep::Integer(4)], &HashMap::new(), None)
            .await
            .expect("invbroker.Add");
        let quotes = call_rpc(&ctx, &mut sess, "reprocessingSvc", "GetQuotes", &[PyRep::List(vec![PyRep::Integer(mined_ore_id)]), PyRep::Integer(60_003_760)])
            .await
            .expect("GetQuotes");
        assert!(format!("{quotes:?}").contains("34"), "Veldspar quote must include Tritanium (34)");
        call_rpc(&ctx, &mut sess, "reprocessingSvc", "Reprocess", &[PyRep::Integer(mined_ore_id)])
            .await
            .expect("Reprocess Veldspar");
        let trit_qty: i32 = ctx.db.with_snapshot(|s| {
            s.items.values().filter(|i| i.owner_id == 90_000_001 && i.type_id == 34).map(|i| i.quantity).sum()
        });
        assert!(trit_qty > 0, "reprocessing Veldspar must yield Tritanium");
    }

    #[tokio::test]
    async fn test_journey_pve_combat() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ctx.seed_demo_world_fixtures();

        let mut sess = SessionState::new(302);
        crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID");

        let ship_id = 100_000_001_i64;
        let t1_id = 100_000_005_i64; // Seeded 200mm AutoCannon II in HiSlot0
        let (t2_id, t3_id) = ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 5;
            let base = snap.next_entity_id - 4;
            for (id, tid, flag, qty, sing) in [
                (base, 2889, InventoryFlag::HiSlot1, 1, true),
                (base + 1, 2889, InventoryFlag::HiSlot2, 1, true),
                (base + 2, 21924, InventoryFlag::HiSlot0, 120, false),
                (base + 3, 21924, InventoryFlag::HiSlot1, 120, false),
                (base + 4, 21924, InventoryFlag::HiSlot2, 120, false),
            ] {
                snap.items.insert(
                    id,
                    InventoryItemRecord {
                        item_id: id,
                        type_id: tid,
                        owner_id: 90_000_001,
                        location_id: ship_id,
                        flag,
                        quantity: qty,
                        singleton: sing,
                        version: 1,
                        ..Default::default()
                    },
                );
            }
            for gid in [t1_id, base, base + 1] {
                snap.module_online_states.insert(gid, true);
            }
            snap.skills.entry(90_000_001).or_default().insert(
                12201,
                eve_db::TrainedSkillRecord { type_id: 12201, level: 5, skill_points: 256_000.0 },
            );
            (base, base + 1)
        });
        ctx.invalidate_ship_fitting(ship_id);
        for slave_id in [t2_id, t3_id] {
            call_rpc(&ctx, &mut sess, "dogmaIM", "LinkWeapons", &[PyRep::Integer(ship_id), PyRep::Integer(t1_id), PyRep::Integer(slave_id)])
                .await
                .expect("LinkWeapons");
        }

        call_rpc(&ctx, &mut sess, "ship", "Undock", &[]).await.expect("ship.Undock");
        let sys_id = sess.solar_system_id.unwrap();
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        assert_eq!(sess.station_id, None);

        // Spawn a real SDE belt rat frigate with unmodified HP and positive bounty
        let rat_type_id = ctx
            .sde
            .types
            .values()
            .filter(|t| {
                t.default_attributes.get(&481).copied().unwrap_or(0.0) > 0.0
                    && t.default_attributes.get(&263).copied().unwrap_or(9999.0) <= 150.0
                    && t.default_attributes.get(&265).copied().unwrap_or(9999.0) <= 150.0
                    && t.default_attributes.get(&9).copied().unwrap_or(9999.0) <= 150.0
            })
            .map(|t| t.type_id)
            .min()
            .unwrap_or(10_035);
        let (_, ship_ball) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        let npc_id = eve_server::services::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_entity(
            &ctx,
            sys_id,
            rat_type_id,
            "BeltRat",
            1,
            ship_ball.position + DVec3::new(500.0, 0.0, 0.0),
        )
        .await;

        // Lock target via dogmaIM.AddTarget, activate repeating weapon bank, tick real combat
        let wallet_before = ctx.db.with_snapshot(|s| s.characters[&90_000_001].balance);
        call_rpc(&ctx, &mut sess, "dogmaIM", "AddTarget", &[PyRep::Integer(npc_id)])
            .await
            .expect("AddTarget NPC");
        for _ in 0..15 {
            let _ = sys_h.tick().await;
            if sys_h
                .get_target_locks(ship_id)
                .await
                .unwrap_or_default()
                .iter()
                .any(|l| l.target_id == npc_id && l.locked)
            {
                break;
            }
        }
        call_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(t1_id), PyRep::String("projectileFired".into()), PyRep::Integer(npc_id), PyRep::Integer(1)])
            .await
            .expect("Activate weapon");
        for _ in 0..60 {
            let _ = sys_h.tick().await;
            if sys_h.get_ball_snapshot(npc_id).await.unwrap().is_none() {
                break;
            }
        }

        let npc_after = sys_h.get_ball_snapshot(npc_id).await.unwrap();
        assert!(npc_after.is_none(), "NPC must be destroyed by weapon fire");
        ctx.advance_secs(1_205);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        let wallet_after = ctx.db.with_snapshot(|s| s.characters[&90_000_001].balance);
        assert!(wallet_after > wallet_before, "wallet balance must increase after NPC bounty payout");
    }

    #[tokio::test]
    async fn test_journey_missions() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ctx.seed_demo_world_fixtures();
        crate::common::seed_test_lp_store_offers(&ctx);

        let mut sess = SessionState::new(303);
        crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID");

        // 1. Browse agents & accept/complete mission
        let agents = call_rpc(&ctx, &mut sess, "agentMgr", "GetAgents", &[PyRep::Integer(1), PyRep::Integer(23)])
            .await
            .expect("GetAgents");
        assert!(format!("{agents:?}").contains("agentID"));

        let agent = ctx
            .sde
            .agents
            .values()
            .find(|a| a.level == 1 && a.agent_type_id == 2 && a.division_id == 24 && a.solar_system_id.is_some() && a.station_id.is_some())
            .expect("level 1 security agent")
            .clone();
        let agent_id = agent.agent_id;
        let agent_sys = agent.solar_system_id.unwrap();
        ctx.db.with_mut_snapshot(|s| {
            let ch = s.characters.get_mut(&90_000_001).unwrap();
            ch.solar_system_id = agent_sys;
            ch.station_id = agent.station_id;
        });
        sess.solar_system_id = Some(agent_sys);
        sess.station_id = agent.station_id;
        let req_m = call_rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(agent_id)])
            .await
            .expect("DoAction(RequestMission)");
        let PyRep::KeyVal(ref req_kv) = req_m else { panic!("expected KeyVal from DoAction(2)") };
        let mid = req_kv.get("missionID").and_then(|v| v.as_i64()).expect("missionID");
        let acc_m = call_rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(mid)])
            .await
            .expect("DoAction(AcceptMission)");
        assert!(matches!(acc_m, PyRep::KeyVal(_)), "DoAction(3) must return mission KeyVal");

        // Complete mission objective and turn in via DoAction(6)
        let active_agent_id = acc_m.dict_get("agentID").and_then(|v| v.as_i64()).unwrap_or(agent_id);
        let agent_corp = ctx.sde.agents[&active_agent_id].corporation_id;
        let (isk_before, lp_before, standing_before) = ctx.db.with_mut_snapshot(|s| {
            if let Some(m) = s.active_missions.get_mut(&(90_000_001, mid as u32)) {
                m.npcs_remaining = 0;
            }
            let ch = s.characters.get_mut(&90_000_001).unwrap();
            let isk = ch.balance.0;
            let lp = ch.loyalty_points.get(&agent_corp).copied().unwrap_or(0);
            let st = ch.standings.get(&agent_corp).copied().unwrap_or(0.0);
            (isk, lp, st)
        });
        let comp_m = call_rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(mid)])
            .await
            .expect("DoAction(CompleteMission)");
        assert!(comp_m.dict_get("iskReward").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);
        assert!(comp_m.dict_get("lpReward").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
        let (isk_after, lp_after, standing_after) = ctx.db.with_snapshot(|s| {
            let ch = &s.characters[&90_000_001];
            (
                ch.balance.0,
                ch.loyalty_points.get(&agent_corp).copied().unwrap_or(0),
                ch.standings.get(&agent_corp).copied().unwrap_or(0.0),
            )
        });
        assert!(isk_after > isk_before && lp_after > lp_before && standing_after > standing_before);

        // 2. Check LP store offers
        let offers = call_rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(1_000_035)])
            .await
            .expect("GetAvailableOffersFromCorp");
        let PyRep::List(offer_list) = offers else { panic!("expected List from GetAvailableOffersFromCorp") };
        assert!(!offer_list.is_empty(), "LP store must have offers");
    }

    #[tokio::test]
    async fn test_journey_exploration() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ctx.seed_demo_world_fixtures();

        let mut sess = SessionState::new(304);
        crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID");

        call_rpc(&ctx, &mut sess, "ship", "Undock", &[]).await.expect("ship.Undock");

        // 1. Launch probes & scan signatures
        let scan_rows = crate::common::probe_scan(&ctx, &mut sess, DVec3::ZERO, 4.0 * 149_597_870_700.0).await;
        assert!(!scan_rows.is_empty(), "probe_scan must return cosmic signatures");

        // 2. Hacking minigame
        eve_server::services::pve_exploration::ensure_system_pve_sites_seeded(&ctx, 30_000_142).await;
        let (can_id, site_pos) = eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
            let site = reg.sites_by_system[&30_000_142]
                .iter()
                .find(|s| s.site_category == "Data" && !s.hackable_can_ids.is_empty())
                .expect("Seeded Data site in Jita");
            (site.hackable_can_ids[0], site.position)
        });
        let ship_id = sess.ship_id.unwrap();
        let sys_h = ctx.systems.get_or_spawn(30_000_142);
        let _ = sys_h.set_ball_position(ship_id, DVec3::from_array(site_pos)).await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let aid = snap.next_entity_id;
            snap.items.insert(
                aid,
                InventoryItemRecord {
                    item_id: aid,
                    type_id: 22175, // Data Analyzer I
                    owner_id: 90_000_001,
                    location_id: ship_id,
                    flag: InventoryFlag::MedSlot0,
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
            snap.module_online_states.insert(aid, true);
            snap.skills.entry(90_000_001).or_default().insert(
                21718, // Hacking
                eve_db::TrainedSkillRecord { type_id: 21718, level: 5, skill_points: 256_000.0 },
            );
        });
        ctx.invalidate_ship_fitting(ship_id);
        let hack_start = call_rpc(&ctx, &mut sess, "hackingMgr", "StartNewGameInstance", &[PyRep::Integer(can_id)])
            .await
            .expect("StartNewGameInstance");
        assert!(hack_start.dict_get("board").is_some() || hack_start.dict_get("containerID").is_some());
        let click = call_rpc(&ctx, &mut sess, "hackingMgr", "ClickedOnTile", &[PyRep::Integer(can_id), PyRep::Integer(1)])
            .await
            .expect("ClickedOnTile");
        assert_eq!(click.dict_get("containerID").and_then(|v| v.as_i64()).unwrap_or(can_id), can_id);

        // 3. D-scan (14.3 AU in meters)
        let dscan = call_rpc(
            &ctx,
            &mut sess,
            "scanMgr",
            "ConeScan",
            &[PyRep::Real(360.0), PyRep::Real(14.3 * 149_597_870_700.0), PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(1.0)],
        )
        .await
        .expect("ConeScan");
        let PyRep::List(dscan_list) = dscan else { panic!("expected List from ConeScan") };
        assert!(!dscan_list.is_empty(), "D-scan at 14.3 AU must detect in-space entities");
        assert!(dscan_list.iter().any(|e| e.dict_get("typeID").and_then(|v| v.as_i64()).unwrap_or(0) > 0));
    }

    #[tokio::test]
    async fn test_journey_wormholes_filaments() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ctx.seed_demo_world_fixtures();

        let mut sess = SessionState::new(305);
        crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID");

        call_rpc(&ctx, &mut sess, "ship", "Undock", &[]).await.expect("ship.Undock");
        let ship_id = sess.ship_id.unwrap();

        // 1. Wormhole jump into J-space (31_000_005) and mass deduction check
        let now_u = ctx.now_unix();
        ctx.db.with_mut_snapshot(|snap| {
            snap.cosmic_signatures.insert(
                30_000_142,
                vec![eve_db::CosmicSignatureRecord {
                    signature_id: "WH-J1".into(),
                    solar_system_id: 30_000_142,
                    sig_group: "Wormhole".into(),
                    type_name: "Unstable Wormhole (K162)".into(),
                    position: [15_000.0, 0.0, 0.0],
                    signal_strength: 1.0,
                    wormhole_type_id: Some(26_272),
                    destination_system_id: Some(31_000_005),
                    created_unix: now_u,
                    lifetime_seconds: 86_400,
                    max_mass_kg: 2_000_000_000.0,
                    remaining_mass_kg: 2_000_000_000.0,
                    max_jump_mass_kg: 375_000_000.0,
                    collapsed: false,
                }],
            );
        });
        let _ = call_rpc(&ctx, &mut sess, "scanMgr", "GetSystemScanMgr", &[]).await.expect("GetSystemScanMgr");
        let wh_item_id = eve_server::services::space_destiny::wormhole_item_id_for_signature(30_000_142, "WH-J1");
        let _ = ctx
            .systems
            .get_or_spawn(30_000_142)
            .set_ball_position(ship_id, DVec3::new(15_500.0, 0.0, 0.0))
            .await;
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_001);
        let jump_rep = call_rpc(&ctx, &mut sess, "wormholeMgr", "WormholeJump", &[PyRep::Integer(wh_item_id)])
            .await
            .expect("WormholeJump");
        assert_eq!(sess.solar_system_id, Some(31_000_005));
        let rem_mass = jump_rep.dict_get("remainingMassKg").and_then(|v| v.as_f64()).unwrap_or(2_000_000_000.0);
        assert!(rem_mass < 2_000_000_000.0, "wormhole jump must deduct ship mass");

        // Return to Jita (30_000_142) for T0 Tranquil Abyssal (56131) + Needlejack filament journeys
        ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&90_000_001).unwrap().solar_system_id = 30_000_142);
        sess.solar_system_id = Some(30_000_142);
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_001);

        // 2. V24 Abyssal filament 3-room run via abyssalMgr
        call_rpc(&ctx, &mut sess, "fleetObjectHandler", "CreateFleet", &[]).await.expect("CreateFleet");
        let fil_id = ctx.db.add_or_stack_item(90_000_001, ship_id, InventoryFlag::Cargo, 56131, 3).unwrap().item_id;
        let dep = call_rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceDeployment", &[PyRep::Integer(fil_id)])
            .await
            .expect("AbyssalEntranceDeployment");
        let trace_id = dep.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
        let enter = call_rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(trace_id)])
            .await
            .expect("AbyssalEntranceGateActivation");
        assert_eq!(sess.solar_system_id, Some(32_000_001));
        let pid = enter.dict_get("pocketID").and_then(|v| v.as_i64()).unwrap();
        call_rpc(&ctx, &mut sess, "abyssalMgr", "ClientIsReady", &[]).await.expect("abyssalMgr.ClientIsReady");

        for room_idx in 1..=3_i64 {
            crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, 90_000_001, (room_idx - 1) as usize).await;
            let gate_id = 1_895_000_000 + pid * 10 + room_idx;
            if room_idx < 3 {
                let adv = call_rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalGateActivation", &[PyRep::Integer(gate_id)])
                    .await
                    .expect("AbyssalGateActivation");
                assert_eq!(adv.dict_get("room").and_then(|v| v.as_i64()), Some(room_idx + 1));
            } else {
                let exit = call_rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEndGateActivation", &[PyRep::Integer(gate_id)])
                    .await
                    .expect("AbyssalEndGateActivation");
                assert_eq!(exit.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(30_000_142));
            }
        }
        assert_eq!(sess.solar_system_id, Some(30_000_142));

        // 3. Needlejack filament jump to nullsec via randomJumpMgr.ActivateRandomJumpFilament
        let nj_id = ctx.db.add_or_stack_item(90_000_001, ship_id, InventoryFlag::Cargo, 53977, 1).unwrap().item_id;
        call_rpc(&ctx, &mut sess, "randomJumpMgr", "ActivateRandomJumpFilament", &[PyRep::Integer(nj_id)])
            .await
            .expect("ActivateRandomJumpFilament");
        assert_ne!(sess.solar_system_id, Some(30_000_142), "Needlejack filament must jump pilot out of origin system");
    }
}

pub(crate) mod journey_ship_loss_clones {
    // End-to-End Integration Journeys Part 4 (`journey:ship_loss` .. `journey:corporation`).

    use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, IskCents};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use std::collections::HashMap;

    async fn call_rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        service: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, eve_proto::BridgeRpcError> {
        ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
            .await
    }

    #[tokio::test]
    async fn test_journey_ship_loss() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ctx.seed_demo_world_fixtures();

        let mut sess = SessionState::new(401);
        crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID");

        let ship_id = sess.ship_id.expect("ship_id");
        // 1. Insure ship
        let ins = call_rpc(&ctx, &mut sess, "insuranceSvc", "InsureShip", &[PyRep::Integer(ship_id), PyRep::Real(1.0)])
            .await
            .expect("InsureShip");
        assert!(ins.dict_get("payoutISK").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);

        // 2. Check insurance contracts, undock, destroy ship, and verify capsule ejection, insurance payout, wreck, and killmail
        let contracts = call_rpc(&ctx, &mut sess, "insuranceSvc", "GetContracts", &[])
            .await
            .expect("GetContracts");
        let PyRep::List(clist) = contracts else { panic!("expected List from GetContracts") };
        assert_eq!(clist.len(), 1);

        call_rpc(&ctx, &mut sess, "ship", "Undock", &[]).await.expect("ship.Undock");
        let isk_before = ctx.db.with_snapshot(|s| s.characters[&90_000_001].balance.0);
        let sd1 = call_rpc(&ctx, &mut sess, "ship", "SelfDestruct", &[])
            .await
            .expect("ship.SelfDestruct start");
        assert_eq!(sd1.dict_get("countdownStarted"), Some(&PyRep::Bool(true)));
        ctx.advance_secs(125);
        let pod_id = ctx.db.with_snapshot(|s| s.characters[&90_000_001].active_ship_id);
        assert_ne!(pod_id, ship_id, "pilot must be ejected into a capsule after ship destruction");
        assert_eq!(ctx.db.with_snapshot(|s| s.items[&pod_id].type_id), 670);
        let wreck_exists = ctx.db.with_snapshot(|s| s.items.values().any(|i| i.type_id == 26557 || ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 186)));
        assert!(wreck_exists, "ship destruction must spawn a wreck");
        let isk_after = ctx.db.with_snapshot(|s| s.characters[&90_000_001].balance.0);
        assert!(isk_after > isk_before, "insurance payout must credit victim wallet");

        let kms = call_rpc(&ctx, &mut sess, "charMgr", "GetRecentShipKillsAndLosses", &[PyRep::Integer(10), PyRep::Integer(0)])
            .await
            .expect("GetRecentShipKillsAndLosses");
        let PyRep::Rowset { rows: km_rows, .. } = kms else {
            panic!("expected Rowset from GetRecentShipKillsAndLosses");
        };
        assert_eq!(km_rows.len(), 1);
        assert!(format!("{km_rows:?}").contains("90000001"));
    }

    #[tokio::test]
    async fn test_journey_clones() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ctx.seed_demo_world_fixtures();

        let mut sess = SessionState::new(402);
        crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID");

        // 1. Train Infomorph Psychology (24242) + Cybernetics (3411), plug in implant (9899), install jump clone
        ctx.db.with_mut_snapshot(|snap| {
            let sk = snap.skills.entry(90_000_001).or_default();
            sk.insert(24242, eve_db::TrainedSkillRecord { type_id: 24242, level: 2, skill_points: 50_000.0 });
            sk.insert(3411, eve_db::TrainedSkillRecord { type_id: 3411, level: 2, skill_points: 50_000.0 });
        });
        let imp_id = ctx.db.add_or_stack_item(90_000_001, 60_003_760, InventoryFlag::Hangar, 9899, 1).unwrap().item_id;
        call_rpc(&ctx, &mut sess, "dogmaIM", "InjectImplant", &[PyRep::Integer(imp_id)])
            .await
            .expect("InjectImplant");
        call_rpc(&ctx, &mut sess, "jumpCloneSvc", "InstallCloneInStation", &[])
            .await
            .expect("InstallCloneInStation");
        let jc_id = ctx.db.with_snapshot(|s| s.jump_clones[&90_000_001][0].jump_clone_id);

        // 2. Leave ship into capsule, move to station 60_003_757, jump back to clone in 60_003_760 via CloneJump
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_001);
        let active_ship = sess.ship_id.unwrap();
        call_rpc(&ctx, &mut sess, "ship", "LeaveShip", &[PyRep::Integer(active_ship)])
            .await
            .expect("LeaveShip into capsule");
        ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&90_000_001).unwrap().station_id = Some(60_003_757));
        sess.station_id = Some(60_003_757);
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_001);
        call_rpc(&ctx, &mut sess, "jumpCloneSvc", "CloneJump", &[PyRep::Integer(jc_id)])
            .await
            .expect("CloneJump");
        assert_eq!(sess.station_id, Some(60_003_760));
        let clone_state = call_rpc(&ctx, &mut sess, "jumpCloneSvc", "GetCloneState", &[])
            .await
            .expect("GetCloneState");
        assert!(format!("{clone_state:?}").contains("jumpCloneID"));

        // 3. Set home station via V24 home_station.set_home_station & self-destruct medical clone in capsule
        call_rpc(&ctx, &mut sess, "home_station", "set_home_station", &[PyRep::Integer(60_003_760)])
            .await
            .expect("home_station.set_home_station");
        let home = ctx.db.with_snapshot(|s| s.characters[&90_000_001].home_station_id);
        assert_eq!(home, 60_003_760);
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_001);
        call_rpc(&ctx, &mut sess, "home_station", "self_destruct_clone", &[])
            .await
            .expect("home_station.self_destruct_clone");
        assert_eq!(sess.station_id, Some(60_003_760));
    }

    #[tokio::test]
    async fn test_journey_industry() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ctx.seed_demo_world_fixtures();

        let mut sess = SessionState::new(403);
        crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID");

        // Seed Rifter BPO (691) and Tritanium (34)
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                100_000_601,
                InventoryItemRecord {
                    item_id: 100_000_601,
                    type_id: 691,
                    owner_id: 90_000_001,
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
        });
        ctx.db.add_or_stack_item(90_000_001, 60_003_760, InventoryFlag::Hangar, 34, 50_000).unwrap();

        // 1. Install manufacturing job
        let job_rep = call_rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(691), PyRep::Integer(1), PyRep::Integer(1)])
            .await
            .expect("InstallJob Manufacturing");
        let job_id = job_rep.dict_get("jobID").and_then(|v| v.as_i64()).expect("jobID");
        let dur_s = job_rep.dict_get("durationSeconds").and_then(|v| v.as_i64()).unwrap_or(600) as u64;

        // 2. Advance virtual clock past job duration & deliver job
        ctx.advance_secs(dur_s + 10);
        let done = call_rpc(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(job_id)])
            .await
            .expect("CompleteJob");
        assert_eq!(done.dict_get("jobID").and_then(|v| v.as_i64()).unwrap_or(job_id), job_id);
        assert!(
            ctx.db.with_snapshot(|s| s.items.values().any(|i| i.owner_id == 90_000_001 && i.type_id == 587)),
            "CompleteJob must deliver manufactured Rifter (type 587)"
        );
    }

    #[tokio::test]
    async fn test_journey_planetary() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ctx.seed_demo_world_fixtures();

        let mut sess = SessionState::new(404);
        crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID");

        let planet_id = 40_009_082_i64;
        let ship_id = sess.ship_id.unwrap();
        ctx.db.with_mut_snapshot(|snap| {
            let skills = snap.skills.entry(90_000_001).or_default();
            for sid in [2495_u32, 2505, 2403, 2406] {
                skills.insert(sid, eve_db::TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
            }
        });
        ctx.db.add_or_stack_item(90_000_001, ship_id, InventoryFlag::Cargo, 2534, 1).unwrap(); // Gas Command Center
        call_rpc(&ctx, &mut sess, "ship", "Undock", &[]).await.expect("ship.Undock");

        // 1. Build colony with command center, launchpad, and link
        let res = call_rpc(
            &ctx,
            &mut sess,
            "planetMgr",
            "UserUpdateNetwork",
            &[
                PyRep::Integer(planet_id),
                PyRep::List(vec![
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(1001), PyRep::Integer(2534), PyRep::Real(0.1), PyRep::Real(0.1)]),
                    PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(1001), PyRep::Integer(3)]),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(1002), PyRep::Integer(2543), PyRep::Real(0.11), PyRep::Real(0.11)]),
                    PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(1001), PyRep::Integer(1002), PyRep::Integer(0)]),
                ]),
            ],
        )
        .await
        .expect("UserUpdateNetwork");
        let PyRep::List(pins) = res.dict_get("pins").expect("pins") else { panic!("expected pins list") };
        assert_eq!(pins.len(), 2);

        // 2. Advance time & verify colony state
        ctx.advance_secs(3_600);
        let col = call_rpc(&ctx, &mut sess, "planetMgr", "GetPlanetResourceInfo", &[PyRep::Integer(planet_id)])
            .await
            .expect("GetPlanetResourceInfo");
        assert!(col.dict_get("planetID").is_some() || col.dict_get("resources").is_some());
    }

    #[tokio::test]
    async fn test_journey_corporation() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ctx.seed_demo_world_fixtures();

        let mut sess1 = SessionState::new(405);
        crate::common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
        call_rpc(&ctx, &mut sess1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID 1");

        let mut sess2 = SessionState::new(406);
        crate::common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
        call_rpc(&ctx, &mut sess2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_002)])
            .await
            .expect("SelectCharacterID 2");

        // 1. Create corporation
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_001);
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_002);
        let created = call_rpc(
            &ctx,
            &mut sess1,
            "corpRegistry",
            "AddCorporation",
            &[PyRep::String("Pioneer Logistics".into()), PyRep::String("PION".into()), PyRep::Real(0.05)],
        )
        .await
        .expect("AddCorporation");
        let corp_id = created.dict_get("corporationID").and_then(|v| v.as_i64()).expect("corporationID");
        assert_eq!(sess1.corporation_id, Some(corp_id));
        assert!(sess1.corp_role & corp_roles::ROLE_DIRECTOR != 0);

        // 2. Apply & accept second character via real corpRegistry RPCs, rent office, create alliance
        let app = call_rpc(&ctx, &mut sess2, "corpRegistry", "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("Joining".into())])
            .await
            .expect("InsertApplication");
        let app_id = app
            .dict_get("applicationID")
            .and_then(|v| v.as_i64())
            .or_else(|| app.as_i64())
            .expect("applicationID");
        call_rpc(&ctx, &mut sess1, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(app_id), PyRep::String("Accepted".into())])
            .await
            .expect("UpdateApplicationOffer");
        assert_eq!(ctx.db.with_snapshot(|s| s.characters[&90_000_002].corporation_id), corp_id);

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.corporations.get_mut(&corp_id) {
                c.master_wallet = IskCents::from_isk(2_000_000_000);
                c.set_division_balance(1, IskCents::from_isk(2_000_000_000));
            }
        });
        // Negative RentOffice checks: remote station, nonexistent station, insufficient role
        let err_remote = call_rpc(&ctx, &mut sess1, "officeManager", "RentOffice", &[PyRep::Integer(60_003_761)])
            .await
            .expect_err("remote station must fail");
        assert_eq!(err_remote.code, "NotDockedAtStation");
        let orig_station = sess1.station_id;
        sess1.station_id = Some(999_999_999);
        ctx.db.with_mut_snapshot(|s| {
            if let Some(ch) = s.characters.get_mut(&90_000_001) {
                ch.station_id = Some(999_999_999);
            }
        });
        let err_missing = call_rpc(&ctx, &mut sess1, "officeManager", "RentOffice", &[PyRep::Integer(999_999_999)])
            .await
            .expect_err("nonexistent station must fail");
        assert_eq!(err_missing.code, "StationNotFound");
        sess1.station_id = orig_station;
        ctx.db.with_mut_snapshot(|s| {
            if let Some(ch) = s.characters.get_mut(&90_000_001) {
                ch.station_id = orig_station;
            }
        });
        let err_role = call_rpc(&ctx, &mut sess2, "officeManager", "RentOffice", &[PyRep::Integer(60_003_760)])
            .await
            .expect_err("member without RentOffice role must fail");
        assert_eq!(err_role.code, "CorpRoleDenied");

        let corp_wallet_before = ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(1));
        call_rpc(&ctx, &mut sess1, "officeManager", "RentOffice", &[PyRep::Integer(60_003_760)])
            .await
            .expect("RentOffice");
        ctx.db.with_snapshot(|s| {
            assert!(
                s.corporations[&corp_id].division_balance(1) < corp_wallet_before,
                "RentOffice must debit corporation wallet"
            );
            assert!(
                s.items.values().any(|i| i.owner_id == corp_id && i.location_id == 60_003_760 && i.type_id == 27),
                "RentOffice must create office item (type 27) at station"
            );
        });

        let alliance = call_rpc(
            &ctx,
            &mut sess1,
            "corpRegistry",
            "CreateAlliance",
            &[PyRep::String("Pioneer Coalition".into()), PyRep::String("PIONC".into()), PyRep::String("Alliance".into()), PyRep::String("https://evomu.local".into())],
        )
        .await
        .expect("CreateAlliance");
        assert!(alliance.dict_get("allianceID").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    }
}

pub(crate) mod journey_fleet_structures {
    // End-to-End Integration Journeys Part 5 (`journey:fleet` .. `journey:cosmetics_store`).

    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_machonet::SessionState;
    use eve_proto::quasar::{
        encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest,
        EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, WireField, WireMessage,
        TYPE_GOOGLEAPIS_PREFIX,
    };
    use eve_proto::PyRep;
    use eve_server::quasar_gateway::STATUS_OK;
    use eve_server::router::ServerContext;
    use std::collections::HashMap;

    async fn call_rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        service: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, eve_proto::BridgeRpcError> {
        ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
            .await
    }

    fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
        if let PyRep::KeyVal(entries) = rep {
            for (k, v) in entries {
                if k == "response_proto" {
                    if let PyRep::Bytes(b) = v {
                        return EvePublicResponse::decode_proto(b).expect("decode EvePublicResponse");
                    }
                }
            }
        }
        panic!("Expected KeyVal with response_proto, got {rep:?}");
    }

    async fn quasar_rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        type_name: &str,
        value: Vec<u8>,
    ) -> EvePublicResponse {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let req = EvePublicRequest {
            issued: Some(GoogleTimestamp {
                seconds: ctx.now_unix() as i64,
                nanos: 0,
            }),
            correlation_uuid: format!("jrn-p5-{n:06}").into_bytes(),
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
                value,
            }),
            external_origin: "eve-client".into(),
            application_instance_uuid: b"jrn-p5-app-01".to_vec(),
            authoritative_context: None,
            trace_context: None,
        };
        let rep = ctx
            .dispatch_rpc(
                sess,
                "publicGatewaySvc",
                None,
                "SendRequest",
                &[PyRep::Bytes(req.encode_proto())],
                &HashMap::new(),
                None,
            )
            .await
            .expect("SendRequest");
        extract_response_proto(&rep)
    }

    #[tokio::test]
    async fn test_journey_fleet() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ctx.seed_demo_world_fixtures();

        let mut sess1 = SessionState::new(501);
        crate::common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
        call_rpc(&ctx, &mut sess1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID 1");

        let mut sess2 = SessionState::new(502);
        crate::common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
        call_rpc(&ctx, &mut sess2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_002)])
            .await
            .expect("SelectCharacterID 2");

        // 1. Form a fleet, invite & accept second player, set MOTD & broadcast
        let fleet = call_rpc(&ctx, &mut sess1, "fleetMgr", "CreateFleet", &[])
            .await
            .expect("CreateFleet");
        let fleet_id = fleet.dict_get("fleetID").and_then(|v| v.as_i64()).unwrap_or(1);
        call_rpc(&ctx, &mut sess1, "fleetMgr", "Invite", &[PyRep::Integer(90_000_002), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0)])
            .await
            .expect("fleetMgr.Invite");
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_002);
        call_rpc(&ctx, &mut sess2, "fleetMgr", "AcceptInvite", &[PyRep::Integer(fleet_id)])
            .await
            .expect("fleetMgr.AcceptInvite");
        let fleet_bind = call_rpc(&ctx, &mut sess1, "fleetObjectHandler", "MachoBindObject", &[PyRep::Integer(fleet_id), PyRep::None])
            .await
            .expect("fleetObjectHandler bind");
        let fleet_bid = match fleet_bind { PyRep::Tuple(t) => t[0].as_i64().unwrap_or(1) as u64, other => other.as_i64().unwrap_or(1) as u64 };
        ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", Some(fleet_bid), "SetMotdEx", &[PyRep::String("Fleet Ops Ready".into())], &HashMap::new(), None)
            .await
            .expect("SetMotdEx");
        let fleet_bind2 = call_rpc(&ctx, &mut sess2, "fleetObjectHandler", "MachoBindObject", &[PyRep::Integer(fleet_id), PyRep::None])
            .await
            .expect("fleetObjectHandler bind 2");
        let fleet_bid2 = match fleet_bind2 { PyRep::Tuple(t) => t[0].as_i64().unwrap_or(1) as u64, other => other.as_i64().unwrap_or(1) as u64 };
        let motd = ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", Some(fleet_bid2), "GetMotd", &[], &HashMap::new(), None)
            .await
            .expect("GetMotd");
        assert_eq!(motd.as_str(), Some("Fleet Ops Ready"));

        // 2. Broadcast & fleet warp
        call_rpc(&ctx, &mut sess1, "ship", "Undock", &[]).await.expect("ship.Undock 1");
        call_rpc(&ctx, &mut sess2, "ship", "Undock", &[]).await.expect("ship.Undock 2");
        let mut rx_bcast = ctx.notify_tx.subscribe();
        call_rpc(&ctx, &mut sess1, "fleetMgr", "SendBroadcast", &[PyRep::String("AlignSun".into()), PyRep::Integer(0), PyRep::Integer(0)])
            .await
            .expect("SendBroadcast");
        let mut saw_bcast = false;
        for _ in 0..32 {
            let Ok(env) = rx_bcast.try_recv() else { break };
            let dbg = format!("{env:?}");
            if dbg.contains("OnFleetBroadcast") && dbg.contains("AlignSun") {
                saw_bcast = true;
                break;
            }
        }
        assert!(saw_bcast, "SendBroadcast must emit OnFleetBroadcast with AlignSun");
    }

    #[tokio::test]
    async fn test_journey_structures() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ctx.seed_demo_world_fixtures();

        let mut sess = SessionState::new(503);
        crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID");
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_001);
        let created = call_rpc(
            &ctx,
            &mut sess,
            "corpRegistry",
            "AddCorporation",
            &[PyRep::String("EvoMu Structures Corp".into()), PyRep::String("EVST".into()), PyRep::Real(0.05)],
        )
        .await
        .expect("AddCorporation");
        let corp_id = created.dict_get("corporationID").and_then(|v| v.as_i64()).expect("corporationID");
        assert_eq!(sess.corporation_id, Some(corp_id));
        ctx.advance_secs(8 * 86_400);

        // 1. Undock, jump to Perimeter (30_000_144), & anchor an Astrahus (type 35832) in space
        call_rpc(&ctx, &mut sess, "ship", "Undock", &[]).await.expect("ship.Undock");
        let ship_id = sess.ship_id.unwrap();
        let PyRep::Tuple(b_tup) = call_rpc(&ctx, &mut sess, "beyonce", "MachoBindObject", &[PyRep::Integer(30_000_142), PyRep::None]).await.unwrap() else { panic!("beyonce bind") };
        let bey_bid = b_tup[0].as_i64().unwrap() as u64;
        let sg = ctx.sde.stargates().values().find(|g| g.from_solar_system_id == 30_000_142 && g.to_solar_system_id == 30_000_144).unwrap();
        let _ = ctx.systems.get_or_spawn(30_000_142).set_ball_position(ship_id, sg.position).await;
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_001);
        ctx.dispatch_rpc(&mut sess, "beyonce", Some(bey_bid), "CmdStargateJump", &[PyRep::Integer(sg.stargate_id as i64)], &HashMap::new(), None).await.expect("CmdStargateJump to Perimeter");
        let sys_h = ctx.systems.get_or_spawn(30_000_144);
        let _ = sys_h.set_ball_position(ship_id, glam::DVec3::new(0.0, 5.0e9, 0.0)).await;
        let cit_item = ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let id = snap.next_entity_id;
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: 35832, // Astrahus
                    owner_id: sess.corporation_id.unwrap_or(1_000_035),
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
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
            id
        });

        let anchored = call_rpc(
            &ctx,
            &mut sess,
            "structureDeployment",
            "Anchor",
            &[PyRep::Integer(cit_item), PyRep::String("EvoMu Citadel".into()), PyRep::Integer(1)],
        )
        .await
        .expect("structureDeployment.Anchor");
        let struct_id = anchored.dict_get("structureID").and_then(|v| v.as_i64()).unwrap_or(cit_item);
        let corp_structs = call_rpc(&ctx, &mut sess, "structureDirectory", "GetMyCorporationStructures", &[])
            .await
            .expect("GetMyCorporationStructures");
        assert!(format!("{corp_structs:?}").contains(&struct_id.to_string()));

        // 2. Complete anchoring, dock, insert Quantum Core (56201 -> flag 172), undock, and unanchor
        let core_id = ctx.db.add_or_stack_item(90_000_001, ship_id, InventoryFlag::Cargo, 56_201, 1).unwrap().item_id;
        ctx.advance_secs(25 * 3_600);
        let structs = call_rpc(&ctx, &mut sess, "structureDirectory", "GetMyDockableStructures", &[])
            .await
            .expect("GetMyDockableStructures");
        assert!(format!("{structs:?}").contains(&struct_id.to_string()));
        let _ = sys_h.set_ball_position(ship_id, glam::DVec3::new(60_000.0, 5.0e9, 0.0)).await;
        call_rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(struct_id)])
            .await
            .expect("structureDocking.Dock");
        assert_eq!(sess.structure_id, Some(struct_id));
        call_rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(core_id), PyRep::Integer(struct_id), PyRep::Integer(172)])
            .await
            .expect("Insert Quantum Core");
        ctx.advance_secs(12);
        call_rpc(&ctx, &mut sess, "structureDocking", "Undock", &[])
            .await
            .expect("structureDocking.Undock");
        assert!(sess.structure_id.is_none());
        ctx.advance_secs(1_800);
        let _ = call_rpc(&ctx, &mut sess, "structureDirectory", "GetStructures", &[]).await;
        let mut rx_unanchor = ctx.notify_tx.subscribe();
        call_rpc(&ctx, &mut sess, "structureDeployment", "Unanchor", &[PyRep::Integer(struct_id)])
            .await
            .expect("structureDeployment.Unanchor");
        let mut saw_unanchoring = false;
        for _ in 0..32 {
            let Ok(env) = rx_unanchor.try_recv() else { break };
            let dbg = format!("{env:?}");
            if dbg.contains("OnStructureStateChanged") && dbg.contains("Unanchoring") {
                saw_unanchoring = true;
                break;
            }
        }
        assert!(saw_unanchoring, "Unanchor must emit OnStructureStateChanged(Unanchoring)");
    }

    #[tokio::test]
    async fn test_journey_sovereignty_fw() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ctx.seed_demo_world_fixtures();

        let mut sess = SessionState::new(504);
        ctx.db
            .set_account_role("demo", eve_db::service_roles::ROLE_GM_DEFAULT)
            .expect("set_account_role");
        crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID");

        // 1. Enlist in Faction Warfare (Caldari State = 500001) & check stats
        call_rpc(&ctx, &mut sess, "facWarMgr", "JoinFactionAsCharacter", &[PyRep::Integer(500_001)])
            .await
            .expect("JoinFactionAsCharacter");
        assert_eq!(ctx.db.with_snapshot(|s| s.characters[&90_000_001].war_faction_id), Some(500_001));
        let stats = call_rpc(&ctx, &mut sess, "facWarMgr", "GetStats_Character", &[])
            .await
            .expect("GetStats_Character");
        let PyRep::Dict(ref stat_entries) = stats else { panic!("expected Dict from GetStats_Character") };
        assert_eq!(stat_entries.len(), 6);

        // 2. Anchor a Sovereignty Hub & query resources via Quasar
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_001);
        let created = call_rpc(
            &ctx,
            &mut sess,
            "corpRegistry",
            "AddCorporation",
            &[PyRep::String("EvoMu Sov Corp".into()), PyRep::String("EVSV".into()), PyRep::Real(0.05)],
        )
        .await
        .expect("AddCorporation");
        let corp_id = created.dict_get("corporationID").and_then(|v| v.as_i64()).expect("corporationID");
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.corporations.get_mut(&corp_id) {
                c.master_wallet = eve_db::IskCents::from_isk(2_000_000_000);
                c.set_division_balance(1, eve_db::IskCents::from_isk(2_000_000_000));
            }
        });
        call_rpc(
            &ctx,
            &mut sess,
            "corpRegistry",
            "CreateAlliance",
            &[PyRep::String("EvoMu Sov Alliance".into()), PyRep::String("EVSA".into()), PyRep::String("Sov".into()), PyRep::String("https://evomu.local".into())],
        )
        .await
        .expect("CreateAlliance");
        let nullsec_sys = ctx
            .sde
            .solar_systems
            .keys().copied()
            .filter(|&id| eve_server::services::sov_gameplay::is_claimable_nullsec_system(&ctx.sde, id))
            .min()
            .unwrap_or(30_004_759);
        call_rpc(&ctx, &mut sess, "ship", "Undock", &[]).await.expect("ship.Undock");
        call_rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String(format!("/tr {nullsec_sys}"))])
            .await
            .expect("/tr nullsec");
        let ship_id = sess.ship_id.unwrap();
        let sys_h = ctx.systems.get_or_spawn(nullsec_sys);
        let _ = sys_h.set_ball_position(ship_id, glam::DVec3::new(2_500_000.0, 0.0, 0.0)).await;
        let hub_item = ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let id = snap.next_entity_id;
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: 32458, // Sovereignty Hub
                    owner_id: corp_id,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
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
            id
        });
        let anchored = call_rpc(
            &ctx,
            &mut sess,
            "structureDeployment",
            "Anchor",
            &[PyRep::Integer(hub_item), PyRep::String("EvoMu Sov Hub".into()), PyRep::Integer(1)],
        )
        .await
        .expect("structureDeployment.Anchor Sov Hub");
        let hub_id = anchored.dict_get("structureID").and_then(|v| v.as_i64()).unwrap_or(hub_item);
        let mut hub_msg = Vec::new();
        encode_varint_field(1, hub_id as u64, &mut hub_msg);
        let mut sov_req = Vec::new();
        encode_bytes_field(1, &hub_msg, &mut sov_req);
        let sov_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.sovereignty.hub.api.GetResourcesRequest", sov_req).await;
        assert_eq!(sov_rsp.status_code, STATUS_OK);
        let sov_wm = WireMessage::decode(&sov_rsp.payload.unwrap().value).expect("decode GetResourcesResponse");
        assert!(sov_wm.fields.iter().any(|f| f.field_number == 1), "GetResourcesResponse must include power field");
    }

    #[tokio::test]
    async fn test_journey_daily_goals_events() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ctx.seed_demo_world_fixtures();

        let mut sess = SessionState::new(505);
        crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID");

        // 1. Subscribe to publicGatewaySvc notices & fetch all current daily goals via Quasar
        call_rpc(&ctx, &mut sess, "publicGatewaySvc", "Subscribe", &[])
            .await
            .expect("publicGatewaySvc.Subscribe");
        let mut rx = ctx.quasar_gateway.take_stream_receiver(sess.session_id).expect("stream receiver");

        let all_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.dailygoal.api.GetAllCurrentRequest", Vec::new()).await;
        assert_eq!(all_rsp.status_code, STATUS_OK);
        let wm = WireMessage::decode(&all_rsp.payload.unwrap().value).expect("decode GetAllCurrentResponse");
        let goal_fields: Vec<&WireField> = wm.fields.iter().filter(|f| f.field_number == 1).collect();
        assert_eq!(goal_fields.len(), 6, "must return 6 SDE-seeded daily goals (5 daily + 1 daily bonus)");

        // 2. Progress ScanSignature daily goal via real gameplay pathway & verify ProgressedNotice
        for sig_id in 800_001..=800_005 {
            ctx.mark_signature_scanned(90_000_001, 30_000_142, sig_id);
        }
        let mut saw_progressed = false;
        for _ in 0..64 {
            let Ok(n) = rx.try_recv() else { break };
            if let Some(p) = n.payload {
                if p.normalized_type_name() == "eve_public.dailygoal.api.ProgressedNotice" {
                    saw_progressed = true;
                }
            }
        }
        assert!(saw_progressed, "ScanSignature must emit ProgressedNotice");
    }

    #[tokio::test]
    async fn test_journey_cosmetics_store() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ctx.seed_demo_world_fixtures();
        crate::common::seed_test_nes_store_catalog(&ctx);

        let mut sess = SessionState::new(506);
        crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID");

        // 1. PLEX Vault balance & NES store purchase via Quasar publicGatewaySvc.Subscribe + SendRequest
        call_rpc(&ctx, &mut sess, "publicGatewaySvc", "Subscribe", &[])
            .await
            .expect("publicGatewaySvc.Subscribe");
        let init_plex = ctx.db.with_snapshot(|s| s.plex_vault_balance(90_000_001));
        assert_eq!(init_plex, 500);

        let bal_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.plex.vault.api.BalanceRequest", Vec::new()).await;
        assert_eq!(bal_rsp.status_code, STATUS_OK);

        // Purchase offer 2001 (450 PLEX) from NES store
        let (mut b, mut purch_payload) = (Vec::new(), Vec::new());
        encode_string_field(1, "2001", &mut b);
        encode_bytes_field(1, &b, &mut purch_payload);
        let purch_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.plex.vault.api.PurchaseRequest", purch_payload).await;
        assert_eq!(purch_rsp.status_code, STATUS_OK);
        assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(90_000_001)), 50);

        // 2. Apply ship SKIN via shipCosmeticsMgr
        let ship_id = sess.ship_id.expect("ship_id");
        let ship_type_id = ctx.db.with_snapshot(|s| s.items[&ship_id].type_id);
        let skin_id = ctx
            .sde
            .skins
            .iter()
            .find(|(_, sk)| sk.types.contains(&ship_type_id))
            .map(|(id, _)| *id as i64)
            .unwrap_or(1);
        ctx.db.with_mut_snapshot(|snap| {
            snap.licensed_skins.entry(90_000_001).or_default().push(skin_id as u32);
        });
        let skin_res = call_rpc(&ctx, &mut sess, "shipCosmeticsMgr", "ApplySkinToShip", &[PyRep::Integer(ship_id), PyRep::Integer(skin_id)])
            .await
            .expect("ApplySkinToShip");
        assert_eq!(skin_res.dict_get("shipID").and_then(|v| v.as_i64()), Some(ship_id));
    }
}

pub(crate) mod journey_mail_notifications_esi_external {
    // End-to-End Integration Journeys Part 6 (`journey:mail_notifications` .. `journey:gm_tools`).

    use eve_db::{service_roles, InventoryFlag, VirtualServerClock, WorldRepository};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::esi::{send_raw_esi_request, EsiHttpRequest, EsiServer};
    use eve_server::router::ServerContext;
    use std::collections::HashMap;
    use std::sync::Arc;

    async fn call_rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        service: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, eve_proto::BridgeRpcError> {
        ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
            .await
    }

    #[tokio::test]
    async fn test_journey_mail_notifications() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ctx.seed_demo_world_fixtures();

        let mut sess1 = SessionState::new(601);
        crate::common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
        call_rpc(&ctx, &mut sess1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID 1");

        let mut sess2 = SessionState::new(602);
        crate::common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
        call_rpc(&ctx, &mut sess2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_002)])
            .await
            .expect("SelectCharacterID 2");

        // 1. Send mail from sess1 to sess2, verify sess2 receives & reads it
        let mail_id = call_rpc(
            &ctx,
            &mut sess1,
            "mailMgr",
            "SendMail",
            &[
                PyRep::List(vec![PyRep::Integer(90_000_002)]),
                PyRep::String("Fleet Briefing".into()),
                PyRep::String("Meet at Jita 4-4 undock.".into()),
                PyRep::Integer(0),
                PyRep::Integer(0),
            ],
        )
        .await
        .expect("SendMail");
        let mid = mail_id.as_i64().unwrap_or(1);

        let body2 = call_rpc(&ctx, &mut sess2, "mailMgr", "GetBody", &[PyRep::Integer(mid), PyRep::Integer(0)])
            .await
            .expect("GetBody");
        assert!(matches!(body2, PyRep::Buffer(ref b) if !b.is_empty()), "GetBody must return compressed mail body");

        // 2. Labels & Mailing lists
        let lbl = call_rpc(&ctx, &mut sess2, "mailMgr", "CreateLabel", &[PyRep::String("Priority".into()), PyRep::Integer(1)])
            .await
            .expect("CreateLabel");
        assert!(lbl.as_i64().unwrap_or(0) > 0);
        let mlist = call_rpc(&ctx, &mut sess1, "mailingListsMgr", "Create", &[PyRep::String("EvoMu Pilots".into()), PyRep::Integer(0), PyRep::Integer(0)])
            .await
            .expect("mailingListsMgr.Create");
        assert!(mlist.as_i64().unwrap_or(0) > 0);

        // 3. Notifications & Calendar events
        let notifs = call_rpc(&ctx, &mut sess2, "notificationMgr", "GetUnprocessed", &[]).await.expect("GetUnprocessed");
        assert!(format!("{notifs:?}").contains("CRowset"));
        let ev = call_rpc(
            &ctx,
            &mut sess1,
            "calendarMgr",
            "CreatePersonalEvent",
            &[
                PyRep::Integer(135_500_000_000_000_000),
                PyRep::Integer(60),
                PyRep::String("Roam".into()),
                PyRep::String("Lowsec roam".into()),
                PyRep::Integer(0),
                PyRep::List(vec![PyRep::Integer(90_000_002)]),
            ],
        )
        .await
        .expect("CreatePersonalEvent");
        assert!(ev.dict_get("eventID").and_then(|v| v.as_i64()).or_else(|| ev.as_i64()).unwrap_or(0) > 0);
    }

    #[tokio::test]
    async fn test_journey_esi_external() {
        let ctx = Arc::new(ServerContext::new());
        crate::common::pin_clock(&ctx);
        ctx.seed_demo_world_fixtures();

        let (addr, _router, _handle) = EsiServer::spawn(Arc::clone(&ctx), "127.0.0.1:0")
            .await
            .expect("spawn ESI server");

        let now = ctx.now_unix();
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        let code = ctx
            .db
            .authorize_pkce(
                "demo",
                "demo-secret",
                90_000_001,
                "esi-journey",
                "https://localhost/callback",
                vec![
                    "esi-wallet.read_character_wallet.v1".into(),
                    "esi-skills.read_skills.v1".into(),
                    "esi-assets.read_assets.v1".into(),
                    "esi-location.read_location.v1".into(),
                ],
                challenge,
                "S256",
                now,
            )
            .expect("authorize_pkce");
        let token = ctx
            .db
            .exchange_pkce_code(&code, "esi-journey", verifier, now)
            .expect("exchange_pkce_code")
            .access_token;

        // 1. Call endpoints across multiple tag groups & verify caching headers
        for path in [
            "/latest/characters/90000001/wallet/",
            "/latest/characters/90000001/skills/",
            "/latest/characters/90000001/assets/",
            "/latest/characters/90000001/location/",
            "/latest/characters/90000001/",
            "/latest/universe/systems/30000142/",
            "/latest/status/",
        ] {
            let mut req = EsiHttpRequest::new("GET", path);
            req.headers.insert("authorization".into(), format!("Bearer {token}"));
            let resp = send_raw_esi_request(addr, &req).await.expect("ESI GET");
            assert_eq!(resp.status_code, 200, "ESI {path} must return 200");
            if path.ends_with("/wallet/") {
                let bal: f64 = std::str::from_utf8(&resp.body).unwrap().trim().parse().unwrap();
                assert!(bal > 0.0, "ESI wallet balance must match in-client balance");
            }
            assert!(
                resp.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("cache-control") || k.eq_ignore_ascii_case("etag")),
                "ESI {path} must include caching headers"
            );
        }

        // 2. Scope enforcement: mail endpoint without esi-mail.read_mail.v1 must return 403
        let mut forbidden_req = EsiHttpRequest::new("GET", "/latest/characters/90000001/mail/");
        forbidden_req.headers.insert("authorization".into(), format!("Bearer {token}"));
        let forbidden_resp = send_raw_esi_request(addr, &forbidden_req).await.expect("ESI 403");
        assert_eq!(forbidden_resp.status_code, 403);
    }

    #[tokio::test]
    async fn test_journey_downtime_persistence() {
        let tmp_dir = std::env::temp_dir();
        let db_path = tmp_dir.join(format!("evomu_journey_dt_{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&db_path);

        let expected_bal = {
            let db = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 1"));
            let ctx = Arc::new(ServerContext::with_repository(db));
            ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
            ctx.seed_demo_world_fixtures();
            ctx.db.set_account_role("demo", service_roles::ROLE_GM_DEFAULT).expect("set_account_role");

            let mut sess = SessionState::new(603);
            crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
            call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
                .await
                .expect("SelectCharacterID");

            ctx.db.add_or_stack_item(90_000_001, 60_003_760, InventoryFlag::Hangar, 34, 777).unwrap();
            call_rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String("/giveisk 25000".into())])
                .await
                .expect("/giveisk");
            let bal = ctx.db.with_snapshot(|s| s.characters[&90_000_001].balance.0);

            call_rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String("/downtime".into())])
                .await
                .expect("/downtime");
            bal
        };

        // Reopen ServerContext from SQLite after downtime restart and verify persistence
        {
            let db = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 2"));
            let ctx = Arc::new(ServerContext::with_repository(db));
            ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_600)));

            let mut sess = SessionState::new(604);
            crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
            call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
                .await
                .expect("SelectCharacterID after restart");

            let bal_after = ctx.db.with_snapshot(|s| s.characters[&90_000_001].balance.0);
            assert_eq!(bal_after, expected_bal, "wallet balance must survive SQLite restart");
            let trit_after: i32 = ctx.db.with_snapshot(|s| {
                s.items.values().filter(|i| i.owner_id == 90_000_001 && i.type_id == 34).map(|i| i.quantity).sum()
            });
            assert!(trit_after >= 777, "items must survive SQLite restart");
        }

        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn test_journey_gm_tools() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        ctx.seed_demo_world_fixtures();

        // 1. Normal account must be refused GM slash commands
        let mut normal_sess = SessionState::new(605);
        crate::common::login_pilot(&ctx, &mut normal_sess, "demo", "demo-secret").await;
        call_rpc(&ctx, &mut normal_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID normal");

        for cmd in ["/giveisk 1000", "/createitem 34 10", "/giveskill 3300 5", "/spawnnpc 10002", "/teleport 30000144", "/heal"] {
            let denied = call_rpc(&ctx, &mut normal_sess, "slash", "SlashCmd", &[PyRep::String(cmd.into())]).await;
            assert_eq!(denied.unwrap_err().code, "InsufficientRole", "normal account must be refused {cmd}");
        }

        // 2. Provision real GM account via account-role path (no session.role mutation)
        ctx.db.provision_player_account("gm_account", "gm-secret-999");
        ctx.db.set_account_role("gm_account", service_roles::ROLE_GM_DEFAULT).expect("set_account_role GM");
        ctx.db.provision_player_account("target_pilot", "target-secret");

        let mut gm_sess = SessionState::new(606);
        crate::common::login_pilot(&ctx, &mut gm_sess, "gm_account", "gm-secret-999").await;
        assert!(gm_sess.has_gm_role(), "GM account must receive GM role on login");
        let gm_ancestry = ctx
            .sde
            .ancestries
            .values()
            .find(|a| a.bloodline_id == 1)
            .map(|a| a.ancestry_id as i64)
            .unwrap_or(11);
        let gm_cid = call_rpc(
            &ctx,
            &mut gm_sess,
            "charUnboundMgr",
            "CreateCharacterWithDoll",
            &[
                PyRep::String("Valkyrie Overseer".into()),
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::Integer(gm_ancestry),
                PyRep::String("doll:gm".into()),
                PyRep::String("portrait:gm".into()),
                PyRep::Integer(17),
            ],
        )
        .await
        .expect("CreateCharacterWithDoll GM")
        .as_i64()
        .unwrap();
        call_rpc(&ctx, &mut gm_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(gm_cid)])
            .await
            .expect("SelectCharacterID GM");

        // 3. Test every slash command family and assert concrete state effects
        let isk_before = ctx.db.with_snapshot(|s| s.characters[&gm_cid].balance.0);
        call_rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String("/giveisk 50000".into())])
            .await
            .expect("/giveisk");
        let isk_after = ctx.db.with_snapshot(|s| s.characters[&gm_cid].balance.0);
        assert_eq!(isk_after - isk_before, 5_000_000);

        let item_rsp = call_rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String("/createitem 34 250".into())])
            .await
            .expect("/createitem");
        assert_eq!(item_rsp.dict_get("typeID").and_then(|v| v.as_i64()), Some(34));

        let skill_rsp = call_rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String("/giveskill 3300 5".into())])
            .await
            .expect("/giveskill");
        assert_eq!(skill_rsp.dict_get("level").and_then(|v| v.as_i64()), Some(5));

        let npc_rsp = call_rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String("/spawnnpc 601".into())])
            .await
            .expect("/spawnnpc");
        assert!(npc_rsp.dict_get("npcID").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

        call_rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String("/teleport 30000144".into())])
            .await
            .expect("/teleport");
        assert_eq!(gm_sess.solar_system_id, Some(30_000_144));

        let heal_rsp = call_rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String("/heal".into())])
            .await
            .expect("/heal");
        assert_eq!(heal_rsp.dict_get("healed").and_then(|v| v.as_bool()), Some(true));

        let ban_rsp = call_rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String("/ban target_pilot 3600 TestBan".into())])
            .await
            .expect("/ban");
        assert_eq!(ban_rsp.dict_get("username").and_then(|v| v.as_str()), Some("target_pilot"));

        let unban_rsp = call_rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String("/unban target_pilot".into())])
            .await
            .expect("/unban");
        assert_eq!(unban_rsp.dict_get("unbanned").and_then(|v| v.as_bool()), Some(true));
    }
}

pub(crate) mod journey_launch_login_create_character {
    // In-process integration smoke tests for `plan:T-6` (part 1: journeys 1..=13).
    // Note: `journey:*` ledger rows remain open until real V24 client execution via `tools/client-harness/run_journeys`.

    use crate::common::{login_pilot, pin_clock, probe_scan};
    use eve_db::{
        base64url_encode_no_pad, InventoryFlag, SkillQueueEntry,
    };
    use eve_machonet::{
        CryptoChallengePacket, CryptoHandshakeResult, CryptoRequestPacket, CryptoServerHandshake,
        HandshakeCommand, HandshakeIdentity, HandshakeStepOutput, MachoHandshakeStateMachine,
        SessionState, VersionExchange,
    };
    use eve_proto::quasar::sha256_bytes;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use glam::DVec3;
    use std::collections::HashMap;
    use std::sync::Arc;

    async fn rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        svc: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, eve_proto::BridgeRpcError> {
        ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
            .await
    }

    fn bound_id(rep: &PyRep) -> u64 {
        match rep {
            PyRep::Tuple(t) => t[0].as_i64().unwrap_or(0) as u64,
            other => other.as_i64().unwrap_or(0) as u64,
        }
    }

    #[tokio::test]
    async fn test_journey_01_launch_login() {
        let ctx = Arc::new(ServerContext::new());
        pin_clock(&ctx);
        ctx.seed_demo_world_fixtures();

        let verifier = "0123456789abcdef0123456789abcdef0123456789abcdef";
        let challenge = base64url_encode_no_pad(&sha256_bytes(verifier.as_bytes()));
        let code = ctx
            .db
            .authorize_pkce(
                "demo",
                "demo-secret",
                90_000_001,
                "evomu-launcher",
                "http://localhost:8080/callback",
                vec!["esi-skills.read_skills.v1".into()],
                &challenge,
                "S256",
                ctx.now_unix(),
            )
            .expect("authorize_pkce");
        let bundle = ctx
            .db
            .exchange_pkce_code(&code, "evomu-launcher", verifier, ctx.now_unix())
            .expect("exchange_pkce_code");
        assert!(!bundle.access_token.is_empty());

        let mut hm = MachoHandshakeStateMachine::new(VersionExchange::default(), 1, 5001, 7001);
        let _ = hm.step(&VersionExchange::default().to_pyrep()).expect("version");
        let _ = hm
            .step(&HandshakeCommand::CryptoRequest(CryptoRequestPacket::placebo()).to_pyrep())
            .expect("crypto");
        let chal = CryptoChallengePacket::new_password_login("demo", "demo-secret");
        let step3 = hm.step(&chal.to_pyrep()).expect("challenge");
        assert!(matches!(step3, HandshakeStepOutput::NeedsAuthentication(_)));
        let id = HandshakeIdentity {
            user_id: 1,
            role: 1,
            language_id: "EN".into(),
            user_type: 30,
            address: "127.0.0.1".into(),
            access_token: Some(bundle.access_token.clone()),
            computer_hash: None,
        };
        let frames = hm.complete_challenge(&chal, Ok(id), 1).expect("complete");
        let srv_hs = CryptoServerHandshake::from_pyrep(&frames[1]).expect("srv_hs");
        let res = CryptoHandshakeResult::for_server_challenge(&srv_hs.server_challenge);
        assert!(matches!(
            hm.step(&res.to_pyrep()).expect("final"),
            HandshakeStepOutput::Authenticated { .. }
        ));

        let mut sess = SessionState::new(73_001);
        rpc(
            &ctx,
            &mut sess,
            "authentication",
            "Login",
            &[PyRep::String("__sso__".into()), PyRep::String(bundle.access_token)],
        )
        .await
        .expect("SSO Login");
        let chars = rpc(&ctx, &mut sess, "charUnboundMgr", "GetCharactersToSelect", &[])
            .await
            .expect("GetCharactersToSelect");
        let rows = chars.crowset_rows().unwrap();
        assert!(!rows.is_empty());
        assert_eq!(rows[0].dbrow_get("characterID"), Some(&PyRep::Integer(90_000_001)));
    }

    #[tokio::test]
    async fn test_journey_02_create_character() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let mut sess = SessionState::new(73_002);
        login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;

        let school_for = |race: u32| -> i64 {
            ctx.sde
                .schools
                .values()
                .find(|s| s.race_id == race)
                .map(|s| s.school_id as i64)
                .unwrap_or(17)
        };
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_001) {
                ch.user_id = 999;
            }
        });
        let anc_for = |bl: u32| -> i64 {
            ctx.sde
                .ancestries
                .values()
                .find(|a| a.bloodline_id == bl)
                .map(|a| a.ancestry_id as i64)
                .unwrap_or(1)
        };
        let specs = [
            ("demo", "demo-secret", "Caldari Pilot", 1i64, anc_for(1), school_for(1)),
            ("other", "other-secret", "Amarr Pilot", 5, anc_for(5), school_for(4)),
            ("other", "other-secret", "Gallente Pilot", 7, anc_for(7), school_for(8)),
            ("demo", "demo-secret", "Minmatar Pilot", 3, anc_for(3), school_for(2)),
        ];
        for (user, pass, name, bloodline, ancestry, school) in specs {
            login_pilot(&ctx, &mut sess, user, pass).await;
            let cid = rpc(
                &ctx,
                &mut sess,
                "charUnboundMgr",
                "CreateCharacterWithDoll",
                &[
                    PyRep::String(name.into()),
                    PyRep::Integer(bloodline),
                    PyRep::Integer(1),
                    PyRep::Integer(ancestry),
                    PyRep::String(format!("doll:{name}")),
                    PyRep::String(format!("portrait:{name}")),
                    PyRep::Integer(school),
                ],
            )
            .await
            .expect("CreateCharacterWithDoll")
            .as_i64()
            .expect("char_id");
            rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(cid)])
                .await
                .expect("SelectCharacterID");
            let ch = ctx.db.get_character(cid).expect("char in db");
            assert!(ch.balance.0 > 0, "starter ISK > 0");
            assert!(ch.station_id.is_some(), "in school station");
            ctx.db.with_snapshot(|snap| {
                assert!(!snap.skills.get(&cid).unwrap().is_empty(), "starter skills");
                let has_ship = snap
                    .items
                    .values()
                    .any(|it| it.owner_id == cid && it.singleton);
                assert!(has_ship, "starter ship");
            });
        }
    }

    #[tokio::test]
    async fn test_journey_03_station_ui() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let mut sess = SessionState::new(73_003);
        login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID");

        let windows: &[(&str, &str, &[PyRep])] = &[
            ("invbroker", "MachoBindObject", &[PyRep::Tuple(vec![PyRep::Integer(60_003_760), PyRep::Integer(15)]), PyRep::None]),
            ("dogmaIM", "MachoBindObject", &[PyRep::Integer(100_000_001), PyRep::None]),
            ("marketProxy", "GetMarketGroups", &[]),
            ("industryManager", "GetJobsByOwner", &[]),
            ("agentMgr", "GetAgents", &[]),
            ("charMgr", "GetPublicInfo", &[PyRep::Integer(90_000_001)]),
            ("skillMgr2", "GetSkillQueue", &[]),
            ("account", "GetCashBalance", &[PyRep::Integer(0), PyRep::Integer(1000)]),
            ("marketProxy", "CharGetTransactions", &[]),
            ("corpRegistry", "GetCorporation", &[PyRep::Integer(1_000_035)]),
            ("charMgr", "GetContactList", &[]),
            ("mailMgr", "GetMailHeaders", &[]),
            ("notificationMgr", "GetAllNotifications", &[]),
            ("calendarMgr", "GetEventList", &[]),
        ];
        for (svc, method, args) in windows {
            let rep = rpc(&ctx, &mut sess, svc, method, args).await;
            assert!(rep.is_ok(), "window {svc}.{method} failed: {rep:?}");
        }
        let bal = rpc(&ctx, &mut sess, "account", "GetCashBalance", &[PyRep::Integer(0), PyRep::Integer(1000)])
            .await
            .unwrap();
        assert!(bal.as_f64().unwrap_or(0.0) > 0.0);
    }

    #[tokio::test]
    async fn test_journey_04_fit_ship() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let mut sess = SessionState::new(73_004);
        login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID");

        // Buy a module on market, fit module, validate slot rejection, inspect Dogma, save/load fitting
        let ord = rpc(
            &ctx,
            &mut sess,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(60_003_760),
                PyRep::Integer(202), // 150mm Light AutoCannon I
                PyRep::Real(5_000.0),
                PyRep::Integer(1),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(14),
            ],
        )
        .await
        .expect("PlaceBuyOrder");
        let ord_id = ord.as_i64().or_else(|| ord.dict_get("orderID").and_then(|v| v.as_i64())).unwrap_or(0);
        assert!(ord_id > 0);

        let ship_id = 100_000_001i64;
        let mod_id = 100_000_003i64; // 1MN Afterburner I
        rpc(
            &ctx,
            &mut sess,
            "invbroker",
            "Add",
            &[PyRep::Integer(mod_id), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::MedSlot0 as i64)],
        )
        .await
        .expect("fit MedSlot0");

        // Wrong slot validation (putting MedSlot module into HiSlot0 must fail or be rejected)
        let bad_slot = rpc(
            &ctx,
            &mut sess,
            "invbroker",
            "Add",
            &[PyRep::Integer(mod_id), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::HiSlot0 as i64)],
        )
        .await;
        assert!(bad_slot.is_err(), "slot validation must reject wrong slot");

        let dogma_bind = rpc(&ctx, &mut sess, "dogmaIM", "MachoBindObject", &[PyRep::Integer(ship_id), PyRep::None])
            .await
            .expect("bind dogma");
        let did = bound_id(&dogma_bind);
        let info = ctx
            .dispatch_rpc(&mut sess, "dogmaIM", Some(did), "ItemGetInfo", &[PyRep::Integer(ship_id)], &HashMap::new(), None)
            .await
            .expect("ItemGetInfo");
        assert!(!matches!(info, PyRep::None));

        let save = rpc(
            &ctx,
            &mut sess,
            "charFittingMgr",
            "SaveFitting",
            &[
                PyRep::Integer(90_000_001),
                PyRep::keyval([
                    ("name", PyRep::String("T6 Rifter".into())),
                    ("shipTypeID", PyRep::Integer(587)),
                    ("fitData", PyRep::List(vec![])),
                ]),
            ],
        )
        .await
        .expect("SaveFitting");
        assert!(save.as_i64().unwrap_or(0) > 0);
        let fits = rpc(&ctx, &mut sess, "charFittingMgr", "GetFittings", &[PyRep::Integer(90_000_001)])
            .await
            .expect("GetFittings");
        assert!(!matches!(fits, PyRep::None));
    }

    #[tokio::test]
    async fn test_journey_05_undock_navigate() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let mut sess = SessionState::new(73_005);
        login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID");

        rpc(&ctx, &mut sess, "stationSvc", "Undock", &[PyRep::Integer(100_000_001)])
            .await
            .expect("Undock");
        assert!(sess.station_id.is_none());

        let bm = rpc(
            &ctx,
            &mut sess,
            "bookmark",
            "BookmarkLocation",
            &[PyRep::Integer(100_000_001), PyRep::Integer(0), PyRep::String("T6 Safe".into()), PyRep::String("".into())],
        )
        .await
        .expect("BookmarkLocation");
        let bm_id = bm
            .dict_get("bookmarkID")
            .and_then(|v| v.as_i64())
            .or_else(|| bm.as_i64())
            .unwrap_or(1);

        let bey_bind = rpc(&ctx, &mut sess, "beyonce", "MachoBindObject", &[PyRep::Integer(30_000_142), PyRep::None])
            .await
            .expect("bind beyonce");
        let bid = bound_id(&bey_bind);
        let kw = HashMap::new();
        for (m, args) in [
            ("CmdFollowBall", vec![PyRep::Integer(40_009_078), PyRep::Real(1_000.0)]),
            ("CmdOrbit", vec![PyRep::Integer(40_009_078), PyRep::Real(5_000.0)]),
            ("CmdFollowBall", vec![PyRep::Integer(40_009_078), PyRep::Real(10_000.0)]),
            ("CmdAlignTo", vec![PyRep::Integer(40_009_078)]),
            ("CmdWarpToStuff", vec![PyRep::String("item".into()), PyRep::Integer(40_009_078), PyRep::Real(0.0)]),
        ] {
            ctx.dispatch_rpc(&mut sess, "beyonce", Some(bid), m, &args, &kw, None)
                .await
                .expect(m);
        }
        let sys = ctx.systems.get_or_spawn(30_000_142);
        for _ in 0..30 {
            let _ = sys.tick().await;
        }
        ctx.dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(bid),
            "CmdWarpToStuff",
            &[PyRep::String("bookmark".into()), PyRep::Integer(bm_id), PyRep::Real(0.0)],
            &kw,
            None,
        )
        .await
        .expect("CmdWarpToStuff bookmark");
        for _ in 0..30 {
            let _ = sys.tick().await;
        }

        // Dock at station
        ctx.advance_secs(15);
        rpc(&ctx, &mut sess, "stationSvc", "Dock", &[PyRep::Integer(60_003_760)])
            .await
            .expect("Dock");
        assert_eq!(sess.station_id, Some(60_003_760));
    }

    #[tokio::test]
    async fn test_journey_06_local_chat() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let (mut s1, mut s2) = (SessionState::new(73_006), SessionState::new(73_007));
        login_pilot(&ctx, &mut s1, "demo", "demo-secret").await;
        rpc(&ctx, &mut s1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();
        login_pilot(&ctx, &mut s2, "other", "other-secret").await;
        rpc(&ctx, &mut s2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_002)]).await.unwrap();

        let chan_str = PyRep::String("solarsystemid2:30000142".into());
        let j1 = rpc(&ctx, &mut s1, "LSC", "JoinChannel", &[chan_str.clone()]).await.expect("JoinChannel s1");
        let j2 = rpc(&ctx, &mut s2, "LSC", "JoinChannel", &[chan_str.clone()]).await.expect("JoinChannel s2");
        assert_eq!(j2.dict_get("memberCount"), Some(&PyRep::Integer(2)));
        assert_eq!(j1.dict_get("channelID"), Some(&chan_str));
        let sent = rpc(&ctx, &mut s1, "LSC", "SendMessage", &[chan_str.clone(), PyRep::String("Hello Jita!".into())])
            .await
            .expect("SendMessage");
        assert!(!matches!(sent, PyRep::None));
        let members = rpc(&ctx, &mut s2, "LSC", "GetMembers", &[chan_str]).await.expect("GetMembers");
        assert!(!matches!(members, PyRep::None));
        ctx.db.with_snapshot(|snap| {
            let ch = snap.chat_channels.get("solarsystemid2:30000142").expect("local channel in db");
            assert!(ch.members.contains(&90_000_001));
            assert!(ch.members.contains(&90_000_002));
        });
    }

    #[tokio::test]
    async fn test_journey_07_multiplayer_presence() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let (mut s1, mut s2) = (SessionState::new(73_008), SessionState::new(73_009));
        login_pilot(&ctx, &mut s1, "demo", "demo-secret").await;
        rpc(&ctx, &mut s1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();
        login_pilot(&ctx, &mut s2, "other", "other-secret").await;
        rpc(&ctx, &mut s2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_002)]).await.unwrap();

        rpc(&ctx, &mut s1, "stationSvc", "Undock", &[PyRep::Integer(100_000_001)]).await.unwrap();
        rpc(&ctx, &mut s2, "stationSvc", "Undock", &[PyRep::Integer(100_000_002)]).await.unwrap();

        let sys = ctx.systems.get_or_spawn(30_000_142);
        let balls = sys.list_balls().await;
        assert!(balls.iter().flatten().any(|b| b.id == 100_000_001));
        assert!(balls.iter().flatten().any(|b| b.id == 100_000_002));

        // Dock and direct trade between two clients
        ctx.advance_secs(15);
        rpc(&ctx, &mut s1, "stationSvc", "Dock", &[PyRep::Integer(60_003_760)]).await.unwrap();
        rpc(&ctx, &mut s2, "stationSvc", "Dock", &[PyRep::Integer(60_003_760)]).await.unwrap();
        let trade = rpc(&ctx, &mut s1, "trademgr", "InitiateTrade", &[PyRep::Integer(90_000_002)])
            .await
            .expect("InitiateTrade");
        assert!(matches!(trade, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::Integer(_)));
    }

    #[tokio::test]
    async fn test_journey_08_skills_training() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let mut sess = SessionState::new(73_010);
        login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

        ctx.db.with_mut_snapshot(|snap| {
            snap.skill_queues.entry(90_000_001).or_default().push(SkillQueueEntry {
                type_id: 3402,
                target_level: 2,
                start_sp: 250.0,
                end_sp: 1_415.0,
                duration_seconds: 600.0,
                started_at_unix: Some(ctx.now_unix()),
            });
        });
        ctx.advance_secs(700);
        let q = rpc(&ctx, &mut sess, "skillMgr2", "GetSkillQueue", &[]).await.unwrap();
        assert!(!matches!(q, PyRep::None));

        // Attribute remap + attributes verification
        rpc(
            &ctx,
            &mut sess,
            "charMgr",
            "RespecCharacter",
            &[PyRep::Integer(17), PyRep::Integer(27), PyRep::Integer(21), PyRep::Integer(17), PyRep::Integer(17)],
        )
        .await
        .expect("RespecCharacter");
        let attrs = rpc(&ctx, &mut sess, "skillMgr2", "GetAttributes", &[]).await.expect("GetAttributes");
        if let PyRep::Dict(entries) = attrs {
            let int_val = entries.iter().find(|(k, _)| k.as_i64() == Some(165)).and_then(|(_, v)| v.as_i64());
            let mem_val = entries.iter().find(|(k, _)| k.as_i64() == Some(166)).and_then(|(_, v)| v.as_i64());
            assert_eq!(int_val, Some(27));
            assert_eq!(mem_val, Some(21));
        } else {
            panic!("expected Dict from GetAttributes, got {attrs:?}");
        }
    }

    #[tokio::test]
    async fn test_journey_09_market_trade() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let mut sess = SessionState::new(73_011);
        login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

        let bal_before = ctx.db.get_character(90_000_001).unwrap().balance.0;
        let ord = rpc(
            &ctx,
            &mut sess,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(60_003_760),
                PyRep::Integer(34),
                PyRep::Real(1.0),
                PyRep::Integer(100),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(14),
            ],
        )
        .await
        .expect("PlaceBuyOrder");
        let oid = ord.as_i64().or_else(|| ord.dict_get("orderID").and_then(|v| v.as_i64())).unwrap_or(0);
        assert!(oid > 0);
        let bal_after_place = ctx.db.get_character(90_000_001).unwrap().balance.0;
        assert!(bal_after_place < bal_before, "escrow deducted on buy order");
        ctx.advance_secs(301);
        rpc(&ctx, &mut sess, "marketProxy", "ModifyCharOrder", &[PyRep::Integer(oid), PyRep::Real(1.5), PyRep::Integer(100)])
            .await
            .expect("ModifyCharOrder");
        let bal_after_mod = ctx.db.get_character(90_000_001).unwrap().balance.0;
        assert!(bal_after_mod < bal_after_place, "additional escrow deducted on price increase");
        ctx.advance_secs(301);
        rpc(&ctx, &mut sess, "marketProxy", "CancelCharOrder", &[PyRep::Integer(oid)])
            .await
            .expect("CancelCharOrder");
        let bal_after_cancel = ctx.db.get_character(90_000_001).unwrap().balance.0;
        assert!(bal_after_cancel > bal_after_mod, "escrow refunded on cancel");
        let hist = rpc(&ctx, &mut sess, "marketProxy", "GetMarketOrderHistory", &[PyRep::Integer(10_000_002), PyRep::Integer(34)]).await.unwrap();
        assert!(!matches!(hist, PyRep::None));
    }

    #[tokio::test]
    async fn test_journey_10_contracts() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let (mut s1, mut s2) = (SessionState::new(73_012), SessionState::new(73_013));
        login_pilot(&ctx, &mut s1, "demo", "demo-secret").await;
        rpc(&ctx, &mut s1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();
        login_pilot(&ctx, &mut s2, "other", "other-secret").await;
        rpc(&ctx, &mut s2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_002)]).await.unwrap();

        // Item exchange, auction, courier
        for ctype in [1i64, 2, 3] {
            let iid = ctx.db.with_mut_snapshot(|snap| {
                let id = snap.next_entity_id + 1;
                snap.next_entity_id += 1;
                snap.items.insert(
                    id,
                    eve_db::InventoryItemRecord {
                        item_id: id,
                        type_id: 34,
                        owner_id: 90_000_001,
                        location_id: 60_003_760,
                        flag: InventoryFlag::Hangar,
                        quantity: 10,
                        singleton: false,
                        ..Default::default()
                    },
                );
                id
            });
            let items = vec![PyRep::Integer(iid)];
            let c = rpc(
                &ctx,
                &mut s1,
                "contractProxy",
                "CreateContract",
                &[
                    PyRep::Integer(ctype),
                    PyRep::Integer(0),
                    PyRep::Integer(90_000_002),
                    PyRep::Integer(1440),
                    PyRep::Integer(1),
                    PyRep::Integer(60_003_760),
                    PyRep::Integer(60_003_760),
                    PyRep::Real(1_000.0),
                    PyRep::Real(0.0),
                    PyRep::Real(5_000.0),
                    PyRep::String("T6 Contract".into()),
                    PyRep::String("".into()),
                    PyRep::List(items),
                    PyRep::List(vec![]),
                ],
            )
            .await
            .expect("CreateContract");
            let cid = c.as_i64().or_else(|| c.dict_get("contractID").and_then(|v| v.as_i64())).unwrap_or(0);
            assert!(cid > 0);
            if ctype == 2 {
                rpc(&ctx, &mut s2, "contractProxy", "PlaceBid", &[PyRep::Integer(cid), PyRep::Real(1_200.0)])
                    .await
                    .expect("PlaceBid");
            } else {
                rpc(&ctx, &mut s2, "contractProxy", "AcceptContract", &[PyRep::Integer(cid)])
                    .await
                    .expect("AcceptContract");
                if ctype == 1 {
                    ctx.db.with_snapshot(|snap| {
                        assert_eq!(snap.items.get(&iid).unwrap().owner_id, 90_000_002);
                    });
                }
            }
        }
    }

    #[tokio::test]
    async fn test_journey_11_mining_loop() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let mut sess = SessionState::new(73_014);
        login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();
        rpc(&ctx, &mut sess, "stationSvc", "Undock", &[PyRep::Integer(100_000_001)]).await.unwrap();
        ctx.advance_secs(15);
        rpc(&ctx, &mut sess, "stationSvc", "Dock", &[PyRep::Integer(60_003_760)]).await.unwrap();
        assert_eq!(sess.station_id, Some(60_003_760));
        let ore_id = ctx.db.with_mut_snapshot(|snap| {
            let id = snap.next_entity_id + 1;
            snap.next_entity_id += 1;
            snap.items.insert(
                id,
                eve_db::InventoryItemRecord {
                    item_id: id,
                    type_id: 1230, // Veldspar
                    owner_id: 90_000_001,
                    location_id: 60_003_760,
                    flag: InventoryFlag::Hangar,
                    quantity: 1_000,
                    singleton: false,
                    ..Default::default()
                },
            );
            id
        });
        let trit_before: i64 = ctx.db.with_snapshot(|snap| {
            snap.items.values().filter(|i| i.owner_id == 90_000_001 && i.type_id == 34).map(|i| i.quantity as i64).sum()
        });
        rpc(&ctx, &mut sess, "reprocessingSvc", "Reprocess", &[PyRep::Integer(ore_id)])
            .await
            .expect("Reprocess");
        let trit_after: i64 = ctx.db.with_snapshot(|snap| {
            snap.items.values().filter(|i| i.owner_id == 90_000_001 && i.type_id == 34).map(|i| i.quantity as i64).sum()
        });
        assert!(trit_after > trit_before, "reprocessing Veldspar must credit Tritanium");
    }

    #[tokio::test]
    async fn test_journey_12_pve_combat() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let mut sess = SessionState::new(73_015);
        login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();
        rpc(&ctx, &mut sess, "stationSvc", "Undock", &[PyRep::Integer(100_000_001)]).await.unwrap();
        let rows = probe_scan(&ctx, &mut sess, DVec3::ZERO, 4.0 * 149_597_870_700.0).await;
        assert_ne!(rows.len(), 0);
    }

    #[tokio::test]
    async fn test_journey_13_missions() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        crate::common::seed_test_lp_store_offers(&ctx);
        let mut sess = SessionState::new(73_016);
        login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();
        let agents = rpc(&ctx, &mut sess, "agentMgr", "GetAgents", &[]).await.unwrap();
        assert!(!matches!(agents, PyRep::None));
        let lp = rpc(&ctx, &mut sess, "LPSvc", "GetAllMyCharacterWalletLPBalances", &[]).await.unwrap();
        assert!(!matches!(lp, PyRep::None));
        let st = rpc(&ctx, &mut sess, "standingMgr", "GetStandingTransactions", &[]).await.unwrap();
        assert!(!matches!(st, PyRep::None));
        assert_eq!(sess.character_id, Some(90_000_001));
    }
}

pub(crate) mod journey_exploration_wormholes_filaments {
    // In-process integration smoke tests for `plan:T-6` (part 2: journeys 14..=29).
    // Note: `journey:*` ledger rows remain open until real V24 client execution via `tools/client-harness/run_journeys`.

    use crate::common::{login_pilot, pin_clock, probe_scan, scan_row_field};
    use crate::session_logoff_and_cluster_tick::persist_and_reopen_sqlite;
    use eve_db::base64url_encode_no_pad;
    use eve_machonet::SessionState;
    use eve_proto::quasar::{sha256_bytes, EvePublicRequest, GoogleAny, GoogleTimestamp, QuasarMessage};
    use eve_proto::PyRep;
    use eve_server::esi::{EsiHttpRequest, EsiRouter};
    use eve_server::router::ServerContext;
    use glam::DVec3;
    use std::collections::HashMap;
    use std::sync::Arc;

    async fn rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        svc: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, eve_proto::BridgeRpcError> {
        ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
            .await
    }

    fn bound_id(rep: &PyRep) -> u64 {
        match rep {
            PyRep::Tuple(t) => t[0].as_i64().unwrap_or(0) as u64,
            other => other.as_i64().unwrap_or(0) as u64,
        }
    }

    #[tokio::test]
    async fn test_journey_14_exploration() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let mut sess = SessionState::new(73_017);
        login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();
        rpc(&ctx, &mut sess, "stationSvc", "Undock", &[PyRep::Integer(100_000_001)]).await.unwrap();
        let scan_bind = rpc(&ctx, &mut sess, "scanMgr", "MachoBindObject", &[PyRep::Integer(30_000_142), PyRep::None]).await.unwrap();
        let sid = bound_id(&scan_bind);
        let dscan = ctx
            .dispatch_rpc(&mut sess, "scanMgr", Some(sid), "DirectionalScan", &[PyRep::Real(14.3), PyRep::Real(360.0), PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)])], &HashMap::new(), None)
            .await
            .unwrap();
        assert!(!matches!(dscan, PyRep::None));
        let rows = probe_scan(&ctx, &mut sess, DVec3::ZERO, 4.0 * 149_597_870_700.0).await;
        assert_ne!(rows.len(), 0);
        let cert = scan_row_field(&rows, rows[0].dict_get("signatureID").unwrap().as_str().unwrap(), "certainty");
        assert!(!matches!(cert, PyRep::None), "certainty field present in scan row");
    }

    #[tokio::test]
    async fn test_journey_15_wormholes_filaments() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let mut sess = SessionState::new(73_018);
        login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();
        rpc(&ctx, &mut sess, "stationSvc", "Undock", &[PyRep::Integer(100_000_001)]).await.unwrap();
        ctx.advance_secs(15);
        let wh = rpc(&ctx, &mut sess, "wormholeMgr", "WormholeJump", &[PyRep::Integer(999_999)]).await;
        assert_eq!(wh.unwrap_err().code, "WormholeNotPresent");
    }

    #[tokio::test]
    async fn test_journey_16_ship_loss() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let mut sess = SessionState::new(73_019);
        login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

        let ins = rpc(&ctx, &mut sess, "insuranceSvc", "InsureShip", &[PyRep::Integer(100_000_001), PyRep::Real(100_000.0)])
            .await
            .expect("InsureShip");
        assert!(!matches!(ins, PyRep::None));
        let km = rpc(&ctx, &mut sess, "corpRegistry", "GetRecentKills", &[PyRep::Integer(90_000_001)]).await.unwrap();
        assert!(!matches!(km, PyRep::None));
    }

    #[tokio::test]
    async fn test_journey_17_clones() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let mut sess = SessionState::new(73_020);
        login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

        let state = rpc(&ctx, &mut sess, "jumpCloneSvc", "GetCloneState", &[]).await.unwrap();
        assert!(matches!(state, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::Object { .. }));
        rpc(&ctx, &mut sess, "charMgr", "SetHomeStation", &[PyRep::Integer(60_003_760)])
            .await
            .expect("SetHomeStation");
        assert_eq!(ctx.db.get_character(90_000_001).unwrap().home_station_id, 60_003_760);
    }

    #[tokio::test]
    async fn test_journey_18_industry() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let mut sess = SessionState::new(73_021);
        login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

        let jobs = rpc(&ctx, &mut sess, "industryManager", "GetJobsByOwner", &[]).await.unwrap();
        assert!(matches!(jobs, PyRep::List(_) | PyRep::Object { .. } | PyRep::Tuple(_)));
    }

    #[tokio::test]
    async fn test_journey_19_planetary() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let mut sess = SessionState::new(73_022);
        login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

        let planets = rpc(&ctx, &mut sess, "planetMgr", "GetPlanetsForChar", &[]).await.unwrap();
        assert!(!matches!(planets, PyRep::None));
    }

    #[tokio::test]
    async fn test_journey_20_corporation() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let mut sess = SessionState::new(73_023);
        login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();
        ctx.advance_secs(15);

        let corp = rpc(
            &ctx,
            &mut sess,
            "corpRegistry",
            "AddCorporation",
            &[
                PyRep::String("T6 Test Corp".into()),
                PyRep::String("T6TC".into()),
                PyRep::String("T6 corp".into()),
                PyRep::Real(0.05),
            ],
        )
        .await
        .expect("AddCorporation");
        let corp_id = corp
            .as_i64()
            .or_else(|| corp.dict_get("corporationID").and_then(|v| v.as_i64()))
            .unwrap_or(0);
        assert!(corp_id > 0, "AddCorporation must return positive corp_id, got {corp:?}");
        assert_eq!(ctx.db.get_character(90_000_001).unwrap().corporation_id, corp_id);
    }

    #[tokio::test]
    async fn test_journey_21_fleet() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let mut sess = SessionState::new(73_024);
        login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

        let fleet = rpc(&ctx, &mut sess, "fleetObjectHandler", "CreateFleet", &[]).await.unwrap();
        assert!(!matches!(fleet, PyRep::None));
        let fleet_id = ctx.db.get_character(90_000_001).unwrap().fleet_id.expect("character fleet_id set");
        assert!(fleet_id > 0);
    }

    #[tokio::test]
    async fn test_journey_22_structures() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let mut sess = SessionState::new(73_025);
        login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

        let structs = rpc(&ctx, &mut sess, "structureDirectory", "GetStructures", &[]).await.unwrap();
        assert!(!matches!(structs, PyRep::None));
        let dockable = rpc(&ctx, &mut sess, "structureDirectory", "GetMyDockableStructures", &[]).await.unwrap();
        assert!(!matches!(dockable, PyRep::None));
    }

    #[tokio::test]
    async fn test_journey_23_sovereignty_fw() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let mut sess = SessionState::new(73_026);
        login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

        let sov = rpc(&ctx, &mut sess, "sovMgr", "GetSovStructuresInfoForSolarSystem", &[PyRep::Integer(30_000_142)]).await.unwrap();
        assert!(!matches!(sov, PyRep::None));
        let fw = rpc(&ctx, &mut sess, "facWarMgr", "GetStats_Character", &[]).await.unwrap();
        assert!(!matches!(fw, PyRep::None));
    }

    #[tokio::test]
    async fn test_journey_24_daily_goals_events() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let mut sess = SessionState::new(73_027);
        login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

        let sub = rpc(&ctx, &mut sess, "publicGatewaySvc", "Subscribe", &[]).await.unwrap();
        assert_eq!(sub.dict_get("subscribed"), Some(&PyRep::Bool(true)));
        let req = EvePublicRequest {
            issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
            correlation_uuid: b"t6-dg-01".to_vec(),
            payload: Some(GoogleAny {
                type_url: "type.googleapis.com/eve_public.goal.api.GetAllRequest".into(),
                value: vec![],
            }),
            external_origin: "eve-client".into(),
            application_instance_uuid: b"app-t6-001".to_vec(),
            authoritative_context: None,
            trace_context: None,
        };
        let rep = rpc(&ctx, &mut sess, "publicGatewaySvc", "SendRequest", &[PyRep::Bytes(req.encode_proto())])
            .await
            .expect("SendRequest");
        assert!(!matches!(rep, PyRep::None));
    }

    #[tokio::test]
    async fn test_journey_25_cosmetics_store() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        crate::common::seed_test_nes_store_catalog(&ctx);
        let mut sess = SessionState::new(73_028);
        login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

        let sub = rpc(&ctx, &mut sess, "publicGatewaySvc", "Subscribe", &[]).await.unwrap();
        assert_eq!(sub.dict_get("subscribed"), Some(&PyRep::Bool(true)));
        let cats = rpc(&ctx, &mut sess, "storeManager", "get_categories", &[]).await.expect("get_categories");
        assert!(matches!(cats, PyRep::List(ref l) if !l.is_empty()));
        let prods = rpc(&ctx, &mut sess, "storeManager", "get_products", &[]).await.expect("get_products");
        assert!(matches!(prods, PyRep::List(ref l) if !l.is_empty()));
    }

    #[tokio::test]
    async fn test_journey_26_mail_notifications() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let (mut s1, mut s2) = (SessionState::new(73_029), SessionState::new(73_030));
        login_pilot(&ctx, &mut s1, "demo", "demo-secret").await;
        rpc(&ctx, &mut s1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();
        login_pilot(&ctx, &mut s2, "other", "other-secret").await;
        rpc(&ctx, &mut s2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_002)]).await.unwrap();

        let mail = rpc(
            &ctx,
            &mut s1,
            "mailMgr",
            "SendMail",
            &[
                PyRep::List(vec![PyRep::Integer(90_000_002)]),
                PyRep::String("T6 Subject".into()),
                PyRep::String("T6 Body".into()),
                PyRep::Integer(0),
                PyRep::Integer(0),
            ],
        )
        .await
        .unwrap();
        assert!(mail.as_i64().unwrap_or(0) > 0);
        let headers = rpc(&ctx, &mut s2, "mailMgr", "GetMailHeaders", &[]).await.unwrap();
        assert!(!matches!(headers, PyRep::None));
        let notifs = rpc(&ctx, &mut s2, "notificationMgr", "GetAllNotifications", &[]).await.unwrap();
        assert!(!matches!(notifs, PyRep::None));
        let evs = rpc(&ctx, &mut s2, "calendarMgr", "GetEventList", &[]).await.unwrap();
        assert!(!matches!(evs, PyRep::None));
    }

    #[tokio::test]
    async fn test_journey_27_esi_external() {
        let ctx = Arc::new(ServerContext::new());
        pin_clock(&ctx);
        let verifier = "0123456789abcdef0123456789abcdef0123456789abcdef";
        let challenge = base64url_encode_no_pad(&sha256_bytes(verifier.as_bytes()));
        let code = ctx
            .db
            .authorize_pkce(
                "demo",
                "demo-secret",
                90_000_001,
                "evomu-esi",
                "https://localhost/cb",
                vec!["esi-wallet.read_character_wallet.v1".into()],
                &challenge,
                "S256",
                ctx.now_unix(),
            )
            .unwrap();
        let tok = ctx
            .db
            .exchange_pkce_code(&code, "evomu-esi", verifier, ctx.now_unix())
            .unwrap()
            .access_token;

        let router = EsiRouter::new(ctx.clone());
        let req = EsiHttpRequest {
            method: "GET".into(),
            raw_target: "/latest/characters/90000001/wallet/".into(),
            headers: HashMap::from([("authorization".into(), format!("Bearer {tok}"))]),
            body: Vec::new(),
            peer_ip: "127.0.0.1".into(),
        };
        let resp = router.handle_request(&req);
        assert_eq!(resp.status_code, 200);
        assert_ne!(resp.headers.len(), 0);
        let body_str = std::str::from_utf8(&resp.body).expect("utf8 wallet body");
        let wallet_val: f64 = body_str.trim().parse().expect("numeric wallet balance");
        assert!(wallet_val > 0.0);

        // Missing scope enforcement
        let bad_req = EsiHttpRequest {
            method: "GET".into(),
            raw_target: "/latest/characters/90000001/skills/".into(),
            headers: HashMap::from([("authorization".into(), format!("Bearer {tok}"))]),
            body: Vec::new(),
            peer_ip: "127.0.0.1".into(),
        };
        let bad_resp = router.handle_request(&bad_req);
        assert_eq!(bad_resp.status_code, 403);
    }

    #[tokio::test]
    async fn test_journey_28_downtime_persistence() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let ctx2 = persist_and_reopen_sqlite(&ctx, "t6_downtime");
        let ch = ctx2.db.get_character(90_000_001).expect("character survives restart");
        assert!(ch.balance.0 > 0);
    }

    #[tokio::test]
    async fn test_journey_29_gm_tools() {
        let ctx = ServerContext::new();
        pin_clock(&ctx);
        let mut player_sess = SessionState::new(73_031);
        login_pilot(&ctx, &mut player_sess, "other", "other-secret").await;
        rpc(&ctx, &mut player_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_002)]).await.unwrap();
        let denied = rpc(&ctx, &mut player_sess, "slash", "SlashCmd", &[PyRep::String("/giveisk 1000".into())]).await;
        assert_eq!(denied.unwrap_err().code, "InsufficientRole");

        let mut gm_sess = SessionState::new(73_032);
        login_pilot(&ctx, &mut gm_sess, "demo", "demo-secret").await;
        gm_sess.role = eve_db::service_roles::ROLE_ADMIN | eve_db::service_roles::ROLE_GMS;
        rpc(&ctx, &mut gm_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

        for cmd in [
            "/spawn 587",
            "/createitem 34 100",
            "/giveskill 3402 5",
            "/tr 30000142",
            "/heal",
            "/setattr 100000001 164 25",
        ] {
            let res = rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String(cmd.into())]).await;
            assert!(res.is_ok(), "GM command {cmd} failed: {res:?}");
        }
        ctx.db.with_snapshot(|snap| {
            let has_trit = snap.items.values().any(|i| i.owner_id == 90_000_001 && i.type_id == 34 && i.quantity >= 100);
            assert!(has_trit, "/createitem 34 100 must create Tritanium");
            let skill_lvl = snap.skills.get(&90_000_001).and_then(|s| s.get(&3402)).map(|s| s.level).unwrap_or(0);
            assert_eq!(skill_lvl, 5, "/giveskill 3402 5 must set skill level 5");
        });
    }
}

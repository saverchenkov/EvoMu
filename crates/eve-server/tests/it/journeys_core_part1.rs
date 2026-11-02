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

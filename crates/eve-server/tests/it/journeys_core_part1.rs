//! End-to-End Integration Journeys Part 1 (`journey:launch_login` .. `journey:multiplayer_presence`).
#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_machonet::{
    CryptoRequestPacket, SessionState, VersionExchange,
};
use eve_proto::PyRep;
use eve_server::macho_listener::{handle_macho_client, read_framed_pyrep, write_framed_pyrep};
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
    ctx.dispatch_rpc(
        sess,
        service,
        Some(bound_id),
        method,
        args,
        &HashMap::new(),
        None,
    )
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
    // parity: journey:launch_login
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
            "eve-client",
            "https://localhost/callback",
            vec!["publicData".into()],
            challenge,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    let token_bundle = ctx
        .db
        .exchange_pkce_code(&code, "eve-client", verifier, now)
        .expect("exchange_pkce_code");
    assert!(!token_bundle.access_token.is_empty());

    // 2. TCP machoNet V24.01 handshake
    let mut stream = TcpStream::connect(addr).await.expect("connect macho");
    let hello = read_framed_pyrep(&mut stream).await.unwrap().unwrap();
    let PyRep::Tuple(hello_items) = &hello else {
        panic!("expected 7-tuple hello, got {hello:?}");
    };
    assert_eq!(hello_items.len(), 7);
    assert_eq!(hello_items[5].as_str(), Some("V24.01@ccp"));

    let ver = VersionExchange {
        birthday: 170_472,
        macho_version: 496,
        user_count: 0,
        version_number: 24.01,
        build_version: 3_624_425,
        project_version: "V24.01@ccp".into(),
    };
    write_framed_pyrep(&mut stream, &ver.to_pyrep()).await.unwrap();
    let crypto_req = CryptoRequestPacket {
        key_version: "placebo".into(),
        key_params: HashMap::new(),
    };
    write_framed_pyrep(&mut stream, &crypto_req.to_pyrep())
        .await
        .unwrap();
    let crypto_reply = read_framed_pyrep(&mut stream).await.unwrap().unwrap();
    assert_eq!(crypto_reply.as_str(), Some("OK CC"));

    // 3. Authenticate and list characters
    let mut sess = SessionState::new(1);
    let login_res = call_rpc(
        &ctx,
        &mut sess,
        "authentication",
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
    )
    .await
    .expect("authentication.Login");
    assert!(!matches!(login_res, PyRep::None));

    let chars = call_rpc(&ctx, &mut sess, "charUnboundMgr", "GetCharactersToSelect", &[])
        .await
        .expect("GetCharactersToSelect");
    assert!(!matches!(chars, PyRep::None));

    call_rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
    )
    .await
    .expect("SelectCharacterID");
    assert_eq!(sess.character_id, Some(90_000_001));
}

#[tokio::test]
async fn test_journey_create_character() {
    // parity: journey:create_character
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
        ("Caldari Pilot", 1_i64, 1_i64, 17_i64, true),
        ("Minmatar Pilot", 3_i64, 1_i64, 14_i64, true),
        ("Amarr Pilot", 5_i64, 1_i64, 11_i64, false),
        ("Gallente Pilot", 7_i64, 1_i64, 20_i64, false),
    ];

    for (name, bloodline_id, gender_id, school_id, use_first) in race_specs {
        let ancestry_id = ctx
            .sde
            .ancestries
            .values()
            .find(|a| a.bloodline_id as i64 == bloodline_id)
            .map(|a| a.ancestry_id as i64)
            .unwrap_or(1);
        let active_sess = if use_first { &mut sess } else { &mut sess2 };
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
                PyRep::String(format!("doll:{name}")),
                PyRep::String(format!("portrait:{name}")),
                PyRep::Integer(school_id),
            ],
        )
        .await
        .expect("CreateCharacterWithDoll");
        let cid = char_id_rep.as_i64().expect("character id");
        assert!(cid > 200_000_000);

        // Verify character exists in DB with starter ISK, station, ship, and skills
        ctx.db.with_snapshot(|snap| {
            let ch = snap.characters.get(&cid).expect("character in DB");
            assert_eq!(ch.name, name);
            assert!(ch.station_id.unwrap_or(0) > 60_000_000);
            assert!(ch.balance.0 > 0);
            let has_ship = snap
                .items
                .values()
                .any(|it| it.owner_id == cid && it.flag == InventoryFlag::Hangar);
            assert!(has_ship, "starter ship in hangar for {name}");
            let has_skills = snap.skills.contains_key(&cid);
            assert!(has_skills, "starter skills for {name}");
        });
    }
}

#[tokio::test]
async fn test_journey_station_ui() {
    // parity: journey:station_ui
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();
    crate::common::seed_test_nes_store_catalog(&ctx);

    let mut sess = SessionState::new(11);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
    )
    .await
    .expect("SelectCharacterID");

    // Hangars / Inventory
    let inv_bind = call_rpc(
        &ctx,
        &mut sess,
        "invbroker",
        "MachoBindObject",
        &[PyRep::Integer(60_003_760), PyRep::None],
    )
    .await
    .expect("invbroker bind");
    let inv_bid = bound_id_from_tuple(&inv_bind);
    let hangar_list = call_bound_rpc(&ctx, &mut sess, "invbroker", inv_bid, "List", &[])
        .await
        .expect("invbroker.List");
    assert!(!matches!(hangar_list, PyRep::None));

    // Market, Character Sheet, Skills, Wallet, Journal, Corp, Bookmarks, Mail, Notifications, Calendar
    let mkt = call_rpc(&ctx, &mut sess, "marketProxy", "GetMarketGroups", &[])
        .await
        .expect("GetMarketGroups");
    assert!(!matches!(mkt, PyRep::None));

    let sheet = call_rpc(
        &ctx,
        &mut sess,
        "charMgr",
        "GetCharacterInfo",
        &[PyRep::Integer(90_000_001)],
    )
    .await
    .expect("GetCharacterInfo");
    assert!(!matches!(sheet, PyRep::None));

    let skill_bind = call_rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "MachoBindObject",
        &[PyRep::Integer(90_000_001), PyRep::None],
    )
    .await
    .expect("skillMgr2 bind");
    let skill_bid = bound_id_from_tuple(&skill_bind);
    let skills = call_bound_rpc(&ctx, &mut sess, "skillMgr2", skill_bid, "GetSkills", &[])
        .await
        .expect("GetSkills");
    assert!(!matches!(skills, PyRep::None));

    let bal = call_rpc(&ctx, &mut sess, "account", "GetCashBalance", &[])
        .await
        .expect("GetCashBalance");
    assert!(bal.as_f64().unwrap_or(0.0) > 0.0);

    let journal = call_rpc(&ctx, &mut sess, "account", "GetTransactions", &[])
        .await
        .expect("GetTransactions");
    assert!(!matches!(journal, PyRep::None));

    let bms = call_rpc(&ctx, &mut sess, "bookmark", "GetBookmarks", &[])
        .await
        .expect("GetBookmarks");
    assert!(!matches!(bms, PyRep::None));

    let mail = call_rpc(&ctx, &mut sess, "mailMgr", "SyncMail", &[])
        .await
        .expect("SyncMail");
    assert!(!matches!(mail, PyRep::None));

    let notifs = call_rpc(&ctx, &mut sess, "notificationMgr", "GetUnprocessed", &[])
        .await
        .expect("GetUnprocessed");
    assert!(!matches!(notifs, PyRep::None));

    let cal = call_rpc(&ctx, &mut sess, "calendarMgr", "GetEventList", &[])
        .await
        .expect("GetEventList");
    assert!(!matches!(cal, PyRep::None));
}

#[tokio::test]
async fn test_journey_fit_ship() {
    // parity: journey:fit_ship
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    // Seed Small Hybrid Turret (3301) level 5, Light Electron Blaster I (type 561) and Antimatter Charge S (type 222)
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills
            .entry(90_000_001)
            .or_default()
            .insert(
                3301,
                eve_db::TrainedSkillRecord {
                    type_id: 3301,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        snap.items.insert(
            100_000_501,
            InventoryItemRecord {
                item_id: 100_000_501,
                type_id: 561,
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
        snap.items.insert(
            100_000_502,
            InventoryItemRecord {
                item_id: 100_000_502,
                type_id: 222,
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
    });

    let mut sess = SessionState::new(12);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
    )
    .await
    .expect("SelectCharacterID");

    let ship_id = sess.ship_id.expect("active ship_id");
    let dogma_bind = call_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "MachoBindObject",
        &[PyRep::Integer(ship_id), PyRep::None],
    )
    .await
    .expect("dogmaIM bind");
    let dogma_bid = bound_id_from_tuple(&dogma_bind);

    // Fit module to HiSlot1 (flag 28)
    call_rpc(
        &ctx,
        &mut sess,
        "invbroker",
        "Add",
        &[
            PyRep::Integer(100_000_501),
            PyRep::Integer(ship_id),
            PyRep::Integer(28),
        ],
    )
    .await
    .expect("fit module");

    // Load ammo into module
    call_bound_rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        dogma_bid,
        "LoadAmmo",
        &[
            PyRep::Integer(ship_id),
            PyRep::Integer(100_000_501),
            PyRep::Integer(100_000_502),
            PyRep::Integer(60_003_760),
        ],
    )
    .await
    .expect("LoadAmmo");

    // Save fitting
    let mut fit_kv = HashMap::new();
    fit_kv.insert("name".into(), PyRep::String("Journey Fit".into()));
    fit_kv.insert("description".into(), PyRep::String("Test fit".into()));
    fit_kv.insert("shipTypeID".into(), PyRep::Integer(603));
    fit_kv.insert("fitData".into(), PyRep::List(vec![]));
    let saved = call_rpc(
        &ctx,
        &mut sess,
        "charFittingMgr",
        "SaveFitting",
        &[PyRep::Integer(90_000_001), PyRep::KeyVal(fit_kv)],
    )
    .await
    .expect("SaveFitting");
    assert!(saved.as_i64().unwrap_or(0) > 0);
    ctx.db.with_snapshot(|snap| {
        let mod_item = snap.items.get(&100_000_501).expect("fitted module");
        assert_eq!(mod_item.location_id, ship_id);
        assert_eq!(mod_item.flag, InventoryFlag::HiSlot1);
    });
}

#[tokio::test]
async fn test_journey_undock_navigate() {
    // parity: journey:undock_navigate
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(13);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
    )
    .await
    .expect("SelectCharacterID");

    // Undock
    call_rpc(&ctx, &mut sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock");
    assert!(sess.station_id.is_none());
    let ship_id = sess.ship_id.expect("ship_id");

    // Bind beyonce and align/stop
    let bey_bind = call_rpc(
        &ctx,
        &mut sess,
        "beyonce",
        "MachoBindObject",
        &[PyRep::Integer(30_000_142), PyRep::None],
    )
    .await
    .expect("beyonce bind");
    let bey_bid = bound_id_from_tuple(&bey_bind);

    call_bound_rpc(
        &ctx,
        &mut sess,
        "beyonce",
        bey_bid,
        "CmdAlignTo",
        &[PyRep::Integer(40_009_076)],
    )
    .await
    .expect("CmdAlignTo");
    ctx.advance_secs(5);

    call_bound_rpc(&ctx, &mut sess, "beyonce", bey_bid, "CmdStop", &[])
        .await
        .expect("CmdStop");
    ctx.advance_secs(2);
    assert!(ship_id > 0);
}

#[tokio::test]
async fn test_journey_local_chat() {
    // parity: journey:local_chat
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess1 = SessionState::new(14);
    crate::common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    call_rpc(
        &ctx,
        &mut sess1,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
    )
    .await
    .expect("SelectCharacterID 1");

    ctx.db.provision_player_account("demo2", "demo2-secret");
    let mut sess2 = SessionState::new(15);
    crate::common::login_pilot(&ctx, &mut sess2, "demo2", "demo2-secret").await;
    let cid2 = call_rpc(
        &ctx,
        &mut sess2,
        "charUnboundMgr",
        "CreateCharacterWithDoll",
        &[
            PyRep::String("Chat Wingman".into()),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(11),
            PyRep::String("doll:chat".into()),
            PyRep::String("portrait:chat".into()),
            PyRep::Integer(17),
        ],
    )
    .await
    .expect("CreateCharacterWithDoll 2")
    .as_i64()
    .unwrap();
    call_rpc(
        &ctx,
        &mut sess2,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(cid2)],
    )
    .await
    .expect("SelectCharacterID 2");

    let tok1 = call_rpc(
        &ctx,
        &mut sess1,
        "chatAuthenticationService",
        "GetAuthenticationToken",
        &[],
    )
    .await
    .expect("GetAuthenticationToken 1");
    assert!(matches!(tok1, PyRep::KeyVal(_)));

    let chan_id = PyRep::String("solarsystemid2_30000142".into());
    let sent = call_rpc(
        &ctx,
        &mut sess1,
        "LSC",
        "SendMessage",
        &[chan_id, PyRep::String("Hello Jita local!".into())],
    )
    .await
    .expect("SendMessage");
    assert_eq!(sess1.solar_system_id, Some(30_000_142));
    assert_eq!(sess2.solar_system_id, Some(30_000_141));
    let _ = sent;
}

#[tokio::test]
async fn test_journey_multiplayer_presence() {
    // parity: journey:multiplayer_presence
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess1 = SessionState::new(16);
    crate::common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    call_rpc(
        &ctx,
        &mut sess1,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
    )
    .await
    .expect("SelectCharacterID 1");

    ctx.db.provision_player_account("demo2", "demo2-secret");
    let mut sess2 = SessionState::new(17);
    crate::common::login_pilot(&ctx, &mut sess2, "demo2", "demo2-secret").await;
    let cid2 = call_rpc(
        &ctx,
        &mut sess2,
        "charUnboundMgr",
        "CreateCharacterWithDoll",
        &[
            PyRep::String("Space Wingman".into()),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(11),
            PyRep::String("doll:space".into()),
            PyRep::String("portrait:space".into()),
            PyRep::Integer(17),
        ],
    )
    .await
    .expect("CreateCharacterWithDoll 2")
    .as_i64()
    .unwrap();
    call_rpc(
        &ctx,
        &mut sess2,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(cid2)],
    )
    .await
    .expect("SelectCharacterID 2");

    // Both undock into Jita (30000142)
    call_rpc(&ctx, &mut sess1, "stationSvc", "Undock", &[])
        .await
        .expect("Undock 1");
    call_rpc(&ctx, &mut sess2, "stationSvc", "Undock", &[])
        .await
        .expect("Undock 2");
    ctx.advance_secs(2);

    let ship1 = sess1.ship_id.expect("ship1");
    let ship2 = sess2.ship_id.expect("ship2");
    assert_ne!(ship1, ship2);
}

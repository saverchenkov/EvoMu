//! In-process integration smoke tests for `plan:T-6` (part 1: journeys 1..=13).
//! Note: `journey:*` ledger rows remain open until real V24 client execution via `tools/client-harness/run_journeys`.
#![forbid(unsafe_code)]

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

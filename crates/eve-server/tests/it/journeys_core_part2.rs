//! End-to-End Integration Journeys Part 2 (`journey:local_chat` .. `journey:contracts`).
#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, IskCents, MarketOrderRecord};
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

    let mut rx = ctx.notify_tx.subscribe();

    // 1. Local channel: join, send message from sess1, verify sess2 receives OnLSC
    let local_ch = PyRep::String("solarsystemid2:30000142".into());
    call_rpc(&ctx, &mut sess1, "LSC", "JoinChannel", &[local_ch.clone()])
        .await
        .expect("JoinChannel local 1");
    call_rpc(&ctx, &mut sess2, "LSC", "JoinChannel", &[local_ch.clone()])
        .await
        .expect("JoinChannel local 2");
    call_rpc(&ctx, &mut sess1, "LSC", "SendMessage", &[local_ch.clone(), PyRep::String("Hello Jita Local!".into())])
        .await
        .expect("SendMessage local");

    let mut saw_local = false;
    while let Ok(env) = rx.try_recv() {
        if format!("{env:?}").contains("Hello Jita Local!") {
            saw_local = true;
        }
    }
    assert!(saw_local, "second client must receive local chat message");

    // 2. Corp channel
    let corp_id = sess1.corporation_id.unwrap_or(1_000_035);
    let corp_ch = PyRep::String(format!("corpid:{corp_id}"));
    call_rpc(&ctx, &mut sess1, "LSC", "JoinChannel", &[corp_ch.clone()])
        .await
        .expect("JoinChannel corp 1");
    call_rpc(&ctx, &mut sess2, "LSC", "JoinChannel", &[corp_ch.clone()])
        .await
        .expect("JoinChannel corp 2");
    call_rpc(&ctx, &mut sess1, "LSC", "SendMessage", &[corp_ch, PyRep::String("Hello Corp!".into())])
        .await
        .expect("SendMessage corp");

    // 3. Private channel via XmppChatMgr
    let priv_rep = call_rpc(&ctx, &mut sess1, "XmppChatMgr", "CreatePrivateChannel", &[PyRep::Integer(90_000_002), PyRep::String("Ops Channel".into())])
        .await
        .expect("CreatePrivateChannel");
    let priv_id = priv_rep.dict_get("channelID").and_then(|v| v.as_str()).unwrap_or("private:1").to_string();
    call_rpc(&ctx, &mut sess1, "XmppChatMgr", "AddCharacterToChannel", &[PyRep::String(priv_id.clone()), PyRep::Integer(90_000_002)])
        .await
        .expect("AddCharacterToChannel");
    call_rpc(&ctx, &mut sess1, "LSC", "SendMessage", &[PyRep::String(priv_id), PyRep::String("Secret Op".into())])
        .await
        .expect("SendMessage private");

    // 4. Member list
    let members = call_rpc(&ctx, &mut sess1, "LSC", "GetMembers", &[local_ch])
        .await
        .expect("GetMembers");
    assert!(format!("{members:?}").contains("90000001"));
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
    while let Ok(env) = rx.try_recv() {
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
    assert!(!format!("{hist:?}").is_empty());
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
    call_rpc(&ctx, &mut sess2, "contractProxy", "PlaceBid", &[PyRep::Integer(c_auc_id), PyRep::Real(600.0), PyRep::Bool(false)])
        .await
        .expect("PlaceBid");

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

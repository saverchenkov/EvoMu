//! Integration tests covering corporate wallet division market accounting, courier plastic wrap
//! unwrapping and collateral forfeiture, corporate role bitmask precision (container, wallet, and
//! governance), corporation departure stasis & personal share retention, and war HQ/ally mechanics.

#![forbid(unsafe_code)]

use eve_db::{corp_roles, InventoryFlag, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const JITA_44: i64 = 60_003_760;
const JITA_SYS: u32 = 30_000_142;
const PERIMETER_SYS: u32 = 30_000_144;
const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
}

async fn slash(ctx: &ServerContext, sess: &mut SessionState, cmd: &str) -> PyRep {
    rpc(ctx, sess, "slash", "SlashCmd", &[PyRep::String(cmd.into())])
        .await
        .unwrap_or_else(|e| panic!("SlashCmd({cmd}) failed: {e:?}"))
}

async fn create_item(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, qty: i32) -> i64 {
    let rep = slash(ctx, sess, &format!("/createitem {type_id} {qty}")).await;
    rep.dict_get("itemID")
        .and_then(|v| v.as_i64())
        .unwrap_or_else(|| panic!("/createitem {type_id} {qty} missing itemID: {rep:?}"))
}

fn secret_for(username: &str) -> &'static str {
    match username {
        "demo" => "demo-secret",
        "other" => "other-secret",
        "ally3" => "ally3-secret",
        "mem4" => "mem4-secret",
        _ => panic!("unknown test user {username}"),
    }
}

async fn refresh_session(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) {
    ctx.advance_secs(12);
    rpc(ctx, sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID refresh");
    ctx.advance_secs(12);
}

async fn relogin_with_role(ctx: &ServerContext, sess: &mut SessionState, username: &str, char_id: i64, role: u64) {
    ctx.db.set_account_role(username, role).unwrap();
    crate::common::login_pilot(ctx, sess, username, secret_for(username)).await;
    refresh_session(ctx, sess, char_id).await;
}

async fn grant_skills_and_isk(ctx: &ServerContext, sess: &mut SessionState, username: &str, char_id: i64, skills: &[u32], isk: u64) {
    relogin_with_role(ctx, sess, username, char_id, eve_db::service_roles::ROLE_GM_DEFAULT).await;
    slash(ctx, sess, &format!("/giveisk {isk}")).await;
    for &sk in skills {
        slash(ctx, sess, &format!("/giveskill {sk} 5")).await;
    }
    relogin_with_role(ctx, sess, username, char_id, eve_db::service_roles::ROLE_PLAYER_DEFAULT).await;
}

async fn create_item_as_gm(ctx: &ServerContext, sess: &mut SessionState, username: &str, char_id: i64, type_id: u32, qty: i32) -> i64 {
    relogin_with_role(ctx, sess, username, char_id, eve_db::service_roles::ROLE_GM_DEFAULT).await;
    let item_id = create_item(ctx, sess, type_id, qty).await;
    relogin_with_role(ctx, sess, username, char_id, eve_db::service_roles::ROLE_PLAYER_DEFAULT).await;
    item_id
}

async fn gm_tr_and_create(ctx: &ServerContext, sess: &mut SessionState, username: &str, char_id: i64, sys_id: u32, type_id: u32, qty: i32) -> i64 {
    relogin_with_role(ctx, sess, username, char_id, eve_db::service_roles::ROLE_GM_DEFAULT).await;
    slash(ctx, sess, &format!("/tr {sys_id}")).await;
    ctx.advance_secs(12);
    let item_id = create_item(ctx, sess, type_id, qty).await;
    relogin_with_role(ctx, sess, username, char_id, eve_db::service_roles::ROLE_PLAYER_DEFAULT).await;
    item_id
}

async fn set_corp_roles(ctx: &ServerContext, ceo_sess: &mut SessionState, member_sess: &mut SessionState, member_id: i64, roles: u64) {
    rpc(ctx, ceo_sess, "corpRegistry", "UpdateMember", &[PyRep::Integer(member_id), PyRep::Integer(roles as i64), PyRep::Integer(0)])
        .await
        .expect("corpRegistry.UpdateMember");
    ctx.advance_secs(86_405);
    let _ = rpc(ctx, ceo_sess, "corpRegistry", "GetMembers", &[]).await.expect("GetMembers promote");
    refresh_session(ctx, member_sess, member_id).await;
}

async fn login_two_docked(ctx: &ServerContext) -> (SessionState, SessionState) {
    let mut s1 = SessionState::new(801);
    crate::common::login_pilot(ctx, &mut s1, "demo", "demo-secret").await;
    rpc(ctx, &mut s1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P1)]).await.unwrap();
    grant_skills_and_isk(ctx, &mut s1, "demo", P1, &[3363, 3368, 3443, 16622], 5_000_000_000).await;

    let mut s2 = SessionState::new(802);
    crate::common::login_pilot(ctx, &mut s2, "other", "other-secret").await;
    rpc(ctx, &mut s2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P2)]).await.unwrap();
    grant_skills_and_isk(ctx, &mut s2, "other", P2, &[3363, 3368, 3443, 16622], 5_000_000_000).await;
    (s1, s2)
}

async fn create_corp_and_join_p2(ctx: &ServerContext, s1: &mut SessionState, s2: &mut SessionState, name: &str, ticker: &str) -> i64 {
    let rep = rpc(ctx, s1, "corpRegistry", "AddCorporation", &[PyRep::String(name.into()), PyRep::String(ticker.into())]).await.expect("AddCorporation");
    let corp_id = rep.dict_get("corporationID").and_then(|v| v.as_i64()).unwrap();
    refresh_session(ctx, s1, P1).await;
    let app = rpc(ctx, s2, "corpRegistry", "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("join".into())]).await.expect("InsertApplication");
    let app_id = app.dict_get("applicationID").and_then(|v| v.as_i64()).unwrap();
    ctx.advance_secs(12);
    rpc(ctx, s1, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(app_id), PyRep::String("Accepted".into())]).await.expect("Accept application");
    refresh_session(ctx, s2, P2).await;
    corp_id
}

#[tokio::test]
async fn test_corp_market_divisions_no_isk_mint_or_div1_corruption() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (mut s1, mut s2) = login_two_docked(&ctx).await;
    let corp_id = create_corp_and_join_p2(&ctx, &mut s1, &mut s2, "Market Div Corp", "MDIV").await;

    let div1_init = IskCents::from_isk(500_000_000);
    let div2_init = IskCents::from_isk(200_000_000);
    // Fund Corp Div 1 with 700M, then move 200M to Div 2 (accountKey 1001) via real wallet RPCs.
    rpc(&ctx, &mut s1, "account", "GiveCash", &[PyRep::Integer(corp_id), PyRep::Real(700_000_000.0), PyRep::String("Div funding".into())])
        .await.expect("GiveCash to corp div 1");
    let mut div2_kw = HashMap::new();
    div2_kw.insert("toAccountKey".into(), PyRep::Integer(1001));
    ctx.dispatch_rpc(
        &mut s1, "account", None, "GiveCashFromCorpAccount",
        &[PyRep::Integer(corp_id), PyRep::Real(200_000_000.0), PyRep::Integer(1000)], &div2_kw, None,
    ).await.expect("Move 200M from Div 1 to Div 2");

    // Pick an unseeded marketable typeID in Jita so P2's buy order fills exclusively against our Div 2 sell order.
    let sell_tid = [4_051_u32, 17_366, 35_892, 35].into_iter().find(|&tid| {
        ctx.sde.types.get(&tid).and_then(|t| t.market_group_id).is_some()
            && ctx.db.with_snapshot(|s| !s.market_orders.values().any(|o| o.type_id == tid && o.station_id == JITA_44))
    }).unwrap_or(35);
    let sell_item_id = create_item_as_gm(&ctx, &mut s1, "demo", P1, sell_tid, 1_000).await;

    rpc(&ctx, &mut s1, "corpRegistry", "SetAccountKey", &[PyRep::Integer(1001)]).await.expect("SetAccountKey 1001");
    let buy_rep = rpc(
        &ctx, &mut s1, "marketProxy", "PlaceBuyOrder",
        &[PyRep::Integer(JITA_44), PyRep::Integer(sell_tid as i64), PyRep::Real(1.0), PyRep::Integer(1_000_000), PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(true), PyRep::Integer(1001)],
    ).await.expect("PlaceBuyOrder on Div 2");
    assert_eq!(buy_rep.dict_get("filledImmediately").and_then(|v| v.as_bool()), Some(false), "Buy order must rest unfilled");
    let buy_order_id = buy_rep.as_i64().or_else(|| buy_rep.dict_get("orderID").and_then(|v| v.as_i64())).unwrap_or_else(|| {
        ctx.db.with_snapshot(|s| *s.market_orders.iter().find(|(_, o)| o.owner_id == corp_id && o.is_bid).map(|(oid, _)| oid).expect("corp buy order"))
    });

    ctx.db.with_snapshot(|snap| {
        let c = &snap.corporations[&corp_id];
        assert_eq!(c.division_balance(1), div1_init, "Div 1 must remain untouched by Div 2 buy order");
        assert_eq!(c.master_wallet, div1_init);
        assert!(c.division_balance(2).0 < div2_init.0);
    });

    rpc(&ctx, &mut s1, "marketProxy", "CancelCharOrder", &[PyRep::Integer(buy_order_id), PyRep::Integer(JITA_SYS as i64)]).await.expect("CancelCharOrder");
    let div2_after_cancel = ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(2));

    rpc(
        &ctx, &mut s1, "marketProxy", "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_44)), ("typeID", PyRep::Integer(sell_tid as i64)), ("itemID", PyRep::Integer(sell_item_id)),
                ("price", PyRep::Real(5_000.0)), ("quantity", PyRep::Integer(1_000)), ("duration", PyRep::Integer(90)),
            ])]),
            PyRep::Bool(true), PyRep::Integer(90), PyRep::None, PyRep::Integer(1001),
        ],
    ).await.expect("PlaceMultiSellOrder on Div 2");

    rpc(
        &ctx, &mut s2, "marketProxy", "PlaceBuyOrder",
        &[PyRep::Integer(JITA_44), PyRep::Integer(sell_tid as i64), PyRep::Real(5_000.0), PyRep::Integer(1_000), PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(0), PyRep::Bool(false), PyRep::Integer(1000)],
    ).await.expect("P2 immediate fill");

    ctx.db.with_snapshot(|snap| {
        let c = &snap.corporations[&corp_id];
        assert_eq!(c.division_balance(1), div1_init, "Div 1 must remain untouched after Div 2 sell proceeds");
        assert_eq!(c.master_wallet, div1_init);
        assert!(c.division_balance(2).0 > div2_after_cancel.0);
        assert!(c.division_balance(2).0 < div2_after_cancel.0 + IskCents::from_isk(5_000_000).0, "No double-credit ISK mint");
    });
}

#[tokio::test]
async fn test_break_plastic_wrap_requires_colocation_and_fails_courier_collateral() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (mut s1, mut s2) = login_two_docked(&ctx).await;

    let cargo_id = create_item_as_gm(&ctx, &mut s1, "demo", P1, 34, 500).await;

    let (collateral, reward) = (10_000_000.0, 1_000_000.0);
    let c_rep = rpc(
        &ctx, &mut s1, "contractProxy", "CreateContract",
        &[
            PyRep::Integer(3), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(1440), PyRep::Integer(1),
            PyRep::Integer(JITA_44), PyRep::Integer(JITA_44), PyRep::Real(0.0), PyRep::Real(reward),
            PyRep::Real(collateral), PyRep::String("Courier".into()), PyRep::String("".into()),
            PyRep::List(vec![PyRep::Integer(cargo_id)]),
        ],
    ).await.expect("Create Courier contract");
    let contract_id = c_rep.dict_get("contractID").and_then(|v| v.as_i64()).unwrap();

    let deposit_isk = c_rep.dict_get("depositISK").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let p1_bal_after_create = ctx.db.get_character(P1).unwrap().balance.0;
    rpc(&ctx, &mut s2, "contractProxy", "AcceptContract", &[PyRep::Integer(contract_id)]).await.expect("AcceptContract");
    let wrap_id = ctx.db.with_snapshot(|s| {
        s.items.values().find(|i| i.type_id == 3468 && i.owner_id == P2).map(|i| i.item_id).expect("plastic wrap item")
    });

    let other_jita_station = ctx.sde.stations.values()
        .find(|st| st.solar_system_id == JITA_SYS && st.station_id != JITA_44)
        .expect("second station in Jita");
    rpc(&ctx, &mut s2, "stationSvc", "Undock", &[PyRep::Bool(false)]).await.expect("Undock P2");
    ctx.advance_secs(12);
    ctx.systems.get_or_spawn(JITA_SYS).set_ball_position(s2.ship_id.unwrap(), other_jita_station.position + DVec3::new(200.0, 0.0, 0.0)).await;
    rpc(&ctx, &mut s2, "stationSvc", "Dock", &[PyRep::Integer(other_jita_station.station_id)]).await.expect("Dock other station");
    assert_eq!(rpc(&ctx, &mut s2, "invbroker", "BreakPlasticWrap", &[PyRep::Integer(wrap_id)]).await.unwrap_err().code, "ItemNotAtStation");

    rpc(&ctx, &mut s2, "stationSvc", "Undock", &[PyRep::Bool(false)]).await.expect("Undock other station");
    ctx.advance_secs(12);
    let jita44_pos = ctx.sde.stations[&JITA_44].position;
    ctx.systems.get_or_spawn(JITA_SYS).set_ball_position(s2.ship_id.unwrap(), jita44_pos + DVec3::new(200.0, 0.0, 0.0)).await;
    rpc(&ctx, &mut s2, "stationSvc", "Dock", &[PyRep::Integer(JITA_44)]).await.expect("Dock Jita 4-4");
    ctx.drain_notifications_for_char(P1);
    ctx.drain_notifications_for_char(P2);
    rpc(&ctx, &mut s2, "invbroker", "BreakPlasticWrap", &[PyRep::Integer(wrap_id)]).await.expect("BreakPlasticWrap co-located");

    let p1_bal_after_break = ctx.db.get_character(P1).unwrap().balance.0;
    assert_eq!(
        p1_bal_after_break - p1_bal_after_create,
        IskCents::from_f64_isk(collateral + reward + deposit_isk).0
    );
    assert!(rpc(&ctx, &mut s2, "contractProxy", "CompleteContract", &[PyRep::Integer(contract_id), PyRep::Integer(JITA_44), PyRep::Integer(4)]).await.is_err());
    assert!(ctx.drain_notifications_for_char(P1).iter().any(|n| n.event == "OnContractFailed"));
    assert!(ctx.drain_notifications_for_char(P2).iter().any(|n| n.event == "OnItemChange"));
}

#[tokio::test]
async fn test_corp_role_bits_container_wallet_and_governance() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (mut s1, mut s2) = login_two_docked(&ctx).await;
    let corp_id = create_corp_and_join_p2(&ctx, &mut s1, &mut s2, "Role Precision Corp", "RPCP").await;

    rpc(&ctx, &mut s1, "account", "GiveCash", &[PyRep::Integer(corp_id), PyRep::Real(100_000_000.0), PyRep::String("Div1 seed".into())])
        .await.expect("GiveCash corp");
    rpc(&ctx, &mut s1, "officeManager", "RentOffice", &[PyRep::Integer(JITA_44)]).await.expect("RentOffice");

    let cont_id = create_item_as_gm(&ctx, &mut s1, "demo", P1, 17366, 1).await;
    rpc(&ctx, &mut s1, "invbroker", "AssembleCargoContainer", &[PyRep::Integer(cont_id)])
        .await.expect("Assemble container");
    rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(cont_id), PyRep::Integer(JITA_44), PyRep::Integer(InventoryFlag::CorpSAG7 as i64)])
        .await.expect("Place container in CorpSAG7");
    let loot_id = create_item_as_gm(&ctx, &mut s1, "demo", P1, 34, 10).await;
    rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(loot_id), PyRep::Integer(cont_id), PyRep::Integer(InventoryFlag::Cargo as i64)])
        .await.expect("Place loot inside CorpSAG7 container");

    set_corp_roles(&ctx, &mut s1, &mut s2, P2, corp_roles::ROLE_RENT_OFFICE | corp_roles::ROLE_HANGAR_CAN_TAKE[6]).await;

    assert_eq!(
        rpc(&ctx, &mut s2, "invbroker", "Add", &[PyRep::Integer(loot_id), PyRep::Integer(JITA_44), PyRep::Integer(4)]).await.unwrap_err().code,
        "CorpRoleDenied"
    );
    set_corp_roles(
        &ctx, &mut s1, &mut s2, P2,
        corp_roles::ROLE_RENT_OFFICE | corp_roles::ROLE_HANGAR_CAN_TAKE[6] | corp_roles::ROLE_CONTAINER_CAN_TAKE[6],
    ).await;
    rpc(&ctx, &mut s2, "invbroker", "Add", &[PyRep::Integer(loot_id), PyRep::Integer(JITA_44), PyRep::Integer(4)])
        .await.expect("ROLE_CONTAINER_CAN_TAKE[6] must grant Div 7 container take");

    set_corp_roles(&ctx, &mut s1, &mut s2, P2, corp_roles::ROLE_HANGAR_CAN_QUERY[0] | corp_roles::ROLE_HANGAR_CAN_QUERY[1]).await;
    assert_eq!(rpc(&ctx, &mut s2, "account", "GiveCashFromCorpAccount", &[PyRep::Integer(1), PyRep::Integer(2), PyRep::Real(1_000.0)]).await.unwrap_err().code, "CorpRoleDenied");
    assert_eq!(rpc(&ctx, &mut s2, "corpRegistry", "SetAccountKey", &[PyRep::Integer(1001)]).await.unwrap_err().code, "CorpRoleDenied");

    set_corp_roles(&ctx, &mut s1, &mut s2, P2, corp_roles::ROLE_ACCOUNT_CAN_TAKE[0] | corp_roles::ROLE_ACCOUNT_CAN_TAKE[1]).await;
    rpc(&ctx, &mut s2, "account", "GiveCashFromCorpAccount", &[PyRep::Integer(1), PyRep::Integer(2), PyRep::Real(1_000.0)]).await.expect("wallet take");
    rpc(&ctx, &mut s2, "corpRegistry", "SetAccountKey", &[PyRep::Integer(1001)]).await.expect("SetAccountKey(1001)");

    assert_eq!(corp_roles::ROLE_DIPLOMAT, 1u64 << 34);
    set_corp_roles(&ctx, &mut s1, &mut s2, P2, corp_roles::ROLE_PERSONNEL_MANAGER).await;
    rpc(&ctx, &mut s2, "corpRegistry", "GetApplications", &[]).await.expect("bitwise-ANY personnel manager");
}

#[tokio::test]
async fn test_corp_leave_add_corp_and_direct_fw_require_docked_stasis_and_keep_shares() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (mut s1, mut s2) = login_two_docked(&ctx).await;
    let corp_id = create_corp_and_join_p2(&ctx, &mut s1, &mut s2, "Share Stasis Corp", "SHST").await;

    rpc(&ctx, &mut s1, "corpRegistry", "MoveCompanyShares", &[PyRep::Integer(corp_id), PyRep::Integer(P2), PyRep::Integer(250)]).await.expect("MoveCompanyShares");
    rpc(&ctx, &mut s1, "corpRegistry", "UpdateMember", &[PyRep::Integer(P2), PyRep::Integer(corp_roles::ROLE_TRADER as i64), PyRep::Integer(0)]).await.expect("Grant ROLE_TRADER");

    rpc(&ctx, &mut s2, "stationSvc", "Undock", &[PyRep::Bool(false)]).await.expect("Undock P2");
    ctx.advance_secs(12);
    assert_eq!(rpc(&ctx, &mut s2, "corpRegistry", "ResignFromCorp", &[]).await.unwrap_err().code, "MustBeDocked");
    assert_eq!(rpc(&ctx, &mut s2, "corpRegistry", "AddCorporation", &[PyRep::String("Escape Corp".into()), PyRep::String("ESCP".into())]).await.unwrap_err().code, "MustBeDocked");
    assert_eq!(rpc(&ctx, &mut s2, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)]).await.unwrap_err().code, "MustBeDocked");

    rpc(&ctx, &mut s2, "stationSvc", "Dock", &[PyRep::Integer(JITA_44)]).await.expect("Dock P2");
    ctx.advance_secs(12);
    assert_eq!(rpc(&ctx, &mut s2, "corpRegistry", "ResignFromCorp", &[]).await.unwrap_err().code, "MemberHasRoles");
    assert_eq!(rpc(&ctx, &mut s2, "corpRegistry", "AddCorporation", &[PyRep::String("Escape Corp".into()), PyRep::String("ESCP".into())]).await.unwrap_err().code, "MemberHasRoles");

    rpc(&ctx, &mut s1, "corpRegistry", "UpdateMember", &[PyRep::Integer(P2), PyRep::Integer(0), PyRep::Integer(0)]).await.expect("Clear P2 roles");
    ctx.advance_secs(3_600);
    assert_eq!(rpc(&ctx, &mut s2, "corpRegistry", "ResignFromCorp", &[]).await.unwrap_err().code, "RoleDropCooldownActive");

    ctx.advance_secs(86_400);
    rpc(&ctx, &mut s2, "corpRegistry", "ResignFromCorp", &[]).await.expect("ResignFromCorp after 24h stasis");
    ctx.db.with_snapshot(|s| {
        let sh = s.corp_governance.get(&corp_id).and_then(|g| g.shares.get(&P2).copied()).unwrap_or(0);
        assert_eq!(sh, 250, "Departing shareholder must retain personal shares");
    });
}

#[tokio::test]
async fn test_war_auth_ally_dispatch_hq_validation_and_duplicate_drain() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (mut s1, mut s2) = login_two_docked(&ctx).await;

    let atk_corp = rpc(&ctx, &mut s1, "corpRegistry", "AddCorporation", &[PyRep::String("War Hawks".into()), PyRep::String("WHWK".into())])
        .await.unwrap().dict_get("corporationID").and_then(|v| v.as_i64()).unwrap();
    let def_corp = rpc(&ctx, &mut s2, "corpRegistry", "AddCorporation", &[PyRep::String("War Targets".into()), PyRep::String("WTGT".into())])
        .await.unwrap().dict_get("corporationID").and_then(|v| v.as_i64()).unwrap();
    refresh_session(&ctx, &mut s1, P1).await;
    refresh_session(&ctx, &mut s2, P2).await;

    let (p3, p4_non_dir) = (90_000_083_i64, 90_000_084_i64);
    ctx.db.provision_gm_account("ally3", "ally3-secret", p3, "Ally CEO");
    ctx.db.provision_gm_account("mem4", "mem4-secret", p4_non_dir, "Line Member");
    let mut s3 = SessionState::new(803);
    crate::common::login_pilot(&ctx, &mut s3, "ally3", "ally3-secret").await;
    rpc(&ctx, &mut s3, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(p3)]).await.unwrap();
    grant_skills_and_isk(&ctx, &mut s3, "ally3", p3, &[3363, 3368], 500_000_000).await;

    let mut s4 = SessionState::new(804);
    crate::common::login_pilot(&ctx, &mut s4, "mem4", "mem4-secret").await;
    rpc(&ctx, &mut s4, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(p4_non_dir)]).await.unwrap();
    grant_skills_and_isk(&ctx, &mut s4, "mem4", p4_non_dir, &[3363], 10_000_000).await;

    let app4 = rpc(&ctx, &mut s4, "corpRegistry", "InsertApplication", &[PyRep::Integer(atk_corp), PyRep::String("join".into())])
        .await.expect("p4 InsertApplication");
    let app4_id = app4.dict_get("applicationID").and_then(|v| v.as_i64()).unwrap();
    ctx.advance_secs(12);
    rpc(&ctx, &mut s1, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(app4_id), PyRep::String("Accepted".into())])
        .await.expect("Accept p4");
    refresh_session(&ctx, &mut s4, p4_non_dir).await;

    rpc(&ctx, &mut s1, "account", "GiveCash", &[PyRep::Integer(atk_corp), PyRep::Real(500_000_000.0), PyRep::String("War fund".into())])
        .await.expect("fund atk_corp");
    rpc(&ctx, &mut s2, "account", "GiveCash", &[PyRep::Integer(def_corp), PyRep::Real(500_000_000.0), PyRep::String("Def fund".into())])
        .await.expect("fund def_corp");
    let ally_corp = rpc(&ctx, &mut s3, "corpRegistry", "AddCorporation", &[PyRep::String("Mercenary Wing".into()), PyRep::String("MERC".into())])
        .await.unwrap().dict_get("corporationID").and_then(|v| v.as_i64()).unwrap();
    refresh_session(&ctx, &mut s3, p3).await;
    rpc(&ctx, &mut s3, "account", "GiveCash", &[PyRep::Integer(ally_corp), PyRep::Real(200_000_000.0), PyRep::String("Ally fund".into())])
        .await.expect("fund ally_corp");

    // Advance past 7-day anchoring gate, undock P1 & P2, move to Perimeter, anchor both Astrahus structures.
    ctx.advance_secs(7 * 86_400 + 30);
    rpc(&ctx, &mut s1, "stationSvc", "Undock", &[PyRep::Bool(false)]).await.expect("Undock P1");
    rpc(&ctx, &mut s2, "stationSvc", "Undock", &[PyRep::Bool(false)]).await.expect("Undock P2");
    ctx.advance_secs(12);

    let _ = gm_tr_and_create(&ctx, &mut s1, "demo", P1, PERIMETER_SYS, 35_832, 1).await;
    let hq_id = rpc(&ctx, &mut s1, "structureDeployment", "Anchor", &[PyRep::Integer(35_832), PyRep::String("Hawk HQ".into())])
        .await.expect("Anchor Hawk HQ").dict_get("structureID").and_then(|v| v.as_i64()).unwrap();

    let _ = gm_tr_and_create(&ctx, &mut s2, "other", P2, PERIMETER_SYS, 35_832, 1).await;
    ctx.systems.get_or_spawn(PERIMETER_SYS).set_ball_position(s2.ship_id.unwrap(), DVec3::new(5_000_000.0, 0.0, 0.0)).await;
    let def_hq_id = rpc(&ctx, &mut s2, "structureDeployment", "Anchor", &[PyRep::Integer(35_832), PyRep::String("Target HQ".into())])
        .await.expect("Anchor Target HQ").dict_get("structureID").and_then(|v| v.as_i64()).unwrap();

    ctx.advance_secs(901);
    let _ = rpc(&ctx, &mut s1, "structureGuests", "GetGuests", &[PyRep::Integer(hq_id)]).await;
    let _ = rpc(&ctx, &mut s2, "structureGuests", "GetGuests", &[PyRep::Integer(def_hq_id)]).await;
    ctx.advance_secs(86_401);
    let _ = rpc(&ctx, &mut s1, "structureGuests", "GetGuests", &[PyRep::Integer(hq_id)]).await;
    let _ = rpc(&ctx, &mut s2, "structureGuests", "GetGuests", &[PyRep::Integer(def_hq_id)]).await;

    // Dock P2 at Target HQ and install its quantum core so defender owns an operational structure.
    let perim = ctx.systems.get_or_spawn(PERIMETER_SYS);
    let def_pos = ctx.db.with_snapshot(|s| DVec3::from_array(s.structures[&def_hq_id].position));
    for _ in 0..120 {
        perim.step_manual(1.0).await.unwrap();
        if perim.get_ball_snapshot(s2.ship_id.unwrap()).await.unwrap().is_some_and(|(_, b)| !matches!(b.mode, eve_destiny::MovementMode::Warp { .. })) {
            break;
        }
    }
    let _ = perim.set_movement_mode(s2.ship_id.unwrap(), eve_destiny::MovementMode::Stop).await;
    perim.set_ball_position(s2.ship_id.unwrap(), def_pos).await;
    rpc(&ctx, &mut s2, "structureDocking", "Dock", &[PyRep::Integer(def_hq_id)]).await.expect("Dock def_hq_id");
    let def_core = create_item_as_gm(&ctx, &mut s2, "other", P2, 56_201, 1).await;
    rpc(&ctx, &mut s2, "invbroker", "Add", &[PyRep::Integer(def_core), PyRep::Integer(def_hq_id), PyRep::Integer(InventoryFlag::StructureDeedBay as i64)])
        .await.expect("Install Target HQ core");
    ctx.advance_secs(901);
    let _ = rpc(&ctx, &mut s2, "structureGuests", "GetGuests", &[PyRep::Integer(def_hq_id)]).await;

    // Non-director cannot declare war, and NoCore Hawk HQ fails WarHqRequired.
    assert!(rpc(&ctx, &mut s4, "warRegistry", "DeclareWarAgainst", &[PyRep::Integer(def_corp), PyRep::Integer(hq_id)]).await.is_err());
    assert_eq!(rpc(&ctx, &mut s1, "warRegistry", "DeclareWarAgainst", &[PyRep::Integer(def_corp), PyRep::Integer(hq_id)]).await.unwrap_err().code, "WarHqRequired");

    // Dock P1 at Hawk HQ, install quantum core, bring online -> ShieldVulnerable.
    let hq_pos = ctx.db.with_snapshot(|s| DVec3::from_array(s.structures[&hq_id].position));
    for _ in 0..120 {
        perim.step_manual(1.0).await.unwrap();
        if perim.get_ball_snapshot(s1.ship_id.unwrap()).await.unwrap().is_some_and(|(_, b)| !matches!(b.mode, eve_destiny::MovementMode::Warp { .. })) {
            break;
        }
    }
    let _ = perim.set_movement_mode(s1.ship_id.unwrap(), eve_destiny::MovementMode::Stop).await;
    perim.set_ball_position(s1.ship_id.unwrap(), hq_pos).await;
    rpc(&ctx, &mut s1, "structureDocking", "Dock", &[PyRep::Integer(hq_id)]).await.expect("Dock hq_id");
    let atk_core = create_item_as_gm(&ctx, &mut s1, "demo", P1, 56_201, 1).await;
    rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(atk_core), PyRep::Integer(hq_id), PyRep::Integer(InventoryFlag::StructureDeedBay as i64)])
        .await.expect("Install Hawk HQ core");
    ctx.advance_secs(901);
    let _ = rpc(&ctx, &mut s1, "structureGuests", "GetGuests", &[PyRep::Integer(hq_id)]).await;

    let war_rep = rpc(&ctx, &mut s1, "warRegistry", "DeclareWarAgainst", &[PyRep::Integer(def_corp), PyRep::Integer(hq_id)]).await.expect("DeclareWarAgainst");
    let war_id = war_rep.as_i64().or_else(|| war_rep.dict_get("warID").and_then(|v| v.as_i64())).unwrap();
    assert!(rpc(&ctx, &mut s1, "warRegistry", "DeclareWarAgainst", &[PyRep::Integer(def_corp), PyRep::Integer(hq_id)]).await.is_err());

    rpc(&ctx, &mut s2, "warRegistry", "SetOpenForAllies", &[PyRep::Integer(war_id), PyRep::Bool(true)]).await.expect("SetOpenForAllies");
    let offer_rep = rpc(
        &ctx, &mut s3, "warRegistry", "CreateWarAllyOffer",
        &[PyRep::Integer(war_id), PyRep::Real(10_000_000.0), PyRep::String("Aid".into())],
    ).await.expect("warRegistry.CreateWarAllyOffer");
    let neg_id = offer_rep.as_i64().or_else(|| offer_rep.dict_get("negotiationID").and_then(|v| v.as_i64())).unwrap();
    let ally_bal_after_first = ctx.db.with_snapshot(|s| s.corporations[&ally_corp].division_balance(1));
    assert!(rpc(&ctx, &mut s3, "warRegistry", "CreateWarAllyOffer", &[PyRep::Integer(war_id), PyRep::Real(10_000_000.0), PyRep::String("Dup".into())]).await.is_err());
    assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&ally_corp].division_balance(1)), ally_bal_after_first);

    ctx.drain_notifications_for_char(P1);
    rpc(&ctx, &mut s2, "warRegistry", "AcceptAllyNegotiation", &[PyRep::Integer(neg_id)]).await.expect("warRegistry.AcceptAllyNegotiation");
    let notifs = ctx.drain_notifications_for_char(P1);
    assert!(notifs.iter().any(|n| n.event == "OnWarChanged" && format!("{:?}", n.args).contains("allyID")));

    // Self-destruct Hawk HQ in space -> war ends automatically when GetWars syncs wars.
    rpc(&ctx, &mut s1, "structureDocking", "Undock", &[PyRep::Integer(hq_id)]).await.expect("Undock hq_id");
    ctx.advance_secs(15);
    rpc(&ctx, &mut s1, "beyonce", "CmdSelfDestructStructure", &[PyRep::Integer(hq_id)]).await.expect("SelfDestruct Hawk HQ");
    ctx.advance_secs(1205);
    let _ = rpc(&ctx, &mut s1, "structureGuests", "GetGuests", &[PyRep::Integer(hq_id)]).await;
    let _ = rpc(&ctx, &mut s1, "warRegistry", "GetWars", &[PyRep::Integer(atk_corp)]).await.unwrap();
    assert!(!ctx.db.with_snapshot(|s| s.wars[&war_id].active));
}

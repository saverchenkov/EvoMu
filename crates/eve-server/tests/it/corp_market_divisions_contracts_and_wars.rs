//! Integration tests covering corporate wallet division market accounting, courier plastic wrap
//! unwrapping and collateral forfeiture, corporate role bitmask precision (container, wallet, and
//! governance), corporation departure stasis & personal share retention, and war HQ/ally mechanics.

#![forbid(unsafe_code)]

use eve_db::{
    corp_roles, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord,
    UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const JITA_44: i64 = 60_003_760;
const JITA_SYS: u32 = 30_000_142;
const PERIMETER_SYS: u32 = 30_000_144;
const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
}

async fn login_two_docked(ctx: &ServerContext) -> (SessionState, SessionState) {
    let mut s1 = SessionState::new(801);
    crate::common::login_pilot(ctx, &mut s1, "demo", "demo-secret").await;
    rpc(ctx, &mut s1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P1)]).await.unwrap();
    let mut s2 = SessionState::new(802);
    crate::common::login_pilot(ctx, &mut s2, "other", "other-secret").await;
    rpc(ctx, &mut s2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P2)]).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        for &cid in &[P1, P2] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.balance = IskCents::from_isk(5_000_000_000);
            }
            for &(sk, lvl) in &[(3363_u32, 5_u8), (3368, 5), (3443, 5), (16622, 5)] {
                snap.skills.entry(cid).or_default().insert(sk, TrainedSkillRecord { type_id: sk, level: lvl, skill_points: 256_000.0 });
            }
        }
    });
    ctx.advance_secs(12);
    (s1, s2)
}

async fn create_corp_and_join_p2(ctx: &ServerContext, s1: &mut SessionState, s2: &mut SessionState, name: &str, ticker: &str) -> i64 {
    let rep = rpc(ctx, s1, "corpRegistry", "AddCorporation", &[PyRep::String(name.into()), PyRep::String(ticker.into())]).await.expect("AddCorporation");
    let corp_id = rep.dict_get("corporationID").and_then(|v| v.as_i64()).unwrap();
    ctx.advance_secs(12);
    let app = rpc(ctx, s2, "corpRegistry", "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("join".into())]).await.expect("InsertApplication");
    let app_id = app.dict_get("applicationID").and_then(|v| v.as_i64()).unwrap();
    ctx.advance_secs(12);
    rpc(ctx, s1, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(app_id), PyRep::String("Accepted".into())]).await.expect("Accept application");
    s2.corporation_id = Some(corp_id);
    ctx.advance_secs(12);
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
    let sell_item_id = 880_482_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.retain(|_, o| !matches!(o.type_id, 34 | 35));
        let corp = snap.corporations.get_mut(&corp_id).unwrap();
        corp.set_division_balance(1, div1_init);
        corp.set_division_balance(2, div2_init);
        snap.items.insert(sell_item_id, InventoryItemRecord {
            item_id: sell_item_id, type_id: 34, owner_id: P1, location_id: JITA_44,
            flag: InventoryFlag::Hangar, quantity: 1_000, singleton: false, version: 1, ..Default::default()
        });
    });

    s1.corp_account_key = 1001;
    let buy_rep = rpc(
        &ctx, &mut s1, "marketProxy", "PlaceBuyOrder",
        &[PyRep::Integer(JITA_44), PyRep::Integer(35), PyRep::Real(10_000.0), PyRep::Integer(1_000), PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(true), PyRep::Integer(1001)],
    ).await.expect("PlaceBuyOrder on Div 2");
    let buy_order_id = buy_rep.as_i64().or_else(|| buy_rep.dict_get("orderID").and_then(|v| v.as_i64())).unwrap_or_else(|| {
        ctx.db.with_snapshot(|s| *s.market_orders.iter().find(|(_, o)| o.owner_id == corp_id && o.is_bid).map(|(oid, _)| oid).expect("corp buy order"))
    });

    ctx.db.with_mut_snapshot(|snap| {
        let c = snap.corporations.get_mut(&corp_id).unwrap();
        c.ensure_wallet_divisions();
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
                ("stationID", PyRep::Integer(JITA_44)), ("typeID", PyRep::Integer(34)), ("itemID", PyRep::Integer(sell_item_id)),
                ("price", PyRep::Real(5_000.0)), ("quantity", PyRep::Integer(1_000)), ("duration", PyRep::Integer(90)),
            ])]),
            PyRep::Bool(true), PyRep::Integer(90), PyRep::None, PyRep::Integer(1001),
        ],
    ).await.expect("PlaceMultiSellOrder on Div 2");

    rpc(
        &ctx, &mut s2, "marketProxy", "PlaceBuyOrder",
        &[PyRep::Integer(JITA_44), PyRep::Integer(34), PyRep::Real(5_000.0), PyRep::Integer(1_000), PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(0), PyRep::Bool(false), PyRep::Integer(1000)],
    ).await.expect("P2 immediate fill");

    ctx.db.with_mut_snapshot(|snap| {
        let c = snap.corporations.get_mut(&corp_id).unwrap();
        c.ensure_wallet_divisions();
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

    let cargo_id = 880_485_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(cargo_id, InventoryItemRecord {
            item_id: cargo_id, type_id: 34, owner_id: P1, location_id: JITA_44,
            flag: InventoryFlag::Hangar, quantity: 500, singleton: false, version: 1, ..Default::default()
        });
    });

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

    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&P2).unwrap().station_id = Some(60_003_763));
    s2.station_id = Some(60_003_763);
    assert_eq!(rpc(&ctx, &mut s2, "invbroker", "BreakPlasticWrap", &[PyRep::Integer(wrap_id)]).await.unwrap_err().code, "ItemNotAtStation");

    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&P2).unwrap().station_id = Some(JITA_44));
    s2.station_id = Some(JITA_44);
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

    let (cont_id, loot_id) = (880_490_001_i64, 880_490_002_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().set_division_balance(1, IskCents::from_isk(100_000_000));
        snap.items.insert(cont_id, InventoryItemRecord {
            item_id: cont_id, type_id: 17366, owner_id: corp_id, location_id: JITA_44,
            flag: InventoryFlag::CorpSAG7, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
        snap.items.insert(loot_id, InventoryItemRecord {
            item_id: loot_id, type_id: 34, owner_id: corp_id, location_id: cont_id,
            flag: InventoryFlag::Unlocked, quantity: 10, singleton: false, version: 1, ..Default::default()
        });
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(
            P2, corp_roles::ROLE_RENT_OFFICE | corp_roles::ROLE_HANGAR_CAN_TAKE[6],
        );
    });

    assert_eq!(
        rpc(&ctx, &mut s2, "invbroker", "Add", &[PyRep::Integer(loot_id), PyRep::Integer(JITA_44), PyRep::Integer(4)]).await.unwrap_err().code,
        "CorpRoleDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        *snap.corporations.get_mut(&corp_id).unwrap().member_roles.entry(P2).or_default() |= corp_roles::ROLE_CONTAINER_CAN_TAKE[6];
    });
    rpc(&ctx, &mut s2, "invbroker", "Add", &[PyRep::Integer(loot_id), PyRep::Integer(JITA_44), PyRep::Integer(4)])
        .await.expect("ROLE_CONTAINER_CAN_TAKE[6] must grant Div 7 container take");

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(
            P2, corp_roles::ROLE_HANGAR_CAN_QUERY[0] | corp_roles::ROLE_HANGAR_CAN_QUERY[1],
        );
    });
    assert_eq!(rpc(&ctx, &mut s2, "account", "GiveCashFromCorpAccount", &[PyRep::Integer(1), PyRep::Integer(2), PyRep::Real(1_000.0)]).await.unwrap_err().code, "CorpRoleDenied");
    assert_eq!(rpc(&ctx, &mut s2, "corpRegistry", "SetAccountKey", &[PyRep::Integer(1001)]).await.unwrap_err().code, "CorpRoleDenied");

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(
            P2, corp_roles::ROLE_ACCOUNT_CAN_TAKE[0] | corp_roles::ROLE_ACCOUNT_CAN_TAKE[1],
        );
    });
    rpc(&ctx, &mut s2, "account", "GiveCashFromCorpAccount", &[PyRep::Integer(1), PyRep::Integer(2), PyRep::Real(1_000.0)]).await.expect("wallet take");
    rpc(&ctx, &mut s2, "corpRegistry", "SetAccountKey", &[PyRep::Integer(1001)]).await.expect("SetAccountKey(1001)");

    assert_eq!(corp_roles::ROLE_DIPLOMAT, 1u64 << 34);
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(P2, corp_roles::ROLE_PERSONNEL_MANAGER);
    });
    s2.corp_role = corp_roles::ROLE_PERSONNEL_MANAGER;
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

    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&P2).unwrap();
        ch.station_id = None;
        ch.active_ship_id = 100_000_002;
    });
    s2.station_id = None;
    s2.ship_id = Some(100_000_002);
    assert_eq!(rpc(&ctx, &mut s2, "corpRegistry", "ResignFromCorp", &[]).await.unwrap_err().code, "MustBeDocked");
    assert_eq!(rpc(&ctx, &mut s2, "corpRegistry", "AddCorporation", &[PyRep::String("Escape Corp".into()), PyRep::String("ESCP".into())]).await.unwrap_err().code, "MustBeDocked");
    assert_eq!(rpc(&ctx, &mut s2, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_001)]).await.unwrap_err().code, "MustBeDocked");

    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&P2).unwrap().station_id = Some(JITA_44));
    s2.station_id = Some(JITA_44);
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

    let (p3, p4_non_dir, hq_id, def_hq_id) = (90_000_083_i64, 90_000_084_i64, 880_506_100_i64, 880_506_101_i64);
    ctx.db.provision_gm_account("ally3", "ally3-secret", p3, "Ally CEO");
    ctx.db.provision_gm_account("mem4", "mem4-secret", p4_non_dir, "Line Member");
    let mut s3 = SessionState::new(803);
    crate::common::login_pilot(&ctx, &mut s3, "ally3", "ally3-secret").await;
    rpc(&ctx, &mut s3, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(p3)]).await.unwrap();
    let mut s4 = SessionState::new(804);
    crate::common::login_pilot(&ctx, &mut s4, "mem4", "mem4-secret").await;
    rpc(&ctx, &mut s4, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(p4_non_dir)]).await.unwrap();
    ctx.advance_secs(12);

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&p3).unwrap().balance = IskCents::from_isk(500_000_000);
        snap.skills.entry(p3).or_default().insert(3363, TrainedSkillRecord { type_id: 3363, level: 5, skill_points: 256_000.0 });
        snap.characters.get_mut(&p4_non_dir).unwrap().corporation_id = atk_corp;
        snap.corporations.get_mut(&atk_corp).unwrap().members.insert(p4_non_dir);
        snap.corporations.get_mut(&atk_corp).unwrap().set_division_balance(1, IskCents::from_isk(500_000_000));
        snap.corporations.get_mut(&def_corp).unwrap().set_division_balance(1, IskCents::from_isk(500_000_000));
        snap.structures.insert(hq_id, UpwellStructureRecord {
            structure_id: hq_id, type_id: 35_832, owner_corp_id: atk_corp, solar_system_id: PERIMETER_SYS,
            name: "Hawk HQ".into(), state: "ShieldVulnerable".into(), quantum_core_installed: false,
            shield_hp: 1e5, armor_hp: 1e5, hull_hp: 1e5, ..Default::default()
        });
        snap.structures.insert(def_hq_id, UpwellStructureRecord {
            structure_id: def_hq_id, type_id: 35_832, owner_corp_id: def_corp, solar_system_id: PERIMETER_SYS,
            name: "Target HQ".into(), state: "ShieldVulnerable".into(), quantum_core_installed: true,
            shield_hp: 1e5, armor_hp: 1e5, hull_hp: 1e5, ..Default::default()
        });
    });
    s4.corporation_id = Some(atk_corp);
    s4.corp_role = 0;
    let ally_corp = rpc(&ctx, &mut s3, "corpRegistry", "AddCorporation", &[PyRep::String("Mercenary Wing".into()), PyRep::String("MERC".into())])
        .await.unwrap().dict_get("corporationID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&ally_corp).unwrap().set_division_balance(1, IskCents::from_isk(200_000_000));
    });

    assert!(rpc(&ctx, &mut s4, "warRegistry", "DeclareWarAgainst", &[PyRep::Integer(def_corp), PyRep::Integer(hq_id)]).await.is_err());
    assert_eq!(rpc(&ctx, &mut s1, "warRegistry", "DeclareWarAgainst", &[PyRep::Integer(def_corp), PyRep::Integer(hq_id)]).await.unwrap_err().code, "WarHqRequired");

    ctx.db.with_mut_snapshot(|s| s.structures.get_mut(&hq_id).unwrap().quantum_core_installed = true);
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

    ctx.db.with_mut_snapshot(|s| s.structures.get_mut(&hq_id).unwrap().quantum_core_installed = false);
    let _ = rpc(&ctx, &mut s1, "warRegistry", "GetWars", &[PyRep::Integer(atk_corp)]).await.unwrap();
    assert!(!ctx.db.with_snapshot(|s| s.wars[&war_id].active));
}

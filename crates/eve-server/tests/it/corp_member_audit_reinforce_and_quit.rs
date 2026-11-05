//! Live corporation lifecycle tests driven only by player RPCs (no world-state injection):
//! - `feat:corporation.member-audit`: `corpRegistry.UpdateMember` history read back through
//!   `corpmgr.AuditMember` with target, issuer, old/new roles and change timestamps.
//! - `feat:corporation.structure-reinforce-default`: role-gated `SetStructureReinforceDefault`,
//!   adopted by a freshly anchored and onlined corp Raitaru (`structureDirectory.GetStructureInfo`).
//! - `feat:lifecycle.corp-quit-cleanup`: self-quit (`KickOutMember(self)`) moves the pilot to its
//!   SDE bloodline NPC corporation and clears titles, pending role grants, account key and corp chat.

#![forbid(unsafe_code)]

use crate::common;
use crate::industry_live_support::{
    board_loaded_freighter, deploy_online_upwell, fund_pilots, int, login, rpc, train_skills, upwell_cargo, PERIMETER_SYS,
    PILOT_1, PILOT_2,
};
use eve_db::corp_roles;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::world_seeder::seed_full_market_catalog;

pub(crate) async fn found_corp_with_member(ctx: &ServerContext, s1: &mut eve_machonet::SessionState, s2: &mut eve_machonet::SessionState, ticker: &str) -> i64 {
    rpc(ctx, s1, "corpRegistry", "AddCorporation", &[PyRep::String(format!("{ticker} Holdings")), PyRep::String(ticker.into())])
        .await
        .expect("AddCorporation");
    let corp_id = s1.corporation_id.unwrap();
    let app = rpc(ctx, s2, "corpRegistry", "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("o7".into())])
        .await
        .expect("InsertApplication");
    ctx.advance_secs(12);
    rpc(ctx, s1, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(int(&app, "applicationID")), PyRep::String("Accepted".into())])
        .await
        .expect("Accept application");
    s2.corporation_id = Some(corp_id);
    corp_id
}

#[tokio::test]
async fn test_live_member_audit_history_and_corp_quit_cleanup() {
    // parity: feat:lifecycle.corp-quit-cleanup
    eve_testkit::attest(&["feat:lifecycle.corp-quit-cleanup"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    fund_pilots(&ctx, &[(PILOT_1, 5_000_000_000.0), (PILOT_2, 100_000_000.0)]).await;
    let mut s1 = login(&ctx, 8901, "demo", PILOT_1).await;
    let mut s2 = login(&ctx, 8902, "other", PILOT_2).await;
    let corp_id = found_corp_with_member(&ctx, &mut s1, &mut s2, "AUDT").await;

    // Two role changes by the CEO, then the audit trail read back in order.
    ctx.advance_secs(120);
    let t1 = ctx.now_unix() as i64;
    let r1 = corp_roles::ROLE_ACCOUNTANT;
    rpc(&ctx, &mut s1, "corpRegistry", "UpdateMember", &[PyRep::Integer(PILOT_2), PyRep::Integer(r1 as i64)]).await.expect("grant Accountant");
    ctx.advance_secs(300);
    let t2 = ctx.now_unix() as i64;
    let r2 = r1 | corp_roles::ROLE_TRADER;
    rpc(&ctx, &mut s1, "corpRegistry", "UpdateMember", &[PyRep::Integer(PILOT_2), PyRep::Integer(r2 as i64)]).await.expect("add Trader");
    let audit = rpc(&ctx, &mut s1, "corpmgr", "AuditMember", &[PyRep::Integer(PILOT_2)]).await.expect("AuditMember");
    let PyRep::Rowset { columns, rows } = &audit else { panic!("AuditMember rowset, got {audit:?}") };
    let col = |n: &str| columns.iter().position(|c| c == n).unwrap_or_else(|| panic!("column {n}"));
    let hist: Vec<(i64, i64, i64, i64, i64)> = rows
        .iter()
        .map(|r| {
            let g = |n: &str| r[col(n)].as_i64().unwrap();
            (g("characterID"), g("issuerID"), g("oldRoles"), g("newRoles"), g("changeTime"))
        })
        .collect();
    assert_eq!(
        hist,
        vec![(PILOT_2, PILOT_1, 0, r1 as i64, t1), (PILOT_2, PILOT_1, r1 as i64, r2 as i64, t2)],
        "audit rows carry target, issuer, old/new roles and change time"
    );
    let filtered = rpc(&ctx, &mut s1, "corpmgr", "AuditMember", &[PyRep::Integer(PILOT_1)]).await.expect("AuditMember CEO");
    assert!(matches!(&filtered, PyRep::Rowset { rows, .. } if rows.is_empty()), "history is filtered by target, not issuer");
    let denied = rpc(&ctx, &mut s2, "corpmgr", "AuditMember", &[PyRep::Integer(PILOT_2)]).await.expect_err("non-director audit");
    assert_eq!(denied.code, "CorpRoleDenied");

    // Quit-cleanup preconditions created through real RPCs: title with a delayed hangar role,
    // a pending (24h-delayed) hangar role grant, an account key and corp chat membership.
    let take = corp_roles::ROLE_HANGAR_CAN_TAKE;
    rpc(&ctx, &mut s1, "corpRegistry", "UpdateTitle", &[PyRep::Integer(1), PyRep::String("Hangar Crew".into()), PyRep::Integer(take[0] as i64), PyRep::Integer(0)])
        .await
        .expect("UpdateTitle");
    rpc(&ctx, &mut s1, "corpRegistry", "UpdateMember", &[PyRep::Integer(PILOT_2), PyRep::Integer((r2 | take[1]) as i64), PyRep::Integer(1)])
        .await
        .expect("grant title + hangar role");
    s2.corp_role = r2;
    ctx.advance_secs(15);
    rpc(&ctx, &mut s2, "corpRegistry", "SetAccountKey", &[PyRep::Integer(1001)]).await.expect("SetAccountKey 1001");
    let corp_chan = format!("corpid:{corp_id}");
    ctx.db.with_snapshot(|s| {
        let g = s.corp_governance.get(&corp_id).expect("corp governance");
        assert_eq!(g.member_titles.get(&PILOT_2).copied(), Some(1));
        assert!(g.pending_role_grants.contains_key(&PILOT_2), "hangar role grant is pending (24h delay)");
        assert_eq!(g.member_account_keys.get(&PILOT_2).copied(), Some(1001));
        assert!(s.chat_channels[&corp_chan].members.contains(&PILOT_2));
    });

    ctx.advance_secs(15);
    let quit = rpc(&ctx, &mut s2, "corpRegistry", "KickOutMember", &[PyRep::Integer(PILOT_2)]).await.expect("self-quit");
    assert_eq!(quit.dict_get("removed").and_then(|v| v.as_bool()), Some(true));
    let (bloodline, new_corp) = ctx.db.with_snapshot(|s| (s.characters[&PILOT_2].bloodline_id, s.characters[&PILOT_2].corporation_id));
    let expected_npc = ctx.sde.bloodline(bloodline).map(|b| b.corporation_id).expect("SDE bloodline corporation");
    assert!(expected_npc > 0);
    assert_eq!(new_corp, expected_npc, "self-quit lands in the bloodline NPC corporation");
    assert_eq!(s2.corporation_id, Some(expected_npc));
    ctx.db.with_snapshot(|s| {
        let g = s.corp_governance.get(&corp_id).expect("corp governance survives (CEO remains)");
        assert!(!g.member_titles.contains_key(&PILOT_2), "titles cleared");
        assert!(!g.pending_role_grants.contains_key(&PILOT_2), "pending role grants cleared");
        assert!(!g.pending_title_role_grants.contains_key(&PILOT_2), "pending title role grants cleared");
        assert!(!g.member_account_keys.contains_key(&PILOT_2), "account key cleared");
        assert!(!s.corporations[&corp_id].members.contains(&PILOT_2));
        assert!(!s.chat_channels[&corp_chan].members.contains(&PILOT_2), "left corp channel");
        assert!(s.chat_channels[&format!("corpid:{expected_npc}")].members.contains(&PILOT_2), "joined NPC corp channel");
    });
}

#[tokio::test]
async fn test_live_structure_reinforce_default_adopted_by_new_structure() {

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    seed_full_market_catalog(&ctx.db, &ctx.sde);
    fund_pilots(&ctx, &[(PILOT_1, 60_000_000_000.0), (PILOT_2, 100_000_000.0)]).await;
    let mut s1 = login(&ctx, 8903, "demo", PILOT_1).await;
    let mut s2 = login(&ctx, 8904, "other", PILOT_2).await;
    train_skills(&ctx, &mut s1, &[20_185, 35_825], true).await;
    found_corp_with_member(&ctx, &mut s1, &mut s2, "RNFD").await;
    ctx.advance_virtual_clock(8 * 86_400);

    let denied = rpc(&ctx, &mut s2, "corpRegistry", "SetStructureReinforceDefault", &[PyRep::Integer(3)])
        .await
        .expect_err("member without Director role");
    assert_eq!(denied.code, "CorpRoleDenied");
    let set = rpc(&ctx, &mut s1, "corpRegistry", "SetStructureReinforceDefault", &[PyRep::Integer(19)]).await.expect("CEO sets default");
    assert_eq!(set.as_i64(), Some(19));
    let got = rpc(&ctx, &mut s2, "corpRegistry", "GetStructureReinforceDefault", &[]).await.expect("GetStructureReinforceDefault");
    assert_eq!(got.as_i64(), Some(19));

    let mut cargo = upwell_cargo(&ctx, 35_825, &[]);
    cargo.push((4_051, 200));
    board_loaded_freighter(&ctx, &mut s1, &cargo).await;
    let (raitaru, _) = deploy_online_upwell(&ctx, &mut s1, PERIMETER_SYS, 35_825, &[], 200).await;
    let info = rpc(&ctx, &mut s1, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(raitaru)]).await.expect("GetStructureInfo");
    assert_eq!(info.dict_get("reinforceHour").and_then(|v| v.as_i64()), Some(19), "new corp structure adopts the corp default hour");
}
